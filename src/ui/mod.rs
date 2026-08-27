use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    AnyElement, Context, Div, Entity, FocusHandle, Focusable as _, FontWeight, ScrollHandle,
    SharedString, Subscription, Task, Window, div, img, prelude::*, px, rgb, rgba, svg,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::activity::{self, Activity};
use crate::assets;
use crate::avatar;
use crate::browser;
use crate::cache;
use crate::compose::{self, ComposeState, ComposeStatus};
use crate::config::Config;
use crate::fixture::Fixture;
use crate::image_cache;
use crate::like;
use crate::log;
mod auto_refresh;
mod fade;
mod list_picker;
mod list_sync;
mod reload_policy;
mod render;
mod scroll;
mod sync_row;
mod tasks;
mod toast;

// `ui` の兄弟ではなく子モジュールにする (#126): 子モジュールは親の
// プライベート項目を参照できるので､`TimelineState`､`ReloadNotice`､
// `TimelineView` 自体は `pub(crate)` へ広げずに `ui` の内側へ留まる｡
// 隣のファイルから届かせるためだけに広げると､「クレート内のどこからでも
// 触ってよい」という意味になり､それはファイルを分割した目的と
// 正反対になる｡
use auto_refresh::{FollowMode, Pending, pending_after_poll, pending_label};
use fade::Fade;
use list_sync::{SyncOff, SyncStatus, SyncTrigger};
use reload_policy::{
    CooldownTick, at_the_post_cap, cooldown_label, cooldown_tick, newly_arrived, offers_load_older,
    preserved_scroll_target, reload_failure_outcome, reload_gate, reload_outcome_label,
    reload_start_state,
};
use render::Addressable as _;
use render::{
    AVATAR_SIZE, MAX_RENDERED_MEDIA, MEDIA_CELL_HEIGHT, author_link, avatar_placeholder, byline,
    compose_error_message, format_timestamp, header_title_element, like_row, link_row, media_badge,
    media_columns, new_posts_bar, notice, offers_delete, offers_like, offers_quote,
    offers_reauthorize, offers_reply, offers_repost, open_post_link, quote_card, quote_row,
    reload_notice_banner, render_thread_chain, reply_banner_label, reply_row, reply_target_label,
    repost_banner_label, repost_row, session_notice_banner, sign_in_pill, thread_action_label,
    thread_toggle_row, usage_color, usage_label, with_count,
};
use render::{RowCounts, row_counts};
use toast::Toast;

use crate::menu::{
    BlurComposer, CloseWindow, FocusComposer, KEY_CONTEXT, Minimize, Reload, ScrollToTop,
    ShowAbout, ShowNewPosts, ToggleFollowNewPosts,
};
use crate::oauth;
use crate::paths::Paths;
use crate::rate_limit;
use crate::repost;
use crate::sync;
use crate::theme::{self, Theme};
use crate::thread::{self, ThreadChain};
use crate::toggle::{ToggleState, ToggleStatus};
use crate::usage;
use crate::x_api::{
    Denial, Denied, Draft, PostLink, PostMedia, PostMetrics, QuotedPost, RepliedTo, TimelineItem,
    XClient, action_post_id,
};

/// ある reply の "Show thread" の辿り (#12) について分かっていること｡
/// [`TimelineView::threads`] では reply 自身の post id を鍵にする｡その map に
/// 無いことは「まだ要求していない」を意味する — トグルは取得を提案しつづける｡
enum ThreadFetchState {
    Loading,
    Loaded(ThreadChain),
    /// エラー文言を保持し､行を固まったままにする代わりに再試行のクリックを
    /// その場に出せるようにする｡
    Failed(SharedString),
}

#[derive(Debug)]
enum TimelineState {
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
    /// [`TimelineView::reload_notice`] が `state` とは独立にそれを持ち
    /// (#54 の `session_notice` に倣っている)､カウントダウンやエラー行の
    /// 場所を作るためだけに timeline が捨てられることは無くなった｡この
    /// variant が今も到達可能なのは､body が他に描けるものを何も持たない
    /// 狭いケースのフォールバックとしてだけである —
    /// [`reload_failure_outcome`] を見よ｡
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
enum Cooldown {
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
/// reload が成功した瞬間に消える — [`TimelineView::reload`] の結果処理を
/// 見よ — ので､`session_notice` と違って報告していた対象より長く生き残る
/// ことは決してない｡
#[derive(Debug, Clone, PartialEq)]
enum ReloadNotice {
    /// リクエストの発行前か発行中に cooldown (#10 自身の interval か X の
    /// rate limit) で阻まれた｡[`cooldown_label`] がすでに描画に使っている
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
enum PrimaryAction {
    Reload,
    SignIn,
}

/// [`TimelineView::reload`] がそもそも `config.min_fetch_interval_seconds`
/// (#10) を尊重すべきかどうか｡この interval は *polling* を抑えるために
/// あり､ユーザーが意図してやったことの結果を確認するのを阻むためのもので
/// はない｡#57 はまさにそのバグだった: post やサインインはそれぞれすでに
/// 自分のリクエストを使っているのに､守る理由の無い interval で即座に
/// 阻まれていた｡
#[derive(Debug, Clone, Copy)]
enum ReloadTrigger {
    /// 頼まれていない reload — 起動時の cache miss の経路か "Reload" ボタン｡
    /// ユーザー操作への直接の応答ではない他の fetch と同じく､設定された
    /// interval に従う｡
    Polling,
    /// すでに自分のリクエストを使ったユーザー操作の直接の結果 (成功した
    /// サインイン､成功した post): polling のための interval を待たされては
    /// ならない｡
    UserAction,
}

/// [`TimelineView::start`] の背景側が見つけたもの｡executor の境界を越えて､
/// それを `self` へ適用する `update` クロージャまで運ばれる｡
///
/// タプルではなくローカルな enum にしてあるのは､`Home` が post と一緒に
/// 解決済みの [`cache::MeEntry`] を運ぶからで､純粋な cache hit のときでも
/// `/me` への二度目の往復なしに header と `home_user_id` が埋まる｡#33 まで
/// は三つ目の variant があった — `SingleUser`､app-only の bearer token が
/// 解決した先の形である｡
enum StartOutcome {
    NotAuthenticated {
        session_notice: Option<String>,
    },
    Home {
        credential: oauth::Credential,
        cached: Option<(cache::MeEntry, Vec<TimelineItem>)>,
        session_notice: Option<String>,
    },
}

/// ウィンドウの最初の一画面がどこから来るか (#146)｡
///
/// この enum が引く継ぎ目こそが要点である: これができるまで
/// [`TimelineView::new`] はいつも [`TimelineView::start`] へ直行していて､
/// そこで credential を解決し､レスポンスの cache を読み､空で返って
/// きたら fetch していた｡view へ timeline を渡す方法は無かった — view が
/// 自分で取りに行っていた｡
///
/// そこで今は `main` がデータの出どころを決め､view は渡されたものを
/// 描く｡これがアカウント無しで画面を再現可能にしている
/// ものである｡
#[derive(Debug)]
pub(crate) enum Startup {
    /// credential を解決し､cache を読み､ファイルに何も無ければ fetch する｡
    /// #146 より前はすべての起動がこうしていたし､普通の起動は今もこう
    /// している｡
    Live,
    /// この post を描いて終わる｡
    ///
    /// **このモードでは `XClient` を一切組み立てない**｡これは慣習ではなく､
    /// fixture が何の課金も起こしえない理由そのものである: この view の
    /// 課金される経路はすべて `self.client` の向こうにあり､そこには
    /// 届く先が何も無い｡
    Fixture(Box<Fixture>),
}

impl Startup {
    /// この起動の window が､画面がロックされていても (occluded でも) 描き
    /// 続けるべきかどうか — fork した gpui の patch (Cargo.toml の
    /// `[patch.crates-io]`) が読むスイッチの値｡
    ///
    /// fixture の window は撮られるためにある｡upstream の gpui はロック中に
    /// 開いた window を 1 フレームも描かず､capture が真っ黒になる｡live の
    /// window は入れない: 隠れているあいだ描画を止める upstream の挙動は
    /// 本番にとって正しい｡
    ///
    /// `main` が `open_window` の **前** に `gpui::set_draw_while_occluded`
    /// へ渡す｡gpui は platform の window を作ってから root view を組むので､
    /// view の構築中に入れたのでは window 生成時の判定に間に合わない｡
    pub(crate) fn draws_while_occluded(&self) -> bool {
        matches!(self, Self::Fixture(_))
    }
}

pub(crate) struct TimelineView {
    config: Config,
    paths: Paths,
    /// 構築時に `config.theme` から一度だけ解決する — [`TimelineView::new`]
    /// を見よ｡`Copy` なので､下にある自由関数の render ヘルパーへ lifetime の
    /// 雑音なしに渡せる｡
    theme: Theme,
    /// credential が使えるようになるまで `None` — [`TimelineState::NotAuthenticated`] を見よ｡
    client: Option<XClient>,
    state: TimelineState,
    /// この task を保持している間は進行中の fetch (あるいは起動時の
    /// credential 解決) が生きつづける; drop すると取り消される｡
    fetch: Option<Task<()>>,
    /// これを保持している間は対話的なサインイン導線が生きつづける; 新しい
    /// ものを代入する (二度目のクリック) と走っていたものが drop され取り
    /// 消され､loopback の socket も閉じる — `oauth::callback` を見よ｡
    sign_in_flow: Option<Task<()>>,
    /// 直近の reload をいつ始めたか｡[`Self::reload`] が
    /// `config.min_fetch_interval_seconds` (#10) をボタン自体へのクライアント
    /// 側 throttle として効かせるために使う — 追跡している API の rate-limit
    /// 状態が `rate_limit::decision` 経由で言うこととは独立で､それに上乗せ
    /// される｡最初の reload までは `None` で､だから決して throttle されない｡
    last_reload_at: Option<i64>,
    /// [`Self::reload`] が今進行中かどうか (#57)｡
    /// `state == TimelineState::Loading` とは別物である: あの variant は
    /// *まだ何も表示されていない* を意味するが､こちらは `state` が `Loaded`
    /// で前の post を出しつづけている間も reload が走っていれば `true` の
    /// ままになる — [`reload_start_state`] を見よ｡header の busy ラベルを
    /// 駆動する; `body` は `state` から直接描くので同等の判定は要らず､この
    /// フラグは意図的に `state` に手を触れない｡
    reloading: bool,
    /// `client` の credential が app-only の bearer token ではなく OAuth
    /// session から来たものかどうか (#31)｡header が "Sign in with X" を
    /// 出しつづけるかを決める: bearer token で動いているのは機能している
    /// 状態ではあるが厳密により狭い状態なので､credential が一つも無いとき
    /// にだけ現れるのではなく､この誘導は届くところに置いておかねば
    /// ならない｡
    signed_in_with_oauth: bool,
    /// サインインしたユーザー自身の id｡`GET /2/users/me` で解決する｡
    /// home-timeline の endpoint を呼ぶのと [`Self::load_older`] でさらに
    /// 遡るのに要る｡`/me` が一度解決するまでは `None`｡
    home_user_id: Option<String>,
    /// サインインしたユーザー自身の screen name (これも `/me` から)｡header に
    /// 出る — [`render::header_title`] を見よ｡
    home_username: Option<String>,
    /// どの timeline がウィンドウを埋めるか (#161): [`Self::new`] の中で
    /// [`list_picker::initial_source`] が決め､再代入するのは
    /// [`Self::switch_source`] (#164) だけで､そこではウィンドウではなく
    /// 一つの source に属する下記のものもすべてリセットする｡
    ///
    /// timeline に触る経路はすべてこれを読む: [`Self::start`]､
    /// [`Self::reload`]､[`Self::load_older`]､[`Self::confirm_delete`] は
    /// どれもこれを取り､読む cache ファイル､リクエストを使う endpoint､
    /// delete が書き換えるファイルが同じ source になるようにしている｡
    source: cache::TimelineSource,
    /// picker が名前を挙げられる list (#164)｡cache か直近の fetch から来る｡
    /// fetch ボタンが一度押されるまでは空｡
    owned_lists: Vec<crate::x_api::ListSummary>,
    /// 進行中の `owned_lists` の fetch があればそれ; `fetch` と同じ drop で
    /// 取り消す契約であり､二度目のクリックを止めるものでもある｡
    lists_fetch: Option<Task<()>>,
    /// 切り替えを覚えておく場所 ([`Paths::selection_file`])｡fixture の
    /// ウィンドウでは `None` — [`list_picker::saved_selection_for`] を見よ｡
    selection_file: Option<PathBuf>,
    /// 直近の home-timeline レスポンスの `meta.next_token` があればそれ
    /// (#11)｡"Load older" ボタンが出るかを決める — [`offers_load_older`] を
    /// 見よ｡これ以上遡って取るものが無いとき､またはまだネットワークから
    /// 何も来ていないときは `None`: cursor 自体は永続化しないので､cache
    /// だけの描画は token を持たない｡
    next_page_token: Option<String>,
    /// "Show thread" の fetch (#12)｡reply 自身の post id を鍵にする — 見えて
    /// いる reply が複数同時に thread を開けるので､単一のスロットではなく
    /// map にしてある｡無いことは「まだ要求していない」を意味する｡
    threads: HashMap<String, ThreadFetchState>,
    /// 進行中の thread の辿り｡`threads` と同じ鍵で持ち､`fetch` の drop で
    /// 取り消す契約に倣う: view を drop すると､まだ走っている辿りも一緒に
    /// すべて取り消される｡
    thread_fetches: HashMap<String, Task<()>>,
    /// post composer の下書き本文と submit の状態 (#14) — これがこの struct
    /// に散らばったフィールドではなく独立した純粋な型である理由は
    /// `compose.rs` のモジュール doc を見よ｡下流のすべて (カウンタ､
    /// `can_submit`､`submit_post`) にとって権威ある *本文* はこちらであり､
    /// 直接読むのではなく `InputEvent::Change` のたびに `compose_input`
    /// 自身のバッファから写す (#38) — [`Self::on_compose_input_event`] を
    /// 見よ｡こうすれば `compose.rs` の純粋なロジックは素の `&str` を相手に
    /// しつづけ､このウィジェットができる前とまったく同じく gpui 無しで
    /// テストできる｡
    compose: ComposeState,
    /// composer の本物のテキスト入力ウィジェット (#38)｡`div().on_key_down()`
    /// でやっていた生のキーストローク読みを置き換えたもの: 実体は
    /// `gpui_component::input::InputState` で､`EntityInputHandler` を
    /// きちんと実装しているため､IME の変換 (日本語､中国語､韓国語)､カーソル
    /// 移動､選択､コピー/ペーストがすべて動く｡ユーザーが実際に見て打ち込む
    /// のはこのバッファであり､上の `compose` がこちらへ追従する｡逆ではない｡
    /// 意図的な例外が一つだけある — [`Self::submit_post`] の成功経路を見よ｡
    /// そこではこれを明示的に消している｡submit 成功時に `compose` だけを
    /// `text.clear()` しても､ウィジェットは古い下書きを表示したままに
    /// なるからである｡
    compose_input: Entity<InputState>,
    /// `compose_input` の change subscription を生かしておく — drop すると
    /// 上の `compose` が二度と写されなくなるのに､何も言わない｡`fetch` や
    /// この struct の他の `Task` 保持フィールドと同じ取り消し/生存維持の
    /// 慣習で､対象が `Subscription` に変わっただけである; 先頭の
    /// アンダースコア (決して読まず､保持するだけ) は gpui-component が自身の
    /// search-input subscription でこの同じパターンに付けている名前に倣う｡
    _compose_input_subscription: Subscription,
    /// これを保持している間は進行中の `POST /2/tweets` が生きつづける｡
    /// `fetch` の drop で取り消す契約に倣っている｡実際には submit の
    /// サイクル一回につき一度しか代入されない: 一つ未完了の間ずっと
    /// [`ComposeState::can_submit`] は false で､それはここに手を付ける前に
    /// [`Self::submit_post`] の先頭で同期的に確認される — なぜそれが二度目の
    /// 送信がこのフィールドに届くことを実際に排除するのかは､そのメソッドの
    /// doc を見よ｡
    submit_task: Option<Task<()>>,
    /// サインインした session に与えられた scope｡解決済みの credential から
    /// 写す (#14) — bearer の credential や scope が記録されていない OAuth
    /// session では `None`｡[`offers_reauthorize`] と [`Self::submit_post`]
    /// 自身の scope 判定へ渡る｡
    oauth_scope: Option<String>,
    /// 直近の credential 解決で､保存済みの OAuth session をそのままでは
    /// 使えなかった理由を人間が読める形で説明したもの (#54) — 何も劣化して
    /// いなければ `None` (新鮮な session､更新に成功した session､保存済み
    /// session が無い場合､そもそも OAuth が関わらない場合)｡`state` に
    /// 関わらず消えないバナーとして描く — [`session_notice_banner`] を
    /// 見よ — このフィールドが直すために在る欠陥が､まさに timeline が何も
    /// 起きなかったかのように描かれてしまうことだからである｡設定するのは
    /// [`Self::start`] で一度だけ (credential 自体が起動時にしか解決され
    /// ないのに倣う — そのメソッドの doc を見よ); 新規サインインか再認可が
    /// 成功した瞬間に [`Self::sign_in`] で
    /// 消される｡
    session_notice: Option<SharedString>,
    /// auto-refresh のポーリングが止まった理由 (#239) — 走っているか､
    /// そもそも off なら `None`｡`session_notice` と同じく `state` に
    /// 関わらず消えないバナーとして描く｡別のフィールドなのは出所が別だから
    /// だ: あちらは起動時の credential 解決が言うことで､こちらは何時間も
    /// 経ってから X が取得を断ったと言うことだ｡片方がもう片方を上書きしては
    /// ならない｡
    ///
    /// 設定するのは [`auto_refresh::TimelineView::apply_poll`] で一度だけ｡
    /// ループはその直後に終わるので二度は通らない｡消えるのは
    /// [`Self::start_auto_refresh`] が新しいループを始めるとき — つまり
    /// サインインし直したときだ｡
    auto_refresh_notice: Option<SharedString>,
    /// cooldown か失敗した reload｡`state` から独立に保つ (#57) — 理由は
    /// [`ReloadNotice`] の doc を見よ｡直近の reload の試み (あれば) が
    /// 阻まれても失敗してもいないときは `None`; [`Self::reload`] の早期
    /// return､[`Self::load_older`] の同じ経路､
    /// [`Self::apply_reload_failure`] の結果処理で設定し､reload が始まるか
    /// 成功した瞬間に消す｡
    reload_notice: Option<ReloadNotice>,
    /// `reload_notice` が生きた `ReloadNotice::Cooldown` を持っている間､
    /// そのカウントダウンを毎秒刻む (#57 の項目 3) —
    /// [`cooldown_label`] は描画時にしか文言を計算し直さないので､定期的な
    /// `cx.notify()` が無ければバナーは最後に描かれたときの秒数で固まる｡
    /// 今 cooldown が刻まれていないときは `None`｡`fetch`/`sign_in_flow` と
    /// 同じ drop で取り消す慣習: これを代入し直す (まだ走っているものを
    /// 新しい cooldown が置き換える) か消す (成功時や素の失敗時の即時
    /// 停止 — [`Self::apply_reload_failure`] を見よ) と､走っていたループは
    /// drop され取り消される｡ループ自体は
    /// [`Self::start_cooldown_ticker`] を
    /// 見よ｡
    cooldown_ticker: Option<Task<()>>,
    /// 追跡しているすべての endpoint を通じたリクエスト数の合計 (#18)｡header
    /// に出る — [`Self::refresh_usage`] を見よ｡最初の refresh が終わるまで
    /// ゼロだが､これはプレースホルダではなく正直な「まだ何も観測していない」
    /// である｡空の `usage.json` を読んでも `usage::Totals::default()` と
    /// まったく同じになるからだ｡
    usage_totals: usage::Totals,
    /// これを保持している間は header の usage refresh が生きつづける;
    /// `fetch` の drop で取り消す契約に倣う｡代入し直す (別の refresh) と､
    /// まだ走っていた読み取りは drop され取り消される｡
    usage_refresh: Option<Task<()>>,
    /// 背景の list sync を生かしておく — `config.list_id` のメンバーを
    /// このアプリがフォローしているアカウントに合わせつづけるループである｡
    /// [`Self::start`] から始め､[`Self::sign_in`] からも始める｡sync が
    /// 拒まれていたかもしれない scope を与えるのは再認可だからだ｡代入し
    /// 直すと前のループが drop される — `usage_refresh` の drop で取り消す
    /// 契約 — ので､同じ plan ファイルを扱うものが二つ以上になることは
    /// 無く､最後の一つはウィンドウと一緒に
    /// 退く｡
    auto_sync: Option<Task<()>>,
    /// list sync が何をしているか｡status bar が報告するためのもの (#174)｡
    ///
    /// ループが tick のたびに書き､始まる前には gate が書く; 読むのは
    /// [`Self::status_bar`] だけ｡#174 まではこの機能はウィンドウから
    /// まったく見えなかった: 止まった sync も､1100 件の書き込みだけ遅れた
    /// sync も､やることの無い sync も､見た目はどれも同じ､つまり何も
    /// 無いのと同じだった｡
    sync_status: SyncStatus,
    /// 手動 sync の確認ダイアログが開いているかどうか (#174, #205)｡この
    /// ウィンドウで最も高くつくクリックの背後にある確認｡#174 では
    /// `pending_delete` と同じ二段構えのクリックで､#205 でダイアログになった｡
    pending_sync: bool,
    /// ダイアログが開いた瞬間にディスクから読んだ､前の実行が残した計画の
    /// 残件数 (#205)｡
    ///
    /// `sync_status` からは取れない｡あれの `pending` を埋めるのは tick 1 回で､
    /// その tick こそダイアログが尋ねている当のもの｡毎フレームではなく
    /// [`Self::ask_to_sync`] で 1 回読む｡
    sync_plan_pending: usize,
    /// sync の行が今どれだけ濃いか (#205)｡[`Fade`] を参照｡
    ///
    /// `sync_status` とは別に持つ｡status は今どうなっているかで､これは画面が
    /// そこへどこまで追いついたか｡消えていく行は､もう報告するものが無い
    /// status を出したまま薄くなる｡
    sync_fade: Fade,
    /// フェードを 1 段ずつ進めるタイマー (#205)｡
    ///
    /// `auto_sync` と同じ drop で取り消す契約｡目的地に着いたら
    /// [`Self::fade_sync_row`] が外すので､落ち着いた行がフレームを焚き
    /// 続けない｡
    sync_fade_task: Option<Task<()>>,
    /// auto-refresh のループを生かしておく (#21) — ウィンドウが開いている間､
    /// timeline に新しい post が無いか polling するタイマーである｡`fetch`
    /// ではなく専用のスロットを持つのは意図的だ: ここから `fetch` に代入
    /// すると､読み手が始めたばかりの reload を取り消してしまうし､二つは
    /// 択一ではない｡`auto_sync` と同じ drop で取り消す契約で､
    /// `config.auto_refresh` が off のときはそもそも spawn しない —
    /// [`Self::start_auto_refresh`] を見よ｡これが #21 の「切れば
    /// アプリは何も送らない」を傾向ではなく保証にしている
    /// ものである｡
    auto_refresh: Option<Task<()>>,
    /// 直近の poll が取ってきたもの｡読み手が求めるまで画面へ出さずに
    /// 抑えておく (#21) — [`Pending`] と
    /// [`pending_after_poll`] を見よ｡
    ///
    /// auto-refresh が単に `state` を置き換えない理由のすべてがこれだ: 誰も
    /// 頼んでいない fetch が､読み手の目の下で文字を動かしてはならない｡
    /// `keep_the_reader_in_place` は読み手が押した reload のために scroll を
    /// 補正するが､あれは別の状況だ — そのときは一覧が変わることを期待して
    /// いる｡ここでは pill が押されるまで何も
    /// 変わらない｡
    ///
    /// 待っているものが何も無いときは `None`: poll がまだ着いていない､
    /// 直前の poll が新しいものを持ってこなかった､あるいはその後何かが
    /// より新しい source から timeline を置き換えた — どの経路がそうする
    /// のか､そして古くなったバッファがなぜ単に無駄なだけでなく誤りなのかは
    /// [`Self::clear_pending`] を見よ｡
    pending: Option<Pending>,
    /// 読み手が最上部にいると分かった poll が､新しい post をそのまま画面へ
    /// 流し込んでよいかどうか (#22) — 誰がいつ設定するのかは
    /// [`auto_refresh::follows`] と [`FollowMode`] を見よ｡
    follow: FollowMode,
    /// glide を生かしておく (#22) — follow が上へ post を差し込んだあと､
    /// scroll の offset を最上部へ戻していくフレームタイマーである｡専用の
    /// スロットなのは `auto_refresh` と同じ理由だ: 他のどの task スロットも､
    /// glide が取り消してよいものではない｡timeline を置き換えるか scroll
    /// 自体を動かす経路はすべてこれを drop する (ループを取り消す); ループ
    /// 自身も､offset が置いていった場所に無いと分かった瞬間に止まる｡それは
    /// 読み手がホイールを握った合図である｡
    glide: Option<Task<()>>,
    /// follow が流し込んだ新着のうち､まだ viewport の上に残っている数
    /// (#206)｡toast の countdown はこれを数える｡
    ///
    /// [`Self::follow`] が件数を置き､scroll 位置が動くたびに
    /// [`Self::note_scroll_position`] が減らす｡増える経路は follow だけで､
    /// timeline を置き換える経路は [`Self::clear_pending`] と同じ理由で 0 に
    /// 戻す — 置き換えられた行を基準に数えた数だからだ｡
    unseen: usize,
    /// 新着の toast が今どう見えているか (#206)｡[`Toast`] を参照｡
    ///
    /// `pending` と `unseen` が今何件かで､これは画面がそこへどこまで追い
    /// ついたか — `sync_fade` が `sync_status` に対してそうであるのと同じ｡
    toast: Toast,
    /// toast のフェードを 1 段ずつ進めるタイマー (#206)｡`sync_fade_task` と
    /// 同じ契約で､目的地に着いたら [`Self::fade_toast`] が外す｡
    toast_fade_task: Option<Task<()>>,
    /// 読み手自身の scroll の状態 (#175): ホイールが向かっている目標と､
    /// 端を越えて引いた rubber band｡入力は [`Self::on_wheel`] が渡し､
    /// `body` が band のずれを読む｡純粋なモデルで､なぜ gpui の既定の
    /// handler に任せないかは [`scroll`] のモジュール doc を見よ｡
    scroller: scroll::Scroller,
    /// `scroller` に動くものがあるあいだだけ生きるフレームのループ
    /// (#175)｡`glide` と同じ契約 — drop すれば止まる — で､`glide` と
    /// 同時に生きることは無い: 入力は glide を drop してからこちらを
    /// 始める｡
    scroll_motion: Option<Task<()>>,
    /// fixture の模擬 poll を生かしておく (#22): ウィンドウが開いてから､
    /// 抑えてあった post が [`Self::present_poll`] を通っていくまでの遅延で､
    /// 課金されるリクエスト無しに follow を手で観察できる｡live の
    /// ウィンドウでは常に `None` — [`Self::show_fixture`] を
    /// 見よ｡
    fixture_arrival: Option<Task<()>>,
    /// ローカルの記録によれば､このアプリが repost したすべての post id
    /// (#15) — 見えている timeline が変わるたびにディスクから読み直す
    /// ([`Self::refresh_reposted_ids`] を見よ)｡[`Self::repost_state_for`]
    /// の既定の出どころである; このセッションですでに触れた post について
    /// は下の `repost_overrides` が優先する｡
    reposted_ids: HashSet<String>,
    /// [`Self::refresh_reposted_ids`] の進行中の読み取りを生かしておく;
    /// `usage_refresh` の drop で取り消す契約に倣う｡
    reposted_ids_refresh: Option<Task<()>>,
    /// このセッションで触れた post についての post ごとの repost ボタンの
    /// 状態 (#15) — 進行中､失敗､あるいは完了したリクエストがすでに確定
    /// させた値であり､次の refresh が追いつくまでは `reposted_ids` より
    /// 権威がある｡無いことは「`reposted_ids` の素の on/off の値を使う」を
    /// 意味する — [`Self::repost_state_for`] を見よ｡
    repost_overrides: HashMap<String, ToggleState>,
    /// 進行中の repost の作成/削除リクエスト｡post id を鍵にし､
    /// `thread_fetches` の drop で取り消す契約に倣う: view を drop すると､
    /// まだ走っている toggle も一緒にすべて取り消される｡
    repost_tasks: HashMap<String, Task<()>>,
    /// ローカルの記録によれば､このアプリが like したすべての post id (#68)
    /// — `reposted_ids` の like 側の対応物｡二つの記録は独立した toggle が
    /// 書く別々のファイルなので､それぞれ別の set にしてある｡
    liked_ids: HashSet<String>,
    /// [`Self::refresh_liked_ids`] の進行中の読み取りを生かしておく;
    /// `reposted_ids_refresh` に倣う｡
    liked_ids_refresh: Option<Task<()>>,
    /// post ごとの like ボタンの状態 (#68) — これがそのまま倣っている
    /// `repost_overrides` を見よ｡
    like_overrides: HashMap<String, ToggleState>,
    /// 進行中の like の作成/削除リクエスト｡post id を鍵にする —
    /// `repost_tasks` を見よ｡
    like_tasks: HashMap<String, Task<()>>,
    /// 進行中の `open(1)` の spawn を生かしておく (#70); `usage_refresh` の
    /// drop で取り消す契約に倣う｡保持するのは一つだけだ: 最初のものがまだ
    /// spawn 中に二つ目のリンクを開くのは､待ち行列に入れる価値のある話では
    /// ない｡
    /// ダウンロード済みのアバター (#64)｡API 自身の `profile_image_url` を
    /// 鍵にする — 鍵は届いたままの URL であって実際に取得したより大きい
    /// variant ではないので､行は `avatar::preferred_url` の推測を繰り返さず
    /// に自分を引ける｡無いことは「まだダウンロードしていない」を意味し､
    /// プレースホルダが描かれる｡
    avatar_paths: HashMap<String, PathBuf>,
    /// ダウンロード済みの post の media (#65)｡media の URL を鍵にする —
    /// `avatar_paths` と同じ形だが､二つは別の cache ディレクトリに置かれ
    /// 別のフィールドから来るので､それぞれ別の map にしてある｡
    media_paths: HashMap<String, PathBuf>,
    /// 進行中のアバターのダウンロードを生かしておく (#64)｡行ごとに一つでは
    /// なく､一つの task が見えている timeline 全体を辿る; 代入し直す
    /// (reload) とまだダウンロード中のものは取り消されるが､次の呼び出しが
    /// 新しい timeline から集め直すのでかまわない｡
    avatar_fetch: Option<Task<()>>,
    /// 進行中の media のダウンロードを生かしておく (#65) — これが倣って
    /// いる `avatar_fetch` を見よ｡
    media_fetch: Option<Task<()>>,
    /// 今 delete の確認を出している post があればそれ (#72)｡一度に一つ
    /// だけ: 別の場所で二度目の "Delete" をクリックすると､二つ開くのでは
    /// なく確認が移る｡`None` はどの行も尋ねていないことを意味する｡
    ///
    /// modal ではなく二段構えのクリックにしたのは､delete が取り返しの
    /// つかない操作で､このアプリに dialog の仕掛けが無いからだ — 肝心なのは
    /// 一度のクリックで post を壊せないことで､これはそれを保証する｡
    pending_delete: Option<String>,
    /// 進行中の delete を生かしておく (#72)｡
    delete_task: Option<Task<()>>,
    /// 直近の delete が失敗した理由 (#72)｡尋ねた行の上に出る｡失敗が自分の
    /// 行に付いたままになるよう post id を鍵にする｡
    delete_failures: HashMap<String, String>,
    open_task: Option<Task<()>>,
    /// 直近の open の試みが失敗した理由 (#70)｡次の試みが消すまで header に
    /// 出る｡`None` が普通の場合である — open に成功すればアプリには言う
    /// ことが何も無い｡
    open_failure: Option<String>,
    /// timeline の一覧のスクロール位置 (#22)｡
    ///
    /// reload が一覧を置き換える前に読み､あとで読み手を元いた行へ戻すのに
    /// 使う: そうしないと､スクロール済みの一覧へ post を差し込んだときに
    /// すべてが読み手の下へずり下がる｡
    list_scroll: ScrollHandle,
    /// timeline 自身の root 要素の focus (#118)｡
    ///
    /// gpui は focus されている要素の祖先を辿って action を解決するので､
    /// これが無いと composer をクリックするまでここには何も届かない:
    /// `cmd-r` は何にも一致せず､メニューバーの Reload / New Post /
    /// Submit Post は灰色になるか行き先の無い dispatch になっていた｡`Quit`
    /// だけが `App` の上に居ることで逃れていた｡起動時に focus され､
    /// composer を離れたときに戻ってくる｡
    focus_handle: FocusHandle,
}

impl TimelineView {
    pub(crate) fn new(
        config: Config,
        paths: Paths,
        startup: Startup,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        // 毎回の render ではなく､ここで一度だけ解決する: `system` は OS の
        // appearance を読むのに生きた `Window` を要るし､`Theme` は `Copy`
        // なので､解決し直す代わりに持ち回っても何の代償も無い｡
        // `light`/`dark` の config 値はそもそもウィンドウに一切
        // 依存しない｡
        let theme = config.theme.resolve(window.appearance());
        // #38: 下で `Input` ウィジェットを組み立てる前に､gpui-component 自身
        // の global theme を同じ解決済みパレットへ向ける — そもそもなぜこれ
        // が要るのかは `theme::sync_gpui_component_theme` の doc を見よ
        // (その色はまったく別の global に居る)｡
        theme::sync_gpui_component_theme(theme, window, cx);

        let compose_input = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(2, 8)
                .placeholder("What's happening?")
        });
        let compose_input_subscription = cx.subscribe(&compose_input, Self::on_compose_input_event);

