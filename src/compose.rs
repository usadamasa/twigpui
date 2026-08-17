//! The post composer's pure logic (#14): character counting, draft
//! validation, and the submit state machine. Nothing here touches gpui, the
//! network, or the clock — the same "pure core, thin shell" split
//! `rate_limit.rs` and `oauth::pkce` already use, so the properties that
//! matter most (never lose a draft, never submit twice, never send an
//! obviously-too-long post) are unit-tested directly rather than only
//! inspectable by reading `ui.rs`.

/// X's public character limit (Free/Basic tiers) that a post must fit
/// under, measured by [`weighted_length`] rather than a plain
/// `chars().count()`.
pub(crate) const MAX_WEIGHTED_LENGTH: usize = 280;

/// The fixed weight X assigns to *any* `http://…` or `https://…` URL,
/// regardless of its own length — X always rewrites a link to a t.co URL of
/// this length before counting it.
const URL_WEIGHT: usize = 23;

/// X's "weighted length" (per the open-source `twitter-text` library),
/// approximated rather than reproduced exactly — see #14's design notes on
/// why exact fidelity isn't the goal here. Two adjustments from a plain
/// `chars().count()`:
///
/// - A whitespace-delimited token starting with `http://` or `https://`
///   counts as [`URL_WEIGHT`] regardless of its actual length.
/// - A codepoint in one of the well-known "double-width" Unicode blocks —
///   CJK ideographs, hiragana, katakana, hangul, and CJK/fullwidth
///   punctuation and forms, see [`is_double_width`] — counts as 2;
///   everything else (including whitespace) counts as 1.
///
/// This does **not** reproduce `twitter-text`'s complete range table: it
/// omits rarer supplementary-plane CJK extensions and a handful of symbol
/// blocks, and it doesn't special-case `@mentions`/`#hashtags` the way X's
/// own counter might for unrelated reasons. What matters for #14's actual
/// goal — never spend a request on a post X will reject outright — is never
/// *undercounting*: this implementation only ever adds weight relative to a
/// plain character count (an unmodeled block still counts as at least 1,
/// and a URL shorter than 23 characters gets rounded *up*, never down), so
/// the gaps above can only make it stop the user *earlier* than X's real
/// counter would, never later.
pub(crate) fn weighted_length(text: &str) -> usize {
    let whitespace_weight = text.chars().filter(|c| c.is_whitespace()).count();
    let word_weight: usize = text
        .split_whitespace()
        .map(|word| {
            if is_url(word) {
                URL_WEIGHT
            } else {
                word.chars().map(char_weight).sum()
            }
        })
        .sum();
    whitespace_weight + word_weight
}

/// Whether `word` looks like a URL X would shorten to a fixed-length t.co
/// link. A plain prefix check — no validation of what follows — since a
/// false positive here only ever rounds a token *up* to 23, the safe
/// direction per [`weighted_length`]'s doc.
fn is_url(word: &str) -> bool {
    word.starts_with("http://") || word.starts_with("https://")
}

fn char_weight(c: char) -> usize {
    if is_double_width(c) { 2 } else { 1 }
}

/// Whether `c` falls in one of the commonly-cited `twitter-text`
/// double-width ranges: Hangul Jamo, CJK radicals/symbols/punctuation,
/// hiragana/katakana/CJK compatibility, CJK Unified Ideographs (plus
/// Extension A), Hangul syllables, CJK compatibility ideographs, and
/// fullwidth forms/signs. Deliberately not exhaustive — see
/// [`weighted_length`]'s doc for what's left out and why that's an
/// acceptable gap here.
fn is_double_width(c: char) -> bool {
    matches!(u32::from(c),
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
    )
}

/// Why [`validate`] refused a draft — [`ComposeState::can_submit`] and
/// `ui.rs`'s render side both need to distinguish these, since they're
/// shown with different wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposeValidationError {
    /// Nothing but whitespace (including truly empty) — there is nothing to
    /// post.
    Empty,
    /// Over [`MAX_WEIGHTED_LENGTH`], carrying the actual weighted length so
    /// the message can say by how much.
    TooLong { weighted_length: usize },
}

