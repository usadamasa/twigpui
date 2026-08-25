//! post composer の純粋なロジック (#14): 文字数の勘定､下書きの検証､そして
//! 送信のステートマシン｡ここは gpui にもネットワークにも時計にも触れない —
//! `rate_limit.rs` と `oauth::pkce` が既に使っているのと同じ「純粋な核と薄い
//! 殻」の分け方だ｡おかげで最も大事な性質 (下書きを決して失わない､二度送信
//! しない､明らかに長すぎる post を送らない) を､`ui.rs` を読んで確かめるしか
//! ないのではなく直接ユニットテストできる｡
//!
//! #16 は下書きの本文と並んで任意の引用対象を足す: 引用される post で､
//! [`ComposeState`] 上に [`QuoteTarget`] として載る｡下書きの本文が既に持つ
//! 「一緒に失敗を生き延びる」規則に従い (#14) — [`ComposeState::apply_result`]
//! を見よ — 下書きとは独立に消せる ("Quote" の押し間違いが､打ったものを
//! 捨てさせてはならない)｡

use crate::x_api::QuotedPost;

/// post が収まらねばならない X の公開された文字数上限 (Free/Basic の階層)｡
/// 素の `chars().count()` ではなく [`weighted_length`] で測る｡
pub(crate) const MAX_WEIGHTED_LENGTH: usize = 280;

/// `http://…` または `https://…` の *どの* URL にも､それ自身の長さに関わらず
/// X が割り当てる固定の重み — X は数える前に必ずリンクをこの長さの t.co の
/// URL へ書き換える｡
const URL_WEIGHT: usize = 23;

/// X の「重み付き長さ」(オープンソースの `twitter-text` ライブラリに従う)｡
/// 素の `chars().count()` からの調整は二つ:
///
/// - `http://` または `https://` で始まる空白区切りのトークンは､実際の長さに
///   関わらず [`URL_WEIGHT`] として数える｡トークンの *あいだ* の空白も他の
///   どの文字とも同じ規則で重み付けし､重み 1 と決めてかからない — 表意文字
///   空白 (U+3000) は 2 を量る｡
/// - `twitter-text` の「重み 1」の範囲の外にある符号位置は — [`is_low_weight`]
///   を見よ — 2 として数える｡その中にあるもの (素の ASCII､Latin-1､
///   Latin / Greek / Cyrillic ブロックの残り､それといくつかの句読点の範囲) は
///   1 として数える｡
///
/// #61: この関数の以前の版は規則が逆だった — *全角* のブロック (CJK 統合
/// 漢字､ひらがな/カタカナ､ハングル､全角形､…) を並べ､載っていないものは
/// すべて重み 1 として扱っていた｡その一覧は必然的に不完全で — U+FF60 で
/// 止まっており､U+FF61–U+FF9F の半角カナと半角句読点などを取りこぼしていた —
/// そしてその手の穴はどれも *過小計上* だった: `twitter-text` が 2 と量る
/// 文字が､載っていない既定の 1 へ落ちる｡だからこの勘定は､X なら拒否する
/// 下書きをまだ収まると利用者に告げ得た｡`twitter-text` 自身は規則を逆向きに
/// 定めている: 短く固定された範囲の一覧が重み 1 で､*それ以外すべて* が重み
/// 2 だ｡その形をここで — 逆ではなく — 映せば､この関数が名指しで聞いたことの
/// ない符号位置は既定で安全な側 (2) の重みを得るので､過小計上が漏れ出す穴は
/// もう残らない｡
pub(crate) fn weighted_length(text: &str) -> usize {
    // 空白も他のすべてと同じやり方で重み付けする (#61): 表意文字空白
    // (U+3000) は `twitter-text` にとって重み 2 であり､ここで空白文字を
    // すべて 1 と数えれば､この関数の doc が閉じたと言う過小計上をそのまま
    // 開け直すことになる｡
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
    // `saturating_add` (#47): `usize` を overflow させるほど長い下書きは存在
    // し得ないが､この勘定と誤った答えのあいだに立つものとしては､debug でしか
    // 効かない panic より明示的にそう言うほうがよい｡
    whitespace_weight.saturating_add(word_weight)
}