        // #161/#164: 下で `config` が move される前に取っておく｡
        let source = list_picker::initial_source(
            list_picker::saved_selection_for(&startup, &paths),
            config.list_id.as_deref(),
        );
        let owned_lists = list_picker::cached_lists_or_empty(&paths);
        let selection_file = matches!(startup, Startup::Live).then(|| paths.selection_file());
        // #22: `source` と同じく､下で `config` が move される前に取る｡
        let follow = FollowMode::from_config(config.follow_new_posts);

        let mut this = Self {
            config,
            paths,
            theme,
            client: None,
            state: TimelineState::Loading,
            fetch: None,
            sign_in_flow: None,
            last_reload_at: None,
            reloading: false,
            signed_in_with_oauth: false,
            home_user_id: None,
            home_username: None,
            source,
            owned_lists,
            lists_fetch: None,
            selection_file,
            next_page_token: None,
            threads: HashMap::new(),
            thread_fetches: HashMap::new(),
            compose: ComposeState::new(),
            compose_input,
            _compose_input_subscription: compose_input_subscription,
            submit_task: None,
            oauth_scope: None,
            session_notice: None,
            auto_refresh_notice: None,
            reload_notice: None,
            cooldown_ticker: None,
            usage_totals: usage::Totals::default(),
            usage_refresh: None,
            auto_sync: None,
            // #174: 正直な出発点｡まだ何もサインインしておらず､それは
            // gate の一つでもある｡そう言うほうが､一度も走っていない
            // "idle" よりましだ｡
            sync_status: SyncStatus::Off(SyncOff::NotSignedIn),
            pending_sync: false,
            sync_plan_pending: 0,
            sync_fade: Fade::Hidden,
            sync_fade_task: None,
            auto_refresh: None,
            pending: None,
            follow,
            glide: None,
            unseen: 0,
            toast: Toast::HIDDEN,
            toast_fade_task: None,
            scroller: scroll::Scroller::default(),
            scroll_motion: None,
            fixture_arrival: None,
            reposted_ids: HashSet::new(),
            reposted_ids_refresh: None,
            repost_overrides: HashMap::new(),
            repost_tasks: HashMap::new(),
            liked_ids: HashSet::new(),
            liked_ids_refresh: None,
            like_overrides: HashMap::new(),
            like_tasks: HashMap::new(),
            avatar_paths: HashMap::new(),
            media_paths: HashMap::new(),
            avatar_fetch: None,
            media_fetch: None,
            pending_delete: None,
            delete_task: None,
            delete_failures: HashMap::new(),
            open_task: None,
            open_failure: None,
            list_scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
        };
        // #118: 何よりも先に｡最初のフレームから focus の経路に空のものでは
        // なく timeline が乗るようにするため｡
        window.focus(&this.focus_handle);
        match startup {
            Startup::Live => this.start(cx),
            Startup::Fixture(fixture) => this.show_fixture(*fixture, cx),
        }
        this.refresh_usage(cx);
        this
    }

    /// fixture が未読の post を抑えておく時間｡それを運んできたはずの poll
    /// を模擬するまでの長さである (#22)｡ウィンドウを画面に出して手を
    /// ホイールから離すには十分に長く､眺めるのが面倒にならない程度には
    /// 短く｡
    const FIXTURE_ARRIVAL_SECONDS: u64 = 5;

