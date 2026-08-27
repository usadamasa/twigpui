//! `TimelineView` の状態を表す enum (#241)｡
//!
//! `ui/mod.rs` にあったものをそのまま移した｡`TimelineView` の struct 本体と
//! 同じく `ui` の内側に留め､`pub(super)` で兄弟ファイルから届く｡

use gpui::SharedString;

use crate::cache;
use crate::oauth;
use crate::thread::ThreadChain;
use crate::x_api::TimelineItem;

/// ある reply の "Show thread" の辿り (#12) について分かっていること｡
/// [`super::TimelineView::threads`] では reply 自身の post id を鍵にする｡その map に
/// 無いことは「まだ要求していない」を意味する — トグルは取得を提案しつづける｡
pub(super) enum ThreadFetchState {
    Loading,
    Loaded(ThreadChain),
    /// エラー文言を保持し､行を固まったままにする代わりに再試行のクリックを
    /// その場に出せるようにする｡
    Failed(SharedString),
}

#[derive(Debug)]
pub(super) enum TimelineState {
    /// 使える資格情報がまだ無い: 新鮮な､あるいは更新できる保存済み OAuth
    /// session も bearer token も無い｡起動時､サインイン導線が走る前に出る｡
    NotAuthenticated,
    /// 対話的な "Sign in with X" の導線が動いている — ブラウザを開き､
    /// loopback の callback を待っている｡
    SigningIn,
    Loading,
    Loaded(Vec<TimelineItem>),
    /// まだ一度も読み込めておらず､直近の取得の試みが reset 時刻の分かる rate
    /// limit に当たった (#10) — どちら側が課したものかは [`Cooldown`] を見る｡
    /// #57 以降､画面にすでに post が並んでいる間の cooldown *や失敗した
    /// reload* はこの形では報告しない: 代わりに
    /// [`super::TimelineView::reload_notice`] が `state` とは独立にそれを持ち
    /// (#54 の `session_notice` に倣っている)､カウントダウンやエラー行の
    /// 場所を作るためだけに timeline が捨てられることは無くなった｡この
    /// variant が今も到達可能なのは､body が他に描けるものを何も持たない
    /// 狭いケースのフォールバックとしてだけである —
    /// [`super::reload_failure_outcome`] を見よ｡
    RateLimited {
        reset_at: i64,
        cooldown: Cooldown,
    },
    Failed(SharedString),
}

/// どちら側がアプリを待たせているか｡どちらもカウントダウンを描くが､別々の
/// 事実であり同じ言葉で説明してはいけない: 実際にはアプリが自分で設定した
/// fetch interval を守っているだけなのに「X が rate limit をかけた」と言うのは､
/// 起きたことの端的な言い間違いになる｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Cooldown {
    /// `config.min_fetch_interval_seconds` — 自分で課したもので､何も送って
    /// いないし X も何も言っていない｡
    LocalInterval,
    /// 追跡している `x-rate-limit-*` header が示す X 自身の rate-limit window｡
    ApiRateLimit,
}

/// 直近の reload の試みについての一時的な通知｡`state` から独立に保つ理由は
/// #54 の `session_notice` フィールドとまったく同じである (その doc を見よ):
/// リクエストを阻んだ cooldown も､走ったあとの失敗も､*リクエスト* に今何が
/// 起きたかを述べるものであって､すでに画面に並んでいる post について述べる
/// ものではない — この二つを一つの `state` へ畳んだことが #57 を可能にした
/// (カウントダウンやエラーが､実際には何も変わっていない timeline を追い出す)｡
/// reload が成功した瞬間に消える — [`super::TimelineView::reload`] の結果処理を
/// 見よ — ので､`session_notice` と違って報告していた対象より長く生き残る
/// ことは決してない｡
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ReloadNotice {
    /// リクエストの発行前か発行中に cooldown (#10 自身の interval か X の
    /// rate limit) で阻まれた｡[`super::cooldown_label`] がすでに描画に使っている
    /// のと同じ `reset_at`/`cooldown` の組を持つので､カウントダウンの
    /// 文言は保存せず描画時に毎回計算する — #57 の項目 3 (カウントダウンを
    /// 実際に進ませること) は別の､まだ開いたままの
    /// 課題である｡
    Cooldown { reset_at: i64, cooldown: Cooldown },
    /// リクエストは出ていったが､reset 時刻の分からない理由で
    /// 失敗した｡
    Failed(SharedString),
    /// リクエストが出ていって帰ってきた (#141) — 何件の post を持ち帰ったか｡
    /// ゼロ件も含む｡
    ///
    /// 他の二つの variant は何かが失敗したことを報告するもので､これが
    /// できるまで成功した reload は何も言わなかった: header のボタンが
    /// `Loading…` へ切り替わって戻るだけで､応答が速ければ 1､2 フレームの
    /// 話であり､`cmd-r` のあとに誰かが見ている場所でもない｡問題ではない
    /// 唯一の variant なので､`danger` ではなく muted の色で
    /// 描く｡
    Outcome(SharedString),
}

/// header の主ボタンが何をするか｡今のラベルとは独立している —
/// `self.state` を借りずにクリックのクロージャへ取り込めるよう `Copy` の
/// ままにしてある｡
#[derive(Clone, Copy)]
pub(super) enum PrimaryAction {
    Reload,
    SignIn,
}

/// [`super::TimelineView::reload`] がそもそも `config.min_fetch_interval_seconds`
/// (#10) を尊重すべきかどうか｡この interval は *polling* を抑えるために
/// あり､ユーザーが意図してやったことの結果を確認するのを阻むためのもので
/// はない｡#57 はまさにそのバグだった: post やサインインはそれぞれすでに
/// 自分のリクエストを使っているのに､守る理由の無い interval で即座に
/// 阻まれていた｡
#[derive(Debug, Clone, Copy)]
pub(super) enum ReloadTrigger {
    /// 頼まれていない reload — 起動時の cache miss の経路か "Reload" ボタン｡
    /// ユーザー操作への直接の応答ではない他の fetch と同じく､設定された
    /// interval に従う｡
    Polling,
    /// すでに自分のリクエストを使ったユーザー操作の直接の結果 (成功した
    /// サインイン､成功した post): polling のための interval を待たされては
    /// ならない｡
    UserAction,
}

/// [`super::TimelineView::start`] の背景側が見つけたもの｡executor の境界を越えて､
/// それを `self` へ適用する `update` クロージャまで運ばれる｡
///
/// タプルではなくローカルな enum にしてあるのは､`Home` が post と一緒に
/// 解決済みの [`cache::MeEntry`] を運ぶからで､純粋な cache hit のときでも
/// `/me` への二度目の往復なしに header と `home_user_id` が埋まる｡#33 まで
/// は三つ目の variant があった — `SingleUser`､app-only の bearer token が
/// 解決した先の形である｡
pub(super) enum StartOutcome {
    NotAuthenticated {
        session_notice: Option<String>,
    },
    Home {
        credential: oauth::Credential,
        cached: Option<(cache::MeEntry, Vec<TimelineItem>)>,
        session_notice: Option<String>,
    },
}