/// `word` が､X なら固定長の t.co リンクへ縮める URL に見えるかどうか｡素の
/// 前置き検査で — 後ろに続くものの検証はしない — ここでの偽陽性はトークンを
/// 23 へ切り *上げる* だけであり､[`weighted_length`] の doc に言う安全な
/// 向きだからだ｡
fn is_url(word: &str) -> bool {
    word.starts_with("http://") || word.starts_with("https://")
}

fn char_weight(c: char) -> usize {
    if is_low_weight(c) { 1 } else { 2 }
}

/// `c` が `twitter-text` の「重み 1」の範囲のどれかに入るかどうか (#61):
/// `0x0000..=0x10FF` (ASCII､Latin-1 Supplement､それと Latin Extended /
/// Greek / Cyrillic ブロックの残りなど)､`0x2000..=0x200D` (一般句読点の空白
/// とダッシュ)､`0x2010..=0x201F` (一般句読点のハイフンと引用符)､そして
/// `0x2032..=0x2037` (プライム記号)｡この四つの範囲のどれにも入ら *ない* もの
/// — CJK 統合漢字､ハングル､ひらがな/カタカナ (全角も半角も)､全角形､絵文字
/// など — は [`char_weight`] の既定を通して重み 2 になる｡重み *1* のものを
/// (重み 2 のものではなく) 並べることが二度と過小計上させない理由は
/// [`weighted_length`] の doc を見よ｡
fn is_low_weight(c: char) -> bool {
    matches!(u32::from(c),
        0x0000..=0x10FF
        | 0x2000..=0x200D
        | 0x2010..=0x201F
        | 0x2032..=0x2037
    )
}

/// [`validate`] が下書きを拒んだ理由 — [`ComposeState::can_submit`] と
/// `ui.rs` の描画側はどちらもこれらを区別する必要がある｡違う言い回しで
/// 見せるからだ｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposeValidationError {
    /// 空白しか無い (本当に空の場合も含む) — post するものが無い｡
    Empty,
    /// [`MAX_WEIGHTED_LENGTH`] を超えている｡どれだけ超えたかをメッセージが
    /// 言えるよう､実際の重み付き長さを持つ｡
    TooLong { weighted_length: usize },
}

/// `text` が post 可能かどうか｡明らかな拒否がリクエストを費やさないよう
/// (#14)､完全にクライアント側で確かめる｡空かどうかを試すのは `text.trim()`
/// だが — 空白だけの下書きには言うことが無い — *長さ* の検査は trim して
/// いない本文に対して走る｡X が実際に受け取るものに合わせるためだ｡
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

/// composer の状態｡本文とは独立している — [`ComposeState`] の `text`
/// フィールドとは別の enum にしてあるので､「失敗した送信は状態だけを変える」
/// が型の性質になる｡`ui.rs` が守るのを覚えておくべき決まり事ではなくなる｡
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComposeStatus {
    Idle,
    /// `POST /2/tweets` のリクエストが飛んでいる｡この状態では
    /// [`ComposeState::can_submit`] が false になる — #14 の二重送信の番人だ｡
    Submitting,
    /// 直近の送信が失敗した｡`ui.rs` が描くためのメッセージを持つ｡それ自体は
    /// 再挑戦を拒む理由ではない — [`ComposeState::can_submit`] を見よ｡
    Failed(String),
}

/// 下書きが引用する post (#16): その id (`quote_tweet_id` が X へ送るもの) と､
/// 既に判っている著者と本文 ([`QuotedPost`] を再利用する｡#13 自身の「カードと
/// して埋め込まれた post」の形だ)｡おかげで `ui.rs` は二度目の参照無しに同じ
/// 引用カードを composer の中へ描ける — "Quote" ボタンを差し出した timeline
/// の行が､既にこのデータを画面に持っていたからだ｡
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QuoteTarget {
    pub(crate) post_id: String,
    pub(crate) quoted: QuotedPost,
}