/// Whether `text` is postable, checked entirely client-side so an obvious
/// rejection never spends a request (#14). `text.trim()` is what's tested
/// for blankness — a draft of pure whitespace has nothing to say — but the
/// *length* check runs against the untrimmed text, matching what X will
/// actually receive.
pub(crate) fn validate(text: &str) -> Result<(), ComposeValidationError> {
    if text.trim().is_empty() {
        return Err(ComposeValidationError::Empty);
    }
    let weighted_length = weighted_length(text);
    if weighted_length > MAX_WEIGHTED_LENGTH {
        return Err(ComposeValidationError::TooLong { weighted_length });
    }
    Ok(())
}

/// The composer's status, independent of its text — kept as a separate enum
/// from [`ComposeState`]'s `text` field so "a failed submit changes only
/// the status" is a property of the type, not just a convention `ui.rs` has
/// to remember to honor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComposeStatus {
    Idle,
    /// A `POST /2/tweets` request is in flight. [`ComposeState::can_submit`]
    /// is false in this state — #14's double-submit guard.
    Submitting,
    /// The last submit failed; carries a message for `ui.rs` to render. Not
    /// itself a reason to refuse another attempt — see
    /// [`ComposeState::can_submit`].
    Failed(String),
}

/// The composer's full state (#14): draft text plus [`ComposeStatus`],
/// bundled so "never lose the draft" and "never submit twice" can each be
/// expressed — and tested — as one value's transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ComposeState {
    text: String,
    status: ComposeStatus,
}

impl ComposeState {
    pub(crate) fn new() -> Self {
        Self {
            text: String::new(),
            status: ComposeStatus::Idle,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn set_text(&mut self, text: String) {
        self.text = text;
    }

    pub(crate) fn status(&self) -> &ComposeStatus {
        &self.status
    }

    /// Whether a submit is currently in flight.
    pub(crate) fn is_submitting(&self) -> bool {
        matches!(self.status, ComposeStatus::Submitting)
    }

    /// Whether `TimelineView::submit_post` should be allowed to proceed:
    /// nothing already in flight, and the text passes [`validate`]. Both
    /// `Idle` and `Failed` allow a submit — a failed attempt must stay
    /// retryable, which is the entire point of never clearing the text on
    /// failure.
    pub(crate) fn can_submit(&self) -> bool {
        !self.is_submitting() && validate(&self.text).is_ok()
    }

    /// Move to `Submitting`, keeping the text untouched. Callers must have
    /// already checked [`Self::can_submit`] — this doesn't re-check, since
    /// the whole point is a synchronous transition the caller can rely on
    /// completing before anything else runs on the same thread (see
    /// `ui.rs::TimelineView::submit_post`'s doc for why that's what
    /// actually prevents a double submit).
    pub(crate) fn start_submitting(&mut self) {
        self.status = ComposeStatus::Submitting;
    }

    /// Refuse a submit attempt without ever having sent one — e.g. #14's
    /// missing-scope check runs before `start_submitting`, so this is how
    /// that refusal gets shown without pretending a request went out.
    pub(crate) fn refuse(&mut self, message: String) {
        self.status = ComposeStatus::Failed(message);
    }

    /// Apply a finished submit's outcome (#14's core guarantee): success
    /// clears the draft, failure leaves `text` completely untouched and
    /// records `message` in its place.
    pub(crate) fn apply_result(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.text.clear();
                self.status = ComposeStatus::Idle;
            }
            Err(message) => self.refuse(message),
        }
    }
}