    /// fixture だけを描き､他は何も描かない (#146)｡
    ///
    /// `client` は `None` のままで､これがこれを単に安いのではなく無料に
    /// している: この view のリクエストはすべてそこを通るので､ここから
    /// 課金へ至る経路が無い — reload も､like も､thread の辿りも｡client を
    /// 要るボタンは単に何もしない｡
    ///
    /// それでも `signed_in_with_oauth` と scope は設定する｡それらが門番を
    /// している affordance こそが､見るものの大半だからだ｡サインアウト状態の
    /// timeline として描かれた fixture では､確かめる価値のある行が
    /// 欠けてしまう｡
    fn show_fixture(&mut self, fixture: Fixture, cx: &mut Context<'_, Self>) {
        self.signed_in_with_oauth = true;
        // アプリが要求するすべての scope｡scope が足りないせいで affordance
        // が引っ込むことのないようにする｡fixture は決して fetch しないが
        // `list.read` (#161) はここに要る: `offers_reauthorize` が読むのは
        // ネットワークではなく scope なので､これを外すと list モードの
        // fixture すべてに "Re-authorize" ボタンが出た — レイアウトを
        // 見比べるための画面に常駐する備品である｡
        self.oauth_scope = Some(format!(
            "{} {} {}",
            oauth::tokens::TWEET_WRITE_SCOPE,
            oauth::tokens::LIKE_WRITE_SCOPE,
            oauth::tokens::LIST_READ_SCOPE
        ));
        self.home_user_id = Some(fixture.signed_in_as.id);
        self.home_username = Some(fixture.signed_in_as.username);
        self.owned_lists = fixture.lists;
        self.state = TimelineState::Loaded(fixture.items);
        // #21: 本物の poll のバッファと同じやり方で､同じ純粋関数から組む —
        // fixture が供給するのは件数ではなく post なので､bar が poll には
        // 言えないことを言うことはありえない｡バッファは表示することになる
        // 一覧の全体を持つ｡それは fixture の未読の post に､すでに画面に
        // 出ているものが続いたものである｡
        if !fixture.pending.is_empty() {
            let displayed: Vec<&str> = match &self.state {
                TimelineState::Loaded(items) => items.iter().map(|item| item.id.as_str()).collect(),
                _ => Vec::new(),
            };
            let combined: Vec<TimelineItem> = fixture
                .pending
                .iter()
                .cloned()
                .chain(match &self.state {
                    TimelineState::Loaded(items) => items.clone(),
                    _ => Vec::new(),
                })
                .collect();
            let waiting = pending_after_poll(&displayed, combined);
            if self.follow.is_following() {
                // #22: follow が on のとき､fixture に見せてほしいのは到着
                // そのものだ — だから pill をあらかじめ埋めるのではなく､
                // post を抑えておき､本物の poll が使う戸口を通らせる｡
                // 起動して手を触れずにいれば流れ込んでくるのが見える;
                // 先に下へスクロールしておけば､同じ配達が代わりに pill へ
                // 着く｡
                self.fixture_arrival = waiting.map(|waiting| {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_secs(Self::FIXTURE_ARRIVAL_SECONDS))
                            .await;
                        let _ = this.update(cx, |this, cx| this.present_poll(waiting, cx));
                    })
                });
            } else {
                self.pending = waiting;
            }
        }
        // #205: sync の状態｡`show_sync` を通すので､行が出るかどうかも
        // 出入りのフェードも本物の tick とまったく同じ経路を通る｡
        if let Some(sync) = fixture.sync {
            self.show_fixture_sync(&sync, cx);
        }
        // アバターと添付画像は今もダウンロードされる｡API ではなく
        // `pbs.twimg.com` からで､quota も credit も要らない (`avatar` を
        // 見よ)｡URL に届かない fixture は､まだ取得中のときと同じ枠を
        // 描く｡レイアウトの確認に要るのはどのみち
        // それである｡
        self.refresh_images(cx);
        cx.notify();
    }

    /// `post_id` について描く like ボタンの状態 (#68) — これが倣っている
    /// [`Self::repost_state_for`] を見よ｡
    fn like_state_for(&self, post_id: &str) -> ToggleState {
        self.like_overrides
            .get(post_id)
            .cloned()
            .unwrap_or_else(|| ToggleState::new(self.liked_ids.contains(post_id)))
    }

    /// 一つの post の本文の下に置く添付 media のグリッド (#65)｡
    ///
    /// サムネイルは最大 [`MAX_RENDERED_MEDIA`] 枚､[`media_columns`] 列で
    /// 並べる｡各セルは固定の高さなので､行の高さがどの画像のダウンロードを
    /// 終えたかに依存することはありえない — 画像が着くたびに読み手の下で
    /// 組み直される timeline は､埋まるのを待つ枠を見せる timeline より
    /// 悪い｡
    fn media_grid(&self, media: &[PostMedia], cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = self.theme;
        let shown: Vec<&PostMedia> = media.iter().take(MAX_RENDERED_MEDIA).collect();
        let columns = media_columns(shown.len());

        let mut grid = div().flex().flex_col().gap_1();
        for chunk in shown.chunks(columns) {
            let mut row = div().flex().gap_1();
            for media in chunk {
                row = row.child(self.media_cell(media, theme, cx));
            }
            grid = grid.child(row);
        }
        grid.into_any_element()
    }

    /// [`Self::media_grid`]｡描く media が無いときは何も返さない (#123) —
    /// quote card に要るものである｡ほとんどの quote は media を持たず､
    /// 空のグリッドでも card に gap を足してしまうからだ｡
    fn media_grid_for(
        &self,
        media: &[PostMedia],
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        (!media.is_empty()).then(|| self.media_grid(media, cx))
    }

    /// サムネイル一つ: ダウンロードした画像が着いていればそれ､無ければ同じ
    /// 大きさの枠 (#65)｡クリックすると原寸の画像をブラウザで開く (#70) —
    /// このアプリに lightbox は無く､ちゃんと見る手段の無いサムネイルは機能
    /// の半分でしかない｡動画やアニメーション GIF は静止画と､それがどちらか
    /// を示す badge を出す; どちらもここでは再生されない｡
    fn media_cell(
        &self,
        media: &PostMedia,
        theme: Theme,
        cx: &mut Context<'_, TimelineView>,
    ) -> AnyElement {
        let url = media.url.clone();

        let inner = match self.media_paths.get(&media.url) {
            Some(path) => img(path.clone())
                .h(MEDIA_CELL_HEIGHT)
                .rounded(theme::RADIUS_THUMB)
                .into_any_element(),
            None => div()
                .h(MEDIA_CELL_HEIGHT)
                .w(MEDIA_CELL_HEIGHT)
                .rounded(theme::RADIUS_THUMB)
                .bg(rgb(theme.border))
                .into_any_element(),
        };

        let mut cell = div()
            .addressable(format!("media-{}", media.url))
            .flex()
            .flex_col()
            .gap_1()
            .child(inner)
            .on_click(cx.listener(move |this, _event, _window, cx| {
                this.open_in_browser(url.clone(), cx);
            }));

        if let Some(badge) = media_badge(media.kind.as_deref()) {
            cell = cell.child(div().text_color(rgb(theme.text_muted)).child(badge));
        }
        if let Some(alt) = media.alt_text.as_ref() {
            // hover の裏に隠さず出す: このアプリ自身には screen reader の
            // 経路が無く､目の見える読み手が読める alt text のほうが､誰も
            // たどり着けない alt text より役に立つ｡
            cell = cell.child(
                div()
                    .text_color(rgb(theme.text_muted))
                    .child(format!("Alt: {alt}")),
            );
        }
        cell.into_any_element()
    }

    /// 一つの post の著者のアバター (#64): ディスクに落ちていればその画像､
    /// 無ければ [`avatar_placeholder`] — 二つは同じ大きさなので､画像が
    /// 着いても行は組み直されない｡
    fn avatar(&self, item: &TimelineItem, theme: Theme) -> AnyElement {
        let cached = item
            .author_avatar_url
            .as_deref()
            .and_then(|url| self.avatar_paths.get(url));

        match cached {
            Some(path) => img(path.clone())
                .size(AVATAR_SIZE)
                .flex_shrink_0()
                .rounded(theme::AVATAR_RADIUS)
                .into_any_element(),
            None => avatar_placeholder(&item.author_name, theme),
        }
    }

    /// `post_id` を削除する前に確認を求める (#72) — 二段構えの一度目の
    /// クリック｡他の行の確認待ちを置き換えるので､削除まであと一クリックの
    /// post は常に一つだけである｡
    fn ask_to_delete(&mut self, post_id: String, cx: &mut Context<'_, Self>) {
        self.delete_failures.remove(&post_id);
        self.pending_delete = Some(post_id);
        cx.notify();
    }

    /// 何も削除せずに delete の確認を引っ込める (#72)｡
    fn cancel_delete(&mut self, cx: &mut Context<'_, Self>) {
        self.pending_delete = None;
        cx.notify();
    }

    /// 一つの post の delete の affordance (#72): "Delete"､あるいは
    /// クリックされたあとの確認の二つ組｡直近の試みが失敗していれば､その
    /// 理由も添える｡
    fn delete_row(&self, item: &TimelineItem, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = self.theme;
        let asking = self.pending_delete.as_deref() == Some(item.id.as_str());

        let controls = if asking {
            let confirm_id = item.id.clone();
            div()
                .flex()
                .gap_3()
                .child(
                    div()
                        .addressable(format!("delete-confirm-{}", item.id))
                        .text_color(rgb(theme.danger))
                        .child("Delete permanently")
                        .on_click(cx.listener(move |this, _event, _window, cx| {
                            this.confirm_delete(confirm_id.clone(), cx);
                        })),
                )
                .child(
                    div()
                        .addressable(format!("delete-cancel-{}", item.id))
                        .text_color(rgb(theme.text_muted))
                        .child("Cancel")
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.cancel_delete(cx);
                        })),
                )
        } else {
            let ask_id = item.id.clone();
            div().child(
                div()
                    .addressable(format!("delete-{}", item.id))
                    .text_color(rgb(theme.text_muted))
                    .child("Delete")
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.ask_to_delete(ask_id.clone(), cx);
                    })),
            )
        };

        match self.delete_failures.get(&item.id) {
            Some(message) => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_color(rgb(theme.danger)).child(message.clone()))
                .child(controls)
                .into_any_element(),
            None => controls.into_any_element(),
        }
    }

    /// 一つの post の like/unlike の toggle (#68)｡[`offers_like`] が `item`
    /// について許すときに描く｡
    fn like_button(&self, item: &TimelineItem, cx: &mut Context<'_, Self>) -> AnyElement {
        // #52: 行は自分自身の id を鍵にするが (行ごとに一意なので､一つの
        // 原文への二つの repost が要素として衝突しない)､リクエストが作用
        // するのは原文のほうである｡
        let target = action_post_id(item);
        let state = self.like_state_for(target);
        like_row(&item.id, target, &state, self.theme, cx)
    }

    /// `post_id` について描くボタンの状態 (#15): このセッションがすでに
    /// 知っていること (進行中､失敗､あるいは完了したリクエストが確定させた
    /// 値) があればそれ｡無ければ `refresh_reposted_ids` が最後に読んだ
    /// ローカルの記録の素の on/off の値｡
    fn repost_state_for(&self, post_id: &str) -> ToggleState {
        self.repost_overrides
            .get(post_id)
            .cloned()
            .unwrap_or_else(|| ToggleState::new(self.reposted_ids.contains(post_id)))
    }

    /// 一つの post の repost/un-repost の toggle (#15)｡[`offers_repost`] が
    /// `item` について許すときに描く｡
    fn repost_button(&self, item: &TimelineItem, cx: &mut Context<'_, Self>) -> AnyElement {
        // #52: 要素の id は行から､リクエストの対象は原文から取る｡
        let target = action_post_id(item);
        let state = self.repost_state_for(target);
        repost_row(&item.id, target, &state, self.theme, cx)
    }

    /// `InputEvent::Change` のたびに `compose_input` のバッファを
    /// `self.compose` へ写す (#38) — `compose.rs` がウィジェットを直接読む
    /// のではなくそもそもこの写しが在る理由は､`compose_input` フィールドの
    /// doc を見よ｡`PressEnter`/`Focus`/`Blur` はこの view に要るものを何も
    /// 運ばない: 複数行モードではウィジェット自身の中ですでに Enter が改行に
    /// なる (`InputState::enter`) ので､ここでの `PressEnter` は submit では
    /// なく素の scroll-into-view でしか発火しない｡
    // `Context::subscribe` のコールバックの境界は `&Entity<T2>` ではなく
    // `Entity<T2>` を値で要求する — こちら側で変えられるものは無い｡
    #[allow(clippy::needless_pass_by_value)]
    fn on_compose_input_event(
        &mut self,
        input: Entity<InputState>,
        event: &InputEvent,
        cx: &mut Context<'_, Self>,
    ) {
        if let InputEvent::Change = event {
            self.compose.set_text(input.read(cx).value().to_string());
            cx.notify();
        }
    }

    /// `compose.quote()` が持っていれば､composer の中に出す quote の対象
    /// (#16)｡二つ目を作らず #13 の [`quote_card`] の描画を再利用し､その下に
    /// "Remove quote" の操作を足してある｡"Quote" の押し間違いで下書き全体を
    /// 捨てずに済むようにするためだ — それは `submit_post` ではなく必ず
    /// `ComposeState::clear_quote` を通るので､どちらにせよ下書きの本文には
    /// 手が触れない｡
    fn composer_quote_card(
        &self,
        target: &compose::QuoteTarget,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(quote_card(&target.quoted, theme, None))
            .child(
                div()
                    .addressable("compose-remove-quote")
                    .text_color(rgb(theme.accent))
                    .child("Remove quote")
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.compose.clear_quote();
                        cx.notify();
                    })),
            )
    }

    /// `compose.reply()` が持っていれば､composer の中に出す reply の対象
    /// (#71)｡
    ///
    /// quote の対象と同じ [`quote_card`] の描画を使い､その上に明示的な
    /// "Replying to" の見出しを置く — card だけでは下書きが二つのどちらな
    /// のか言えないし､その違いは後からでは見えない: reply は会話の下に
    /// 着くが､quote はそうではない｡
    fn composer_reply_card(
        &self,
        target: &compose::ReplyTarget,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_color(rgb(theme.text_muted))
                    .child(reply_target_label(&target.replying_to.author_username)),
            )
            .child(quote_card(&target.replying_to, theme, None))
            .child(
                div()
                    .addressable("compose-remove-reply")
                    .text_color(rgb(theme.accent))
                    .child("Remove reply")
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.compose.clear_reply();
                        cx.notify();
                    })),
            )
    }

    /// post の composer (#14): 本物のテキスト入力 (#38)､文字数カウンタ､
    /// submit ボタン｡session が OAuth でサインインしていれば出る —
    /// `tweet.write` scope が無くてもこれを丸ごと隠さない理由は
    /// [`Render::render`] の doc を見よ｡#16 で quote の対象の card が
    /// 設定されていれば加わる — [`Self::composer_quote_card`] を見よ｡
    fn composer(&self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = self.theme;
        let text = self.compose.text().to_string();
        let length = compose::weighted_length(&text);
        let over_limit = length > compose::MAX_WEIGHTED_LENGTH;
        let can_submit = self.compose.can_submit();
        let is_submitting = self.compose.is_submitting();
        let counter_color = if over_limit {
            theme.danger
        } else {
            theme.text_muted
        };
        // #95: カウンタと Post ボタンは入力欄が使われはじめてから現れる｡
        // 何もしていないウィンドウには､誰も書いていない post のための
        // 件数とボタンではなく､静かな一行だけが出るようにするためだ｡
        //
        // 空でない下書きがあれば focus に関わらず出しつづける｡下書きが
        // あるのにボタンを隠すと､それを送る唯一の道が入力欄をクリック
        // し直す先に隠れてしまう — #14 は下書きを決して失わないことを
        // composer の主たる約束としているのに､隠れた送信ボタンはそれを
        // 黙って破る｡
        let showing_controls = self.compose_input.focus_handle(cx).is_focused(window)
            || !text.trim().is_empty()
            || is_submitting;

        div()
            .flex()
            .flex_col()
            .gap_2()
            .px(theme::ROW_PAD_X)
            .py(theme::ROW_PAD_Y)
            .border_b_1()
            .border_color(rgb(theme.border))
            // submit が進行中の間は編集を拒む｡下の submit ボタン自身の
            // 無効状態に倣う — なぜそれが大事なのかは
            // `ComposeState::can_submit` の doc を見よ｡
            .child(Input::new(&self.compose_input).disabled(is_submitting))
            // #16: "Quote" が設定していれば quote の対象 —
            // `composer_quote_card` の doc を見よ｡
            .when_some(self.compose.quote(), |column, target| {
                column.child(self.composer_quote_card(target, cx))
            })
            // #71: "Reply" が設定していれば reply の対象｡両方になることは
            // 決してない — `ComposeState::set_reply` を見よ｡
            .when_some(self.compose.reply(), |column, target| {
                column.child(self.composer_reply_card(target, cx))
            })
            .when_some(
                compose_error_message(self.compose.status()),
                |column, message| column.child(div().text_color(rgb(theme.danger)).child(message)),
            )
            .when(showing_controls, |composer| {
                composer.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                // #95: 本文ではなく､操作の脇に添える
                                // 読み取り値｡
                                .text_size(theme::TEXT_META)
                                .text_color(rgb(counter_color))
                                .child(format!("{length}/{}", compose::MAX_WEIGHTED_LENGTH)),
                        )
                        .child(
                            div()
                                .addressable("compose-submit")
                                .px_2()
                                .py_1()
                                .rounded(theme::RADIUS_CONTROL)
                                // #95: これは *本当に* default ボタンである
                                // — composer の存在意義そのものだ — ので､
                                // 押せる間は accent の塗りを保つ｡変えたのは
                                // もう一方の状態だ: 押せないボタンは以前
                                // 濃い灰色の塗りつぶしで､それは off の操作
                                // ではなく単に色が違う操作に見える｡macOS は
                                // 代わりに塗りを
                                // 抜く｡
                                .when(can_submit, |button| {
                                    button
                                        .bg(rgb(theme.accent))
                                        .text_color(rgb(theme.button_label))
                                })
                                .when(!can_submit, |button| {
                                    button
                                        .border_1()
                                        .border_color(rgb(theme.border))
                                        .text_color(rgb(theme.text_tertiary))
                                })
                                .text_size(theme::TEXT_META)
                                .child(if is_submitting { "Posting…" } else { "Post" })
                                // #14 の二重送信ガード､その二: submit が
                                // 進行中の間 (あるいは下書きが空か長さ超過
                                // のとき) ボタンは無効に見える見た目だけで
                                // なく､click ハンドラをそもそも持たない —
                                // `submit_post` はどのみち同じ条件を再確認
                                // するが､click がそこへ届くこと自体を
                                // 止めているのはこちらである｡
                                .when(can_submit, |button| {
                                    button.on_click(cx.listener(|this, _event, window, cx| {
                                        this.submit_post(window, cx);
                                    }))
                                }),
                        ),
                )
            })
    }

    fn header(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        // #57: `state` の match に畳み込まず､その手前で判定する — post が
        // すでに出ている間の進行中の reload は `state` を `Loaded` のままに
        // する (`reload_start_state` を見よ) ので､その場合に fetch が走って
        // いることを示す信号はこれだけである｡
        let (label, busy, action) = if self.reloading {
            ("Loading…".to_string(), true, PrimaryAction::Reload)
        } else {
            match self.state {
                TimelineState::Loading => ("Loading…".to_string(), true, PrimaryAction::Reload),
                TimelineState::SigningIn => {
                    ("Signing in…".to_string(), true, PrimaryAction::SignIn)
                }
                TimelineState::NotAuthenticated => {
                    ("Sign in with X".to_string(), false, PrimaryAction::SignIn)
                }
                // 今も `PrimaryAction::Reload` に繋ぐ: クリックし直しても
                // (ネットワーク不要の) rate-limit 判定が走り直るだけだ — #10 が
                // 禁じるのは window を寝て過ごすことで､安い判定の再実行ではない｡
                TimelineState::RateLimited { reset_at, cooldown } => (
                    cooldown_label(cooldown, reset_at, oauth::unix_now()),
                    true,
                    PrimaryAction::Reload,
                ),
                TimelineState::Loaded(_) | TimelineState::Failed(_) => {
                    ("Reload".to_string(), false, PrimaryAction::Reload)
                }
            }
        };

        let theme = self.theme;

        div()
            .flex()
            .items_center()
            .gap_3()
            // #95: 二行の masthead ではなく toolbar である｡タイトルの下に
            // 居たリクエスト数は `status_bar` へ移り､残るのは一行 — なので
            // この帯は､二行を積んだときに要る高さへ詰め物をするのではなく､
            // macOS の toolbar と同じ寸法にしてある｡
            .h(theme::TOOLBAR_HEIGHT)
            .px(theme::ROW_PAD_X)
            .bg(rgb(theme.bg_header))
            .border_b_1()
            .border_color(rgb(theme.border))
            // #95 の枠に #164 の segment: Home と所有するすべての list —
            // ウィンドウより広い picker が右側の操作 (サインイン､reload) を
            // 画面の外へ押し出すのではなく､縮んで切り取られるように包んで
            // ある｡flex item を中身が望むより狭くできるのが `min_w(0)` で､
            // これが無いと 560px で 11 個のタブが "Sign in with X" を
            // ウィンドウの外へ追い出し､body の文はそれをクリックせよと
            // 言っていた｡切り取られたタブをもっとうまく見せること
            // (スクロール､ドロップダウン) は #192 の仕事｡
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .child(self.list_picker(cx))
                    .children(self.lists_control(cx))
                    .child(header_title_element(self.home_username.as_deref(), theme)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .ml_auto()
                    // #14: #14 より前からサインイン済みの session は
                    // `tweet.write` scope を持たない — 主ボタンが今何と
                    // 言っていようとこれが届くところに残らないかぎり､#31 の
                    // 教訓がそのまま繰り返される (すでに有効な session が
                    // 自分の格上げ経路を隠してしまう)｡
                    .when(
                        offers_reauthorize(
                            self.signed_in_with_oauth,
                            self.oauth_scope.as_deref(),
                            matches!(self.source, cache::TimelineSource::List(_)),
                        ),
                        |row| row.child(sign_in_pill("reauthorize", "Re-authorize", theme, cx)),
                    )
                    .child(self.primary_action_control(&label, busy, action, cx)),
            )
    }

    /// toolbar の唯一の action: reload､あるいはまだ session が無いときは
    /// サインイン (#95)｡
    ///
    /// 二つがまったく似ていないのは意図的だ｡reload はアイコンである —
    /// この操作は不変で頻繁で､どのアプリも共有する記号で名指されるので､
    /// 枠付きのボタンに書き下すと毎フレームの隅が timeline より騒がしく
    /// なった｡言うことのある状態 ("Loading…"､rate limit のカウントダウン)
    /// のために `label` は今も在るが､それらはすでに `body` と #57 のバナー
    /// 経由で読み手に届くので､ここではアイコンを暗くする
    /// だけである｡
    ///
    /// サインインは言葉と塗りを保つ: session が無ければウィンドウで他に
    /// できることは無いし､ラベルの無い字形は､アプリが自分を説明せねば
    /// ならないまさにその瞬間に謎かけになる｡
    fn primary_action_control(
        &self,
        label: &str,
        busy: bool,
        action: PrimaryAction,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let on_click = cx.listener(move |this, _event, _window, cx| match action {
            PrimaryAction::Reload => this.reload(ReloadTrigger::Polling, cx),
            PrimaryAction::SignIn => this.sign_in(cx),
        });

        match action {
            PrimaryAction::Reload => div()
                .addressable("primary-action")
                .p_1()
                .rounded(theme::RADIUS_CONTROL)
                .child(
                    svg()
                        .path(assets::RELOAD_ICON)
                        .size(theme::ICON_SIZE)
                        .text_color(rgb(if busy {
                            theme.text_tertiary
                        } else {
                            theme.text_muted
                        })),
                )
                .on_click(on_click)
                .into_any_element(),
            PrimaryAction::SignIn => div()
                .addressable("primary-action")
                .px_2()
                .py_1()
                .rounded(theme::RADIUS_CONTROL)
                .text_size(theme::TEXT_META)
                .when(busy, |button| {
                    button
                        .border_1()
                        .border_color(rgb(theme.border))
                        .text_color(rgb(theme.text_tertiary))
                })
                .when(!busy, |button| {
                    button
                        .bg(rgb(theme.accent))
                        .text_color(rgb(theme.button_label))
                })
                .child(label.to_string())
                .on_click(on_click)
                .into_any_element(),
        }
    }

    /// ウィンドウの下端に沿う帯 (#95)｡
    ///
    /// #95 まではリクエスト数がウィンドウのタイトルの下に居て､毎フレーム
    /// 最初に読まれる座をアカウント名と奪い合っていた｡macOS はウィンドウの
    /// 累計を代わりに status bar に置く — Finder の項目数が同じ考えだ —
    /// ので､こちらもそこへ置く｡#18 の段階的な色づけは移動しても変わらない:
    /// 数は今も `daily_request_budget` へ近づけば `warning` になり､
    /// 超えれば `danger` に
    /// なる｡
    ///
    /// 保持している post の数は timeline が読み込まれてからしか出さない｡
    /// サインイン中や取得中には出せる数が無いし､"0 / 200" は答えの無い
    /// 問いではなく空の cache のように読めてしまう｡
    fn status_bar(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = self.theme;

        // #18: リクエスト数は常に出す; 見積り金額は `request_price` が
        // 設定されているときだけ後ろに足す (`usage_label` の doc を
        // 見よ)｡
        let usage_status =
            usage::budget_status(self.usage_totals.today, self.config.daily_request_budget);
        let usage_text = usage_label(
            self.usage_totals.today,
            self.usage_totals.total,
            self.config.request_price,
        );
        let kept = match self.state {
            TimelineState::Loaded(ref items) => Some(items.len()),
            _ => None,
        };

        div()
            // #205: sync の行が「footer の 1 段上」に居ることをテストが
            // 読み返せるように名前を持つ｡帯そのものに名前が要るのは､
            // 中の区画の bounds では帯の上端が分からないからだ｡
            .addressable("status-bar")
            .flex()
            .items_center()
            .gap_3()
            .h(theme::STATUS_BAR_HEIGHT)
            .px(theme::ROW_PAD_X)
            .bg(rgb(theme.bg_header))
            .border_t_1()
            .border_color(rgb(theme.border))
            .text_size(theme::TEXT_META)
            .child(
                div()
                    .addressable("status-usage")
                    .text_color(rgb(usage_color(usage_status, theme)))
                    .child(usage_text),
            )
            // #174: list sync を 1 回始める手段｡toolbar ではなくリクエスト数の
            // 隣に置くのは､同じ種類の事実 (timeline ではなくアプリについての
            // 累計) だから｡
            //
            // #205: sync が何をしているかは上の行へ移った｡ここに残るのは
            // 入口だけで､文言は状態によらず動かない｡
            //
            // この margin は､どう読めようとも行の `gap_3` と重複しては
            // いない｡ここはウィンドウで唯一､裸のテキスト span が二つ兄弟に
            // なる場所で — 他はどこも子が自分の padding を持つ — 画面上で
            // gap はそれらをまったく引き離さない: "Total: 11 req" と
            // "List sync: …" は "11 reqList sync" のようにくっついて
            // 描かれる｡gap を `gap_8` へ上げても何も変わらないので､間隔は
            // ここで実際に効くと示せる場所から来なければ
            // ならない｡
            //
            // #184: この margin は今テストの下にある｡どちらの segment にも
            // 名前が付いているので､ウィンドウのテストが配置後の bounds を
            // 読み返して､それらが接していないことを要求できる — それこそが
            // 欠陥そのもので､このコメントを書いた時点ではスクリーンショット
            // 以外に捕まえる手が無かった
            // ものである｡
            .child(
                div()
                    .addressable("status-sync")
                    .ml(theme::ROW_PAD_X)
                    .child(self.sync_segment(cx)),
            )
            .when_some(kept, |bar, kept| {
                bar.child(
                    div()
                        .ml_auto()
                        .text_color(rgb(theme.text_tertiary))
                        .child(format!("{kept} / {} posts kept", cache::MAX_CACHED_POSTS)),
                )
            })
    }

    fn post_row(&self, item: &TimelineItem, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = self.theme;
        let byline = byline(&item.author_username);

        let counts = row_counts(item.metrics.as_ref());

        // #64: アバターは左の独立した列に座るので､下の body は別に組み立てて
        // からその隣に置く｡
        let body = div()
            .flex()
            .flex_col()
            .flex_1()
            // #140: `flex_1` が取るのは *余った* 幅であって､中身より狭く
            // 縮むことは許さない｡flex の子の `min-width` の既定が `auto`
            // だからだ｡そのため長い文が列を行より広く押し広げ､はみ出しは
            // 切り取られていた｡代わりに折り返させるのが `min_w_0` である｡
            //
            // これがあの時点で表に出たのは #103 のせいだ: アバターが
            // `flex_shrink_0` を得る前は､アバターが潰れることではみ出しを
            // 吸収していた｡それを固定したのは正しく､そして余った幅の
            // 行き先として body だけが
            // 残った｡
            .min_w_0()
            .gap_1()
            // #95: meta 行は一本｡著者､byline､timestamp､そして "reposted" /
            // "replying to" のうち当てはまるほうが､みな一緒に並ぶ — #95 まで
            // は後ろの二つが名前の上の全幅の行を占めていて､二行の post が
            // 四行に膨らんでいた｡
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .text_size(theme::TEXT_META)
                    // #70: 著者名と handle は x.com のプロフィールを開く｡
                    // username が展開されなかったときは `profile_url` が
                    // `None` を返し､その場合は行き先の無いリンクにはならず
                    // ただの文字のままになる｡
                    .child(author_link(item, theme, cx))
                    .child(div().text_color(rgb(theme.text_muted)).child(byline))
                    .child(
                        div()
                            .text_color(rgb(theme.text_tertiary))
                            .child(format_timestamp(item.created_at.as_deref())),
                    )
                    // #13: repost は誰が repost したかを言う — この時点で
                    // body が持っているのはすでに *原文* の post であって
                    // (`TimelineResponse::into_items` の join を見よ)､外側の
                    // post 自身の著者ではない｡
                    .when_some(item.reposted_by.as_deref(), |line, reposted_by| {
                        line.child(
                            div()
                                .text_color(rgb(theme.text_tertiary))
                                .child(format!("· {}", repost_banner_label(reposted_by))),
                        )
                    })
                    // #12: この post が誰に返信しているか｡追加のリクエスト
                    // 費用ゼロで出せる — 親の著者は #13 の expansions により
                    // すでに `includes` に入っている｡
                    .when_some(item.replied_to.as_ref(), |line, replied_to| {
                        line.child(
                            div()
                                .text_color(rgb(theme.text_tertiary))
                                .child(format!("· {}", reply_banner_label(replied_to))),
                        )
                    }),
            )
            .child(div().child(item.text.clone()))
            // #70: 本文中のリンク｡本文が持つ `t.co` の短縮リンクから展開
            // したもの — 本文の中ではなく下に置く理由は `link_row` の doc を
            // 見よ｡
            .when(!item.links.is_empty(), |column| {
                column.child(link_row(&item.links, theme, cx))
            })
            // #65: 添付画像を､body の下のサムネイルとして出す｡
            .when(!item.media.is_empty(), |column| {
                column.child(self.media_grid(&item.media, cx))
            })
            // #13: quote (quote の repost も含む) は引用元を本文の下に枠付き
            // の card として埋め込む｡
            .when_some(item.quoted.as_ref(), |column, quoted| {
                column.child(quote_card(
                    quoted,
                    theme,
                    self.media_grid_for(&quoted.media, cx),
                ))
            })
            // #95: すべての action を横一行に並べ､それぞれが自分の件数を
            // 添える｡これがこの issue の主たる不満だ — 同じ一式が以前は
            // 行の下へ一行一ラベルで積み上がっていた｡
            .child(self.action_row(item, &counts, cx))
            // #12: "Show thread" — 提示するのは reply のときだけだ｡辿る親が
            // あるのはその場合だけだからである｡意図的に `action_row` の一部
            // にしていない: 読み込まれた thread は post の連なり全体へ広がる
            // ので､一行の帯の中には収まらない｡
            .when_some(item.replied_to.as_ref(), |column, replied_to| {
                column.child(self.thread_section(&item.id, replied_to, cx))
            });

        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .px(theme::ROW_PAD_X)
                    .py(theme::ROW_PAD_Y)
                    .child(self.avatar(item, theme))
                    .child(body),
            )
            // #95: 区切り線はアバターの下を走らず､本文が始まる位置から
            // 始まる｡macOS 自身の一覧 (Mail､Messages) が使う inset である｡
            // 行自身の下枠ではなく行の兄弟にしてあるのは､この inset を
            // padding として言い直さずに
            // 済ませるためだ｡
            .child(
                div()
                    .h(px(1.0))
                    .ml(theme::SEPARATOR_INSET)
                    .bg(rgb(theme.border)),
            )
            .into_any_element()
    }

    /// 一つの post のすべての action を横一行に (#95)｡
    ///
    /// どの action が現れるかは変わっていない — 決めるのは今も各 `offers_*`
    /// の述語である — が､今は一行一つずつ積み上がって別の metrics 行の上に
    /// 並ぶのではなく､engagement の件数を脇に添えて横に並ぶ｡リクエストが
    /// 失敗した like/repost は今もそのメッセージを描き､その行についてはこの
    /// 帯が下へ伸びる; それは `like_row`/`repost_row` 自身の仕業で､ここでは
    /// そのままにしてある｡
    fn action_row(
        &self,
        item: &TimelineItem,
        counts: &RowCounts,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = self.theme;

        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_4()
            .text_size(theme::TEXT_META)
            .text_color(rgb(theme.text_muted))
            // #71: "Reply" — composer の対象を設定する; 下書きが submit
            // されるまで何も送られない｡
            .when(offers_reply(self.signed_in_with_oauth, item), |row| {
                row.child(with_count(
                    reply_row(item, theme, cx),
                    counts.replies.as_deref(),
                    theme,
                ))
            })
            // #15: repost/un-repost — どの post に付くかは `offers_repost`
            // の doc をきっちり見よ｡
            .when(
                offers_repost(
                    self.signed_in_with_oauth,
                    self.home_user_id.as_deref(),
                    self.home_username.as_deref(),
                    item,
                ),
                |row| {
                    row.child(with_count(
                        self.repost_button(item, cx),
                        counts.reposts.as_deref(),
                        theme,
                    ))
                },
            )
            // #68: like/unlike — どの post に付くかは `offers_like` の doc を
            // 見よ｡repost と違い､これは自分自身の post にも提示される｡
            .when(
                offers_like(
                    self.signed_in_with_oauth,
                    self.home_user_id.as_deref(),
                    item,
                ),
                |row| {
                    row.child(with_count(
                        self.like_button(item, cx),
                        counts.likes.as_deref(),
                        theme,
                    ))
                },
            )
            // #16: "Quote" — どの post に付くかは `offers_quote` の doc を
            // きっちり見よ (repost の行が控えられるのは､`offers_repost` が
            // 自分のボタンを控えるのと同じ理由による)｡
            .when(offers_quote(self.signed_in_with_oauth, item), |row| {
                row.child(quote_row(item, theme, cx))
            })
            // #70: post そのものを x.com で開く｡
            .child(open_post_link(item, theme, cx))
            // #72: delete — 自分の post のみ､そして決して一クリックでは行わない｡
            .when(
                offers_delete(
                    self.signed_in_with_oauth,
                    self.home_user_id.as_deref(),
                    self.home_username.as_deref(),
                    item,
                ),
                |row| row.child(self.delete_row(item, cx)),
            )
            .into_any_element()
    }

    /// 1 つの返信 (#12) についての "Show thread" のトグル､読み込み中/エラーの
    /// 状態､あるいは組み上がったチェーン — `self.threads.get(reply_post_id)` が
    /// 今どれだと言うかによる｡[`Self::post_row`] から切り出したのは読みやすさの
    /// ためだけで､トグルのクリックハンドラのために `cx` は依然として要る｡
    fn thread_section(
        &self,
        reply_post_id: &str,
        replied_to: &RepliedTo,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = self.theme;

        let state = self.threads.get(reply_post_id);

        if let Some(ThreadFetchState::Loaded(chain)) = state {
            return render_thread_chain(chain, theme);
        }
        if matches!(state, Some(ThreadFetchState::Loading)) {
            return div()
                .text_color(rgb(theme.text_muted))
                .child("Loading thread…")
                .into_any_element();
        }

        // ここへ届く状態: `None` (一度も要求していない) と `Failed` — どちらも
        // クリックできるトグルを出す｡違うのはラベルだけで､詳しくは
        // `thread_action_label` を見る｡
        let label = thread_action_label(state).unwrap_or_default();
        let toggle = thread_toggle_row(
            reply_post_id.to_string(),
            replied_to.post_id.clone(),
            label,
            theme,
            cx,
        );

        if let Some(ThreadFetchState::Failed(message)) = state {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_color(rgb(theme.danger)).child(message.clone()))
                .child(toggle)
                .into_any_element()
        } else {
            toggle.into_any_element()
        }
    }

    fn body(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = self.theme;

        // `overflow_y_scroll` は StatefulInteractiveElement 側にあるので､
        // スクロールさせるには要素に先に id が要る｡
        let content = div()
            .addressable("timeline")
            .flex()
            .flex_col()
            .flex_1()
            .overflow_y_scroll()
            // #22: そもそもスクロール位置を読めるようにしているのがこの
            // ハンドルだ｡これが無ければリロードはビューポートを*ピクセル*の
            // 位置に留めることしかできず､上に行が挿入された後ではそこは
            // 間違った場所になる｡
            .track_scroll(&self.list_scroll)
            // #175: 端を越えて引いたぶんだけ一覧をずらす — 最上部では
            // 下へ､末尾では上へ｡offset は gpui が prepaint で clamp する
            // ので､端の向こうは offset ではなく位置で見せるしかない｡
            .relative()
            .top(px(self.scroller.shift()));

        let list = match &self.state {
            // ツールバー側のボタンを指す文ではなく､ボタンそのものを置く｡
            // リストのタブが増えるとそのボタンは右端の外へ押し出されるし
            // (#192)､唯一の案内が見えないボタンの名を挙げるだけのサインアウト
            // 済みウィンドウは､画面からは復帰しようがない — X が更新を拒んだ
            // セッションで 2026-08-24 に実測した状態だ｡
            TimelineState::NotAuthenticated => content.child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .child(
                        div()
                            .text_color(rgb(theme.text_muted))
                            .child("Not signed in."),
                    )
                    .child(sign_in_pill("sign-in-body", "Sign in with X", theme, cx)),
            ),
            TimelineState::SigningIn => content.child(notice(
                "Waiting for the browser to finish sign-in…",
                theme.text_muted,
            )),
            TimelineState::Loading => {
                content.child(notice("Fetching the timeline…", theme.text_muted))
            }
            TimelineState::RateLimited { reset_at, cooldown } => content.child(notice(
                cooldown_label(*cooldown, *reset_at, oauth::unix_now()),
                theme.danger,
            )),
            TimelineState::Failed(message) => content.child(notice(message.clone(), theme.danger)),
            TimelineState::Loaded(items) if items.is_empty() => {
                content.child(notice("No posts were returned.", theme.text_muted))
            }
            TimelineState::Loaded(items) => {
                // `.children(items.iter().map(...))` ではなく素のループにして
                // ある｡`post_row` は (#12 の "Show thread" のクリック
                // ハンドラのために) `cx` を要求するし､`.map` が呼ぶ `FnMut`
                // クロージャは､自分が捕捉した `cx` から借りた値を返り値の要素へ
                // 逃がせない｡
                let mut rows: Vec<AnyElement> = Vec::with_capacity(items.len());
                for item in items {
                    rows.push(self.post_row(item, cx));
                }
                content
                    .children(rows)
                    // #11: 再開に使う `meta.next_token` をレスポンスが実際に
                    // 運んできて初めて出す｡しかも取得するページの分だけ上限の
                    // 下に余地があるあいだだけだ｡
                    .when(
                        offers_load_older(self.next_page_token.as_deref(), &self.state),
                        |list| list.child(load_older_row(theme, cx)),
                    )
                    .when(at_the_post_cap(&self.state), |list| {
                        list.child(notice(
                            format!(
                                "Showing the most recent {} posts — that is as far back as \
                                 twigpui keeps.",
                                cache::MAX_CACHED_POSTS
                            ),
                            theme.text_muted,
                        ))
                    })
            }
        };

        // #175: ずれない wrapper｡band で一覧がずれても外へは描かせず､
        // ホイールを横取りする canvas はここに重ねる — ずれた一覧に
        // 重ねると､跳ねている最中に露出した端の上で入力が死ぬ｡
        div()
            .flex()
            .flex_col()
            .flex_1()
            .relative()
            .overflow_hidden()
            .child(list)
            .child(Self::wheel_capture(cx))
    }
}