/// 下書きが返信する post (#71): その id (`in_reply_to_tweet_id` が X へ送る
/// もの) と､既に判っている著者と本文｡[`QuoteTarget`] と同じやり方で
/// [`QuotedPost`] を再利用する — #13 の「カードとして埋め込まれた post」の形
/// であり､返信対象はまさにそれとして､違う見出しで描かれる｡
///
/// `post_id` は実際に返信される post の id でなければならない｡repost の行なら
/// それは retweet という行為のものではなく *元の post の* id だ｡それを解決
/// するのが `x_api::action_post_id` である (#52)｡間違えても声高に失敗はし
/// ない: 返信が別の会話の下に着地するだけだ｡
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplyTarget {
    pub(crate) post_id: String,
    pub(crate) replying_to: QuotedPost,
}

/// composer の完全な状態 (#14, #16, #71): 下書きの本文､任意の対象 (引用か
/// 返信)､そして [`ComposeStatus`]｡「下書きを決して失わない」「対象を決して
/// 失わない」「二度送信しない」がそれぞれ一つの値の遷移として表現できる —
/// そしてテストできる — ようにまとめてある｡
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ComposeState {
    text: String,
    quote: Option<QuoteTarget>,
    /// #71｡`quote` とは排他だ — X の API なら受け付けるにもかかわらず､この
    /// クレートが両方を兼ねた post を組むのを拒む理由は [`Self::set_reply`]
    /// を見よ｡
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

    /// 今引用している post があればそれ (#16)｡
    pub(crate) fn quote(&self) -> Option<&QuoteTarget> {
        self.quote.as_ref()
    }

    /// 引用対象を設定する (または差し替える) (#16) — 書いている最中に post の
    /// "Quote" を押すと､それまであったものを単に上書きする｡下書きが一度に
    /// 引用する post は常に一つだけだからだ｡
    pub(crate) fn set_quote(&mut self, target: QuoteTarget) {
        self.quote = Some(target);
        // #71: 対象は一度に一つ — `set_reply` の doc を見よ｡
        self.reply = None;
    }

    /// 今返信している post があればそれ (#71)｡
    pub(crate) fn reply(&self) -> Option<&ReplyTarget> {
        self.reply.as_ref()
    }

    /// 返信対象を設定し (または差し替え) (#71)､引用対象があれば消す｡
    ///
    /// X の API は返信でも引用でもある post を受け付けるが､この composer は
    /// 意図的にそれを組むのを拒む: 小さな composer では二つはほとんど同じに
    /// 読め､取り違えて送っても目に見える間違いにならない — 返信は会話の下に
    /// 着地し､引用はしない｡対象を一度に一つにすれば「この post は何になるか」
    /// が UI の一行から答えられる｡だから引用が設定されている最中の "Reply"
    /// は追加ではなく切り替えであり､下書きの本文はどちらにせよ生き残る｡
    pub(crate) fn set_reply(&mut self, target: ReplyTarget) {
        self.reply = Some(target);
        self.quote = None;
    }

    /// 下書きの本文に触れずに返信対象を消す (#71) — [`Self::clear_quote`] が
    /// 与えるのと同じ押し間違いからの回復だ｡
    pub(crate) fn clear_reply(&mut self) {
        self.reply = None;
    }

    /// 下書きの本文に触れずに引用対象を消す (#16) — issue が求める押し間違い
    /// からの回復だ: 誤った引用を取り除くために､既に打ったものを捨てさせては
    /// ならない｡
    pub(crate) fn clear_quote(&mut self) {
        self.quote = None;
    }

    /// 今送信が飛んでいるかどうか｡
    pub(crate) fn is_submitting(&self) -> bool {
        matches!(self.status, ComposeStatus::Submitting)
    }

    /// `TimelineView::submit_post` を進ませてよいかどうか: 既に飛んでいるもの
    /// が無く､本文が [`validate`] を通ること｡`Idle` も `Failed` も送信を許す
    /// — 失敗した試みは再挑戦できるままでなければならず､失敗時に本文を決して
    /// 消さないことのすべての眼目がそこにある｡
    pub(crate) fn can_submit(&self) -> bool {
        !self.is_submitting() && validate(&self.text).is_ok()
    }

    /// 本文には触れずに `Submitting` へ移る｡呼び出し側は先に
    /// [`Self::can_submit`] を確かめておかねばならない — こちらは確かめ直さ
    /// ない｡眼目は､同じスレッドで他の何かが走る前に完了すると呼び出し側が
    /// 頼れる同期的な遷移だからだ (それが実際に二重送信を防ぐ仕組みである
    /// 理由は `ui.rs::TimelineView::submit_post` の doc を見よ)｡
    pub(crate) fn start_submitting(&mut self) {
        self.status = ComposeStatus::Submitting;
    }

    /// 一度も送らずに送信の試みを拒む — 例えば #14 の scope 欠落の検査は
    /// `start_submitting` より前に走るので､リクエストが出たふりをせずにその
    /// 拒否を見せる道がこれだ｡
    pub(crate) fn refuse(&mut self, message: String) {
        self.status = ComposeStatus::Failed(message);
    }

    /// 終わった送信の結果を適用する (#14 の中核の保証を #16 が広げたもの):
    /// 成功は下書き *と* 引用対象を消す — 引用していた post はもう送られた
    /// ので､composer に見せ続けるものは残っていない｡失敗は `text` も `quote`
    /// もまったく触れずに置き､代わりに `message` を記録する｡
    pub(crate) fn apply_result(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.text.clear();
                self.quote = None;
                // #71: 返信対象は､引用対象がそうするのと同じ理由で､属して
                // いた下書きと一緒に去る — 次の下書きは何も無いところから
                // 始まる｡
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
                media: Vec::new(),
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
        // "こうじゃ。" が 9/280 と表示されたという画面報告への回帰確認だ｡
        // 5 文字 — こ (U+3053)､う (U+3046)､じ (U+3058)､ゃ (U+3083)､
        // そして 。 (U+3002) — はすべて `is_low_weight` の範囲 (句読点以外の
        // codepoint では U+10FF が上限) の外にあるので､それぞれ既定の
        // 重み 2 になり､正しい weighted length は 5 * 2 = 10 だ｡この
        // assertion が失敗するようなら､バグは表示側ではなく
        // `weighted_length`/`is_low_weight` にある｡
        assert_eq!(weighted_length("こうじゃ。"), 10);
    }

    #[test]
    fn a_fullwidth_period_counts_double_while_a_halfwidth_period_counts_single() {
        // U+3002 (全角の "。") は `is_low_weight` の範囲の外なので既定の
        // 重み 2 になる｡U+002E (半角の ".") は素の ASCII で
        // `0x0000..=0x10FF` の中にあるので重み 1 のままだ — 2 つは似て
        // 見えるが､ここでは交換できない｡
        assert_eq!(weighted_length("。"), 2);
        assert_eq!(weighted_length("."), 1);
    }

    #[test]
    fn counts_koujaa_with_a_trailing_halfwidth_period_as_ten() {
        // #61 — 実際のバグ報告: "こうじゃ｡" の末尾は *半角* の句点
        // (U+FF61) であって､上の古い回帰テストが使う全角の U+3002 では
        // ない｡古い範囲表は U+FF60 で止まっていたので U+FF61 は表に無い
        // 既定の 1 へ落ち､X が 10 と数えるところを composer は 9/280 と
        // 表示した｡ここの 5 つの codepoint — こ (U+3053)､う (U+3046)､
        // じ (U+3058)､ゃ (U+3083)､｡ (U+FF61) — はすべて重み 2 でなければ
        // ならず､合計は 5 * 2 = 10 だ｡
        assert_eq!(weighted_length("こうじゃ\u{FF61}"), 10);
    }

    #[test]
    fn counts_halfwidth_katakana_double() {
        // #61 の完了条件 — 半角カタカナ (U+FF61–U+FF9F) は､古い表の
        // `0xFF00..=0xFF60` という上限がちょうど取りこぼしていた範囲だ｡
        assert_eq!(weighted_length("ｱ"), 2);
    }

    #[test]
    fn counts_an_emoji_double() {
        // #61 の完了条件 — 😀 (U+1F600) は重み 1 のどの範囲からも大きく
        // 外れるので､倍幅の一覧に名前が挙がることに頼らず既定で重み 2 に
        // ならねばならない｡
        assert_eq!(weighted_length("😀"), 2);
    }

    #[test]
    fn counts_latin_diacritics_and_cyrillic_single() {
        // #61 の完了条件 — é (U+00E9, Latin-1 Supplement) と И
        // (U+0418, Cyrillic) はどちらも 0x0000..=0x10FF の中に入るので､
        // 素の ASCII と同じく重み 1 のままでなければならない｡
        assert_eq!(weighted_length("é"), 1);
        assert_eq!(weighted_length("И"), 1);
    }

    #[test]
    fn counts_an_ideographic_space_double() {
        // #61 が扱うのと同じ数え落としが､書き直した範囲表の届かなかった
        // 唯一の場所で起きる: 空白は一律 1 として別に合計されていたので､
        // `twitter-text` では重み 2 の U+3000 が､表を反転させた後もすり
        // 抜けていた｡
        assert_eq!(weighted_length("\u{3000}"), 2);
    }

    #[test]
    fn still_counts_an_ordinary_space_single() {
        assert_eq!(weighted_length("a b"), 3);
    }

    // --- #71: reply の対象 ---

    fn a_post() -> QuotedPost {
        QuotedPost {
            author_name: "Developers".to_string(),
            author_username: "XDevelopers".to_string(),
            text: "the post being answered".to_string(),
            media: Vec::new(),
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
        // 意図的だ: X は両方を兼ねた post も受け付けるが､小さな composer
        // では 2 つがほとんど同じに見え､誤ったほうを送っても目に見える
        // 間違いにならない｡
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
        // 誤クリックからの復帰: 誤った対象を外すために､すでに入力した
        // ものを捨てさせてはならない｡
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
        // リトライは同じ post へ同じ reply を送らねばならない｡失敗時に
        // 対象を失えば､リトライが黙ってトップレベルの post に化ける｡
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

    // --- ComposeState の遷移 ---

    #[test]
    fn a_fresh_composer_is_idle_and_empty() {
        let state = ComposeState::new();
        assert_eq!(state.text(), "");
        assert_eq!(state.status(), &ComposeStatus::Idle);
    }

    #[test]
    fn a_fresh_composer_has_no_quote_target() {
        // #16: 既定は普通の post だ — どこかの行で "Quote" が押されるまで
        // 何も引用されていない｡
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
        // #14: 二重送信のガード — リクエストが 1 つ進行中のあいだの
        // 2 度目のクリックは､何もすることを見つけてはならない｡
        let mut state = ComposeState::new();
        state.set_text("hello".to_string());
        state.start_submitting();
        assert!(!state.can_submit());
    }

    #[test]
    fn a_failed_submit_keeps_the_text_and_allows_a_retry() {
        // #14 の中心的な保証: 入力したテキストを失うのが最悪の結果だ｡
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
        // 素のテキストに対する #14 の保証に対応する､#16 の中心的な保証だ:
        // 失敗した quote は､下書き自体を落とさないのと同様に､引用していた
        // ものを黙って落としてはならない｡
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
        // 下書きが一度に引用する post は常に 1 つだけだ — 作成中に 2 つめの
        // 行で "Quote" を押すと積み重ならず上書きされる｡
        let mut state = ComposeState::new();
        state.set_quote(sample_target("1700000000000000001"));
        state.set_quote(sample_target("1700000000000000002"));
        assert_eq!(state.quote(), Some(&sample_target("1700000000000000002")));
    }

    #[test]
    fn clear_quote_removes_the_target_without_touching_the_draft_text() {
        // #16: "Quote" の誤クリックが下書きを捨てさせてはならない —
        // quote の対象を外すのが逃げ道だ｡
        let mut state = ComposeState::new();
        state.set_text("hello".to_string());
        state.set_quote(sample_target("1700000000000000001"));
        state.clear_quote();

        assert_eq!(state.quote(), None);
        assert_eq!(state.text(), "hello");
    }

    #[test]
    fn refuse_records_a_message_without_ever_having_started_submitting() {
        // scope 不足による拒否 (#14): `Submitting` に一度も入っていないので
        // リクエストは送られていないが､それでもテキストに触れてはならない｡
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
