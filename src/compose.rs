//! The post composer's pure logic (#14): character counting, draft
//! validation, and the submit state machine. Nothing here touches gpui, the
//! network, or the clock — the same "pure core, thin shell" split
//! `rate_limit.rs` and `oauth::pkce` already use, so the properties that
//! matter most (never lose a draft, never submit twice, never send an
//! obviously-too-long post) are unit-tested directly rather than only
//! inspectable by reading `ui.rs`.
//!
//! #16 adds an optional quote target alongside the draft text: the post
//! being quoted, carried as [`QuoteTarget`] on [`ComposeState`]. It follows
//! the same "survive a failure together" rule the draft text already has
//! (#14) — see [`ComposeState::apply_result`] — and can be cleared
//! independently of the draft (a mis-click on "Quote" shouldn't force
//! discarding what was typed).

use crate::x_api::QuotedPost;

/// X's public character limit (Free/Basic tiers) that a post must fit
/// under, measured by [`weighted_length`] rather than a plain
/// `chars().count()`.
pub(crate) const MAX_WEIGHTED_LENGTH: usize = 280;

/// The fixed weight X assigns to *any* `http://…` or `https://…` URL,
/// regardless of its own length — X always rewrites a link to a t.co URL of
/// this length before counting it.
const URL_WEIGHT: usize = 23;

/// X's "weighted length" (per the open-source `twitter-text` library). Two
/// adjustments from a plain `chars().count()`:
///
/// - A whitespace-delimited token starting with `http://` or `https://`
///   counts as [`URL_WEIGHT`] regardless of its actual length. The
///   whitespace *between* tokens is weighed by the same rule as any other
///   character, not assumed to be weight 1 — an ideographic space (U+3000)
///   weighs 2.
/// - A codepoint outside `twitter-text`'s "weight 1" ranges — see
///   [`is_low_weight`] — counts as 2; everything inside them (plain ASCII,
///   Latin-1, the rest of the Latin/Greek/Cyrillic block, and a few
///   punctuation ranges) counts as 1.
///
/// #61: earlier versions of this function had the rule backwards — it
/// listed the *double-width* blocks (CJK ideographs, hiragana/katakana,
/// hangul, fullwidth forms, …) and treated everything unlisted as weight 1.
/// That list was necessarily incomplete — it stopped at U+FF60 and missed
/// halfwidth kana and halfwidth punctuation at U+FF61–U+FF9F, among other
/// gaps — and every such gap was an *undercount*: a character
/// `twitter-text` weighs 2 fell through to the unlisted default of 1, so
/// this counter could tell a user a draft still fit when X would reject
/// it. `twitter-text` itself defines the rule the other way around: a
/// short, fixed list of ranges weighs 1, and *everything else* weighs 2.
/// Mirroring that shape here — rather than the inverse — means any
/// codepoint this function has never specifically heard of already gets
/// the safe (2) weight by default, so there is no longer a gap left to
/// undercount through.
pub(crate) fn weighted_length(text: &str) -> usize {
    // Whitespace is weighed the same way as everything else (#61): an
    // ideographic space (U+3000) is weight 2 to `twitter-text`, and
    // counting every whitespace character as 1 here would reopen exactly
    // the undercount this function's doc says is closed.
    let whitespace_weight: usize = text
        .chars()
        .filter(|c| c.is_whitespace())
        .map(char_weight)
        .sum();
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
    if is_low_weight(c) { 1 } else { 2 }
}

/// Whether `c` falls in one of `twitter-text`'s "weight 1" ranges (#61):
/// `0x0000..=0x10FF` (ASCII, Latin-1 Supplement, and the rest of the Latin
/// Extended / Greek / Cyrillic block, among others), `0x2000..=0x200D`
/// (general punctuation spaces and dashes), `0x2010..=0x201F` (general
/// punctuation hyphens and quotation marks), and `0x2032..=0x2037` (prime
/// marks). Everything *not* in one of these four ranges — CJK ideographs,
/// hangul, hiragana/katakana (fullwidth and halfwidth alike), fullwidth
/// forms, emoji, and so on — is weight 2 via [`char_weight`]'s default. See
/// [`weighted_length`]'s doc for why listing what's weight *1* (rather than
/// what's weight 2) is what keeps this from ever undercounting again.
fn is_low_weight(c: char) -> bool {
    matches!(u32::from(c),
        0x0000..=0x10FF
        | 0x2000..=0x200D
        | 0x2010..=0x201F
        | 0x2032..=0x2037
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

/// The post a draft quotes (#16): its id (what `quote_tweet_id` sends X) and
/// the already-known author/text (reusing [`QuotedPost`], #13's own "post
/// embedded as a card" shape) so `ui.rs` can render the same quote card
/// inside the composer without a second lookup — the timeline row that
/// offered the "Quote" button already had this data on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QuoteTarget {
    pub(crate) post_id: String,
    pub(crate) quoted: QuotedPost,
}

/// The post a draft replies to (#71): its id (what `in_reply_to_tweet_id`
/// sends X) and the already-known author/text, reusing [`QuotedPost`] the
/// same way [`QuoteTarget`] does — it is #13's "a post embedded as a card"
/// shape, and a reply target is rendered as exactly that, with a different
/// heading.
///
/// `post_id` must be the id of the post actually being replied to, which
/// for a repost row is the *original's* id, not the retweet activity's —
/// `x_api::action_post_id` is what resolves that (#52). Getting it wrong
/// does not fail loudly: the reply lands under a different conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplyTarget {
    pub(crate) post_id: String,
    pub(crate) replying_to: QuotedPost,
}