/// リストの後ろに足す "Load older" の行 (#11)｡見た目は [`notice`] と同じだが
/// クリックできる — `cache::splice` 経由で､すでに表示されているものの*後ろ*へ
/// post を足すのであって､通常のリロードのように前へマージすることはない｡
fn load_older_row(theme: Theme, cx: &mut Context<'_, TimelineView>) -> impl IntoElement {
    div()
        .addressable("load-older")
        .px_4()
        .py_3()
        .text_color(rgb(theme.accent))
        .child("Load older")
        .on_click(cx.listener(|this, _event, _window, cx| this.load_older(cx)))
}

impl Render for TimelineView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = self.theme;

        div()
            // #58: どのバインディングもグローバルに登録するのではなく､この
            // コンテキストへ閉じてある — `init` を見る｡
            .key_context(KEY_CONTEXT)
            // #118: コンテキストが効くのは､その要素がウィンドウのフォーカス
            // パス上にあるあいだだけで､本当のルートは
            // `gpui_component::Root` だ (`main` を見る) — なのでこれが無いと
            // パスがコンテキストの手前で止まり､全バインディングが外れた｡
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Reload, _window, cx| {
                // ヘッダーのボタンが通るのと同じ経路｡#10 の間隔と #57 の
                // クールダウン報告も含む｡ショートカットが､このアプリが
                // ループで金を使うのを止めるためにあるスロットルの抜け道に
                // なってはいけない｡
                this.reload(ReloadTrigger::UserAction, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusComposer, window, cx| {
                this.compose_input
                    .update(cx, |input, cx| input.focus(window, cx));
            }))
            .on_action(cx.listener(|this, _: &BlurComposer, window, _cx| {
                // フォーカスだけ｡下書きは打ったとおりに残す｡誤爆した `esc` で
                // 失うと取り返しがつかないし､#14 はすでに下書きを絶対に
                // 失わないことを composer の主たる約束としている｡
                //
                // `window.blur()` で落とすのではなく timeline へ戻す (#118)｡
                // フォーカスパスが空になると `Timeline` コンテキストへ手が
                // 届かなくなり､次のクリックまで `esc` がショートカットと
                // メニューバーの半分を無効にしていた｡
                window.focus(&this.focus_handle);
            }))
            .on_action(cx.listener(|_this, _: &ShowAbout, window, cx| {
                // レシーバは待たずに落とす｡ボタンは 1 つしかないので､どれが
                // 押されたかは何の情報も運ばない｡`App` まで届く必要がある
                // アクションは `Quit` のほうで､こちらはウィンドウへ届けば
                // よいだけなので､残りと一緒にここへ置いてある｡
                drop(window.prompt(
                    gpui::PromptLevel::Info,
                    "twigpui",
                    Some(&format!(
                        "Version {}\n\nA development-only X timeline viewer \
                         for macOS, built with gpui.",
                        env!("CARGO_PKG_VERSION")
                    )),
                    &["OK"],
                    cx,
                ));
            }))
            .on_action(cx.listener(|this, _: &ShowNewPosts, _window, cx| {
                // #21: バーのクリックハンドラへキーボードから届く経路｡無料だ
                // — タイマーがすでに払った取得を見せるだけで､無いときは何も
                // しない｡
                this.apply_pending(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleFollowNewPosts, _window, cx| {
                // #22: メニューバーはチェックマークを描けないので､切り替えが
                // どちらへ倒れたかは､リロード完了時に使うバナーで報告する —
                // 失敗ではないほうのバリアントなので `Outcome` を使う｡
                this.follow = this.follow.flipped();
                let outcome = if this.follow.is_following() {
                    "Following new posts."
                } else {
                    "Not following — new posts will wait behind the pill."
                };
                this.reload_notice = Some(ReloadNotice::Outcome(outcome.into()));
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ScrollToTop, _window, cx| {
                // #22: 完全にローカル — リクエストもゲートも無いし､報告する
                // ことも無い｡ピクセルのオフセットではなく
                // `scroll_to_top_of_item(0)` にしてあるのは､最新の行そのものへ
                // 着地させるためだ｡進行中の glide も同じ場所へ歩いている —
                // ジャンプがそれに取って代わる｡ホイールの目標も同じ (#175):
                // 飛んだ先から古い目標へ引き戻してはいけない｡
                this.glide = None;
                this.scroll_motion = None;
                this.scroller.release();
                this.list_scroll.scroll_to_top_of_item(0);
                cx.notify();
            }))
            .on_action(cx.listener(|_this, _: &Minimize, window, _cx| {
                window.minimize_window();
            }))
            .on_action(cx.listener(|_this, _: &CloseWindow, window, _cx| {
                // ウィンドウが 1 つなのでこれはアプリを終える｡`cmd-q` と
                // まったく同じで､`cmd-q` と同様に先に確認もしない (#109)｡
                // 未送信の下書きも道連れになる — `cmd-q` が昔から持っていた
                // 危険と同じもので､これが新しく持ち込むものではない｡
                window.remove_window();
            }))
            .flex()
            .flex_col()
            .size_full()
            // #205: 手動 sync のダイアログの覆いが `absolute` で寄る先｡無いと
            // ウィンドウではなく最も近い配置済みの祖先が基準になる｡
            .relative()
            .bg(rgb(theme.bg))
            .text_color(rgb(theme.text))
            .text_size(theme::TEXT_BODY)
            .child(self.header(cx))
            // #54: `state` に関わらず出す — これが直す不具合はまさに､何も
            // 起きなかったかのように描かれる timeline なので､このバナーは下の
            // `body` が今何を出していようと独立して生き残らねばならない｡
            .when_some(self.session_notice.clone(), |column, message| {
                column.child(session_notice_banner("banner-session", message, theme))
            })
            // #239: 止まった auto-refresh｡上の `session_notice` とまったく
            // 同じ理屈で出す — timeline は起動時に読んだ post を出したまま
            // でいられるので､取得がもう走っていないことを言えるのはここ
            // しかない｡
            .when_some(self.auto_refresh_notice.clone(), |column, message| {
                column.child(session_notice_banner("banner-auto-refresh", message, theme))
            })
            // #57: 上の `session_notice` と同じ理屈 — クールダウンや失敗した
            // リロードは `body` とは独立に生き残らねばならない｡この時点の
            // `body` は前の post を出したままである可能性が十分にある｡
            .when_some(self.reload_notice.clone(), |column, notice| {
                column.child(reload_notice_banner(&notice, theme, oauth::unix_now()))
            })
            // #21: 自動更新が取得して抑えているもの｡`body` の中ではなく
            // バナーの隣に置くのは､バナーがそうである理由と同じだ —
            // `new_posts_bar` を見る — ただしこちらは報告ではなく､申し出
            // そのものである点が違う｡
            .when_some(
                self.pending.as_ref().map(|pending| pending.count),
                |column, count| column.child(new_posts_bar(count, theme, cx)),
            )
            // #70: 開けなかったリンク｡上の 2 つと同じバナーの扱いで､理由も
            // 同じだ｡何も起きていないように見えるクリックこそ潰す価値のある
            // 結末で､下の timeline はそれについて何も言えない｡
            .when_some(self.open_failure.clone(), |column, message| {
                column.child(session_notice_banner(
                    "banner-open-failure",
                    SharedString::from(message),
                    theme,
                ))
            })
            // #14: 投稿は scope に関わらず OAuth を要求する — `tweet.write`
            // scope が欠けている場合は `submit_post` 自身の中で捕まえる
            // (直し方はヘッダーの "Re-authorize" ボタン)｡composer ごと隠して
            // なぜ消えたのかを知る手立てを残さない､という形は取らない｡
            .when(self.signed_in_with_oauth, |column| {
                column.child(self.composer(window, cx))
            })
            .child(self.body(cx))
            // #205: sync が今していることは footer の 1 段上｡`when_some` なので
            // 無いときは行そのものが無い｡高さ 0 の要素を置き続けるのではない｡
            .when_some(self.sync_row(), ParentElement::child)
            // #95: ステータスバー｡ヘッダーがツールバーになった今､累計の
            // リクエスト数が住んでいるのはここだ｡
            .child(self.status_bar(cx))
            // #205: 手動 sync の確認｡`absolute` なので列の中で場所を取らず
            // ウィンドウ全体を覆う｡最後の子なのは重なり順のため｡
            .when_some(self.sync_dialog(cx), ParentElement::child)
    }
}

#[cfg(test)]
mod tests {
    use super::auto_refresh::Poll;
    use super::reload_policy::{
        newly_arrived, preserved_scroll_target, reload_cooldown, reload_outcome_label,
    };
    use super::render::{
        avatar_initial, header_title, is_own_post, like_action_label, post_permalink, profile_url,
        repost_action_label,
    };
    use super::{
        ComposeStatus, Cooldown, CooldownTick, Denial, Denied, Fade, Fixture, PostLink, PostMedia,
        PostMetrics, ReloadNotice, ReloadTrigger, RepliedTo, RowCounts, Startup, SyncOff,
        SyncStatus, Theme, ThreadFetchState, TimelineItem, TimelineState, ToggleState,
        action_post_id, at_the_post_cap, byline, compose_error_message, cooldown_label,
        cooldown_tick, format_timestamp, media_badge, media_columns, offers_delete, offers_like,
        offers_load_older, offers_quote, offers_reauthorize, offers_reply, offers_repost,
        pending_after_poll, rate_limit, reload_failure_outcome, reload_gate, reload_start_state,
        reply_banner_label, reply_target_label, repost_banner_label, row_counts,
        thread_action_label, usage, usage_color, usage_label,
    };

    fn item_with(id: &str, author_username: &str, reposted_by: Option<&str>) -> TimelineItem {
        TimelineItem {
            id: id.to_string(),
            text: String::new(),
            created_at: None,
            author_name: String::new(),
            author_username: author_username.to_string(),
            reposted_by: reposted_by.map(str::to_string),
            quoted: None,
            replied_to: None,
            metrics: None,
            links: Vec::new(),
            author_avatar_url: None,
            original_post_id: None,
            media: Vec::new(),
        }
    }

    #[test]
    fn a_count_rides_beside_each_action() {
        // #95: 件数はかつて本文の下の 1 行の散文だった｡今はアクションごとに
        // 1 つずつ､3 つの別々のラベルになっているので､それぞれが単独で
        // 返ってこなければならない｡
        let counts = row_counts(Some(&PostMetrics {
            replies: 12,
            reposts: 34,
            likes: 56,
        }));
        assert_eq!(counts.replies.as_deref(), Some("12"));
        assert_eq!(counts.reposts.as_deref(), Some("34"));
        assert_eq!(counts.likes.as_deref(), Some("56"));
    }

    #[test]
    fn a_zero_count_is_nothing_rather_than_a_zero() {
        // #67 の規則を #95 が引き継いだもの｡いいねしか付いていない行は数字を
        // 1 つ出すのであって､他のアクションの横に 0 を 2 つ並べない｡
        let counts = row_counts(Some(&PostMetrics {
            replies: 0,
            reposts: 0,
            likes: 3,
        }));
        assert_eq!(counts.replies, None);
        assert_eq!(counts.reposts, None);
        assert_eq!(counts.likes.as_deref(), Some("3"));
    }

    #[test]
    fn a_post_with_no_engagement_yet_carries_no_counts() {
        assert_eq!(
            row_counts(Some(&PostMetrics::default())),
            RowCounts::default()
        );
    }

    #[test]
    fn a_post_whose_metrics_never_expanded_carries_no_counts() {
        // `metrics: None` は全部ゼロの metrics とは別物だ — レスポンスが
        // そもそも含めていなかっただけ — が､どちらでも行の描画は同じになる｡
        // これは以前は呼び出し側の `when_some` が扱っていたケースだ｡
        assert_eq!(row_counts(None), RowCounts::default());
    }

    #[test]
    fn a_large_count_is_abbreviated() {
        // これも #67 の規則｡アクションの横に 7 桁も並べば､ストリップの
        // 残りを押しのけてしまう｡
        let counts = row_counts(Some(&PostMetrics {
            replies: 1000,
            reposts: 12_345,
            likes: 2_400_000,
        }));
        assert_eq!(counts.replies.as_deref(), Some("1K"));
        assert_eq!(counts.reposts.as_deref(), Some("12.3K"));
        assert_eq!(counts.likes.as_deref(), Some("2.4M"));
    }

    // --- #64: アバター ---

    #[test]
    fn an_avatar_initial_is_the_uppercased_first_character() {
        assert_eq!(avatar_initial("Developers"), "D");
        assert_eq!(avatar_initial("developers"), "D");
    }

    #[test]
    fn an_avatar_initial_handles_a_multi_byte_first_character() {
        // ここでバイト単位に切ると panic するか文字化けする｡
        assert_eq!(avatar_initial("うさだ"), "う");
        assert_eq!(avatar_initial("Émile"), "É");
    }

    #[test]
    fn there_is_no_avatar_initial_without_a_name() {
        // 名前が展開されなかった投稿者 — でっち上げた文字を出すのではなく､
        // 円だけが残る｡
        assert_eq!(avatar_initial(""), "");
    }

    // --- #70: リンクを開く ---

    #[test]
    fn a_post_permalink_uses_the_authors_handle() {
        assert_eq!(
            post_permalink("XDevelopers", "1700000000000000001"),
            "https://x.com/XDevelopers/status/1700000000000000001"
        );
    }

    #[test]
    fn a_post_permalink_falls_back_to_the_id_only_form() {
        // 投稿者が展開されなかった post にも届けなければならない —
        // `x.com//status/…` は 404 になるが､X 自身の `/i/web/` の形はならない｡
        assert_eq!(
            post_permalink("", "1700000000000000001"),
            "https://x.com/i/web/status/1700000000000000001"
        );
    }

    #[test]
    fn a_permalink_is_something_the_browser_helper_will_actually_open() {
        // 両者は一致していなければならない｡こちらが組んだ URL を `browser` が
        // 拒めば､それは何も起きないクリックになる｡
        assert!(crate::browser::is_openable(&post_permalink(
            "XDevelopers",
            "1"
        )));
        assert!(crate::browser::is_openable(&post_permalink("", "1")));
        assert!(crate::browser::is_openable(
            &profile_url("XDevelopers").unwrap()
        ));
    }

    #[test]
    fn a_profile_url_uses_the_handle() {
        assert_eq!(
            profile_url("XDevelopers").as_deref(),
            Some("https://x.com/XDevelopers")
        );
    }

    #[test]
    fn there_is_no_profile_url_without_a_handle() {
        // post と違って id だけのフォールバックが無いので､間違った先を指す
        // のではなくアフォーダンス自体を出さない｡
        assert_eq!(profile_url(""), None);
    }

    // --- #68: いいね ---

    #[test]
    fn offers_like_on_an_ordinary_post() {
        assert!(offers_like(
            true,
            Some("me-id"),
            &item_with("1", "alice", None)
        ));
    }

    #[test]
    fn offers_like_on_ones_own_post() {
        // #68 が明言している｡X は自分の post のリポストは拒むが､いいねは
        // 受け入れる｡だから `is_own_post` を #15 から持ち越してはいけない｡
        assert!(offers_like(
            true,
            Some("me-id"),
            &item_with("1", "me", None)
        ));
    }

    #[test]
    fn does_not_offer_like_before_the_signed_in_id_resolves() {
        assert!(!offers_like(true, None, &item_with("1", "alice", None)));
    }

    #[test]
    fn does_not_offer_like_without_an_oauth_session() {
        assert!(!offers_like(
            false,
            Some("me-id"),
            &item_with("1", "alice", None)
        ));
    }

    #[test]
    fn like_action_label_offers_to_like_when_not_liked() {
        assert_eq!(like_action_label(&ToggleState::new(false)), "Like");
    }

    #[test]
    fn like_action_label_shows_liked_once_it_is() {
        assert_eq!(like_action_label(&ToggleState::new(true)), "Liked");
    }

    #[test]
    fn like_action_label_shows_the_pending_direction() {
        let mut liking = ToggleState::new(false);
        liking.start_toggle();
        assert_eq!(like_action_label(&liking), "Liking…");

        let mut unliking = ToggleState::new(true);
        unliking.start_toggle();
        assert_eq!(like_action_label(&unliking), "Unliking…");
    }

    #[test]
    fn offers_reauthorize_for_a_session_that_predates_the_like_scope() {
        // #68: `like.write` は別途付与されるので､それ以前のセッションは投稿と
        // リポストはできてもいいねはできない — しかも直し方を伝えねばならない｡
        // `toggle_like` の拒否がまさにこのボタンを指しているからだ｡
        assert!(offers_reauthorize(
            true,
            Some("tweet.read users.read tweet.write offline.access"),
            false
        ));
    }

    /// #13 の join が組むとおりのリポスト行｡本文は元投稿のもの､`id` は
    /// リツイートのアクティビティのもの､そして書き込み系のエンドポイントが
    /// 対象にすべきなのは #52 の `original_post_id` だ｡
    fn repost_row_item(row_id: &str, original_id: &str, original_author: &str) -> TimelineItem {
        let mut item = item_with(row_id, original_author, Some("bob"));
        item.original_post_id = Some(original_id.to_string());
        item
    }

    // --- #141: リロードが何をしたかを言う ---

    #[test]
    fn a_reload_that_brought_nothing_says_so() {
        // 読み手が「押したのが効かなかった」と受け取りやすいのがこのケースだ｡
        // 前後で画面が一致してしまう｡
        assert_eq!(reload_outcome_label(0), "No new posts.");
    }

    #[test]
    fn one_new_post_is_not_reported_in_the_plural() {
        assert_eq!(reload_outcome_label(1), "1 new post.");
        assert_eq!(reload_outcome_label(6), "6 new posts.");
    }

    #[test]
    fn the_outcome_counts_the_same_posts_the_scroll_does() {
        // どちらも未見の id の先頭の連なりを読むので､リロードが
        // "3 new posts" と言いながら別の数だけスクロールすることは起きない｡
        let previous = ["1", "2", "3"];
        let new_ids = ["a", "b", "1", "2", "3"];

        assert_eq!(newly_arrived(&previous, &new_ids), 2);
        assert_eq!(
            preserved_scroll_target(&previous, &new_ids, 5),
            Some(7),
            "the scroll must move by exactly what the message claims"
        );
    }

    // --- #22: リロードをまたいで読み手をその場に留める ---

    #[test]
    fn a_reader_at_the_top_is_left_alone() {
        // 何も無い上へ新しい post が届くのは､先頭にいる人が見たいものその
        // ものなので､ここはスクロールせずに見送る｡
        assert_eq!(
            preserved_scroll_target(&["2", "3"], &["1", "2", "3"], 0),
            None
        );
    }

    #[test]
    fn a_scrolled_reader_is_moved_down_by_the_number_of_new_posts() {
        // 20 行下にいるところへ post が 6 つ届く｡これが無いとビューポートは
        // 動かず､読み手の目の下のテキストが入れ替わってしまう｡
        let previous: Vec<String> = (0..30).map(|n| n.to_string()).collect();
        let previous_ids: Vec<&str> = previous.iter().map(String::as_str).collect();
        let fresh = ["a", "b", "c", "d", "e", "f"];
        let new_ids: Vec<&str> = fresh.iter().copied().chain(previous_ids.clone()).collect();

        assert_eq!(
            preserved_scroll_target(&previous_ids, &new_ids, 20),
            Some(26)
        );
    }

    #[test]
    fn a_reload_that_brings_nothing_new_leaves_the_position_alone() {
        assert_eq!(
            preserved_scroll_target(&["1", "2", "3"], &["1", "2", "3"], 7),
            None
        );
    }

    #[test]
    fn only_the_leading_run_of_new_ids_counts() {
        // もっと下にある id は､届いた post ではなく動いた post だ｡それの分まで
        // スクロールすると､読み手が見ていたものを行き過ぎてしまう｡
        assert_eq!(
            preserved_scroll_target(&["1", "2"], &["new", "1", "also-new", "2"], 5),
            Some(6)
        );
    }

    // --- #65: 添付メディア ---

    #[test]
    fn one_image_is_laid_out_in_a_single_column() {
        assert_eq!(media_columns(1), 1);
    }

    #[test]
    fn two_or_more_images_are_laid_out_in_two_columns() {
        // 横 3 つでは固定のセル高では 1 枚 1 枚が細すぎて読めないし､X 自身の
        // 上限である 4 枚は 2 つの行に 2 つずつだ｡
        assert_eq!(media_columns(2), 2);
        assert_eq!(media_columns(3), 2);
        assert_eq!(media_columns(4), 2);
    }

    #[test]
    fn the_column_count_is_never_zero() {
        // `media_grid` はこれをそのまま `chunks` へ渡すが､あちらは 0 で panic する｡
        assert_eq!(media_columns(0), 1);
    }

    #[test]
    fn a_photo_gets_no_badge() {
        assert_eq!(media_badge(Some("photo")), None);
    }

    #[test]
    fn video_and_gif_say_which_they_are() {
        // どちらもここでは再生されないので､静止画と写真を見分けられるのは
        // バッジだけだ｡
        assert_eq!(media_badge(Some("video")), Some("Video"));
        assert_eq!(media_badge(Some("animated_gif")), Some("GIF"));
    }

    #[test]
    fn an_unrecognized_media_type_gets_no_badge() {
        // 前方互換のため｡X が後から作るものは､誰にも解釈できないラベルでは
        // なく素の静止画として描かれるべきだ｡
        assert_eq!(media_badge(Some("hologram")), None);
        assert_eq!(media_badge(None), None);
    }

    // --- #72: 削除 ---

    #[test]
    fn offers_delete_on_ones_own_post() {
        assert!(offers_delete(
            true,
            Some("me-id"),
            Some("bob"),
            &item_with("1", "bob", None)
        ));
    }

    #[test]
    fn does_not_offer_delete_on_someone_elses_post() {
        // X が拒むうえに取り返しがつかない — 失敗しかしないクリックを出す
        // 理由が無い｡
        assert!(!offers_delete(
            true,
            Some("me-id"),
            Some("bob"),
            &item_with("1", "alice", None)
        ));
    }

    #[test]
    fn does_not_offer_delete_on_a_repost_row_even_of_ones_own_post() {
        // #52 以降の他のすべてのアクションと違い､これだけは出さないままに
        // する｡行は「自分のリポスト」として読めるが､削除は元投稿を壊して
        // しまう｡リポストを取り消すのはリポストのトグルの仕事だ｡
        let mut item = item_with("activity-id", "bob", Some("bob"));
        item.original_post_id = Some("original-id".to_string());
        assert!(!offers_delete(true, Some("me-id"), Some("bob"), &item));
    }

    #[test]
    fn does_not_offer_delete_before_the_signed_in_id_resolves() {
        assert!(!offers_delete(
            true,
            None,
            Some("bob"),
            &item_with("1", "bob", None)
        ));
    }

    #[test]
    fn does_not_offer_delete_before_the_signed_in_handle_resolves() {
        // `is_own_post` は解決していないハンドルを「自分のではない」として
        // 扱う｡取り返しのつかないアクションにとっては安全な側だ｡
        assert!(!offers_delete(
            true,
            Some("me-id"),
            None,
            &item_with("1", "bob", None)
        ));
    }

    #[test]
    fn does_not_offer_delete_without_an_oauth_session() {
        assert!(!offers_delete(
            false,
            Some("me-id"),
            Some("bob"),
            &item_with("1", "bob", None)
        ));
    }

    // --- #71: 返信 ---

    #[test]
    fn offers_reply_once_signed_in_with_oauth() {
        assert!(offers_reply(true, &item_with("1", "alice", None)));
    }

    #[test]
    fn does_not_offer_reply_without_oauth() {
        // OAuth が無ければ composer 自体に手が届かない — 返信の行き先が無い｡
        assert!(!offers_reply(false, &item_with("1", "alice", None)));
    }

    #[test]
    fn offers_reply_on_ones_own_post() {
        // X は自分への返信を受け入れるし､自分でスレッドを繋ぐのは普通の
        // 書き方だ｡
        assert!(offers_reply(true, &item_with("1", "me", None)));
    }

    #[test]
    fn reply_target_label_names_the_author() {
        assert_eq!(
            reply_target_label("XDevelopers"),
            "Replying to @XDevelopers"
        );
    }

    #[test]
    fn reply_target_label_without_a_handle() {
        // `reply_banner_label` がすでに扱っているのと同じ穴｡展開されなかった
        // 投稿者のことだ｡
        assert_eq!(reply_target_label(""), "Replying to a post");
    }

    // --- #52: リポスト行は元投稿に対して働く ---

    #[test]
    fn a_repost_row_acts_on_the_original_post_not_the_retweet_activity() {
        let item = repost_row_item("activity-id", "original-id", "alice");
        assert_eq!(action_post_id(&item), "original-id");
    }

    #[test]
    fn an_ordinary_row_acts_on_its_own_id() {
        assert_eq!(action_post_id(&item_with("1", "alice", None)), "1");
    }

    #[test]
    fn offers_repost_on_a_repost_row_now_that_the_original_id_is_carried() {
        // これが置き換える回避策は､`item.id` がリツイートのアクティビティの
        // id であるためにここでボタンを出さなかった｡#52 は元投稿の id を
        // 運ぶので､ボタンを出しても安全だ｡
        let item = repost_row_item("activity-id", "original-id", "alice");
        assert!(offers_repost(true, Some("2244994945"), Some("bob"), &item));
    }

    #[test]
    fn offers_quote_on_a_repost_row() {
        let item = repost_row_item("activity-id", "original-id", "alice");
        assert!(offers_quote(true, &item));
    }

    #[test]
    fn offers_like_on_a_repost_row() {
        let item = repost_row_item("activity-id", "original-id", "alice");
        assert!(offers_like(true, Some("me-id"), &item));
    }

    #[test]
    fn a_repost_row_still_withholds_repost_when_the_original_is_ones_own_post() {
        // `is_own_post` のガードは今や*元投稿の*投稿者と比べる｡実際に
        // リポストされるのはその人の post だからだ — リポストした人の
        // ハンドルは､API が何を拒むかとは関係が無い｡
        let item = repost_row_item("activity-id", "original-id", "bob");
        assert!(!offers_repost(true, Some("2244994945"), Some("bob"), &item));
    }

    #[test]
    fn replying_from_a_repost_row_answers_the_original_post() {
        // #71 が名指しする罠｡`in_reply_to_tweet_id` がリツイートの
        // アクティビティを指すと､返信が別の会話にぶら下がってしまい､その失敗は
        // 何ひとつ目に見えない｡
        let item = repost_row_item("activity-id", "original-id", "alice");
        assert_eq!(action_post_id(&item), "original-id");
        assert!(offers_reply(true, &item));
    }

    #[test]
    fn a_repost_rows_permalink_points_at_the_original_post() {
        let item = repost_row_item("activity-id", "original-id", "alice");
        assert_eq!(
            post_permalink(&item.author_username, action_post_id(&item)),
            "https://x.com/alice/status/original-id"
        );
    }

    // --- offers_reauthorize (#14) ---

    #[test]
    fn offers_reauthorize_when_signed_in_with_oauth_but_missing_the_write_scope() {
        // #14: #7 の当初の最小限の scope 要求が作り出すそのままの筋書き —
        // 本物で動く OAuth セッションなのに､ただ投稿だけができない｡
        assert!(offers_reauthorize(
            true,
            Some("tweet.read users.read offline.access"),
            false
        ));
    }

    #[test]
    fn offers_reauthorize_when_the_scope_was_never_recorded() {
        // #14 より前のトークン｡「不明」は「不足」と同じに扱う｡
        assert!(offers_reauthorize(true, None, false));
    }

    #[test]
    fn does_not_offer_reauthorize_once_every_write_scope_is_granted() {
        // `like.write` は #68 でこの集合に加わった｡`tweet.write` しか持たない
        // セッションは今や本当に scope 不足で､それは上のテストが押さえている｡
        assert!(!offers_reauthorize(
            true,
            Some("tweet.read tweet.write like.write offline.access"),
            false
        ));
    }

    #[test]
    fn offers_reauthorize_for_a_session_that_predates_the_list_scope() {
        // #161: #167 が `list.read` を足す前に認可されたセッションでリストを
        // 設定すると､ウィンドウが読む唯一のエンドポイントから 403 が返る｡
        // 説明はこのボタンがすべてなので､出さないわけにいかない｡
        assert!(offers_reauthorize(
            true,
            Some("tweet.read tweet.write like.write offline.access"),
            true
        ));
    }

    #[test]
    fn does_not_offer_reauthorize_for_a_list_once_list_read_is_granted() {
        assert!(!offers_reauthorize(
            true,
            Some("tweet.read tweet.write like.write list.read offline.access"),
            true
        ));
    }

    #[test]
    fn does_not_ask_for_list_read_when_no_list_is_configured() {
        // ホームタイムラインを読んでいる人はその 403 に届きようがないので､
        // 使っていない scope をせっつくのはノイズでしかない｡
        assert!(!offers_reauthorize(
            true,
            Some("tweet.read tweet.write like.write offline.access"),
            false
        ));
    }

    #[test]
    fn does_not_offer_reauthorize_without_an_oauth_session() {
        // そもそも OAuth でサインインしていない — ここで関係するアフォーダンス
        // は `offers_sign_in` であって､こちらではない｡
        assert!(!offers_reauthorize(false, None, false));
    }

    // --- compose_error_message (#14) ---

    #[test]
    fn compose_error_message_is_none_while_idle() {
        assert_eq!(compose_error_message(&ComposeStatus::Idle), None);
    }

    #[test]
    fn compose_error_message_is_none_while_submitting() {
        assert_eq!(compose_error_message(&ComposeStatus::Submitting), None);
    }

    #[test]
    fn compose_error_message_surfaces_a_failed_submits_message() {
        let status = ComposeStatus::Failed("network error".to_string());
        assert_eq!(
            compose_error_message(&status).map(|message| message.to_string()),
            Some("network error".to_string())
        );
    }

    #[test]
    fn header_title_names_the_signed_in_account() {
        // アカウントだけ｡どの timeline を表示しているかは #95 以降タブバーが
        // 言うことで､44px の帯の中で二度言ったせいでツールバーは場所を
        // 使い果たした｡
        assert_eq!(header_title(Some("alice")), "@alice");
    }

    #[test]
    fn header_title_falls_back_before_me_has_resolved() {
        // #33 以降に残った唯一のケース: ウィンドウは常にホームタイムラインを
        // 表示するので､分からないのは誰のものかだけだ｡`/me` が答えるまでは
        // 名指しできるアカウントが無く､macOS のツールバーがその代わりに
        // 載せるのはアプリ自身の名前だ｡
        assert_eq!(header_title(None), "twigpui");
    }

    #[test]
    fn offers_load_older_when_a_next_page_token_is_present_and_the_timeline_is_loaded() {
        assert!(offers_load_older(
            Some("cursor-abc"),
            &TimelineState::Loaded(Vec::new())
        ));
    }

    #[test]
    fn does_not_offer_load_older_without_a_next_page_token() {
        assert!(!offers_load_older(None, &TimelineState::Loaded(Vec::new())));
    }

    #[test]
    fn does_not_offer_load_older_at_the_post_cap() {
        // `cache::splice` は上限まで切り詰めるので､ここでクリックすると本物の
        // API リクエストを使ったうえで､買ったものをすべて捨てることになる｡
        let full: Vec<_> = (0..crate::cache::MAX_CACHED_POSTS)
            .map(|n| TimelineItem {
                id: n.to_string(),
                text: String::new(),
                created_at: None,
                author_name: String::new(),
                author_username: String::new(),
                reposted_by: None,
                quoted: None,
                replied_to: None,
                metrics: None,
                links: Vec::new(),
                author_avatar_url: None,
                original_post_id: None,
                media: Vec::new(),
            })
            .collect();
        let state = TimelineState::Loaded(full);

        assert!(!offers_load_older(Some("cursor-abc"), &state));
        // ...そしてボタンがただ消えるのではなく､本文が自分で説明する｡
        assert!(at_the_post_cap(&state));
    }

    #[test]
    fn is_not_at_the_post_cap_below_it() {
        assert!(!at_the_post_cap(&TimelineState::Loaded(Vec::new())));
    }

    #[test]
    fn does_not_offer_load_older_while_not_in_the_loaded_state() {
        assert!(!offers_load_older(
            Some("cursor-abc"),
            &TimelineState::Loading
        ));
    }

    #[test]
    fn prefixes_a_byline_with_an_at_sign() {
        assert_eq!(byline("XDevelopers"), "@XDevelopers");
    }

    #[test]
    fn renders_a_missing_author_as_nothing_rather_than_a_bare_at() {
        assert_eq!(byline(""), "");
    }

    #[test]
    fn labels_a_repost_with_who_reposted_it() {
        assert_eq!(repost_banner_label("reposter1"), "@reposter1 reposted");
    }

    #[test]
    fn labels_a_repost_generically_when_the_reposter_is_missing() {
        // byline の著者が空のときのフォールバック (#13) に倣う: 素の
        // "@ reposted" は壊れているように読める｡
        assert_eq!(repost_banner_label(""), "Reposted");
    }

    #[test]
    fn keeps_a_timestamp_too_short_to_parse() {
        assert_eq!(format_timestamp(Some("2026-08-16T09")), "2026-08-16T09");
    }

    #[test]
    fn shows_an_rfc3339_timestamp_in_jst() {
        assert_eq!(
            format_timestamp(Some("2026-08-16T09:00:00.000Z")),
            "2026-08-16 18:00"
        );
    }

    #[test]
    fn shows_a_timestamp_without_fractional_seconds_in_jst() {
        // fractional seconds は RFC 3339 では任意｡
        assert_eq!(
            format_timestamp(Some("2026-08-16T09:00:00Z")),
            "2026-08-16 18:00"
        );
    }

    #[test]
    fn rolls_the_date_forward_when_jst_crosses_midnight() {
        assert_eq!(
            format_timestamp(Some("2026-08-16T16:30:00Z")),
            "2026-08-17 01:30"
        );
    }

    #[test]
    fn rolls_the_year_forward_when_jst_crosses_new_year() {
        assert_eq!(
            format_timestamp(Some("2026-12-31T23:00:00Z")),
            "2027-01-01 08:00"
        );
    }

    #[test]
    fn normalises_an_offset_timestamp_to_jst() {
        // +05:00 の 09:00 は UTC 04:00 で JST 13:00｡
        assert_eq!(
            format_timestamp(Some("2026-08-16T09:00:00+05:00")),
            "2026-08-16 13:00"
        );
    }

    #[test]
    fn passes_through_an_unexpected_shape() {
        assert_eq!(format_timestamp(Some("yesterday")), "yesterday");
    }

    #[test]
    fn renders_a_missing_timestamp_as_empty() {
        assert_eq!(format_timestamp(None), "");
    }

    #[test]
    fn cooldown_label_counts_down_to_the_reset_time() {
        assert_eq!(
            cooldown_label(Cooldown::ApiRateLimit, 1_060, 1_000),
            "Rate limited by X — retry in 60s"
        );
    }

    #[test]
    fn cooldown_label_clamps_a_reset_time_already_passed() {
        // #10: 0 を跨いだばかりのカウントダウンは "0s" と読めなければならず､
        // 紛らわしい負の数は決して出さない｡
        assert_eq!(
            cooldown_label(Cooldown::ApiRateLimit, 1_000, 1_060),
            "Rate limited by X — retry in 0s"
        );
    }

    #[test]
    fn cooldown_label_does_not_blame_x_for_the_local_fetch_interval() {
        // 自分で課した間隔は何かを送る前にリロードを止めるので､X は何も
        // 言っていない — これをレートリミットと呼ぶのは､起きたことの端的な
        // 言い間違いになる｡
        let label = cooldown_label(Cooldown::LocalInterval, 1_060, 1_000);
        assert_eq!(label, "Waiting out the fetch interval — 60s");
        assert!(!label.contains("Rate limited"), "{label}");
    }

    #[test]
    fn reload_cooldown_allows_the_very_first_reload() {
        assert_eq!(reload_cooldown(None, 60, 1_000), None);
    }

    #[test]
    fn reload_cooldown_blocks_within_the_configured_interval() {
        assert_eq!(reload_cooldown(Some(1_000), 60, 1_030), Some(1_060));
    }

    #[test]
    fn reload_cooldown_allows_once_the_interval_has_elapsed() {
        assert_eq!(reload_cooldown(Some(1_000), 60, 1_060), None);
        assert_eq!(reload_cooldown(Some(1_000), 60, 1_061), None);
    }

    // --- reload_gate (#57) ---

    #[test]
    fn reload_gate_polling_blocks_within_the_configured_interval() {
        // `reload_cooldown` 自身が止めるのと同じ形 — `Polling` はそれに
        // そのまま従わなければならない｡
        assert_eq!(
            reload_gate(ReloadTrigger::Polling, Some(1_000), 60, 1_030),
            Some(1_060)
        );
    }

    #[test]
    fn reload_gate_user_action_bypasses_the_interval_even_when_polling_would_block() {
        // #57 の主症状に対する中心的な修正: 送信後のリロードは即座に通らな
        // ければならない｡上ではまったく同じ `last_reload_at`/`now` の組が
        // `Polling` のリロードを止めているにもかかわらず｡
        assert_eq!(
            reload_gate(ReloadTrigger::UserAction, Some(1_000), 60, 1_030),
            None
        );
    }

    // --- cooldown_tick (#57 の項目 3) ---

    #[test]
    fn cooldown_tick_keeps_waiting_before_reset_at() {
        let notice = ReloadNotice::Cooldown {
            reset_at: 1_060,
            cooldown: Cooldown::LocalInterval,
        };
        assert_eq!(
            cooldown_tick(Some(&notice), 1_030),
            CooldownTick::StillWaiting
        );
    }

    #[test]
    fn cooldown_tick_has_elapsed_once_reset_at_has_passed() {
        let notice = ReloadNotice::Cooldown {
            reset_at: 1_060,
            cooldown: Cooldown::ApiRateLimit,
        };
        assert_eq!(cooldown_tick(Some(&notice), 1_061), CooldownTick::Elapsed);
    }

    #[test]
    fn cooldown_tick_has_elapsed_exactly_at_reset_at() {
        // `reload_cooldown` 自身の `>` の境界に倣う: `reset_at` より厳密に
        // 前は止め､`reset_at` 以降は許す (ここでは経過扱い)｡
        let notice = ReloadNotice::Cooldown {
            reset_at: 1_060,
            cooldown: Cooldown::LocalInterval,
        };
        assert_eq!(cooldown_tick(Some(&notice), 1_060), CooldownTick::Elapsed);
    }

    #[test]
    fn cooldown_tick_is_not_ticking_without_a_notice() {
        assert_eq!(cooldown_tick(None, 1_000), CooldownTick::NotTicking);
    }

    #[test]
    fn cooldown_tick_is_not_ticking_for_a_failed_notice() {
        // `Failed` の通知は進めるべきカウントダウンを持たない — ticker は
        // 永久にポーリングするのではなく止まらなければならない｡
        let notice = ReloadNotice::Failed("boom".into());
        assert_eq!(
            cooldown_tick(Some(&notice), 1_000),
            CooldownTick::NotTicking
        );
    }

    // --- reload_start_state (#57) ---

    #[test]
    fn reload_start_state_keeps_existing_posts_in_place() {
        let items = vec![item_with("1", "alice", None)];
        match reload_start_state(TimelineState::Loaded(items.clone())) {
            TimelineState::Loaded(got) => assert_eq!(got, items),
            other => panic!("expected existing posts to survive, got {other:?}"),
        }
    }

    #[test]
    fn reload_start_state_falls_back_to_loading_when_nothing_was_shown() {
        assert!(matches!(
            reload_start_state(TimelineState::NotAuthenticated),
            TimelineState::Loading
        ));
    }

    // --- reload_failure_outcome (#57) ---

    #[test]
    fn reload_failure_outcome_keeps_existing_posts_on_a_plain_failure() {
        let items = vec![item_with("1", "alice", None)];
        let error = anyhow::anyhow!("network exploded");
        let (state, notice) = reload_failure_outcome(TimelineState::Loaded(items.clone()), &error);
        match state {
            TimelineState::Loaded(got) => assert_eq!(got, items),
            other => panic!("existing posts must survive a failed reload, got {other:?}"),
        }
        assert_eq!(
            notice,
            Some(ReloadNotice::Failed("network exploded".to_string().into()))
        );
    }

    #[test]
    fn reload_failure_outcome_keeps_existing_posts_on_a_rate_limited_failure() {
        let items = vec![item_with("1", "alice", None)];
        let error: anyhow::Error = rate_limit::RateLimited {
            reset_at: Some(1_500),
            opaque: false,
        }
        .into();
        let (state, notice) = reload_failure_outcome(TimelineState::Loaded(items.clone()), &error);
        match state {
            TimelineState::Loaded(got) => assert_eq!(got, items),
            other => panic!("existing posts must survive a rate-limited reload, got {other:?}"),
        }
        assert_eq!(
            notice,
            Some(ReloadNotice::Cooldown {
                reset_at: 1_500,
                cooldown: Cooldown::ApiRateLimit,
            })
        );
    }

    #[test]
    fn reload_failure_outcome_falls_back_to_failed_state_when_nothing_was_shown() {
        let error = anyhow::anyhow!("network exploded");
        let (state, notice) = reload_failure_outcome(TimelineState::Loading, &error);
        assert!(matches!(state, TimelineState::Failed(_)));
        // #57: 状態自身がすでに何が失敗したかを言っているので､まったく同じ
        // ことを言うバナーは失敗の二重表示になる｡
        assert_eq!(notice, None);
    }

    #[test]
    fn reload_failure_outcome_falls_back_to_rate_limited_state_when_nothing_was_shown() {
        let error: anyhow::Error = rate_limit::RateLimited {
            reset_at: Some(1_500),
            opaque: false,
        }
        .into();
        let (state, notice) = reload_failure_outcome(TimelineState::NotAuthenticated, &error);
        assert!(matches!(
            state,
            TimelineState::RateLimited {
                reset_at: 1_500,
                cooldown: Cooldown::ApiRateLimit,
            }
        ));
        // 上の素の失敗のケースと同じ理屈: `RateLimited` はすでにカウント
        // ダウンを持っているので､別の通知は要らない｡
        assert_eq!(notice, None);
    }

    // --- `TimelineView::load_older` は同じ純粋関数を使い回す (#57) ---
    //
    // `load_older` は `state` がすでに `Loaded` のときにしか走らない
    // ("Load older" の行に対する `offers_load_older` のゲートを参照)｡だから
    // これらは上の単一要素のケースを assert し直すのではなく､その呼び出し箇所が
    // 実際に踏む 2 要素で後ろへページングする形を押さえる｡

    #[test]
    fn load_older_keeps_the_current_page_visible_while_its_fetch_is_in_flight() {
        // #57 より前は `load_older` が無条件に
        // `state = TimelineState::Loading` を設定しており､`TimelineView::body`
        // の match を通じて､リクエストのあいだじゅう､ユーザーがページングして
        // いたページと "Load older" の行を一緒に消していた｡
        let items = vec![item_with("1", "alice", None), item_with("2", "bob", None)];
        match reload_start_state(TimelineState::Loaded(items.clone())) {
            TimelineState::Loaded(got) => assert_eq!(got, items),
            other => panic!("load_older must keep the current page visible, got {other:?}"),
        }
    }

    #[test]
    fn load_older_keeps_the_current_page_when_paging_backwards_fails() {
        // #57 より前は､失敗した "Load older" のリクエストが `map_reload_error`
        // 経由で `state` を置き換え､ユーザーがすでにページングしてきたものを
        // すべて捨てていた — 素のリロードの失敗より悪い｡すでに表示されていた
        // post には実際には何も間違いが無かったからだ｡
        let items = vec![item_with("1", "alice", None), item_with("2", "bob", None)];
        let error = anyhow::anyhow!("network exploded");
        let (state, notice) = reload_failure_outcome(TimelineState::Loaded(items.clone()), &error);
        match state {
            TimelineState::Loaded(got) => assert_eq!(got, items),
            other => panic!("load_older must keep the current page, got {other:?}"),
        }
        assert_eq!(
            notice,
            Some(ReloadNotice::Failed("network exploded".to_string().into()))
        );
    }

    #[test]
    fn labels_a_reply_with_who_it_is_replying_to() {
        let replied_to = RepliedTo {
            post_id: "1".to_string(),
            author_name: "Developers".to_string(),
            author_username: "XDevelopers".to_string(),
        };
        assert_eq!(reply_banner_label(&replied_to), "Replying to @XDevelopers");
    }

    #[test]
    fn labels_a_reply_generically_when_the_parent_author_is_missing() {
        // repost_banner_label の著者が空のときのフォールバック (#12) に倣う:
        // 素の "Replying to @" は壊れているように読める｡
        let replied_to = RepliedTo {
            post_id: "1".to_string(),
            author_name: String::new(),
            author_username: String::new(),
        };
        assert_eq!(reply_banner_label(&replied_to), "Replying to a post");
    }

    #[test]
    fn offers_to_show_the_thread_with_the_worst_case_cost_spelled_out() {
        // #12: コストは使う*前*に予測できなければならない — ラベル自身が､
        // クリック 1 回で何リクエストかかりうるかを言う｡
        assert_eq!(
            thread_action_label(None),
            Some("Show thread (up to 5 requests)")
        );
    }

    #[test]
    fn offers_a_retry_after_a_failed_thread_fetch() {
        let state = ThreadFetchState::Failed("boom".into());
        assert_eq!(thread_action_label(Some(&state)), Some("Retry"));
    }

    #[test]
    fn offers_no_toggle_while_loading_or_once_loaded() {
        assert_eq!(thread_action_label(Some(&ThreadFetchState::Loading)), None);
        let loaded = ThreadFetchState::Loaded(crate::thread::ThreadChain::default());
        assert_eq!(thread_action_label(Some(&loaded)), None);
    }

    // --- usage_label / usage_color (#18) ---

    #[test]
    fn usage_label_shows_counts_only_without_a_configured_price() {
        assert_eq!(usage_label(3, 42, None), "Today: 3 req · Total: 42 req");
    }

    #[test]
    fn usage_label_appends_an_estimated_amount_once_a_price_is_configured() {
        assert_eq!(
            usage_label(4, 40, Some(2.5)),
            "Today: 4 req (~10.00) · Total: 40 req"
        );
    }

    #[test]
    fn usage_label_shows_zero_counts_plainly() {
        assert_eq!(usage_label(0, 0, None), "Today: 0 req · Total: 0 req");
    }

    #[test]
    fn usage_color_is_muted_within_budget() {
        let theme = Theme::light();
        assert_eq!(
            usage_color(usage::BudgetStatus::Ok, theme),
            theme.text_muted
        );
    }

    #[test]
    fn usage_color_is_the_warning_slot_near_the_budget() {
        let theme = Theme::light();
        assert_eq!(usage_color(usage::BudgetStatus::Near, theme), theme.warning);
    }

    #[test]
    fn usage_color_is_the_danger_slot_once_the_budget_is_exceeded() {
        let theme = Theme::light();
        assert_eq!(
            usage_color(usage::BudgetStatus::Exceeded, theme),
            theme.danger
        );
    }

    // --- offers_repost / is_own_post (#15) ---

    #[test]
    fn offers_repost_once_signed_in_with_a_resolved_home_id_on_someone_elses_post() {
        let item = item_with("1", "alice", None);
        assert!(offers_repost(true, Some("2244994945"), Some("bob"), &item));
    }

    #[test]
    fn does_not_offer_repost_without_oauth() {
        let item = item_with("1", "alice", None);
        assert!(!offers_repost(
            false,
            Some("2244994945"),
            Some("bob"),
            &item
        ));
    }

    #[test]
    fn does_not_offer_repost_before_home_user_id_resolves() {
        // #11: repost のエンドポイントは*この*アカウントとして作用し､その id は
        // `/me` しか解決しない — それが無いうちは呼ぶものが無い｡
        let item = item_with("1", "alice", None);
        assert!(!offers_repost(true, None, Some("bob"), &item));
    }

    #[test]
    fn does_not_offer_repost_on_ones_own_post() {
        let item = item_with("1", "bob", None);
        assert!(!offers_repost(true, Some("2244994945"), Some("bob"), &item));
    }

    // --- offers_quote (#16) ---

    #[test]
    fn offers_quote_once_signed_in_on_an_ordinary_post() {
        let item = item_with("1", "alice", None);
        assert!(offers_quote(true, &item));
    }

    #[test]
    fn does_not_offer_quote_without_oauth() {
        // OAuth 無しではコンポーザー自体に届かない (`Render::render` のゲートを
        // 参照) — 引用の行き先が無い｡
        let item = item_with("1", "alice", None);
        assert!(!offers_quote(false, &item));
    }

    #[test]
    fn offers_quote_on_ones_own_post() {
        // `offers_repost` と違い､自分の post の引用は許される — #16 の設計
        // 判断のとおり API が拒否しないので､repost の同等のケースが `false`
        // であっても､ここは `true` のままでなければならない｡
        let item = item_with("1", "bob", None);
        assert!(offers_quote(true, &item));
    }

    #[test]
    fn is_own_post_matches_case_insensitively() {
        assert!(is_own_post(Some("Bob"), "bob"));
        assert!(is_own_post(Some("bob"), "BOB"));
    }

    #[test]
    fn is_own_post_is_false_when_home_username_is_unknown() {
        assert!(!is_own_post(None, "bob"));
    }

    #[test]
    fn is_own_post_is_false_for_a_different_author() {
        assert!(!is_own_post(Some("bob"), "alice"));
    }

    // --- repost_action_label (#15) ---

    #[test]
    fn repost_action_label_offers_to_repost_when_not_reposted() {
        assert_eq!(repost_action_label(&ToggleState::new(false)), "Repost");
    }

    #[test]
    fn repost_action_label_shows_reposted_once_it_is() {
        assert_eq!(repost_action_label(&ToggleState::new(true)), "Reposted");
    }

    #[test]
    fn repost_action_label_shows_the_pending_direction() {
        let mut creating = ToggleState::new(false);
        creating.start_toggle();
        assert_eq!(repost_action_label(&creating), "Reposting…");

        let mut deleting = ToggleState::new(true);
        deleting.start_toggle();
        assert_eq!(repost_action_label(&deleting), "Removing repost…");
    }

    /// #55 がまっすぐすり抜けた起動の経路 (#59)｡
    ///
    /// `cargo run` より下はすべてビルドも単体テストも通っていたが､誰も
    /// ウィンドウを開いていなかったので､何かが描画されてはじめて発火する
    /// panic が手つかずのまま `main` へ届いた｡gpui のテストプラットフォームは
    /// 何も無いところへ描く (`TestWindow::draw` は no-op) ので､これには GPU も
    /// ウィンドウサーバーも要らない -- それでいて本物のウィンドウと同じ要素の
    /// ツリーを歩く｡そこがまさに `gpui_component` のウィジェットがウィンドウの
    /// root を求めて遡る場所だ｡
    /// #118: timeline 自身の root は､どこもクリックせずに最初のフレームから
    /// ウィンドウのフォーカス経路に載っていなければならない｡
    ///
    /// これは仕組みではなく性質のほうだ: gpui はフォーカスされた要素の祖先に
    /// 対してアクションを解決するので､フォーカスされていない timeline は
    /// `Timeline` のキーコンテキストと､その下のすべてのハンドラを手の届かない
    /// ところへ持っていく｡`cmd-r` は何にも一致せず､メニューバーの Reload /
    /// New Post / Submit Post はグレーアウトするか､どこへも届かないところへ
    /// dispatch していた｡動いたのは `Quit` だけで､これは `App` に載っている
    /// からだ｡
    #[gpui::test]
    fn the_timeline_is_focused_from_the_first_frame(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        cx.update(gpui_component::init);
        cx.update(crate::menu::init);

        let timeline_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
        let window = {
            let slot = timeline_slot.clone();
            cx.add_window(move |window, cx| {
                let timeline = cx.new(|cx| {
                    super::TimelineView::new(
                        smoke_config(),
                        smoke_paths(),
                        Startup::Live,
                        window,
                        cx,
                    )
                });
                *slot.borrow_mut() = Some(timeline.clone());
                gpui_component::Root::new(timeline, window, cx)
            })
        };
        let timeline = timeline_slot.borrow().clone().unwrap();
        cx.run_until_parked();

        // この前でクリックも `input.focus(..)` も意図的に行わない: アプリが
        // それを必要としていたことがバグだった｡
        cx.update_window(window.into(), |_, window, cx| {
            let _ = window.draw(cx);
            timeline.update(cx, |view, _cx| {
                assert!(
                    view.focus_handle.is_focused(window),
                    "the timeline root is off the focus path, so its actions are unreachable"
                );
            });
        })
        .unwrap();
    }

    /// #118: コンポーザーから抜けるときは､フォーカスを落とすのではなく
    /// 返さなければならない｡
    ///
    /// `window.blur()` はウィンドウのフォーカス経路を空のまま残し､何かが
    /// クリックされるまでショートカットとメニューバーの半分を無効にして
    /// いた — 起動時のものと同じ失敗に､`esc` を押して辿り着く｡
    #[gpui::test]
    fn leaving_the_composer_returns_focus_to_the_timeline(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        cx.update(gpui_component::init);
        cx.update(crate::menu::init);

        let timeline_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
        let window = {
            let slot = timeline_slot.clone();
            cx.add_window(move |window, cx| {
                let timeline = cx.new(|cx| {
                    let mut view = super::TimelineView::new(
                        smoke_config(),
                        smoke_paths(),
                        Startup::Live,
                        window,
                        cx,
                    );
                    view.signed_in_with_oauth = true;
                    view
                });
                *slot.borrow_mut() = Some(timeline.clone());
                gpui_component::Root::new(timeline, window, cx)
            })
        };
        let timeline = timeline_slot.borrow().clone().unwrap();
        cx.run_until_parked();

        cx.update_window(window.into(), |_, window, cx| {
            timeline.update(cx, |view, cx| {
                view.compose_input
                    .update(cx, |input, cx| input.focus(window, cx));
            });
        })
        .unwrap();
        cx.run_until_parked();

        cx.update_window(window.into(), |_, window, cx| {
            let _ = window.draw(cx);
            timeline.update(cx, |view, _cx| {
                assert!(
                    !view.focus_handle.is_focused(window),
                    "the composer should hold focus once focused"
                );
            });
            // `window.focus(..)` を直接ではなくアクション自体を使う: 検査して
            // いるのはハンドラのほうで､その中身をテストの中で再現したら
            // ハンドラが何をしようと通ってしまう｡
            window.dispatch_action(Box::new(crate::menu::BlurComposer), cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update_window(window.into(), |_, window, cx| {
            let _ = window.draw(cx);
            timeline.update(cx, |view, _cx| {
                assert!(
                    view.focus_handle.is_focused(window),
                    "focus must return to the timeline, not be dropped"
                );
            });
        })
        .unwrap();
    }

    /// ウィンドウの smoke テストが対象にする `Config`｡
    fn smoke_config() -> crate::config::Config {
        crate::config::Config {
            oauth_client_id: "client-123".to_string(),
            target_username: "XDevelopers".to_string(),
            max_results: 20,
            min_fetch_interval_seconds: 60,
            theme: crate::theme::ThemeMode::Light,
            log_level: crate::log::Level::default(),
            request_price: None,
            daily_request_budget: None,
            list_id: None,
            // smoke テストでは off: これらはウィンドウを描画するもので､
            // 金のかかるバックグラウンドのループは検査の対象ではない｡
            auto_sync_list: false,
            sync_interval_seconds: 21_600,
            sync_prune_limit_percent: 10,
            sync_writes_per_batch: 2,
            // 同じ理由で off (#21)｡
            auto_refresh: false,
            auto_refresh_interval_seconds: 300,
            // pill の経路を通るテストがその経路に留まるように off にする —
            // テストハーネスは常にスクロールが最上部にある状態を見つけるので､
            // 既定で on の follow (#22) だと､テストが `pending` を覗く前に
            // 空にしてしまう｡follow のテストは代わりにビューごとに on へ
            // 切り替える｡
            follow_new_posts: false,
        }
    }

    /// ウィンドウの smoke テスト用に､使い捨てのディレクトリを根に置いた
    /// `Paths`｡
    fn smoke_paths() -> crate::paths::Paths {
        let home = std::env::temp_dir().join("twigpui-smoke");
        let home = home.display().to_string();
        crate::paths::Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap()
    }

    // --- #146 層 3: 描画せずにウィンドウへ問えること ---
    //
    // gpui はテストプラットフォームでレイアウトを走らせないので､間隔・
    // 折り返し・サイズについてはここでは何も assert できない — #182 が
    // その代償についての恒常的な備忘で､`--fixture` とスクリーンショット
    // (#146 の層 2) がそれらに当てはまる唯一の確認だ｡
    //
    // 観測*できる*のは状態と dispatch のほうだ｡だからこれらのテストは､
    // ピクセルと関係のない側のウィンドウ半分をカバーする: どのアクションが
    // どのメソッドへ届くか､キーストロークが何を変えるか､そして — この
    // ファイルのほとんどは金を使うので — どの経路が使わないと保証されるか｡
    //
    // どれもハンドラの中身を呼ぶのではなく本物のアクションを dispatch する｡
    // 理由は `leaving_the_composer_returns_focus_to_the_timeline` がすでに
    // 述べているとおりだ: 中身を再現するテストは､ハンドラが実際に何へ
    // 繋がっていようと通ってしまう｡

    /// `fixture` から埋めたウィンドウと､その中のビュー｡
    ///
    /// 上の 3 つのテストがすでにこのブロックを 3 重に持っていて､下のものを
    /// 足すと 9 つのコピーになったので切り出した｡ビューだけでなくハンドルも
    /// 返す: アクションの dispatch にはウィンドウが要り､結果の assert には
    /// ビューが要る｡
    fn fixture_window(
        cx: &mut gpui::TestAppContext,
        fixture: Fixture,
    ) -> (
        gpui::WindowHandle<gpui_component::Root>,
        gpui::Entity<super::TimelineView>,
    ) {
        window_with(
            cx,
            smoke_config(),
            smoke_paths(),
            Startup::Fixture(Box::new(fixture)),
        )
    }

    /// 最上部へ貼り付く follow を on にした [`fixture_window`] (#22) —
    /// follow のテストが構築の時点で本物の既定値を必要とする唯一のつまみだ｡
    /// `show_fixture` がこれを読んで､抑えていた post を pill の裏で待たせるか
    /// 自分から届かせるかを決めるからだ｡
    fn following_fixture_window(
        cx: &mut gpui::TestAppContext,
        fixture: Fixture,
    ) -> (
        gpui::WindowHandle<gpui_component::Root>,
        gpui::Entity<super::TimelineView>,
    ) {
        let config = crate::config::Config {
            follow_new_posts: true,
            ..smoke_config()
        };
        window_with(
            cx,
            config,
            smoke_paths(),
            Startup::Fixture(Box::new(fixture)),
        )
    }

    /// `config` と `paths` に対して `startup` で起動したウィンドウと､その中の
    /// ビュー — [`fixture_window`] から､呼び出し側が選びたいものを引いたものだ
    /// (#164 の `a_switch_is_remembered_on_disk_at_once` は他が書き込まない
    /// ように自分のディレクトリの下で live 起動する｡#22 の follow のテストは
    /// スイッチを on にして起動する)｡
    fn window_with(
        cx: &mut gpui::TestAppContext,
        config: crate::config::Config,
        paths: crate::paths::Paths,
        startup: Startup,
    ) -> (
        gpui::WindowHandle<gpui_component::Root>,
        gpui::Entity<super::TimelineView>,
    ) {
        use gpui::AppContext as _;

        cx.update(gpui_component::init);
        cx.update(crate::menu::init);

        let slot = std::rc::Rc::new(std::cell::RefCell::new(None));
        let window = {
            let slot = slot.clone();
            cx.add_window(move |window, cx| {
                let timeline =
                    cx.new(|cx| super::TimelineView::new(config, paths, startup, window, cx));
                *slot.borrow_mut() = Some(timeline.clone());
                gpui_component::Root::new(timeline, window, cx)
            })
        };
        let timeline = slot.borrow().clone().unwrap();
        cx.run_until_parked();
        (window, timeline)
    }

    /// `shown` がすでに画面にあり､`waiting` を抑えてある fixture — #21 の
    /// "N new posts" のバーが存在する理由になっている形だ｡
    fn fixture_with(shown: &[&str], waiting: &[&str]) -> Fixture {
        Fixture {
            signed_in_as: crate::fixture::FixtureUser {
                id: "5685672".to_string(),
                username: "usadamasa".to_string(),
            },
            items: shown
                .iter()
                .map(|id| item_with(id, "someone", None))
                .collect(),
            pending: waiting
                .iter()
                .map(|id| item_with(id, "someone", None))
                .collect(),
            lists: Vec::new(),
            sync: None,
        }
    }

    /// list sync が何かを負っている [`fixture_with`] (#205)｡sync の行が出て
    /// いて､入口が押せる状態のウィンドウ｡
    fn fixture_with_sync(shown: &[&str], pending: usize) -> Fixture {
        Fixture {
            sync: Some(crate::fixture::FixtureSync {
                pending,
                blocked_for_seconds: 0,
                refusals: 0,
            }),
            ..fixture_with(shown, &[])
        }
    }

    /// list を設定した [`fixture_window`] (#205)｡
    ///
    /// `smoke_config` の `list_id` は `None` なので､素の fixture のウィンドウは
    /// `SyncOff::NoList` で止まる｡手動でも越えられない唯一の gate なので､
    /// sync の経路を通るテストは list を持つ必要がある｡課金はその先の gate
    /// (`client` が無いこと) が止める｡
    fn sync_fixture_window(
        cx: &mut gpui::TestAppContext,
        fixture: Fixture,
    ) -> (
        gpui::WindowHandle<gpui_component::Root>,
        gpui::Entity<super::TimelineView>,
    ) {
        let config = crate::config::Config {
            list_id: Some("1750".to_string()),
            ..smoke_config()
        };
        window_with(
            cx,
            config,
            smoke_paths(),
            Startup::Fixture(Box::new(fixture)),
        )
    }

    /// ウィンドウが現在描画している id｡
    fn shown_ids(view: &super::TimelineView) -> Vec<String> {
        match &view.state {
            TimelineState::Loaded(items) => items.iter().map(|item| item.id.clone()).collect(),
            other => panic!("expected a loaded timeline, got {other:?}"),
        }
    }

    /// #146: fixture のウィンドウは `XClient` をまったく構築しない｡
    ///
    /// `show_fixture` のドキュメントはこれを｢慣習ではなく､fixture がコストを
    /// かけられない理由そのものだ｣と呼んでいる — このビューで金のかかる経路は
    /// すべて `self.client` を通るので､それが無いことがスクリーンショットを
    /// 無料にしている｡今まではそれが 1 文だっただけで､これはその強制だ｡
    #[gpui::test]
    fn a_fixture_window_holds_no_client_to_spend_with(cx: &mut gpui::TestAppContext) {
        let (_window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &[]));

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert!(
                    view.client.is_none(),
                    "a fixture must not be able to reach the API"
                );
            });
        });
    }

    /// #21: fixture が抑えていた post が､そのままバーの件数になる｡
    #[gpui::test]
    fn a_fixtures_waiting_posts_fill_the_new_posts_buffer(cx: &mut gpui::TestAppContext) {
        let (_window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &["4", "3"]));

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(view.pending.as_ref().map(|pending| pending.count), Some(2));
                assert_eq!(
                    shown_ids(view),
                    ["2", "1"],
                    "a poll's posts must not reach the screen on their own"
                );
            });
        });
    }

    /// #21: "Show New Posts" を押すことが､それらを画面に出す操作だ｡
    ///
    /// これが埋める穴は本物だ: デスクトップのウィンドウをクリックする手段の
    /// 無いセッションからは､バーとその `cmd-shift-r` の割り当てを手で試せない
    /// ので､#146 までクリックの経路はまるごと未検証だった｡`dispatch_action`
    /// は `on_action` から下をカバーする — キーストロークと同じ登録を通る｡
    /// その 1 つ上の段､座標がバーの上に落ちるかどうかは､下の #184 のテストだ｡
    #[gpui::test]
    fn showing_new_posts_moves_them_onto_the_timeline(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        let (window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &["4", "3"]));

        cx.update_window(window.into(), |_, window, cx| {
            let _ = window.draw(cx);
            window.dispatch_action(Box::new(crate::menu::ShowNewPosts), cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(shown_ids(view), ["4", "3", "2", "1"]);
                assert!(
                    view.pending.is_none(),
                    "the buffer must be emptied, or the bar keeps offering posts already shown"
                );
            });
        });
    }

    /// #184: 同じ表示を､バー自体へのクリックから辿る｡
    ///
    /// これは #183 の 1 つ上の層だ｡上のテストはアクションを直接 dispatch する
    /// ので､1 段が未検証のまま残る: ある座標のマウスがそもそもバーの上に
    /// 落ちるかどうかだ｡ここでは何も dispatch しない — 描かれたばかりの
    /// フレームからバー自身の bounds を引き､その中心でクリックを模擬し､
    /// `on_click` を見つけなければならないのは gpui の hit test のほうだ｡
    /// assert は dispatch のテストのものと意図的に同一なので､通れば 2 つの
    /// 経路が一致していることになる｡
    ///
    /// 座標はどこにも書き下さない｡`render::Addressable` がバーに名前を 1 つ
    /// 与え､`debug_bounds` がその名前が実際にどこへ配置されたかを読み返し､
    /// クリックはそれに従う — だから `render.rs` でバーを動かせば､クリックも
    /// 一緒に動く｡
    #[gpui::test]
    fn clicking_the_new_posts_bar_moves_them_onto_the_timeline(cx: &mut gpui::TestAppContext) {
        let (window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &["4", "3"]));

        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let bar = visual
            .debug_bounds("new-posts")
            .expect("the bar has to be laid out before a click can reach it");
        visual.simulate_click(bar.center(), gpui::Modifiers::none());

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(shown_ids(view), ["4", "3", "2", "1"]);
                assert!(
                    view.pending.is_none(),
                    "the buffer must be emptied, or the bar keeps offering posts already shown"
                );
            });
        });
    }

    /// #184: 上のテストに意味を持たせているもの｡
    ///
    /// どこに落ちても `on_click` に届いてしまう模擬クリックは､hit test に
    /// ついて何も証明しないまま前のテストを通してしまう｡そこでこちらは
    /// timeline の真ん中をクリックする — バーより下で､どの行にも当たらない
    /// 位置だ｡fixture の 2 件の post は最上部にあるからだ — そしてバッファが
    /// まだ待っていることを要求する｡この 2 つで､決めているのは座標だと言える｡
    /// それが #183 の `dispatch_action` が飛ばしている段だ｡
    ///
    /// 外れのほうも当たりと同じやり方で扱い､バーの中心を何ピクセルかずらす
    /// やり方は採らない: 直値のオフセットは書き下された座標であり､ウィンドウか
    /// バーの高さが変わった瞬間からまたバーの上に落ち始めるからだ｡
    #[gpui::test]
    fn clicking_the_timeline_below_the_bar_reveals_nothing(cx: &mut gpui::TestAppContext) {
        let (window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &["4", "3"]));

        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let body = visual
            .debug_bounds("timeline")
            .expect("the timeline has to be laid out before a click can land in it");
        visual.simulate_click(body.center(), gpui::Modifiers::none());

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(shown_ids(view), ["2", "1"]);
                assert_eq!(
                    view.pending.as_ref().map(|pending| pending.count),
                    Some(2),
                    "a click outside the bar must leave the offer standing"
                );
            });
        });
    }

    /// #206: toast は timeline の下端に重なる capsule で､上のバーではない｡
    ///
    /// 下端「付近」と中央寄せは bounds で言える｡幅を timeline の半分未満に
    /// 押さえるのは､最後の行のアクション列を覆う帯にならないため｡
    #[gpui::test]
    fn the_new_posts_toast_sits_at_the_bottom_of_the_timeline(cx: &mut gpui::TestAppContext) {
        let (mut visual, _timeline) = drawn(cx, fixture_with(&["2", "1"], &["4", "3"]));

        let toast = visual
            .debug_bounds("new-posts")
            .expect("posts are waiting, so the toast has to be laid out");
        let body = visual
            .debug_bounds("timeline")
            .expect("the timeline is laid out");

        assert!(
            toast.bottom() <= body.bottom(),
            "the toast must not hang below the timeline: {toast:?} vs {body:?}"
        );
        assert!(
            body.bottom() - toast.bottom() < gpui::px(48.),
            "the toast sits near the bottom edge, not floating mid-screen: {toast:?} vs {body:?}"
        );
        assert!(
            toast.top() > body.center().y,
            "the toast overlaps the bottom of the timeline, not its middle: {toast:?}"
        );
        assert!(
            toast.size.width < body.size.width / 2.,
            "a capsule, not a bar: {toast:?} vs {body:?}"
        );
        assert!(
            (toast.center().x - body.center().x).abs() < gpui::px(1.),
            "centered: {toast:?} vs {body:?}"
        );
    }

    /// #206: toast は viewport に貼りつく｡一覧が scroll しても動かない｡
    #[gpui::test]
    fn the_toast_stays_put_while_the_timeline_scrolls(cx: &mut gpui::TestAppContext) {
        let ids: Vec<String> = (1..=40).map(|n| n.to_string()).collect();
        let shown: Vec<&str> = ids.iter().map(String::as_str).collect();
        let (mut visual, timeline) = drawn(cx, fixture_with(&shown, &["99"]));
        let body = visual
            .debug_bounds("timeline")
            .expect("the timeline is laid out");
        let before = visual
            .debug_bounds("new-posts")
            .expect("a post is waiting, so the toast is laid out");

        visual.simulate_event(wheel_event(body.center(), -3.));
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(2));
        cx.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert!(
            offset_y(cx, &timeline) < -10.,
            "the list itself has scrolled"
        );
        let after = visual
            .debug_bounds("new-posts")
            .expect("the toast is still there");
        assert_eq!(before, after, "the toast must not scroll with the rows");
    }

    /// #206: 下端の帯のうち capsule の外は timeline のまま — 覆いの wrapper が
    /// クリックを食ってはいけない｡
    #[gpui::test]
    fn clicking_beside_the_toast_reaches_the_timeline_not_the_toast(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline) = drawn(cx, fixture_with(&["2", "1"], &["4", "3"]));
        let toast = visual
            .debug_bounds("new-posts")
            .expect("posts are waiting, so the toast is laid out");
        let body = visual
            .debug_bounds("timeline")
            .expect("the timeline is laid out");

        visual.simulate_click(
            gpui::point(body.left() + gpui::px(10.), toast.center().y),
            gpui::Modifiers::none(),
        );

        cx.update(|cx| {
            assert_eq!(
                timeline
                    .read(cx)
                    .pending
                    .as_ref()
                    .map(|pending| pending.count),
                Some(2),
                "a click beside the capsule must leave the offer standing"
            );
        });
    }

    /// #206: 出るときは段階的に濃くなり､着いたらタイマーを手放す｡
    #[gpui::test]
    fn the_toast_fades_in_and_then_settles(cx: &mut gpui::TestAppContext) {
        let (_visual, timeline) = drawn(cx, fixture_with(&["2", "1"], &["3"]));

        cx.update(|cx| {
            let view = timeline.read(cx);
            assert!(
                matches!(view.toast.fade, Fade::Rising(_)),
                "the first frame is the first step, not the whole capsule: {:?}",
                view.toast.fade
            );
            assert!(view.toast_fade_task.is_some(), "the fade is ticking");
        });

        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();

        cx.update(|cx| {
            let view = timeline.read(cx);
            assert_eq!(view.toast.fade, Fade::Shown);
            assert_eq!(view.toast.count, 1);
            assert!(
                view.toast_fade_task.is_none(),
                "a settled fade must not keep burning frames"
            );
        });
    }

    /// #206: 見せたら薄くなって外れる｡薄くなる間も件数は言い続ける｡
    #[gpui::test]
    fn the_toast_fades_out_and_leaves_once_the_posts_are_shown(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline) = drawn(cx, fixture_with(&["2", "1"], &["4", "3"]));
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();

        let toast = visual
            .debug_bounds("new-posts")
            .expect("posts are waiting, so the toast is laid out");
        visual.simulate_click(toast.center(), gpui::Modifiers::none());
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert!(
            visual.debug_bounds("new-posts").is_some(),
            "the toast fades rather than vanishing in the same frame"
        );
        cx.update(|cx| {
            let view = timeline.read(cx);
            assert!(view.pending.is_none(), "the click showed the posts");
            assert!(
                matches!(view.toast.fade, Fade::Falling(_)),
                "the toast is on its way out: {:?}",
                view.toast.fade
            );
            assert_eq!(
                view.toast.count, 2,
                "while falling the label keeps saying what it said"
            );
        });

        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert!(
            visual.debug_bounds("new-posts").is_none(),
            "a hidden toast is out of the tree, not a transparent capsule"
        );
        cx.update(|cx| {
            let view = timeline.read(cx);
            assert_eq!(view.toast.fade, Fade::Hidden);
            assert!(view.toast_fade_task.is_none());
        });
    }

    /// #206: follow が流し込む間､toast は「まだ視界の上にある数」を数え
    /// 下げ､0 で消える｡
    ///
    /// 途中の値そのものは assert しない — 行の高さはテストの layout が決める｡
    /// 言えるのは 3 から始まり､減る一方で､0 で終わり､途中の値が少なくとも
    /// 1 つ見えたことだ｡
    #[gpui::test]
    fn following_counts_down_as_the_new_posts_glide_in(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline, _body) = scrollable_window(cx);

        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                view.follow = super::FollowMode::Follow;
                let shown: Vec<String> = shown_ids(view);
                let displayed: Vec<&str> = shown.iter().map(String::as_str).collect();
                let incoming: Vec<TimelineItem> = ["43", "42", "41"]
                    .iter()
                    .chain(displayed.iter())
                    .map(|id| item_with(id, "someone", None))
                    .collect();
                let pending = pending_after_poll(&displayed, incoming).expect("three arrived");
                view.present_poll(pending, cx);
                assert_eq!(view.unseen, 3, "every new row starts above the viewport");
            });
        });

        let mut seen = vec![3_usize];
        for _ in 0..600 {
            cx.executor()
                .advance_clock(std::time::Duration::from_secs_f32(super::scroll::FRAME_S));
            cx.run_until_parked();
            visual.update(|window, cx| {
                let _ = window.draw(cx);
            });
            let now = cx.update(|cx| timeline.read(cx).unseen);
            if seen.last() != Some(&now) {
                seen.push(now);
            }
            if now == 0 {
                break;
            }
        }

        assert_eq!(
            seen.last(),
            Some(&0),
            "the glide ends with nothing left above: {seen:?}"
        );
        assert!(
            seen.windows(2).all(|pair| pair[1] < pair[0]),
            "the count only ever goes down: {seen:?}"
        );
        assert!(
            seen.len() > 2,
            "at least one intermediate count is visible on the way: {seen:?}"
        );

        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            visual.debug_bounds("new-posts").is_none(),
            "with nothing left above, the toast is gone"
        );
    }

    /// #206: follow の途中で toast を押すと最上部へ飛ぶ｡pill と同じく無料 —
    /// リクエストも取得も無い｡
    #[gpui::test]
    fn clicking_the_toast_while_following_jumps_to_the_top_for_free(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline, _body) = scrollable_window(cx);

        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                view.follow = super::FollowMode::Follow;
                let shown: Vec<String> = shown_ids(view);
                let displayed: Vec<&str> = shown.iter().map(String::as_str).collect();
                let incoming: Vec<TimelineItem> = ["43", "42", "41"]
                    .iter()
                    .chain(displayed.iter())
                    .map(|id| item_with(id, "someone", None))
                    .collect();
                let pending = pending_after_poll(&displayed, incoming).expect("three arrived");
                view.present_poll(pending, cx);
            });
        });
        // 1 フレームで補正が着地し､次のフレームで glide が歩き出す｡
        for _ in 0..2 {
            cx.executor()
                .advance_clock(std::time::Duration::from_secs_f32(super::scroll::FRAME_S));
            cx.run_until_parked();
            visual.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }
        cx.update(|cx| {
            let view = timeline.read(cx);
            assert!(view.unseen > 0, "the rows are still above the viewport");
            assert!(view.glide.is_some(), "and the glide is still walking");
        });

        let toast = visual
            .debug_bounds("new-posts")
            .expect("rows are above the viewport, so the toast is laid out");
        visual.simulate_click(toast.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        cx.update(|cx| {
            let view = timeline.read(cx);
            assert_eq!(view.unseen, 0, "the jump reveals every row at once");
            assert!(view.glide.is_none(), "the jump replaces the glide");
            assert!(view.pending.is_none());
            assert!(view.client.is_none());
            assert!(
                view.last_reload_at.is_none(),
                "jumping to the top must not count as a fetch"
            );
        });
    }

    /// fixture の window はロック中でも描き続け､live の window は upstream
    /// どおり止まる — fork した gpui の patch が読むスイッチを `main` が
    /// これで決める｡fixture 側が false に戻ると､ロック中に立てた fixture の
    /// capture は真っ黒に戻る｡
    #[test]
    fn only_a_fixture_window_keeps_drawing_while_occluded() {
        assert!(
            Startup::Fixture(Box::new(fixture_with(&["1"], &[]))).draws_while_occluded(),
            "a fixture window exists to be captured, locked screen or not"
        );
        assert!(
            !Startup::Live.draws_while_occluded(),
            "a live window keeps upstream's power-saving behavior"
        );
    }

    // --- #175: 手動 scroll ---

    /// 40 件で開き､1 フレーム描いて timeline の bounds を返す｡ホイールの
    /// event は hit test を通るので､描いていないウィンドウには届かない｡
    fn scrollable_window(
        cx: &mut gpui::TestAppContext,
    ) -> (
        gpui::VisualTestContext,
        gpui::Entity<super::TimelineView>,
        gpui::Bounds<gpui::Pixels>,
    ) {
        let ids: Vec<String> = (1..=40).map(|n| n.to_string()).collect();
        let shown: Vec<&str> = ids.iter().map(String::as_str).collect();
        let (window, timeline) = fixture_window(cx, fixture_with(&shown, &[]));
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let body = visual
            .debug_bounds("timeline")
            .expect("the timeline has to be laid out before a wheel can reach it");
        (visual, timeline, body)
    }

    fn wheel_event(at: gpui::Point<gpui::Pixels>, lines: f32) -> gpui::ScrollWheelEvent {
        gpui::ScrollWheelEvent {
            position: at,
            delta: gpui::ScrollDelta::Lines(gpui::point(0., lines)),
            modifiers: gpui::Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        }
    }

    fn pan_event(
        at: gpui::Point<gpui::Pixels>,
        pixels: f32,
        phase: gpui::TouchPhase,
    ) -> gpui::ScrollWheelEvent {
        gpui::ScrollWheelEvent {
            position: at,
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(pixels))),
            modifiers: gpui::Modifiers::none(),
            touch_phase: phase,
        }
    }

    fn offset_y(
        cx: &mut gpui::TestAppContext,
        timeline: &gpui::Entity<super::TimelineView>,
    ) -> f32 {
        cx.update(|cx| timeline.read(cx).list_scroll.offset().y.into())
    }

    /// #175: ホイールのティックは飛ばずに滑る｡event の直後には動いておらず
    /// (gpui 自身の handler が同じ event に delta を足していればここで
    /// 飛ぶ)､落ち着いたところで delta ぶんちょうど進んでいて､2 回目は
    /// その 2 倍のところに着く｡
    #[gpui::test]
    fn a_wheel_tick_is_smoothed_and_lands_exactly_its_delta_away(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline, body) = scrollable_window(cx);

        visual.simulate_event(wheel_event(body.center(), -3.));
        let right_after = offset_y(cx, &timeline);
        assert!(
            right_after.abs() < 1.,
            "a tick must not jump in the same frame, offset {right_after}"
        );

        cx.executor()
            .advance_clock(std::time::Duration::from_secs(2));
        cx.run_until_parked();
        let one_tick = offset_y(cx, &timeline);
        assert!(
            one_tick < -10.,
            "after settling the list has scrolled down, {one_tick}"
        );
        cx.update(|cx| {
            let view = timeline.read(cx);
            assert!(
                view.scroller.is_settled(),
                "and there is nothing left to animate"
            );
            assert!(view.scroll_motion.is_none(), "so the frame loop has let go");
        });

        visual.simulate_event(wheel_event(body.center(), -3.));
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(2));
        cx.run_until_parked();
        let two_ticks = offset_y(cx, &timeline);
        assert!(
            (two_ticks - one_tick * 2.).abs() < 1.,
            "two equal ticks land twice as far: {two_ticks} vs 2 x {one_tick}"
        );
    }

    /// #175: 読み手が触れたら glide は即座に止まり､2 つの animation が
    /// scroll 位置を取り合わない｡
    #[gpui::test]
    fn a_wheel_tick_stops_a_glide_in_flight(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline, body) = scrollable_window(cx);

        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                view.follow = super::FollowMode::Follow;
                let shown: Vec<String> = shown_ids(view);
                let displayed: Vec<&str> = shown.iter().map(String::as_str).collect();
                let incoming: Vec<TimelineItem> = ["42", "41"]
                    .iter()
                    .chain(displayed.iter())
                    .map(|id| item_with(id, "someone", None))
                    .collect();
                let pending = pending_after_poll(&displayed, incoming).expect("two arrived");
                view.present_poll(pending, cx);
                assert!(view.glide.is_some(), "the reveal starts as a glide");
            });
        });

        visual.simulate_event(wheel_event(body.center(), -1.));
        cx.update(|cx| {
            assert!(
                timeline.read(cx).glide.is_none(),
                "the reader's hand wins over the glide"
            );
        });
    }

    /// #175: 最上部で trackpad をさらに引くと一覧は動かず band が伸び､
    /// 指が離れれば戻る｡見た目のずれは描いた bounds に出る｡
    #[gpui::test]
    fn a_trackpad_pull_past_the_top_bounces_and_relaxes(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline, body) = scrollable_window(cx);

        visual.simulate_event(pan_event(body.center(), 40., gpui::TouchPhase::Started));
        let (offset, shift) = cx.update(|cx| {
            let view = timeline.read(cx);
            (
                f32::from(view.list_scroll.offset().y),
                view.scroller.shift(),
            )
        });
        assert!(
            offset.abs() < 1.,
            "the list itself stays at the top, {offset}"
        );
        assert!(
            shift > 0. && shift < 40.,
            "40px of pull shows as a smaller shift, {shift}"
        );

        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let pulled = visual
            .debug_bounds("timeline")
            .expect("still laid out while pulled");
        let lowered = f32::from(pulled.top()) - f32::from(body.top());
        assert!(
            (lowered - shift).abs() < 0.5,
            "the drawn list sits {shift}px lower, was {lowered}"
        );

        visual.simulate_event(pan_event(body.center(), 0., gpui::TouchPhase::Ended));
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(2));
        cx.run_until_parked();
        cx.update(|cx| {
            let view = timeline.read(cx);
            assert!(
                view.scroller.shift().abs() < 0.5,
                "the band relaxes once the finger lifts"
            );
            assert!(view.scroller.is_settled(), "{:?}", view.scroller);
        });
    }

    /// #22: 読み手が最上部にいるところへ来た poll はそのまま流れる — pill も
    /// 押下も無い｡描画されていないウィンドウは「最上部」と読まれる
    /// (`logical_scroll_top` はレイアウト前には `(0, 0px)` を答える)｡開いた
    /// ばかりのウィンドウもまたそれだ｡
    #[gpui::test]
    fn a_poll_flows_onto_the_screen_when_the_reader_is_at_the_top(cx: &mut gpui::TestAppContext) {
        let (_window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &[]));

        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                view.follow = super::FollowMode::Follow;
                let pending = pending_after_poll(
                    &["2", "1"],
                    ["4", "3", "2", "1"]
                        .map(|id| item_with(id, "someone", None))
                        .to_vec(),
                )
                .expect("two posts arrived");
                view.present_poll(pending, cx);

                assert_eq!(shown_ids(view), ["4", "3", "2", "1"]);
                assert!(
                    view.pending.is_none(),
                    "followed posts are on screen — a pill would offer them twice"
                );
                assert!(view.glide.is_some(), "the reveal is the glide's to make");
            });
        });
    }

    /// #239: 待っても直らない拒否を受けた poll は､ループを止めて理由を
    /// バナーへ置く｡issue のログはこれが無かった姿で､同じ 403 と 401 を
    /// 3 分ごとに 130 行以上積み上げながら､画面には何も出していなかった｡
    #[gpui::test]
    fn a_denied_poll_halts_the_loop_and_leaves_a_banner(cx: &mut gpui::TestAppContext) {
        let (_window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &[]));

        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                let denied = anyhow::Error::from(Denied {
                    endpoint: rate_limit::Endpoint::ListTimeline,
                    denial: Denial::Forbidden,
                    detail: "Forbidden: Your monthly spend cap has been reached.".to_string(),
                });

                assert_eq!(view.apply_poll(Err(denied), cx), Poll::Halt);
                let notice = view
                    .auto_refresh_notice
                    .clone()
                    .expect("the reader must be told the poll stopped")
                    .to_string();
                assert!(notice.contains("list_timeline"), "{notice}");
                assert!(notice.contains("monthly spend cap"), "{notice}");
                assert_eq!(
                    shown_ids(view),
                    ["2", "1"],
                    "a failed poll never touches what is on screen"
                );
            });
        });
    }

    /// #239: そのバナーは実際に描かれる｡上のテストはフィールドが埋まる
    /// ところまでしか見ないので､`body` の `when_some` を消しても落ちない｡
    #[gpui::test]
    fn the_halted_poll_banner_reaches_the_screen(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline) = drawn(cx, fixture_with(&["2", "1"], &[]));

        visual.update(|_window, cx| {
            timeline.update(cx, |view, _cx| {
                view.auto_refresh_notice =
                    Some(gpui::SharedString::from("auto-refresh has stopped."));
            });
        });
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert!(
            visual.debug_bounds("banner-auto-refresh").is_some(),
            "the reason the poll stopped must be on the screen, not only in the field"
        );
    }

    /// #239 の裏側: 一時的な失敗ではループを止めない｡止めてしまうと､夜中の
    /// 瞬断ひとつで朝まで取得が死ぬ｡
    #[gpui::test]
    fn a_dropped_poll_keeps_the_loop_and_says_nothing(cx: &mut gpui::TestAppContext) {
        let (_window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &[]));

        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                let dropped = anyhow::anyhow!("request to https://api.x.com/2/... failed");

                assert_eq!(view.apply_poll(Err(dropped), cx), Poll::Continue);
                assert!(view.auto_refresh_notice.is_none());
            });
        });
    }

    /// #22: 何も無い画面へ follow したとき — 空の List､入れたばかりの
    /// インストール — は､glide を仕掛けずに最上部へ着く｡位置を保つべき行が
    /// 無いので､補正はリストの末尾を越えた index を指してしまう｡gpui は解決
    /// できない anchor を保持して prepaint のたびに再試行するため､後の
    /// "Load older" でリストがその index を越えて伸びると､目に見える理由も
    /// 無く読み手の下でビューポートが飛ぶことになる｡
    #[gpui::test]
    fn following_onto_an_empty_timeline_snaps_without_a_glide(cx: &mut gpui::TestAppContext) {
        let (_window, timeline) = fixture_window(cx, fixture_with(&[], &[]));

        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                view.follow = super::FollowMode::Follow;
                let pending = pending_after_poll(
                    &[],
                    ["2", "1"].map(|id| item_with(id, "someone", None)).to_vec(),
                )
                .expect("two posts arrived");
                view.present_poll(pending, cx);

                assert_eq!(shown_ids(view), ["2", "1"]);
                assert!(view.pending.is_none());
                assert!(
                    view.glide.is_none(),
                    "with no row to keep in place there is nothing to glide from"
                );
            });
        });
    }

    /// #22: スイッチが off なら､スクロール位置が何であれ､どの poll も pill の
    /// 裏で待つ｡
    #[gpui::test]
    fn a_poll_waits_behind_the_pill_when_follow_is_off(cx: &mut gpui::TestAppContext) {
        let (_window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &[]));

        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                view.follow = super::FollowMode::Pill;
                let pending = pending_after_poll(
                    &["2", "1"],
                    ["4", "3", "2", "1"]
                        .map(|id| item_with(id, "someone", None))
                        .to_vec(),
                )
                .expect("two posts arrived");
                view.present_poll(pending, cx);

                assert_eq!(shown_ids(view), ["2", "1"]);
                assert_eq!(view.pending.as_ref().map(|pending| pending.count), Some(2));
                assert!(view.glide.is_none(), "nothing moved, so nothing glides");
            });
        });
    }

    /// #22: 途中まで下がっている読み手は位置を保つ — follow は pill に譲る｡
    /// これは `preserved_scroll_target` のルールを上から見たものだ｡
    #[gpui::test]
    fn a_poll_waits_behind_the_pill_when_the_reader_is_scrolled_down(
        cx: &mut gpui::TestAppContext,
    ) {
        let (window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &[]));

        // 本物のフレームを描く｡そうすれば `logical_scroll_top` が答える元に
        // する行の bounds ができる — 描画されていないウィンドウは最上部以外に
        // いられない｡
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                let first_row = view
                    .list_scroll
                    .bounds_for_item(0)
                    .expect("the frame above laid the rows out");
                view.list_scroll.set_offset(gpui::point(
                    gpui::px(0.),
                    gpui::px(-f32::from(first_row.size.height)),
                ));

                view.follow = super::FollowMode::Follow;
                let pending = pending_after_poll(
                    &["2", "1"],
                    ["4", "3", "2", "1"]
                        .map(|id| item_with(id, "someone", None))
                        .to_vec(),
                )
                .expect("two posts arrived");
                view.present_poll(pending, cx);

                assert_eq!(
                    shown_ids(view),
                    ["2", "1"],
                    "a reader mid-timeline must not have the screen replaced under them"
                );
                assert_eq!(view.pending.as_ref().map(|pending| pending.count), Some(2));
            });
        });
    }

    /// #22: follow が on なら､fixture が抑えていた post は数秒後に自分から
    /// 届く — それらを運んできたはずの poll の模擬だ｡おかげで金のかかる
    /// リクエスト無しに､この流れを手で
    /// (`cargo run -- --fixture fixtures/timeline.json`) 眺められる｡
    #[gpui::test]
    fn a_fixtures_waiting_posts_arrive_by_themselves_when_follow_is_on(
        cx: &mut gpui::TestAppContext,
    ) {
        let (_window, timeline) =
            following_fixture_window(cx, fixture_with(&["2", "1"], &["4", "3"]));

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(
                    shown_ids(view),
                    ["2", "1"],
                    "nothing arrives before the delay — the arrival is the point"
                );
                assert!(
                    view.pending.is_none(),
                    "the poll is simulated, not pre-filled into the pill's buffer"
                );
            });
        });

        cx.executor().advance_clock(std::time::Duration::from_secs(
            super::TimelineView::FIXTURE_ARRIVAL_SECONDS + 1,
        ));
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(shown_ids(view), ["4", "3", "2", "1"]);
                assert!(
                    view.pending.is_none(),
                    "at the top with follow on, nothing waits behind a pill"
                );
            });
        });
    }

    /// #22: View メニューのトグルは follow を反転させ､今どちら向きかを言う —
    /// メニューバーはチェックマークを出せないので､新しい状態が見えるのは
    /// バナーだけだ｡
    #[gpui::test]
    fn toggling_follow_flips_the_switch_and_reports_itself(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        let (window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &[]));

        cx.update_window(window.into(), |_, window, cx| {
            let _ = window.draw(cx);
            window.dispatch_action(Box::new(crate::menu::ToggleFollowNewPosts), cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert!(
                    view.follow.is_following(),
                    "the test config starts with follow off, so one toggle turns it on"
                );
                assert!(
                    matches!(view.reload_notice, Some(ReloadNotice::Outcome(_))),
                    "the flip must say which way it went, got {:?}",
                    view.reload_notice
                );
            });
        });
    }

    /// #182 に遡って: ステータスバーの 2 つの区画は接触しない｡
    ///
    /// これは #182 が無いままマージされたテストだ｡`Total: 11 req` と
    /// `List sync: …` が `11 reqList sync` と描画されていた — 行の `gap_3` は
    /// 素のテキスト span 2 つを引き離さないし､`gap_8` へ上げても何も変わら
    /// なかったので､修正は明示的な margin になった｡欠陥も修正も､見る手段は
    /// スクリーンショットしか無かった｡
    ///
    /// それは唯一の手段ではなかった｡レイアウトはテストプラットフォームでも
    /// 走る｡`TestWindow::draw` が飛ばすのは `Scene` をピクセルに変える段だ｡
    /// だから配置された bounds は本物で､このテストが読める間隔は assert で
    /// 押さえられる間隔だ (#184)｡特定の gap ではなく意図的に `>` にしてある:
    /// 欠陥は 2 つの箱が接することであって､正確な margin を固定すると､
    /// 意図的な間隔の変更がすべてテストの失敗になってしまう｡
    #[gpui::test]
    fn the_status_bars_segments_keep_apart(cx: &mut gpui::TestAppContext) {
        let (window, _timeline) = fixture_window(cx, fixture_with(&["2", "1"], &[]));

        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let usage = visual
            .debug_bounds("status-usage")
            .expect("the request count is always shown");
        let sync = visual
            .debug_bounds("status-sync")
            .expect("the sync segment is always shown");

        assert!(
            sync.left() > usage.right(),
            "the two segments run together, which reads as `11 reqList sync` \
             on screen: usage ends at {:?}, sync starts at {:?}",
            usage.right(),
            sync.left()
        );
    }

    /// #21: もう一度押しても何も変わらない｡
    ///
    /// 単に落ちなかったことではなく､timeline が*同一*であることを assert する｡
    /// 今の `apply_pending` はバッファが空なら早期 return する｡ここで防いで
    /// いる退行は､後から誰かがこれを無条件に `state` を設定する形にして､
    /// 2 回目の押下で画面を空白にしてしまうことだ｡
    #[gpui::test]
    fn showing_new_posts_with_none_waiting_leaves_the_timeline_alone(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::AppContext as _;

        let (window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &["3"]));

        for _ in 0..2 {
            cx.update_window(window.into(), |_, window, cx| {
                let _ = window.draw(cx);
                window.dispatch_action(Box::new(crate::menu::ShowNewPosts), cx);
            })
            .unwrap();
            cx.run_until_parked();
        }

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(shown_ids(view), ["3", "2", "1"]);
            });
        });
    }

    /// #21: `cmd-shift-r` は何も使わない｡
    ///
    /// `cmd-r` との対がこの設計のすべてだ — 一方は取得を買い､もう一方は
    /// すでに支払い済みのものを見せる — そして `menu.rs` が文章でそう言って
    /// いる｡これはその主張のうち､テストで押さえられる部分だ: dispatch の後も
    /// client はまだ無く､`last_reload_at` も動いていない｡だから何も出ていない
    /// し､試みられてすらいない｡
    #[gpui::test]
    fn showing_new_posts_sends_nothing(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        let (window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &["3"]));

        cx.update_window(window.into(), |_, window, cx| {
            let _ = window.draw(cx);
            window.dispatch_action(Box::new(crate::menu::ShowNewPosts), cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert!(view.client.is_none());
                assert!(
                    view.last_reload_at.is_none(),
                    "showing a buffered fetch must not count as one"
                );
            });
        });
    }

    // --- #164: ツールバーの list picker ---

    /// `shown` に加えて､picker が名前を出せる `lists` を持つ fixture｡
    fn fixture_with_lists(shown: &[&str], lists: &[(&str, &str)]) -> Fixture {
        let mut fixture = fixture_with(shown, &[]);
        fixture.lists = lists
            .iter()
            .map(|(id, name)| crate::x_api::ListSummary {
                id: (*id).to_string(),
                name: (*name).to_string(),
            })
            .collect();
        fixture
    }

    /// `ids` を `list_id` のキャッシュ済み timeline として smoke 用の
    /// ディレクトリへ書く｡そこへ切り替えたときに､client 無しでも描くものが
    /// あるようにするためだ｡
    fn cache_list(list_id: &str, ids: &[&str]) {
        let paths = smoke_paths();
        paths.ensure_dirs().unwrap();
        let items: Vec<TimelineItem> = ids
            .iter()
            .map(|id| item_with(id, "someone", None))
            .collect();
        crate::cache::save_primary_timeline(
            &paths,
            &crate::cache::TimelineSource::List(list_id.to_string()),
            "5685672",
            &items,
            0,
        )
        .unwrap();
    }

    /// 下のクリックのテスト用に､描画済みのウィンドウとその visual context｡
    fn drawn(
        cx: &mut gpui::TestAppContext,
        fixture: Fixture,
    ) -> (gpui::VisualTestContext, gpui::Entity<super::TimelineView>) {
        let (window, timeline) = fixture_window(cx, fixture);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (visual, timeline)
    }

    /// #164: どの区画も配置され､Home が先頭で､どれも重ならない —
    /// `the_status_bars_segments_keep_apart` がステータスバーについて述べる
    /// のと同じ主張を､同じ理由で述べている｡
    #[gpui::test]
    fn the_picker_lays_out_home_and_every_list_left_to_right(cx: &mut gpui::TestAppContext) {
        let (mut visual, _timeline) = drawn(
            cx,
            fixture_with_lists(&["1"], &[("9101", "Following mirror"), ("9102", "Rust")]),
        );

        let home = visual
            .debug_bounds("tab-home")
            .expect("Home is always a segment");
        let first = visual
            .debug_bounds("tab-list-9101")
            .expect("the first fixture list is a segment");
        let second = visual
            .debug_bounds("tab-list-9102")
            .expect("the second fixture list is a segment");
        assert!(first.left() >= home.right(), "{home:?} then {first:?}");
        assert!(second.left() >= first.right(), "{first:?} then {second:?}");

        // 1 段上でまた #182: ツールバーの行の `gap` はタイトルを溝に密着させた
        // ままにするので､`List@usadamasa` と読めてしまう｡
        let title = visual
            .debug_bounds("header-title")
            .expect("the title is always shown");
        assert!(
            title.left() > second.right(),
            "the title runs into the picker: picker ends at {:?}, title starts at {:?}",
            second.right(),
            title.left()
        );
    }

    /// 実測した失敗 (2026-08-24): 560px でリストのタブが 11 個あるウィンドウは､
    /// ツールバーの "Sign in with X" を右端の外へ押し出したうえ､本文の助言は
    /// それをクリックしろというものだけだった｡X が更新を拒否したばかりの
    /// セッションは､画面からは回復できなかったことになる｡"Not signed in" の
    /// 文が居るのは本文なので､ボタンもそこに居る — ツールバーが何をして
    /// いようと手が届く｡
    #[gpui::test]
    fn a_signed_out_window_offers_sign_in_in_the_body(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline) = drawn(cx, fixture_with(&["1"], &[]));
        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                view.state = TimelineState::NotAuthenticated;
                cx.notify();
            });
        });
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let pill = visual
            .debug_bounds("sign-in-body")
            .expect("a signed-out body must carry its own sign-in button");
        let viewport = visual.update(|window, _| window.viewport_size());
        assert!(
            pill.right() <= viewport.width && pill.left() >= gpui::Pixels::ZERO,
            "the body's sign-in button is off-screen: {pill:?} in {viewport:?}"
        );
    }

    /// 同じ失敗のもう半分: ツールバーは 1 本の flex 行で､タブが十分に多いと､
    /// その右にあるものすべて — タイトル､サインイン / リロードのコントロール —
    /// を縮めるのではなくウィンドウの外へ押し出した｡タブ自身のあふれ方の見せ方は
    /// #192 の担当で､ここが押さえるのは､アカウントがいくつリストを持って
    /// いようと行の右端のコントロールがウィンドウから出ないことだけだ｡
    #[gpui::test]
    fn the_toolbar_action_stays_on_screen_under_a_dozen_tabs(cx: &mut gpui::TestAppContext) {
        let lists: [(&str, &str); 12] = [
            ("9101", "The Illustrated Compendium"),
            ("9102", "Watercolour and gouache people"),
            ("9103", "Neighbourhood announcements"),
            ("9104", "International correspondents"),
            ("9105", "Machine fabrication weekly"),
            ("9106", "Dollhouse district news"),
            ("9107", "Secondary creation circle"),
            ("9108", "Drinking club coordination"),
            ("9109", "Probe accounts for twigpui"),
            ("9110", "Long-form essay writers"),
            ("9111", "Camera gear enthusiasts"),
            ("9112", "Weekend hiking companions"),
        ];
        let (mut visual, _timeline) = drawn(cx, fixture_with_lists(&["1"], &lists));

        let action = visual
            .debug_bounds("primary-action")
            .expect("the toolbar always carries its action control");
        let viewport = visual.update(|window, _| window.viewport_size());
        assert!(
            action.right() <= viewport.width,
            "the toolbar's action control is pushed off-screen by the tabs: \
             {action:?} in {viewport:?}"
        );
    }

    /// #164: fixture のウィンドウには client が無いので､ツールバーの中で
    /// リクエストを使う唯一のボタンを出してはならない｡
    #[gpui::test]
    fn a_fixture_window_offers_no_list_fetch(cx: &mut gpui::TestAppContext) {
        let (mut visual, _timeline) = drawn(cx, fixture_with_lists(&["1"], &[("9101", "Rust")]));
        assert!(
            visual.debug_bounds("load-lists").is_none(),
            "a window with no client must not offer to fetch lists"
        );
    }

    /// #164: client を持つ同じウィンドウはそれを*出す*｡picker の右､ツール
    /// バーの内側に配置される｡
    ///
    /// このボタンが実際に描かれる唯一の場所はサインイン済みの live ウィンドウ
    /// だが､それはどのテストにも構築できない — そこでここでは fixture の
    /// ウィンドウに client を渡し (トークンの文字列｡`XClient::new` は何も
    /// 送らない)､描き直す｡これが無いと､ボタンの最初の描画がユーザーの最初の
    /// 起動になる｡「ボタンが無い」と報告されたのはそういう経緯だ｡
    #[gpui::test]
    fn a_signed_in_window_offers_the_list_fetch_beside_the_picker(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline) = drawn(cx, fixture_with_lists(&["1"], &[("9101", "Rust")]));
        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                view.client = Some(crate::x_api::XClient::new("token".to_string()));
                cx.notify();
            });
        });
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let button = visual
            .debug_bounds("load-lists")
            .expect("a window with a client and a known user offers the fetch");
        let last_segment = visual
            .debug_bounds("tab-list-9101")
            .expect("the fixture list is a segment");
        assert!(
            button.left() >= last_segment.right(),
            "the button sits after the picker: {last_segment:?} then {button:?}"
        );
        // live のウィンドウは `Load lists (1 request)@usadamasa` と表示して
        // いた — #182 がステータスバーで見つけたのと同じ gap 0 だ｡
        let title = visual
            .debug_bounds("header-title")
            .expect("the title is always shown");
        assert!(
            title.left() > button.right(),
            "the title runs into the button: button ends at {:?}, title starts at {:?}",
            button.right(),
            title.left()
        );
        assert!(
            button.size.width > gpui::px(0.0) && button.size.height > gpui::px(0.0),
            "the button has a size: {button:?}"
        );
    }

    /// #164 の 2 つ目の完了条件: すでにキャッシュ済みの timeline どうしの
    /// 切り替えでは何も送らない｡
    ///
    /// リストが 2 つ､どちらも前もってキャッシュしてある｡ウィンドウは一方から
    /// もう一方へ､そしてまた戻るようにクリックされ､各クリックの後にはきっかり
    /// キャッシュ済みの行を表示する｡client はまだ無く `last_reload_at` も
    /// 動いていないので､何も出ていないし試みられてもいない —
    /// `showing_new_posts_sends_nothing` が頼るのと同じ証拠だ｡
    #[gpui::test]
    fn switching_between_cached_sources_sends_nothing(cx: &mut gpui::TestAppContext) {
        cache_list("9111", &["12", "11"]);
        cache_list("9112", &["22", "21"]);
        let (mut visual, timeline) = drawn(
            cx,
            fixture_with_lists(&["1"], &[("9111", "first"), ("9112", "second")]),
        );

        for (segment, expected) in [
            ("tab-list-9111", ["12", "11"]),
            ("tab-list-9112", ["22", "21"]),
            ("tab-list-9111", ["12", "11"]),
        ] {
            let bounds = visual
                .debug_bounds(segment)
                .expect("the segment has to be laid out before a click can reach it");
            visual.simulate_click(bounds.center(), gpui::Modifiers::none());
            cx.run_until_parked();

            cx.update(|cx| {
                timeline.update(cx, |view, _cx| {
                    assert_eq!(shown_ids(view), expected, "after clicking {segment}");
                    assert!(view.client.is_none());
                    assert!(
                        view.last_reload_at.is_none(),
                        "a switch to a cached list must not count as a fetch"
                    );
                });
            });
            // 次の参照が､前のフレームが置いた場所ではなく今ある場所で区画を
            // 拾えるように描き直す｡
            visual.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }
    }

    /// #164: クリックは区画の上に落ち､切り替えは前の取得元に属していたものを
    /// リセットする — ここでは poll のバッファで､そうしなければ古いリストの
    /// post を新しいリストに被せて出してしまう｡
    #[gpui::test]
    fn clicking_a_segment_switches_the_source_and_drops_the_old_buffer(
        cx: &mut gpui::TestAppContext,
    ) {
        cache_list("9121", &["32", "31"]);
        let mut fixture = fixture_with_lists(&["2", "1"], &[("9121", "Rust")]);
        fixture.pending = vec![item_with("3", "someone", None)];
        let (mut visual, timeline) = drawn(cx, fixture);

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(view.source, crate::cache::TimelineSource::Home);
                assert!(view.pending.is_some(), "the fixture's buffer is waiting");
            });
        });

        let segment = visual
            .debug_bounds("tab-list-9121")
            .expect("the segment has to be laid out before a click can reach it");
        visual.simulate_click(segment.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(
                    view.source,
                    crate::cache::TimelineSource::List("9121".to_string())
                );
                assert_eq!(shown_ids(view), ["32", "31"]);
                assert!(
                    view.pending.is_none(),
                    "a buffer polled against Home must not be offered over a list"
                );
                assert!(view.next_page_token.is_none());
            });
        });
    }

    /// #164: 選択はウィンドウより長生きする — 区画がクリックされたその瞬間に
    /// ディスク上にあり､クラッシュが飛ばしうる後の保存点ではない｡
    ///
    /// *live* のウィンドウを､自分のディレクトリの下で使う: fixture はこの
    /// ファイルを決して書かない (この次のテスト) し､smoke 用のディレクトリは
    /// 他のすべてのウィンドウのテストと共有しているので､そこにあるファイルを
    /// assert すると､最後にクリックしたテストと競合してしまう｡
    #[gpui::test]
    fn a_switch_is_remembered_on_disk_at_once(cx: &mut gpui::TestAppContext) {
        let home = std::env::temp_dir().join("twigpui-smoke-live-switch");
        let _ = std::fs::remove_dir_all(&home);
        let home_str = home.display().to_string();
        let paths =
            crate::paths::Paths::from_vars(move |key| (key == "HOME").then(|| home_str.clone()))
                .unwrap();
        paths.ensure_dirs().unwrap();
        crate::cache::save_owned_lists(
            &paths,
            &[crate::x_api::ListSummary {
                id: "9131".to_string(),
                name: "Rust".to_string(),
            }],
            0,
        )
        .unwrap();

        // この HOME の下に token は無いので､起動は client を持たない
        // `NotAuthenticated` へ落ち着く — 起動のゲートは越えていて､なお
        // この後のキャッシュミスに何も使えない｡
        let (window, timeline) = window_with(cx, smoke_config(), paths.clone(), Startup::Live);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert!(matches!(view.state, TimelineState::NotAuthenticated));
                assert!(view.client.is_none());
            });
        });

        let segment = visual
            .debug_bounds("tab-list-9131")
            .expect("the segment has to be laid out before a click can reach it");
        visual.simulate_click(segment.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        let remembered = super::list_picker::load_selection(&paths.selection_file());
        assert_eq!(
            remembered.selected,
            Some(super::list_picker::Selection::List {
                id: "9131".to_string()
            })
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// #164: fixture のセグメントは存在しない list を名指すので､そこへの
    /// クリックはファイルを 1 つも残してはならない — さもないと次の live の
    /// 起動が､読めない list を開き､それが分かるまでのリロードを支払うことに
    /// なる｡
    #[gpui::test]
    fn a_fixture_switch_leaves_no_selection_behind(cx: &mut gpui::TestAppContext) {
        cache_list("9151", &["51"]);
        let selection_file = smoke_paths().selection_file();
        let _ = std::fs::remove_file(&selection_file);
        let (mut visual, timeline) = drawn(cx, fixture_with_lists(&["1"], &[("9151", "Rust")]));

        let segment = visual
            .debug_bounds("tab-list-9151")
            .expect("the segment has to be laid out before a click can reach it");
        visual.simulate_click(segment.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(shown_ids(view), ["51"], "the switch itself still happens");
            });
        });
        assert!(
            !selection_file.exists(),
            "a fixture window wrote {}",
            selection_file.display()
        );
    }

    /// #164: すでに持ち上がっているセグメントをクリックしても何も起きない —
    /// 後の timeline は､単に読み込まれたままなのではなく同一だ｡
    #[gpui::test]
    fn clicking_the_showing_segment_changes_nothing(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline) =
            drawn(cx, fixture_with_lists(&["2", "1"], &[("9141", "Rust")]));

        let home = visual
            .debug_bounds("tab-home")
            .expect("Home is always a segment");
        visual.simulate_click(home.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(view.source, crate::cache::TimelineSource::Home);
                assert_eq!(shown_ids(view), ["2", "1"]);
            });
        });
    }

    /// sync が止まっているあいだ､ダイアログは支払う道を差し出さない
    /// (#174, #205)｡
    ///
    /// 整頓ではなく金のためのガード｡確認を押すとフォロー一覧と list の
    /// メンバーシップを両方まるごと読む｡fixture のウィンドウは
    /// `SyncOff::NotSignedIn` で止まっているので､そこに "Sync" を置くと
    /// 資格情報を持たないウィンドウに課金のボタンが載る｡
    ///
    /// #174 は「入口が起動待ちにならない」ことで守っていた｡#205 で入口は
    /// どの状態からでも開くので､ガードは開いた先へ移った｡だから見るのは
    /// `sync-confirm` が灰色であることではなく､*存在しない* こと｡
    #[gpui::test]
    fn a_stopped_sync_opens_a_dialog_that_offers_no_way_to_spend(cx: &mut gpui::TestAppContext) {
        let (window, timeline) = fixture_window(cx, fixture_with(&["1"], &[]));

        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                assert!(matches!(view.sync_status, SyncStatus::Off(_)));
                view.ask_to_sync(cx);
            });
        });

        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert!(
            visual.debug_bounds("sync-dialog").is_some(),
            "the dialog has to open, or the gate has nowhere to be explained"
        );
        assert!(
            visual.debug_bounds("sync-confirm").is_none(),
            "a window with no credential must not offer to spend a sync"
        );
        assert!(
            visual.debug_bounds("sync-cancel").is_some(),
            "the only way out of a dialog must always be there"
        );
    }

    /// #205: sync の行は footer のちょうど真上に座る｡
    ///
    /// issue の「下から 2 段目」を読み返せる形にしたもの｡接していることを
    /// 見るので､順番が入れ替われば離れ､どちらかが消えれば `debug_bounds` が
    /// `None` を返す｡
    #[gpui::test]
    fn the_sync_row_sits_directly_above_the_footer(cx: &mut gpui::TestAppContext) {
        let (window, _timeline) = fixture_window(cx, fixture_with_sync(&["2", "1"], 7));

        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let row = visual
            .debug_bounds("sync-row")
            .expect("a sync with work left has to show its row");
        let bar = visual
            .debug_bounds("status-bar")
            .expect("the footer is always shown");

        assert_eq!(
            row.bottom(),
            bar.top(),
            "the sync row has to sit on the footer, not float above it: \
             row ends at {:?}, footer starts at {:?}",
            row.bottom(),
            bar.top()
        );
    }

    /// #205: フェードの途中でも行の高さは変わらない｡
    ///
    /// 「中間状態で timeline を跳ねさせない」の検査｡高さも補間する実装なら
    /// フレームごとに timeline が押し上げられる｡`sync_fade` を直接歩かせるのは
    /// タイマーを待たずに中間の段を描かせるためで､段そのものは `sync_row` の
    /// 純粋関数のテストが押さえる｡
    #[gpui::test]
    fn a_fading_row_keeps_its_height_so_the_timeline_does_not_bounce(
        cx: &mut gpui::TestAppContext,
    ) {
        let (window, timeline) = fixture_window(cx, fixture_with_sync(&["2", "1"], 7));

        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let settled = visual
            .debug_bounds("sync-row")
            .expect("a sync with work left has to show its row")
            .size
            .height;

        for step in 1..4_u8 {
            cx.update(|cx| {
                timeline.update(cx, |view, cx| {
                    view.sync_fade = Fade::Falling(step);
                    cx.notify();
                });
            });
            visual.update(|window, cx| {
                let _ = window.draw(cx);
            });
            let mid = visual
                .debug_bounds("sync-row")
                .expect("a row that is still fading still occupies its place")
                .size
                .height;
            assert_eq!(
                mid, settled,
                "step {step} of the fade changed the row height, \
                 which pushes the timeline under the reader"
            );
        }
    }

    /// #205: 入口はダイアログを開き､cancel は何も支払わずに閉じる｡
    ///
    /// `simulate_click` なので hit test を通る (#184 の `Addressable`)｡
    /// 金のための assert は cancel のあとに status が動いていないことで､
    /// `start_sync` は必ず status を書き換える (資格情報の無いこのウィンドウ
    /// では gate へ)｡
    ///
    /// 閉じたことは `debug_bounds` では見られない｡gpui 0.2.2 の
    /// `Frame::clear` はあの map を消さないので､一度描かれた名前は最後の
    /// bounds を返し続ける｡言えるのは「一度も描かれていない」までなので､
    /// 閉じたことは `pending_sync` で見る｡
    ///
    /// 下の `a_stopped_sync_opens_a_dialog_that_offers_no_way_to_spend` が
    /// `sync-confirm` の `None` を見られるのは､あちらのウィンドウがそのボタンを
    /// 一度も描いていないから｡
    #[gpui::test]
    fn the_entry_opens_the_dialog_and_cancel_closes_it_without_spending(
        cx: &mut gpui::TestAppContext,
    ) {
        let (window, timeline) = fixture_window(cx, fixture_with_sync(&["2", "1"], 7));

        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let entry = visual
            .debug_bounds("sync-open")
            .expect("the footer always carries the way in");
        visual.simulate_click(entry.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert!(
            visual.debug_bounds("sync-dialog").is_some(),
            "pressing the entry has to open the dialog"
        );

        let cancel = visual
            .debug_bounds("sync-cancel")
            .expect("an open dialog always offers the way out");
        visual.simulate_click(cancel.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert!(
                    !view.pending_sync,
                    "cancel has to clear the flag as well as the pixels"
                );
                assert!(
                    matches!(view.sync_status, SyncStatus::Idle { pending: 7, .. }),
                    "cancel must not have started anything, got {:?}",
                    view.sync_status
                );
            });
        });
    }

    /// #205: confirm だけが手動 sync の経路へ入る｡
    ///
    /// このウィンドウには `client` が無いのでリクエストは飛ばない｡
    /// `start_sync` の gate が先に止め､status を `SyncOff::NotSignedIn` へ
    /// 置く｡そこを見る｡cancel が動かさなかった status を confirm は動かす
    /// ので､2 つのボタンが別々の経路だと言える｡
    #[gpui::test]
    fn confirming_enters_the_manual_sync_path(cx: &mut gpui::TestAppContext) {
        let (window, timeline) = sync_fixture_window(cx, fixture_with_sync(&["2", "1"], 7));

        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let entry = visual
            .debug_bounds("sync-open")
            .expect("the footer always carries the way in");
        visual.simulate_click(entry.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let confirm = visual
            .debug_bounds("sync-confirm")
            .expect("an idle sync with work left is offered");
        visual.simulate_click(confirm.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert!(!view.pending_sync, "confirm has to close the dialog");
                assert!(
                    matches!(view.sync_status, SyncStatus::Off(SyncOff::NotSignedIn)),
                    "confirm has to reach the gate inside start_sync, got {:?}",
                    view.sync_status
                );
            });
        });
    }

    #[gpui::test]
    fn the_window_root_renders_without_panicking(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        let home = std::env::temp_dir().join("twigpui-smoke");
        let home = home.display().to_string();
        let paths =
            crate::paths::Paths::from_vars(move |key| (key == "HOME").then(|| home.clone()))
                .unwrap();
        let config = crate::config::Config {
            oauth_client_id: "client-123".to_string(),
            target_username: "XDevelopers".to_string(),
            max_results: 20,
            min_fetch_interval_seconds: 60,
            theme: crate::theme::ThemeMode::Light,
            log_level: crate::log::Level::default(),
            request_price: None,
            daily_request_budget: None,
            list_id: None,
            // smoke テストでは off: これらはウィンドウを描画するもので､
            // 金のかかるバックグラウンドのループは検査の対象ではない｡
            auto_sync_list: false,
            sync_interval_seconds: 21_600,
            sync_prune_limit_percent: 10,
            sync_writes_per_batch: 2,
            // 同じ理由で off (#21)｡
            auto_refresh: false,
            auto_refresh_interval_seconds: 300,
            // `smoke_config` と同じ理由で off (#22)｡
            follow_new_posts: false,
        };

        cx.update(gpui_component::init);
        // #58: `KeyBinding::new` はパースできないキーストロークで panic する
        // ので､ここで走らせておくと､割り当ての打ち間違いがユーザーの最初の
        // 起動でのクラッシュではなく､失敗するテストになる｡
        cx.update(crate::menu::init);

        // 下でコンポーザーの input をフォーカスできるように保持しておく:
        // `add_window` が返すのは*ルート*のビューへのハンドルで､ここでは
        // 意図的に中の timeline ではなく `Root` のラッパーになっている｡
        let timeline_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
        let window = {
            let slot = timeline_slot.clone();
            cx.add_window(move |window, cx| {
                let timeline = cx.new(|cx| {
                    let mut view =
                        super::TimelineView::new(config, paths, Startup::Live, window, cx);
                    // #55 はこのフラグの裏に隠れていた: コンポーザー --
                    // ウィンドウの root を求めて遡る唯一のウィジェット -- は
                    // OAuth のセッションでしか描画されないので､bearer token での
                    // 実行はすべてこれを見逃していた｡
                    view.signed_in_with_oauth = true;
                    // サインイン中の id が解決されることが､行ごとのアクション
                    // ボタン (`offers_repost`, `offers_like`) を解禁するので､
                    // それが無いと下の walk はそれらを丸ごと飛ばす｡
                    view.home_user_id = Some("2244994945".to_string());
                    view
                });
                *slot.borrow_mut() = Some(timeline.clone());
                // これが無かったせいでアプリが起動時に落ちた､その 1 行 (#55)｡
                gpui_component::Root::new(timeline, window, cx)
            })
        };
        let timeline = timeline_slot.borrow().clone().unwrap();

        // composer がウィンドウの root を辿るのは input にフォーカスが当たって
        // からで､アプリはユーザーがクリックした時点でそれを行う｡
        cx.update_window(window.into(), |_, window, cx| {
            timeline.update(cx, |view, cx| {
                view.compose_input
                    .update(cx, |input, cx| input.focus(window, cx));
            });
        })
        .unwrap();

        cx.run_until_parked();

        // 本文に描く post を与える｡空の timeline は `post_row` を 1 つも描か
        // ないので､これが無ければ下の走査はバナーにも quote のカードにも操作
        // ボタンにも #67 の metrics の行にも届かない -- まさに #59 が塞ぐため
        // に書かれた種類の死角だ｡起動タスクが落ち着いた *後* でなければ
        // ならない: そのタスクは最後に `state` 自身を代入して終わり (ここでは
        // 資格情報が無いので `NotAuthenticated`)､さもなければこれを消す｡
        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                view.state = TimelineState::Loaded(vec![TimelineItem {
                    id: "1700000000000000001".to_string(),
                    text: "a rendered post".to_string(),
                    created_at: Some("2026-08-16T09:00:00.000Z".to_string()),
                    author_name: "Developers".to_string(),
                    author_username: "XDevelopers".to_string(),
                    reposted_by: None,
                    quoted: None,
                    replied_to: None,
                    metrics: Some(PostMetrics {
                        replies: 12,
                        reposts: 34,
                        likes: 5600,
                    }),
                    links: vec![PostLink {
                        url: "https://example.com/an-article".to_string(),
                        label: "example.com/an-article".to_string(),
                    }],
                    author_avatar_url: Some(
                        "https://pbs.twimg.com/profile_images/1/a_normal.jpg".to_string(),
                    ),
                    original_post_id: None,
                    media: vec![PostMedia {
                        url: "https://pbs.twimg.com/media/one.jpg".to_string(),
                        kind: Some("photo".to_string()),
                        width: Some(1200),
                        height: Some(675),
                        alt_text: Some("a rendered image".to_string()),
                    }],
                }]);
                cx.notify();
            });
        });

        // ウィンドウを開くだけでは足りない: まだ何も描画されていないし､#55 が
        // 扱う panic は element のツリーが走査されて初めて起きる｡
        for _ in 0..2 {
            cx.update_window(window.into(), |_, window, cx| {
                let _ = window.draw(cx);
            })
            .unwrap();
        }
    }
}
