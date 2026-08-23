//! A timeline read from a file instead of from X (#146).
//!
//! The window could only ever be filled two ways: from the response cache,
//! or from a paid request. Both depend on a real account, and neither is
//! reproducible — the rows differ every run, so "is this laid out right?"
//! had no fixed thing to ask it of. Every UI check ended up on #115 as a
//! sentence for a human to act out.
//!
//! A fixture is the third way: a file naming exactly which posts to draw.
//! No credential, no request, the same screen every time.
//!
//! ## What it deliberately is not
//!
//! Not a mock of the API. It carries [`TimelineItem`]s — the type the
//! parser already produces and the renderer already consumes — so it
//! cannot describe a timeline the real join could not. A fixture that
//! drifts from what X returns would test the renderer against a fiction.
//!
//! It also does not stand in for `--fetch-only`. That one exists to prove
//! the *network* path works; this one exists to hold the network still.

use std::path::Path;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::x_api::TimelineItem;

/// Who the fixture says is signed in.
///
/// Present because several affordances are withheld until the app knows
/// its own id — a repost button is not offered on your own post (#15), and
/// deleting is offered only on it (#72). Without this the fixture would
/// draw a timeline nobody is looking at, and exactly the rows that differ
/// per viewer would be the ones missing.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct FixtureUser {
    pub id: String,
    pub username: String,
}

/// The whole contents of a fixture file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct Fixture {
    pub signed_in_as: FixtureUser,
    /// Newest first, as everywhere else in this crate.
    pub items: Vec<TimelineItem>,
    /// Posts a poll has fetched but not yet shown (#21), newest first —
    /// what the "N new posts" bar is offering.
    ///
    /// Real [`TimelineItem`]s rather than a bare count, for this module's
    /// own rule: a fixture describes a timeline, never a widget. So the
    /// bar counts these the way it counts a real poll's, and pressing it
    /// prepends exactly these rows — the interaction is checkable, not
    /// only its resting state.
    ///
    /// Empty (and absent from a fixture file) means no bar, which is every
    /// fixture written before this field existed.
    #[serde(default)]
    pub pending: Vec<TimelineItem>,
}

/// Read and parse a fixture file.
///
/// Errors rather than falling back, unlike `cache::load_json`. A cache
/// miss means "fetch it again" and a broken cache file must never stop the
/// app; a fixture is what was explicitly asked for on the command line, so
/// silently opening an empty window instead would answer a question nobody
/// asked.
pub(crate) fn load(path: &Path) -> Result<Fixture> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("could not read the fixture {}", path.display()))?;
    serde_json::from_str(&contents)
        .with_context(|| format!("could not parse the fixture {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "signed_in_as": { "id": "5685672", "username": "usadamasa" },
      "items": [
        {
          "id": "1",
          "text": "a post",
          "created_at": "2026-08-16T09:00:00.000Z",
          "author_name": "Developers",
          "author_username": "XDevelopers"
        }
      ]
    }"#;

    fn write(name: &str, contents: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("twigpui-fixture-{name}.json"));
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn reads_a_fixture() {
        let path = write("ok", SAMPLE);
        let fixture = load(&path).unwrap();

        assert_eq!(fixture.signed_in_as.username, "usadamasa");
        assert_eq!(fixture.items.len(), 1);
        assert_eq!(fixture.items[0].text, "a post");

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_fixture_omitting_the_optional_fields_still_parses() {
        // Every field added since #9 is `#[serde(default)]` on
        // `TimelineItem`, which is what lets a fixture stay readable: it
        // spells out the case it is about and nothing else.
        let path = write("minimal", SAMPLE);
        let fixture = load(&path).unwrap();

        assert!(fixture.items[0].media.is_empty());
        assert!(fixture.items[0].quoted.is_none());
        assert!(fixture.items[0].author_avatar_url.is_none());
        // #21's field is `#[serde(default)]` for the same reason: every
        // fixture written before it existed must keep loading, and "no
        // pending posts" is the right reading of its absence.
        assert!(fixture.pending.is_empty());

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_missing_fixture_is_an_error_and_names_the_path() {
        let path = std::env::temp_dir().join("twigpui-fixture-nope.json");
        let error = load(&path).expect_err("a missing fixture must not be silently empty");

        assert!(
            format!("{error:#}").contains("twigpui-fixture-nope.json"),
            "the error has to say which file: {error:#}"
        );
    }

    #[test]
    fn the_bundled_fixture_parses_and_covers_what_it_claims_to() {
        // A fixture that no longer loads is worse than none: the whole
        // point is being able to reach for it without checking it first.
        // This also pins the cases it exists to show, so a later edit
        // cannot quietly drop the row someone was relying on.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/timeline.json");
        let fixture = load(&path).expect("fixtures/timeline.json must load");

        assert!(
            fixture.items.iter().any(|item| item.text.len() > 200),
            "a row long enough to have to wrap (#140)"
        );
        assert!(
            fixture.items.iter().any(|item| item.media.len() == 4),
            "a row with a full media grid (#65)"
        );
        assert!(
            fixture
                .items
                .iter()
                .any(|item| item.quoted.as_ref().is_some_and(|q| !q.media.is_empty())),
            "a quote whose card carries an image (#123)"
        );
        assert!(
            fixture
                .items
                .iter()
                .any(|item| item.reposted_by.is_some() && !item.media.is_empty()),
            "a repost carrying the original's image (#104)"
        );
        assert!(
            fixture.items.iter().any(|item| item.replied_to.is_some()),
            "a reply, for the thread toggle (#12)"
        );
        assert!(
            fixture
                .items
                .iter()
                .any(|item| item.author_username == fixture.signed_in_as.username),
            "one of one's own posts, the only row offering Delete (#72)"
        );
        assert!(
            fixture.pending.len() > 1,
            "posts waiting behind the new-posts bar, more than one so the \
             plural wording is what gets drawn (#21)"
        );
        // The bar counts new arrivals against what is displayed, so a
        // pending post that is already in `items` would be counted and
        // then reveal nothing — a fixture that quietly stops showing what
        // it was written to show.
        for pending in &fixture.pending {
            assert!(
                !fixture.items.iter().any(|item| item.id == pending.id),
                "pending post {} is already in the timeline (#21)",
                pending.id
            );
        }
    }

    #[test]
    fn a_malformed_fixture_is_an_error() {
        // The failure a hand-edited fixture actually has. Falling back to
        // an empty timeline would look like the app working.
        let path = write("broken", r#"{ "signed_in_as": {} }"#);
        assert!(load(&path).is_err());

        std::fs::remove_file(&path).unwrap();
    }
}
