//! Author avatars (#64): pick a size, then hand the URL to the shared
//! image cache.
//!
//! The downloading, caching and file-naming all live in
//! [`crate::image_cache`], shared with post media (#65). What is
//! avatar-specific — and so stays here — is [`preferred_url`], the choice
//! of which size variant to ask X for, and the fallback when that guess is
//! wrong.

use std::path::PathBuf;

use anyhow::Result;

use crate::image_cache;
use crate::paths::Paths;

/// Upgrade X's default `_normal` (48x48) avatar URL to the 400x400 variant,
/// which is what a Retina display needs to render a 44pt circle without
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

/// The local path for `url`'s avatar, downloading it first if it isn't
/// cached yet (#64).
///
/// Tries [`preferred_url`]'s larger variant first and falls back to the URL
/// exactly as the API gave it, since the size-suffix convention is X's own
/// and not promised by anything. Both are cached under their own key, so a
/// fallback costs one extra request once, not once per render.
pub(crate) fn ensure_cached(paths: &Paths, url: &str) -> Result<PathBuf> {
    let dir = paths.avatar_dir();
    let preferred = preferred_url(url);
    match image_cache::ensure_cached(&dir, &preferred) {
        Ok(path) => Ok(path),
        Err(error) if preferred != url => {
            // The larger variant is a guess; the API's own URL is not.
            image_cache::ensure_cached(&dir, url).map_err(|fallback_error| {
                fallback_error.context(format!("the {preferred} variant also failed: {error:#}"))
            })
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
