//! Opening a URL in the user's browser (#70).
//!
//! The one non-obvious rule here is that **nothing goes to a shell**.
//! `open(1)` is invoked directly through [`std::process::Command`], which
//! `exec`s it with an argument vector — no `sh -c`, so no quoting, globbing
//! or word splitting can happen to a URL that ultimately came from a post's
//! text. The crate forbids `unsafe_code`, so `NSWorkspace` is out anyway;
//! this is the straightforward path the issue asks for.
//!
//! [`is_openable`] is the pure seam carrying this module's test coverage:
//! it decides what is allowed to reach `open` at all. [`open`] itself just
//! spawns the process.

use std::process::Command;

use anyhow::{Result, bail};

/// Whether `url` may be handed to `open(1)`.
///
/// Only `http://` and `https://` pass. This is not decoration: `open` will
/// happily act on a local path, an arbitrary `x-…://` scheme registered by
/// some other app, or — most of all — a leading `-`, which it would read as
/// one of its own flags rather than as a target. A post's text and a
/// `expanded_url` from the API are both *remote input*, so the allowed set
/// is stated positively (these two schemes) rather than by listing things
/// to reject.
///
/// The scheme match is case-insensitive per RFC 3986 §3.1, and something
/// has to follow the scheme — a bare `https://` opens nothing useful and is
/// more likely a parsing accident than an intent.
pub(crate) fn is_openable(url: &str) -> bool {
    let Some(rest) = strip_scheme(url) else {
        return false;
    };
    !rest.is_empty()
}

/// The part of `url` after `http://` or `https://`, or `None` when it
/// carries neither scheme.
fn strip_scheme(url: &str) -> Option<&str> {
    for scheme in ["https://", "http://"] {
        if url.len() >= scheme.len() && url[..scheme.len()].eq_ignore_ascii_case(scheme) {
            return Some(&url[scheme.len()..]);
        }
    }
    None
}

/// Hand `url` to the system browser via `open(1)`, after [`is_openable`]
/// has approved it.
///
/// Spawned rather than waited on: the app has nothing to do with `open`'s
/// exit status, and blocking a gpui click handler on another process is
/// exactly the kind of stall #57 spent effort removing elsewhere. A refused
/// URL is an error rather than a silent no-op, so `ui.rs` can say something
/// instead of the click appearing to do nothing.
///
/// Not unit-tested — it starts a real process. [`is_openable`] carries the
/// coverage, mirroring the convention `cache::reload` and `repost::create`
/// already follow.
pub(crate) fn open(url: &str) -> Result<()> {
    if !is_openable(url) {
        bail!("refusing to open a non-http(s) URL: {url}");
    }
    // `arg`, not a shell string: `open` is exec'd with this as argv[1]
    // verbatim, so nothing in the URL can be interpreted as syntax.
    Command::new("open")
        .arg(url)
        .spawn()
        .map(|_child| ())
        .map_err(|error| anyhow::anyhow!("could not launch the browser: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_an_ordinary_https_url() {
        assert!(is_openable(
            "https://x.com/XDevelopers/status/1700000000000000001"
        ));
    }

    #[test]
    fn allows_http_as_well_as_https() {
        assert!(is_openable("http://example.com/"));
    }

    #[test]
    fn the_scheme_match_is_case_insensitive() {
        assert!(is_openable("HTTPS://example.com/"));
    }

    #[test]
    fn refuses_a_bare_scheme_with_nothing_after_it() {
        assert!(!is_openable("https://"));
    }

    #[test]
    fn refuses_a_leading_dash_that_open_would_read_as_a_flag() {
        // The reason this check exists at all: `open -a Calculator` is a
        // command, not a URL, and post text is remote input.
        assert!(!is_openable("-a Calculator"));
    }

    #[test]
    fn refuses_a_local_path() {
        assert!(!is_openable("/etc/passwd"));
        assert!(!is_openable("file:///etc/passwd"));
    }

    #[test]
    fn refuses_an_unregistered_or_app_specific_scheme() {
        assert!(!is_openable("javascript:alert(1)"));
        assert!(!is_openable("x-apple-something://do-a-thing"));
    }

    #[test]
    fn refuses_empty_input() {
        assert!(!is_openable(""));
    }

    #[test]
    fn refuses_a_scheme_that_only_appears_later_in_the_string() {
        // Anchored at the start, so this is not a URL that happens to
        // mention one.
        assert!(!is_openable("not-a-url https://example.com"));
    }
}
