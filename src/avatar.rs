//! Author avatars (#64): pick a size, name a cache file, download once.
//!
//! Avatars come from `pbs.twimg.com`, not the X API, so fetching one costs
//! **no API quota and no credits** — but it is still an HTTP request, so it
//! happens off the UI thread and only once per URL: [`ensure_cached`]
//! returns immediately when the file is already on disk. A timeline where
//! the same author posts ten times downloads one image, not ten.
//!
//! The pure seams carrying this module's coverage are [`preferred_url`]
//! (which size to ask for) and [`cache_key`] (what to call the file).
//! [`ensure_cached`] itself touches the network and disk and is not
//! unit-tested, the convention `cache::reload` and `repost::create` already
//! follow — the project's tests never make a network request.

use std::fmt::Write as _;
use std::io::Read as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use sha2::{Digest as _, Sha256};

use crate::paths::Paths;

/// Give up on an avatar that hasn't arrived by then. Deliberately short:
/// nothing depends on it, the row renders a placeholder meanwhile, and a
/// hung connection must not keep a background task alive indefinitely.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Refuse anything larger than this. A profile image is tens of kilobytes;
/// a response far past that is a redirect to something else, a server
/// error page, or a resource this app has no business writing to disk.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Upgrade X's default `_normal` (48×48) avatar URL to the 400×400 variant,
/// which is what a Retina display needs to render a 48pt circle without
/// blurring.
///
/// The suffix convention is X's, not a documented API guarantee, so this
/// only ever rewrites a URL that actually ends in `_normal.<ext>` and
/// leaves everything else exactly as it came. A rewritten URL that turns
/// out not to exist is handled one level up: [`ensure_cached`] falls back
/// to the original URL rather than leaving the row without an avatar.
pub(crate) fn preferred_url(url: &str) -> String {
    let Some((stem, extension)) = url.rsplit_once('.') else {
        return url.to_string();
    };
    match stem.strip_suffix("_normal") {
        Some(base) => format!("{base}_400x400.{extension}"),
        None => url.to_string(),
    }
}

/// The file name one avatar URL is cached under.
///
/// A hash of the whole URL, not the URL itself: avatar URLs contain `/`
/// and query strings, and X reuses the same basename (`image_normal.jpg`)
/// across different accounts, so anything shorter would either be an
/// invalid file name or collide between users. The extension is carried
/// over when the URL has a plausible one so the file stays recognizable to
/// anything that opens the cache directory by hand; anything unexpected
/// falls back to `.img` rather than being trusted as a path component.
pub(crate) fn cache_key(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    let mut key = String::with_capacity(digest.len() * 2 + 5);
    for byte in digest {
        // Infallible: writing to a String never fails.
        let _ = write!(key, "{byte:02x}");
    }
    key.push('.');
    key.push_str(extension_of(url));
    key
}

/// The extension to give a cached avatar: the URL's own when it is a short,
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

/// Where one avatar URL's image lives on disk, whether or not it is there
/// yet.
pub(crate) fn cached_path(paths: &Paths, url: &str) -> PathBuf {
    paths.avatar_dir().join(cache_key(url))
}

/// The local path for `url`'s avatar, downloading it first if it isn't
/// cached yet (#64).
///
/// Tries [`preferred_url`]'s larger variant first and falls back to the URL
/// exactly as the API gave it, since the size-suffix convention is X's own
/// and not promised by anything. Both are cached under their own key, so a
/// fallback costs one extra request once, not once per render.
///
/// Not unit-tested — it makes a real HTTP request. [`preferred_url`] and
/// [`cache_key`] carry the coverage.
pub(crate) fn ensure_cached(paths: &Paths, url: &str) -> Result<PathBuf> {
    let preferred = preferred_url(url);
    match fetch_to_cache(paths, &preferred) {
        Ok(path) => Ok(path),
        Err(error) if preferred != url => {
            // The larger variant is a guess; the API's own URL is not.
            fetch_to_cache(paths, url).map_err(|fallback_error| {
                fallback_error.context(format!("the {preferred} variant also failed: {error:#}"))
            })
        }
        Err(error) => Err(error),
    }
}

/// Download one exact URL into the avatar cache, or return the path
/// straight away when it is already there.
fn fetch_to_cache(paths: &Paths, url: &str) -> Result<PathBuf> {
    let path = cached_path(paths, url);
    if path.exists() {
        return Ok(path);
    }

    let dir = paths.avatar_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;

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

    // --- preferred_url ---

    #[test]
    fn upgrades_the_normal_suffix_to_the_larger_variant() {
        assert_eq!(
            preferred_url("https://pbs.twimg.com/profile_images/1/abc_normal.jpg"),
            "https://pbs.twimg.com/profile_images/1/abc_400x400.jpg"
        );
    }

    #[test]
    fn leaves_a_url_without_the_normal_suffix_alone() {
        // The suffix convention is X's, not a documented guarantee, so
        // anything unfamiliar is passed through untouched.
        assert_eq!(
            preferred_url("https://pbs.twimg.com/profile_images/1/abc.jpg"),
            "https://pbs.twimg.com/profile_images/1/abc.jpg"
        );
        assert_eq!(
            preferred_url("https://pbs.twimg.com/profile_images/1/abc_bigger.png"),
            "https://pbs.twimg.com/profile_images/1/abc_bigger.png"
        );
    }

    #[test]
    fn leaves_a_url_with_no_extension_alone() {
        assert_eq!(
            preferred_url("https://example.com/avatar"),
            "https://example.com/avatar"
        );
    }

    // --- cache_key ---

    #[test]
    fn the_cache_key_is_stable_for_one_url() {
        let url = "https://pbs.twimg.com/profile_images/1/abc_normal.jpg";
        assert_eq!(cache_key(url), cache_key(url));
    }

    #[test]
    fn different_urls_get_different_cache_keys() {
        // X reuses the same basename across accounts, so hashing the whole
        // URL is what keeps two authors' avatars apart.
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

    // --- cached_path ---

    #[test]
    fn a_cached_avatar_lives_under_the_avatar_dir() {
        let paths =
            Paths::from_vars(|key| (key == "HOME").then(|| "/home/alice".to_string())).unwrap();
        let path = cached_path(&paths, "https://pbs.twimg.com/a/b_normal.jpg");
        assert_eq!(path.parent().unwrap(), paths.avatar_dir());
    }
}
