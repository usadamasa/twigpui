//! Downloading and caching remote images (#64, #65).
//!
//! Avatars (`avatar.rs`) and post media (`ui.rs`'s media grid) both need
//! the same thing: fetch a URL once, keep the bytes on disk, hand back a
//! path `gpui::img` can render. Neither is X API traffic — both go to
//! `pbs.twimg.com` — so **no quota and no credits**, but they are still
//! HTTP requests, so they happen off the UI thread and only once per URL:
//! [`ensure_cached`] returns immediately when the file is already there.
//!
//! What differs between the two callers is only *which directory* the file
//! lands in, which is why that is a parameter here rather than two copies
//! of this module.
//!
//! [`cache_key`] is the pure seam carrying the coverage; [`ensure_cached`]
//! touches the network and disk and is not unit-tested, the convention
//! `cache::reload` and `repost::create` already follow — the project's
//! tests never make a network request.

use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use sha2::{Digest as _, Sha256};

/// Give up on an image that hasn't arrived by then. Deliberately short:
/// nothing depends on it, the row renders a placeholder meanwhile, and a
/// hung connection must not keep a background task alive indefinitely.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Refuse anything larger than this. A profile image is tens of kilobytes
/// and a timeline photo a few hundred; a response far past that is a
/// redirect to something else, a server error page, or a resource this app
/// has no business writing to disk.
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// The file name one image URL is cached under.
///
/// A hash of the whole URL, not the URL itself: these URLs contain `/` and
/// query strings, and X reuses the same basename (`image_normal.jpg`)
/// across different accounts, so anything shorter would either be an
/// invalid file name or collide between users. The extension is carried
/// over when the URL has a plausible one so the file stays recognizable to
/// anything that opens the cache directory by hand; anything unexpected
/// falls back to `.img` rather than being trusted as a path component.
pub(crate) fn cache_key(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    // Capacity hint only: two hex characters per byte, plus a dot and a
    // short extension. Saturating because it is an optimization, not a
    // bound — an overflow here would be a wrong allocation size, not a
    // wrong key (#47).
    let mut key = String::with_capacity(digest.len().saturating_mul(2).saturating_add(5));
    for byte in digest {
        // Infallible: writing to a String never fails.
        let _ = write!(key, "{byte:02x}");
    }
    key.push('.');
    key.push_str(extension_of(url));
    key
}

/// The extension to give a cached image: the URL's own when it is a short,
/// purely alphanumeric one (`jpg`, `png`, `webp`), else `img`. Never
/// trusted verbatim — an extension is part of a file name this code then
/// writes to.
fn extension_of(url: &str) -> &str {
    let candidate = url
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, extension)| extension)
        .unwrap_or_default();
    let plausible = !candidate.is_empty()
        && candidate.len() <= 5
        && candidate.chars().all(|c| c.is_ascii_alphanumeric());
    if plausible { candidate } else { "img" }
}

/// Where one image URL lives inside `dir`, whether or not it is there yet.
pub(crate) fn cached_path(dir: &Path, url: &str) -> PathBuf {
    dir.join(cache_key(url))
}

/// The local path for `url`, downloading it into `dir` first if it isn't
/// cached yet.
///
/// Not unit-tested — it makes a real HTTP request. [`cache_key`] carries
/// the coverage.
pub(crate) fn ensure_cached(dir: &Path, url: &str) -> Result<PathBuf> {
    let path = cached_path(dir, url);
    if path.exists() {
        return Ok(path);
    }

    std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(FETCH_TIMEOUT))
        .build()
        .into();
    let mut response = agent
        .get(url)
        .call()
        .with_context(|| format!("could not fetch {url}"))?;

    let status = response.status().as_u16();
    if status != 200 {
        bail!("{url} answered {status}");
    }

    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_BYTES)
        .read_to_end(&mut bytes)
        .with_context(|| format!("could not read the image body from {url}"))?;
    if bytes.is_empty() {
        bail!("{url} returned an empty body");
    }

    std::fs::write(&path, &bytes).with_context(|| format!("could not write {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_key_is_stable_for_one_url() {
        let url = "https://pbs.twimg.com/profile_images/1/abc_normal.jpg";
        assert_eq!(cache_key(url), cache_key(url));
    }

    #[test]
    fn different_urls_get_different_cache_keys() {
        // X reuses the same basename across accounts, so hashing the whole
        // URL is what keeps two images apart.
        assert_ne!(
            cache_key("https://pbs.twimg.com/profile_images/1/image_normal.jpg"),
            cache_key("https://pbs.twimg.com/profile_images/2/image_normal.jpg")
        );
    }

    #[test]
    fn the_cache_key_is_a_single_path_component() {
        let key = cache_key("https://pbs.twimg.com/profile_images/1/abc_normal.jpg");
        assert!(!key.contains('/'), "{key} must not be a nested path");
        assert!(!key.contains(".."), "{key} must not escape the cache dir");
    }

    #[test]
    fn the_cache_key_keeps_a_plausible_extension() {
        assert_eq!(
            cache_key("https://pbs.twimg.com/a/b_normal.jpg")
                .rsplit_once('.')
                .map(|(_, extension)| extension),
            Some("jpg")
        );
        assert_eq!(
            cache_key("https://pbs.twimg.com/a/b_normal.png")
                .rsplit_once('.')
                .map(|(_, extension)| extension),
            Some("png")
        );
    }

    #[test]
    fn an_implausible_extension_falls_back_to_img() {
        // Never trusted verbatim: this becomes part of a file name that is
        // then written to.
        for url in [
            "https://example.com/a/b.php?x=/../../etc",
            "https://example.com/avatar",
        ] {
            assert_eq!(
                cache_key(url).rsplit_once('.').map(|(_, ext)| ext),
                Some("img"),
                "{url}"
            );
        }
    }

    #[test]
    fn a_cached_image_lives_in_the_directory_it_was_given() {
        let dir = Path::new("/tmp/twigpui-images");
        assert_eq!(
            cached_path(dir, "https://pbs.twimg.com/a/b.jpg").parent(),
            Some(dir)
        );
    }
}