/// The composer's full state (#14, #16, #71): draft text, an optional
/// target (a quote or a reply), plus [`ComposeStatus`], bundled so "never
/// lose the draft", "never lose the target", and "never submit twice" can
/// each be expressed — and tested — as one value's transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ComposeState {
    text: String,
    quote: Option<QuoteTarget>,
    /// #71. Mutually exclusive with `quote` — see [`Self::set_reply`] for
    /// why this crate refuses to build a post that is both at once, even
    /// though X's API would accept one.
    reply: Option<ReplyTarget>,
    status: ComposeStatus,
}

impl ComposeState {
    pub(crate) fn new() -> Self {
        Self {
            text: String::new(),
            quote: None,
            reply: None,
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

    /// The post currently being quoted, if any (#16).
    pub(crate) fn quote(&self) -> Option<&QuoteTarget> {
        self.quote.as_ref()
    }

    /// Set (or replace) the quote target (#16) — clicking "Quote" on a post
    /// while composing simply overwrites whatever was there before, since a
    /// draft only ever quotes one post at a time.
    pub(crate) fn set_quote(&mut self, target: QuoteTarget) {
        self.quote = Some(target);
        // #71: one target at a time — see `set_reply`'s doc.
        self.reply = None;
    }

    /// The post currently being replied to, if any (#71).
    pub(crate) fn reply(&self) -> Option<&ReplyTarget> {
        self.reply.as_ref()
    }

    /// Set (or replace) the reply target (#71), clearing any quote target.
    ///
    /// X's API would accept a post that is both a reply and a quote, but
    /// this composer deliberately refuses to build one: the two read almost
    /// identically in a small composer, and sending the wrong one is not a
    /// visible mistake — a reply lands under a conversation, a quote does
    /// not. One target at a time makes "what will this post be" answerable
    /// from a single line of UI. Clicking "Reply" while a quote is set is
    /// therefore a switch, not an addition, and the draft text survives it
    /// either way.
    pub(crate) fn set_reply(&mut self, target: ReplyTarget) {
        self.reply = Some(target);
        self.quote = None;
    }

    /// Clear the reply target without touching the draft text (#71) — the
    /// same mis-click recovery [`Self::clear_quote`] provides.
    pub(crate) fn clear_reply(&mut self) {
        self.reply = None;
    }

    /// Clear the quote target without touching the draft text (#16) — the
    /// mis-click recovery the issue calls for: removing a wrong quote must
    /// not force discarding what was already typed.
    pub(crate) fn clear_quote(&mut self) {
        self.quote = None;
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

    /// Apply a finished submit's outcome (#14's core guarantee, extended by
    /// #16): success clears the draft *and* the quote target — the post
    /// that was quoted has now been sent, so there's nothing left to keep
    /// showing in the composer; failure leaves both `text` and `quote`
    /// completely untouched and records `message` in their place.
    pub(crate) fn apply_result(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.text.clear();
                self.quote = None;
                // #71: the reply target goes with the draft it belonged to,
                // for the same reason the quote target does — the next
                // draft starts from nothing.
                self.reply = None;
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
        ComposeState, ComposeStatus, ComposeValidationError, MAX_WEIGHTED_LENGTH, QuoteTarget,
        ReplyTarget, validate, weighted_length,
    };
    use crate::x_api::QuotedPost;

    fn sample_target(post_id: &str) -> QuoteTarget {
        QuoteTarget {
            post_id: post_id.to_string(),
            quoted: QuotedPost {
                author_name: "Developers".to_string(),
                author_username: "XDevelopers".to_string(),
                text: "hello from the timeline".to_string(),
            },
        }
    }

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

    #[test]
    fn counts_koujaa_with_a_trailing_fullwidth_period_as_ten() {
        // Regression check for a screen report that showed "こうじゃ。" as
        // 9/280: five characters — こ (U+3053), う (U+3046), じ (U+3058),
        // ゃ (U+3083), and 。 (U+3002) — all outside `is_low_weight`'s
        // ranges (which top out at U+10FF for non-punctuation codepoints),
        // so each defaults to weight 2 and the correct weighted length is
        // 5 * 2 = 10. If this assertion ever fails, `weighted_length`/
        // `is_low_weight` has a bug, not the display.
        assert_eq!(weighted_length("こうじゃ。"), 10);
    }

    #[test]
    fn a_fullwidth_period_counts_double_while_a_halfwidth_period_counts_single() {
        // U+3002 (fullwidth "。") is outside `is_low_weight`'s ranges, so
        // it defaults to weight 2; U+002E (halfwidth ".") is plain ASCII,
        // inside `0x0000..=0x10FF`, so it stays weight 1 — the two look
        // similar but are not interchangeable here.
        assert_eq!(weighted_length("。"), 2);
        assert_eq!(weighted_length("."), 1);
    }

    #[test]
    fn counts_koujaa_with_a_trailing_halfwidth_period_as_ten() {
        // #61 — the actual bug report: "こうじゃ｡" with a *halfwidth*
        // trailing period (U+FF61), not the fullwidth U+3002 the older
        // regression test above uses. The old range table stopped at
        // U+FF60, so U+FF61 fell through to the unlisted default of 1 and
        // the composer showed 9/280 while X counts 10. All five codepoints
        // here — こ (U+3053), う (U+3046), じ (U+3058), ゃ (U+3083), ｡
        // (U+FF61) — must weigh 2, for a total of 5 * 2 = 10.
        assert_eq!(weighted_length("こうじゃ\u{FF61}"), 10);
    }

    #[test]
    fn counts_halfwidth_katakana_double() {
        // #61 completion criterion — halfwidth katakana (U+FF61–U+FF9F) is
        // exactly the range the old table's `0xFF00..=0xFF60` upper bound
        // dropped off the edge of.
        assert_eq!(weighted_length("ｱ"), 2);
    }

    #[test]
    fn counts_an_emoji_double() {
        // #61 completion criterion — 😀 (U+1F600) is well outside every
        // weight-1 range, so it must default to weight 2 rather than rely
        // on ever being named in a double-width list.
        assert_eq!(weighted_length("😀"), 2);
    }

    #[test]
    fn counts_latin_diacritics_and_cyrillic_single() {
        // #61 completion criterion — é (U+00E9, Latin-1 Supplement) and И
        // (U+0418, Cyrillic) both fall inside 0x0000..=0x10FF and must stay
        // weight 1, same as plain ASCII.
        assert_eq!(weighted_length("é"), 1);
        assert_eq!(weighted_length("И"), 1);
    }

    #[test]
    fn counts_an_ideographic_space_double() {
        // The same undercount #61 is about, in the one place the rewritten
        // range table did not reach: whitespace was summed separately at a
        // flat 1 each, so U+3000 — weight 2 to `twitter-text` — slipped
        // through even after the table was inverted.
        assert_eq!(weighted_length("\u{3000}"), 2);
    }

    #[test]
    fn still_counts_an_ordinary_space_single() {
        assert_eq!(weighted_length("a b"), 3);
    }

    // --- #71: reply target ---

    fn a_post() -> QuotedPost {
        QuotedPost {
            author_name: "Developers".to_string(),
            author_username: "XDevelopers".to_string(),
            text: "the post being answered".to_string(),
        }
    }

    fn a_reply_target(post_id: &str) -> ReplyTarget {
        ReplyTarget {
            post_id: post_id.to_string(),
            replying_to: a_post(),
        }
    }

    fn a_quote_target(post_id: &str) -> QuoteTarget {
        QuoteTarget {
            post_id: post_id.to_string(),
            quoted: a_post(),
        }
    }

    #[test]
    fn a_fresh_composer_is_replying_to_nothing() {
        assert_eq!(ComposeState::new().reply(), None);
    }

    #[test]
    fn set_reply_records_the_target() {
        let mut state = ComposeState::new();
        state.set_reply(a_reply_target("1700000000000000001"));
        assert_eq!(
            state.reply().map(|target| target.post_id.as_str()),
            Some("1700000000000000001")
        );
    }

    #[test]
    fn setting_a_reply_clears_a_quote_and_the_other_way_round() {
        // Deliberate: X would accept a post that is both, but the two read
        // almost identically in a small composer and sending the wrong one
        // is not a visible mistake.
        let mut state = ComposeState::new();
        state.set_quote(a_quote_target("quoted"));
        state.set_reply(a_reply_target("replied"));
        assert_eq!(state.quote(), None);
        assert!(state.reply().is_some());

        state.set_quote(a_quote_target("quoted"));
        assert_eq!(state.reply(), None);
        assert!(state.quote().is_some());
    }

    #[test]
    fn switching_between_a_reply_and_a_quote_keeps_the_draft() {
        let mut state = ComposeState::new();
        state.set_text("already typed".to_string());
        state.set_reply(a_reply_target("1"));
        state.set_quote(a_quote_target("2"));
        assert_eq!(state.text(), "already typed");
    }

    #[test]
    fn clearing_a_reply_keeps_the_draft() {
        // The mis-click recovery: removing the wrong target must not force
        // discarding what was already typed.
        let mut state = ComposeState::new();
        state.set_text("already typed".to_string());
        state.set_reply(a_reply_target("1"));
        state.clear_reply();
        assert_eq!(state.reply(), None);
        assert_eq!(state.text(), "already typed");
    }

    #[test]
    fn a_successful_submit_clears_the_reply_target_with_the_draft() {
        let mut state = ComposeState::new();
        state.set_text("a reply".to_string());
        state.set_reply(a_reply_target("1"));
        state.start_submitting();
        state.apply_result(Ok(()));
        assert_eq!(state.reply(), None);
        assert_eq!(state.text(), "");
    }

    #[test]
    fn a_failed_submit_keeps_the_reply_target_and_the_draft() {
        // A retry has to send the same reply to the same post; losing the
        // target on failure would silently turn the retry into a top-level
        // post.
        let mut state = ComposeState::new();
        state.set_text("a reply".to_string());
        state.set_reply(a_reply_target("1"));
        state.start_submitting();
        state.apply_result(Err("network error".to_string()));
        assert_eq!(
            state.reply().map(|target| target.post_id.as_str()),
            Some("1")
        );
        assert_eq!(state.text(), "a reply");
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
    fn a_fresh_composer_has_no_quote_target() {
        // #16: an ordinary post is the default — nothing is being quoted
        // until "Quote" is clicked on some row.
        let state = ComposeState::new();
        assert_eq!(state.quote(), None);
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
    fn a_failed_quote_submit_keeps_the_quote_target_together_with_the_draft() {
        // #16's central guarantee, mirroring #14's for plain text: a failed
        // quote must not silently drop what was being quoted, any more than
        // it drops the draft itself.
        let mut state = ComposeState::new();
        state.set_text("hello".to_string());
        state.set_quote(sample_target("1700000000000000001"));
        state.start_submitting();
        state.apply_result(Err("network error".to_string()));

        assert_eq!(state.text(), "hello");
        assert_eq!(state.quote(), Some(&sample_target("1700000000000000001")));
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
    fn a_successful_quote_submit_clears_the_quote_target_along_with_the_draft() {
        let mut state = ComposeState::new();
        state.set_text("hello".to_string());
        state.set_quote(sample_target("1700000000000000001"));
        state.start_submitting();
        state.apply_result(Ok(()));

        assert_eq!(state.text(), "");
        assert_eq!(state.quote(), None);
        assert_eq!(state.status(), &ComposeStatus::Idle);
    }

    #[test]
    fn set_quote_records_the_target() {
        let mut state = ComposeState::new();
        state.set_quote(sample_target("1700000000000000001"));
        assert_eq!(state.quote(), Some(&sample_target("1700000000000000001")));
    }

    #[test]
    fn set_quote_replaces_a_previously_set_target() {
        // A draft only ever quotes one post at a time — clicking "Quote" on
        // a second row while composing overwrites, not stacks.
        let mut state = ComposeState::new();
        state.set_quote(sample_target("1700000000000000001"));
        state.set_quote(sample_target("1700000000000000002"));
        assert_eq!(state.quote(), Some(&sample_target("1700000000000000002")));
    }

    #[test]
    fn clear_quote_removes_the_target_without_touching_the_draft_text() {
        // #16: a mis-click on "Quote" must not force discarding the draft —
        // clearing the quote target is the way out.
        let mut state = ComposeState::new();
        state.set_text("hello".to_string());
        state.set_quote(sample_target("1700000000000000001"));
        state.clear_quote();

        assert_eq!(state.quote(), None);
        assert_eq!(state.text(), "hello");
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

    #[test]
    fn refuse_does_not_touch_the_quote_target() {
        let mut state = ComposeState::new();
        state.set_quote(sample_target("1700000000000000001"));
        state.refuse("needs re-authorization".to_string());

        assert_eq!(state.quote(), Some(&sample_target("1700000000000000001")));
    }
}