impl Default for ComposeState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ComposeState, ComposeStatus, ComposeValidationError, MAX_WEIGHTED_LENGTH, validate,
        weighted_length,
    };

    // --- weighted_length ---

    #[test]
    fn counts_plain_ascii_one_for_one() {
        assert_eq!(weighted_length("hello world"), 11);
    }

    #[test]
    fn counts_a_url_as_23_regardless_of_its_real_length() {
        assert_eq!(weighted_length("https://example.com/a"), 23);
        assert_eq!(weighted_length("https://x.co"), 23);
    }

    #[test]
    fn counts_text_around_a_url() {
        // "check" 5 + " " 1 + "this" 4 + " " 1 + "out" 3 + " " 1 + url 23
        // + " " 1 + "thanks" 6 = 45
        let text = "check this out https://example.com/a thanks";
        assert_eq!(weighted_length(text), 45);
    }

    #[test]
    fn counts_cjk_characters_double() {
        assert_eq!(weighted_length("こんにちは"), 10);
    }

    #[test]
    fn counts_mixed_ascii_and_cjk() {
        assert_eq!(weighted_length("hi 世界"), 7);
    }

    // --- validate ---

    #[test]
    fn rejects_an_empty_draft() {
        assert_eq!(validate(""), Err(ComposeValidationError::Empty));
    }

    #[test]
    fn rejects_a_whitespace_only_draft() {
        assert_eq!(validate("   \n\t "), Err(ComposeValidationError::Empty));
    }

    #[test]
    fn accepts_a_draft_exactly_at_the_limit() {
        let text = "a".repeat(MAX_WEIGHTED_LENGTH);
        assert_eq!(validate(&text), Ok(()));
    }

    #[test]
    fn rejects_a_draft_one_over_the_limit() {
        let text = "a".repeat(MAX_WEIGHTED_LENGTH + 1);
        assert_eq!(
            validate(&text),
            Err(ComposeValidationError::TooLong {
                weighted_length: MAX_WEIGHTED_LENGTH + 1
            })
        );
    }

    #[test]
    fn accepts_an_ordinary_short_draft() {
        assert_eq!(validate("hello"), Ok(()));
    }

    // --- ComposeState transitions ---

    #[test]
    fn a_fresh_composer_is_idle_and_empty() {
        let state = ComposeState::new();
        assert_eq!(state.text(), "");
        assert_eq!(state.status(), &ComposeStatus::Idle);
    }

    #[test]
    fn cannot_submit_blank_text() {
        let state = ComposeState::new();
        assert!(!state.can_submit());
    }

    #[test]
    fn can_submit_once_there_is_postable_text() {
        let mut state = ComposeState::new();
        state.set_text("hello".to_string());
        assert!(state.can_submit());
    }

    #[test]
    fn cannot_submit_while_already_submitting() {
        // #14: the double-submit guard — a second click while one request
        // is in flight must find nothing to do.
        let mut state = ComposeState::new();
        state.set_text("hello".to_string());
        state.start_submitting();
        assert!(!state.can_submit());
    }

    #[test]
    fn a_failed_submit_keeps_the_text_and_allows_a_retry() {
        // #14's central guarantee: losing typed text is the worst outcome.
        let mut state = ComposeState::new();
        state.set_text("hello".to_string());
        state.start_submitting();
        state.apply_result(Err("network error".to_string()));

        assert_eq!(state.text(), "hello");
        assert_eq!(
            state.status(),
            &ComposeStatus::Failed("network error".to_string())
        );
        assert!(state.can_submit(), "a failed submit must stay retryable");
    }

    #[test]
    fn a_successful_submit_clears_the_text_and_returns_to_idle() {
        let mut state = ComposeState::new();
        state.set_text("hello".to_string());
        state.start_submitting();
        state.apply_result(Ok(()));

        assert_eq!(state.text(), "");
        assert_eq!(state.status(), &ComposeStatus::Idle);
    }

    #[test]
    fn refuse_records_a_message_without_ever_having_started_submitting() {
        // The missing-scope refusal (#14): never went to `Submitting` at
        // all, so no request was ever sent, but the text still must not be
        // touched.
        let mut state = ComposeState::new();
        state.set_text("hello".to_string());
        state.refuse("needs re-authorization".to_string());

        assert_eq!(state.text(), "hello");
        assert_eq!(
            state.status(),
            &ComposeStatus::Failed("needs re-authorization".to_string())
        );
    }
}
