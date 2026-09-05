use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    AnyElement, Context, Div, Entity, FocusHandle, Focusable as _, FontWeight, ObjectFit,
    ScrollHandle, SharedString, Stateful, Subscription, Task, Window, div, img, prelude::*, px,
    rgb, rgba, svg,
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
mod action_row;
mod auto_refresh;
mod chrome;
mod composer;
mod countdown;
mod fade;
// #188: `main` がキーバインドを登録するので､ここだけ crate へ開く｡
pub(crate) mod image_viewer;
mod lane;
mod layout;
mod list_sync;
mod post_row;
mod reload_policy;
mod render;
mod scroll;
pub(crate) mod source_picker;
mod source_picker_menu;
mod startup;
mod state;
mod sync_row;
mod tasks;
mod toast;

// `ui` の兄弟ではなく子モジュールにする (#126): 子モジュールは親の
// プライベート項目を参照できるので､`TimelineState`､`ReloadNotice`､
// `TimelineView` 自体は `pub(crate)` へ広げずに `ui` の内側へ留まる｡
// 隣のファイルから届かせるためだけに広げると､「クレート内のどこからでも
// 触ってよい」という意味になり､それはファイルを分割した目的と
// 正反対になる｡
use auto_refresh::{FollowMode, Pending, Situation, pending_after_poll};
use fade::Fade;
use list_sync::{SyncOff, SyncStatus, SyncTrigger};
use reload_policy::{
    CooldownTick, at_the_post_cap, cooldown_label, cooldown_tick, newly_arrived, offers_load_older,
    partial_failure_label, preserved_scroll_target, reload_failure_outcome, reload_gate,
    reload_outcome_label, reload_start_state,
};
use render::Addressable as _;
use render::{
    AVATAR_SIZE, MAX_RENDERED_MEDIA, MEDIA_GAP, MediaArrangement, author_link, avatar_placeholder,
    byline, compose_error_message, format_timestamp, header_title_element, icon_button, like_row,
    link_row, media_arrangement, media_aspect, media_badge, media_column_sizes, media_row_sizes,
    notice, offers_delete, offers_like, offers_quote, offers_reauthorize, offers_reply,
    offers_repost, open_post_link, quote_card, quote_row, reload_notice_banner,
    render_thread_chain, reply_banner_label, reply_row, reply_target_label, repost_banner_label,
    repost_row, session_notice_banner, sign_in_pill, thread_action_label, thread_toggle_row,
    toggle_count_color, usage_color, usage_label, with_count,
};
use render::{RowCounts, row_counts};
pub(crate) use startup::Startup;
use state::{
    Cooldown, PrimaryAction, ReloadNotice, ReloadTrigger, StartOutcome, ThreadFetchState,
    TimelineState,
};
use toast::Toast;

use crate::menu::{
    BlurComposer, CloseWindow, FocusComposer, KEY_CONTEXT, Minimize, Reload, ScrollToTop,
    ShowAbout, ShowNewPosts, SyncList, ToggleFloatOnTop, ToggleFollowNewPosts, ToggleTranslucent,
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
use crate::window_state;
use crate::x_api::{
    Denial, Denied, Draft, PostLink, PostMedia, PostMetrics, QuotedPost, RepliedTo, TimelineItem,
    XClient, action_post_id,
};

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
    /// どの timeline の集合がウィンドウを埋めるか (#161, #43): [`Self::new`] の
    /// 中で [`source_picker::initial_sources`] が決め､再代入するのは
    /// [`Self::toggle_source`] (#164, #43) だけで､そこではウィンドウではなく
    /// この集合に属する下記のものもすべてリセットする｡非空 invariant: 最後の
    /// 1 つは外せない｡
    ///
    /// timeline に触る経路はすべてこれを読む: [`Self::start`]､
    /// [`Self::reload`]､[`Self::load_older`]､[`Self::confirm_delete`] は
    /// どれもこれを取り､読む cache ファイル群､リクエストを使う endpoint 群､
    /// delete が書き換えるファイル群が同じ source 集合になるようにしている｡
    sources: Vec<cache::TimelineSource>,
    /// 投稿ごとの出自 (#43): post id → 表示順で最初に載っていた source｡
    /// `lane::load_composite_timeline` が合成のたびに作り直す表示専用の
    /// 派生値で､削除の真実の情報源にはしない — [`Self::confirm_delete`] は
    /// これを見ず `sources` を全部回す｡`sources.len() == 1` のときは
    /// 描画側が出自を出さないので中身を読まない｡
    item_provenance: HashMap<String, cache::TimelineSource>,
    /// source picker のドロップダウンが開いているかどうか (#43, #192)｡
    source_picker_open: source_picker::SourcePickerVisibility,
    /// picker が名前を挙げられる list (#164)｡cache か直近の fetch から来る｡
    /// fetch ボタンが一度押されるまでは空｡
    owned_lists: Vec<crate::x_api::ListSummary>,
    /// 進行中の `owned_lists` の fetch があればそれ; `fetch` と同じ drop で
    /// 取り消す契約であり､二度目のクリックを止めるものでもある｡
    lists_fetch: Option<Task<()>>,
    /// 切り替えを覚えておく場所 ([`Paths::selection_file`])｡fixture の
    /// ウィンドウでは `None` — [`source_picker::saved_selection_for`] を見よ｡
    selection_file: Option<PathBuf>,
    /// ウィンドウを置いた場所を覚えておく場所 ([`Paths::window_state_file`],
    /// #211)｡`selection_file` と同じく fixture のウィンドウでは `None` で､
    /// 読み取り側 ([`crate::window_state::initial_bounds`]) も同じように
    /// 塞いである｡
    window_state_file: Option<PathBuf>,
    /// そのファイルに書く中身の写し (#211, #267)｡矩形とメニューのトグルが
    /// 同じファイルに居るので､どちらを書くときも全部を書く — 矩形だけを
    /// 組んで書けば､ウィンドウを動かすたびにトグルが切れる｡
    window_state: window_state::WindowState,
    /// ウィンドウの矩形が変わったことを知らせる購読 (#211)｡resize でも
    /// 移動でも発火する｡drop すると通知が止まるので､view と同じだけ生きる
    /// 必要がある｡名前の `_` は読まれずに保持されるものの印｡
    _window_bounds_subscription: Subscription,
    /// フォーカスの出入りを知らせる購読 (#267)｡透過が入っていれば描き直す｡
    _window_activation_subscription: Subscription,
    /// 矩形を書くまでの間を空けるタイマー (#211)｡ドラッグの最中は通知が
    /// 連続で来るので､新しい task を入れて前のものを落とし､手が止まって
    /// から 1 度だけ書く｡
    window_state_save: Option<Task<()>>,
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
    /// Posts の resource 数の合計 (#162､#18 の後継) — `usage::posts_totals`
    /// が返すもの｡header に出る — [`Self::refresh_usage`] を見よ｡最初の
    /// refresh が終わるまでゼロだが､これはプレースホルダではなく正直な
    /// 「まだ何も観測していない」である｡空の `usage.json` を読んでも
    /// `usage::Totals::default()` とまったく同じになるからだ｡
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
    /// auto-refresh のループが直近の起床で見たもの (#214)｡footer はここから
    /// 次のポーリングの期限を数え直す — [`countdown::refresh_label`] を見よ｡
    ///
    /// ループが起床ごとに写し､ループが無いとき (off､サインイン前､#239 で
    /// 止まった後) は `None`｡`last_reload_at` だけは写しではなく view の
    /// 値を読む: 手動 reload はループが次に起きるより先に期限を動かす｡
    refresh_situation: Option<Situation>,
    /// footer のカウントダウンを刻む (#214) — [`countdown`] を見よ｡
    /// `cooldown_ticker` と同じ契約で､数えるものが無くなれば自分で終わる｡
    countdown_ticker: Option<Task<()>>,
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
    /// 直近のダウンロードが失敗した media の URL (#188)｡`media_paths` に
    /// 無いことは「まだ取っていない」と「取れなかった」の両方を意味しうる
    /// ので､クリックした先の viewer が「開けない」を言うにはこちらが要る｡
    /// `refresh_media` が取れたら remove する｡
    media_failed: HashSet<String>,
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::theme;

    use super::auto_refresh::{Poll, Situation};
    use super::countdown;
    use super::image_viewer::ImageViewer;
    use super::lane::provenance_label;
    use super::post_row::{MediaClickTarget, media_click_target};
    use super::reload_policy::{
        newly_arrived, partial_failure_label, preserved_scroll_target, reload_cooldown,
        reload_outcome_label,
    };
    use super::render::actions::{like_action_label, repost_action_label};
    use super::render::frame::header_title;
    use super::render::offers::is_own_post;
    use super::render::post::{avatar_initial, post_permalink, profile_url};
    use super::{
        ComposeStatus, Cooldown, CooldownTick, Denial, Denied, Fade, Fixture, MediaArrangement,
        PostLink, PostMedia, PostMetrics, ReloadNotice, ReloadTrigger, RepliedTo, RowCounts,
        Startup, SyncOff, SyncStatus, Theme, ThreadFetchState, TimelineItem, TimelineState,
        ToggleState, action_post_id, at_the_post_cap, byline, compose_error_message,
        cooldown_label, cooldown_tick, format_timestamp, media_arrangement, media_aspect,
        media_badge, media_column_sizes, media_row_sizes, offers_delete, offers_like,
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

    /// 3 桁に達した単位は小数を落として 5 文字で頭打ちにする (X の UI と
    /// 同じ)｡`theme::COUNT_WIDTH` は 5 文字ぶんしか無いので､ここが 6 文字
    /// を返すとその行だけ列が押されて崩れる｡
    #[test]
    fn a_count_never_exceeds_five_characters() {
        let counts = row_counts(Some(&PostMetrics {
            replies: 123_456,
            reposts: 999_999,
            likes: 123_000_000,
        }));
        assert_eq!(counts.replies.as_deref(), Some("123K"));
        assert_eq!(counts.reposts.as_deref(), Some("999K"));
        assert_eq!(counts.likes.as_deref(), Some("123M"));
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
        // #68 が明言している｡X は自分の post への like を受け入れる｡
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

    // --- #65 / #256: 添付メディアの寸法 ---

    /// [`media_row_sizes`] の結果を素の f32 の (幅, 高さ) に直す｡`Pixels`
    /// の四則は `arithmetic_side_effects` に弾かれる (`rust-lint-gauntlet`)｡
    fn row_sizes(aspects: &[f32]) -> Vec<(f32, f32)> {
        media_row_sizes(aspects)
            .into_iter()
            .map(|size| (f32::from(size.width), f32::from(size.height)))
            .collect()
    }

    /// 寸法だけを持つ写真｡
    fn photo(width: Option<u32>, height: Option<u32>) -> PostMedia {
        PostMedia {
            url: "media/x.png".to_string(),
            kind: Some("photo".to_string()),
            width,
            height,
            alt_text: None,
        }
    }

    /// 半 px 以内なら同じ寸法とみなす (`float_cmp`)｡
    fn about(actual: f32, expected: f32) -> bool {
        (actual - expected).abs() < 0.5
    }

    #[test]
    fn a_small_landscape_image_grows_to_the_max_width() {
        // 128×72 の写真は幅の最大値までふくらみ､縦横比は保つ｡
        let sizes = row_sizes(&[media_aspect(&photo(Some(128), Some(72)))]);
        let (width, height) = sizes.first().copied().unwrap_or_default();
        assert!(
            about(width, f32::from(super::render::post::MEDIA_MAX_WIDTH)),
            "grows to the max width: {width}"
        );
        assert!(about(height, width * 72.0 / 128.0), "keeps 16:9: {height}");
    }

    #[test]
    fn a_portrait_image_stops_at_the_max_height() {
        // 縦長は高さが先に上限へ当たり､幅はその高さから出る｡
        let sizes = row_sizes(&[media_aspect(&photo(Some(180), Some(320)))]);
        let (width, height) = sizes.first().copied().unwrap_or_default();
        assert!(
            about(height, f32::from(super::render::post::MEDIA_MAX_HEIGHT)),
            "stops at the max height: {height}"
        );
        assert!(about(width, height * 180.0 / 320.0), "keeps 9:16: {width}");
    }

    #[test]
    fn a_square_image_stops_at_the_max_height() {
        let sizes = row_sizes(&[1.0]);
        let (width, height) = sizes.first().copied().unwrap_or_default();
        assert!(
            about(height, f32::from(super::render::post::MEDIA_MAX_HEIGHT)),
            "{height}"
        );
        assert!(about(width, height), "stays square: {width} x {height}");
    }

    #[test]
    fn four_images_share_one_row_at_one_height() {
        // Tumblr の photoset と同じ: 高さを揃え､幅は縦横比に比例し､
        // 隙間込みで幅の最大値をちょうど使う｡
        let aspects = [16.0 / 9.0, 9.0 / 16.0, 16.0 / 9.0, 1.0];
        let sizes = row_sizes(&aspects);
        assert_eq!(sizes.len(), 4, "one size per photo");
        let (_, first_height) = sizes.first().copied().unwrap_or_default();
        for (&aspect, &(width, height)) in aspects.iter().zip(&sizes) {
            assert!(about(height, first_height), "one height: {height}");
            assert!(
                about(width, aspect * height),
                "width follows aspect: {width}"
            );
        }
        let widths: f32 = sizes.iter().map(|&(width, _)| width).sum();
        let gaps = 3.0 * f32::from(super::MEDIA_GAP);
        assert!(
            about(
                widths + gaps,
                f32::from(super::render::post::MEDIA_MAX_WIDTH)
            ),
            "fills the max width with the gaps: {widths} + {gaps}"
        );
    }

    #[test]
    fn a_missing_or_zero_dimension_counts_as_square() {
        assert!(about(media_aspect(&photo(None, Some(10))), 1.0), "no width");
        assert!(
            about(media_aspect(&photo(Some(10), None)), 1.0),
            "no height"
        );
        assert!(
            about(media_aspect(&photo(Some(10), Some(0))), 1.0),
            "zero height"
        );
    }

    #[test]
    fn an_extreme_aspect_is_clamped() {
        // 1 px の高さのバナーで行の高さがゼロにならない｡
        let wide = media_aspect(&photo(Some(10_000), Some(10)));
        let tall = media_aspect(&photo(Some(10), Some(10_000)));
        assert!(wide <= 10.0, "{wide}");
        assert!(tall >= 0.1, "{tall}");
        let (_, height) = row_sizes(&[wide]).first().copied().unwrap_or_default();
        assert!(height >= 1.0, "a banner still has a height: {height}");
    }

    #[test]
    fn no_media_gives_no_sizes() {
        assert!(row_sizes(&[]).is_empty(), "nothing to lay out");
    }

    #[test]
    fn portrait_photos_sit_side_by_side() {
        // 縦長どうしは横に並べる — 縦長は幅が余るので隣が置ける｡
        assert_eq!(media_arrangement(&[0.5, 0.75]), MediaArrangement::Row);
        assert_eq!(
            media_arrangement(&[0.5, 0.6, 1.5]),
            MediaArrangement::Row,
            "portraits outnumber the landscape"
        );
    }

    #[test]
    fn landscape_photos_stack_into_a_column() {
        // 横長どうしは縦に積む — 横に並べると 1 枚 1 枚が小さすぎる｡
        assert_eq!(media_arrangement(&[1.78, 1.78]), MediaArrangement::Column);
        assert_eq!(
            media_arrangement(&[1.78, 1.78, 0.5]),
            MediaArrangement::Column
        );
        assert_eq!(
            media_arrangement(&[1.78, 0.5]),
            MediaArrangement::Column,
            "a tie stacks: a landscape shrinks the most in a row"
        );
        assert_eq!(media_arrangement(&[1.0]), MediaArrangement::Column);
    }

    #[test]
    fn a_stacked_photo_gets_the_lone_photos_size() {
        // Column の各枚は 1 枚のときと同じ式で箱に収まる｡
        let aspects = [16.0 / 9.0, 9.0 / 16.0, 1.0];
        let stacked = media_column_sizes(&aspects);
        assert_eq!(stacked.len(), 3, "one size per photo");
        for (&aspect, size) in aspects.iter().zip(&stacked) {
            let alone = media_row_sizes(&[aspect])
                .first()
                .copied()
                .unwrap_or_default();
            assert!(
                about(f32::from(size.width), f32::from(alone.width))
                    && about(f32::from(size.height), f32::from(alone.height)),
                "a stacked photo keeps the lone size: {size:?} vs {alone:?}"
            );
        }
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

    #[test]
    fn media_click_target_sends_only_photos_to_the_viewer() {
        // 静止画だけが viewer へ行く｡動画と GIF はここでは再生できないので
        // ブラウザのまま (#188)｡
        assert_eq!(media_click_target(Some("photo")), MediaClickTarget::Viewer);
        assert_eq!(media_click_target(Some("video")), MediaClickTarget::Browser);
        assert_eq!(
            media_click_target(Some("animated_gif")),
            MediaClickTarget::Browser
        );
        assert_eq!(media_click_target(None), MediaClickTarget::Browser);
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
        assert!(offers_repost(true, Some("2244994945"), &item));
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
            &TimelineState::Loaded(Vec::new()),
            true
        ));
    }

    #[test]
    fn does_not_offer_load_older_without_a_next_page_token() {
        assert!(!offers_load_older(
            None,
            &TimelineState::Loaded(Vec::new()),
            true
        ));
    }

    /// #43 の天井: 複数 source を同時にページングし合成するアルゴリズムは
    /// 解いていない｡`next_page_token` があっても複数選択中はボタンを出さない｡
    #[test]
    fn does_not_offer_load_older_with_multiple_sources_selected() {
        assert!(!offers_load_older(
            Some("cursor-abc"),
            &TimelineState::Loaded(Vec::new()),
            false
        ));
    }

    // --- 出自表示 (#43) ---

    #[test]
    fn a_single_selection_shows_no_provenance() {
        let mut provenance = HashMap::new();
        provenance.insert(
            "1".to_string(),
            crate::cache::TimelineSource::List("9".to_string()),
        );
        assert_eq!(provenance_label(1, &provenance, &[], "1"), None);
    }

    #[test]
    fn a_post_found_only_in_home_shows_no_provenance() {
        let mut provenance = HashMap::new();
        provenance.insert("1".to_string(), crate::cache::TimelineSource::Home);
        assert_eq!(provenance_label(2, &provenance, &[], "1"), None);
    }

    #[test]
    fn a_list_post_names_its_list_when_multiple_sources_are_selected() {
        let mut provenance = HashMap::new();
        provenance.insert(
            "1".to_string(),
            crate::cache::TimelineSource::List("9".to_string()),
        );
        let owned = [crate::x_api::ListSummary {
            id: "9".to_string(),
            name: "rust".to_string(),
        }];
        assert_eq!(
            provenance_label(2, &provenance, &owned, "1"),
            Some("rust".to_string())
        );
    }

    #[test]
    fn a_list_post_not_in_the_provenance_map_shows_nothing() {
        let provenance = HashMap::new();
        assert_eq!(provenance_label(2, &provenance, &[], "1"), None);
    }

    // --- 部分失敗の文言 (#43) ---

    #[test]
    fn no_failures_leaves_the_outcome_label_unchanged() {
        assert_eq!(
            partial_failure_label("3 new posts.".to_string(), 0, 3),
            "3 new posts."
        );
    }

    #[test]
    fn some_failures_name_how_many_of_how_many() {
        assert_eq!(
            partial_failure_label("3 new posts.".to_string(), 1, 2),
            "3 new posts. (1 of 3 sources failed)"
        );
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

        assert!(!offers_load_older(Some("cursor-abc"), &state, true));
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
            &TimelineState::Loading,
            true
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

    // --- usage_label / usage_color (#162､#18 の後継) ---

    #[test]
    fn usage_label_shows_posts_counts_with_an_estimated_amount() {
        assert_eq!(
            usage_label(4, 40, 2.5),
            "Posts today: 4 (~$10.00) · total: 40"
        );
    }

    #[test]
    fn usage_label_shows_zero_counts_plainly() {
        assert_eq!(
            usage_label(0, 0, 0.005),
            "Posts today: 0 (~$0.00) · total: 0"
        );
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

    // --- offers_repost (#15, #266) ---

    #[test]
    fn offers_repost_once_signed_in_with_a_resolved_home_id_on_someone_elses_post() {
        let item = item_with("1", "alice", None);
        assert!(offers_repost(true, Some("2244994945"), &item));
    }

    #[test]
    fn does_not_offer_repost_without_oauth() {
        let item = item_with("1", "alice", None);
        assert!(!offers_repost(false, Some("2244994945"), &item));
    }

    #[test]
    fn does_not_offer_repost_before_home_user_id_resolves() {
        // #11: repost のエンドポイントは*この*アカウントとして作用し､その id は
        // `/me` しか解決しない — それが無いうちは呼ぶものが無い｡
        let item = item_with("1", "alice", None);
        assert!(!offers_repost(true, None, &item));
    }

    #[test]
    fn offers_repost_on_ones_own_post() {
        // #266: #15 が置いた `is_own_post` のガードは「API が自分の post の
        // repost を拒む」という前提に立っていたが､実測すると 200 が返る｡
        let item = item_with("1", "bob", None);
        assert!(offers_repost(true, Some("2244994945"), &item));
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
        // #16 の設計判断: API は自分の post の引用を拒否しない｡
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
            theme: theme::ThemeMode::Light,
            log_level: crate::log::Level::default(),
            post_resource_price: 0.005,
            daily_post_budget: 1000,
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

    /// このテスト 1 本だけの XDG ディレクトリを指す [`crate::paths::Paths`]｡
    ///
    /// `smoke_paths` は全テストで同じディレクトリを共有するので､何が
    /// *書かれたか* を見るテストは隣のテストが残したものを読んでしまう｡
    /// `name` で分けるのはそのため — プロセス id だけでは同じテストバイナリ
    /// の中で一致する｡
    fn scratch_paths(name: &str) -> crate::paths::Paths {
        let home = std::env::temp_dir().join(format!("twigpui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let home = home.display().to_string();
        crate::paths::Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap()
    }

    // --- #211: ウィンドウを置いた場所を覚える ---

    #[gpui::test]
    fn a_live_window_remembers_the_size_it_was_left_at(cx: &mut gpui::TestAppContext) {
        let paths = scratch_paths("remembers-window-bounds");
        paths.ensure_dirs().unwrap();
        let (window, _timeline) = window_with(cx, smoke_config(), paths.clone(), Startup::Live);

        cx.simulate_window_resize(window.into(), gpui::size(gpui::px(700.0), gpui::px(900.0)));
        // debounce を跨ぐ｡ドラッグの最中は通知が連続で来るので､手が止まって
        // から 1 度だけ書く｡
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();

        let saved = crate::window_state::load(&paths.window_state_file())
            .bounds
            .expect("a live window has to remember where it was left");
        assert_eq!(
            saved.to_bounds().size,
            gpui::size(gpui::px(700.0), gpui::px(900.0)),
            "the remembered rectangle is the one the window ended up with"
        );
    }

    #[gpui::test]
    fn a_fixture_window_remembers_nothing(cx: &mut gpui::TestAppContext) {
        // fixture は定義上毎回同じ画面である (`fixture-visual-check`)｡撮る
        // ために広げたウィンドウが次の live 起動の大きさを決めてはならない｡
        // 読み取り側 (`window_state::initial_bounds`) と同じように塞いである｡
        let paths = scratch_paths("fixture-remembers-nothing");
        paths.ensure_dirs().unwrap();
        let (window, _timeline) = window_with(
            cx,
            smoke_config(),
            paths.clone(),
            Startup::Fixture(Box::new(fixture_with(&["1"], &[]))),
        );

        cx.simulate_window_resize(window.into(), gpui::size(gpui::px(700.0), gpui::px(900.0)));
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();

        assert_eq!(
            crate::window_state::load(&paths.window_state_file()).bounds,
            None,
            "a fixture window must not write the window state file"
        );
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
    pub(super) fn fixture_window(
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
    pub(super) fn fixture_with(shown: &[&str], waiting: &[&str]) -> Fixture {
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
            sources: Vec::new(),
            list_items: std::collections::BTreeMap::new(),
            picker_open: false,
            liked: Vec::new(),
            reposted: Vec::new(),
            translucent: false,
        }
    }

    /// [`fixture_with`] の 1 行版で､件数を持つ (#156)｡`with_count` が件数の
    /// 要素を描くかどうかの分岐 (`Option<&str>`) をテストが越えるには要る —
    /// `item_with` は `metrics: None` で固定なので使えない｡著者は
    /// `fixture_with` と同じ `"someone"`｡
    fn fixture_with_metrics(id: &str) -> Fixture {
        let mut fixture = fixture_with(&[], &[]);
        fixture.items = vec![TimelineItem {
            id: id.to_string(),
            text: String::new(),
            created_at: None,
            author_name: String::new(),
            author_username: "someone".to_string(),
            reposted_by: None,
            quoted: None,
            replied_to: None,
            metrics: Some(PostMetrics {
                replies: 1,
                reposts: 2,
                likes: 3,
            }),
            links: Vec::new(),
            author_avatar_url: None,
            original_post_id: None,
            media: Vec::new(),
        }];
        fixture
    }

    /// #156: fixture が直接 `liked` を言えば `like_state_for` が on を返す —
    /// `toggle::load_all` の永続ファイルに触らずに済む｡`Fixture` は
    /// `deny_unknown_fields` を使っていないので､JSON ではなく構造体
    /// リテラルで書く｡
    #[gpui::test]
    fn a_fixture_can_say_a_post_is_already_liked(cx: &mut gpui::TestAppContext) {
        let fixture = Fixture {
            liked: vec!["1".to_string()],
            ..fixture_with(&["1"], &[])
        };
        let (_window, timeline) = fixture_window(cx, fixture);
        cx.update(|cx| {
            assert!(
                timeline.read(cx).like_state_for("1").is_on(),
                "a fixture-declared liked post must show as on"
            );
        });
    }

    /// [`a_fixture_can_say_a_post_is_already_liked`] の repost 版｡
    #[gpui::test]
    fn a_fixture_can_say_a_post_is_already_reposted(cx: &mut gpui::TestAppContext) {
        let fixture = Fixture {
            reposted: vec!["1".to_string()],
            ..fixture_with(&["1"], &[])
        };
        let (_window, timeline) = fixture_window(cx, fixture);
        cx.update(|cx| {
            assert!(
                timeline.read(cx).repost_state_for("1").is_on(),
                "a fixture-declared reposted post must show as on"
            );
        });
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

    /// #153: composer は使われるまで 1 行に畳まれている｡
    ///
    /// 空でフォーカスも無いときだけ畳む｡クリック (フォーカス) すれば広がり､
    /// 下書きがあればフォーカスを外しても広がったまま — #14 の「下書きを
    /// 失わない」は､下書きが目に入りつづけることも含む｡空に戻して
    /// フォーカスを外せば､また畳まれる｡
    ///
    /// 「1 行」の絶対値は入力ウィジェット (`gpui-component`) の行の高さと
    /// 余白で決まるので直値では書かず､avatar の 32px より低いことだけを
    /// 要求する｡広がった状態はそれより確実に高い (2 行 + 余白)｡
    #[gpui::test]
    fn the_composer_folds_to_one_line_until_it_is_used(cx: &mut gpui::TestAppContext) {
        let (window, timeline) = fixture_window(cx, fixture_with(&["1"], &[]));
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);

        let height_after_draw = |visual: &mut gpui::VisualTestContext| {
            visual.update(|window, cx| {
                let _ = window.draw(cx);
            });
            visual
                .debug_bounds("compose-input")
                .expect("the composer has to be laid out")
                .size
                .height
        };

        // `Pixels` の四則は `arithmetic_side_effects` に弾かれるので f32 で
        // 比べる (`rust-lint-gauntlet`)｡
        let folded = f32::from(height_after_draw(&mut visual));
        assert!(
            folded < 40.0,
            "empty and unfocused, the composer is one line: {folded}px"
        );

        // クリックの代わりにフォーカスを当てる: 広がる条件はフォーカスで､
        // クリックはそれを起こす手段の一つにすぎない｡
        visual.update(|window, cx| {
            timeline.update(cx, |view, cx| {
                view.compose_input
                    .update(cx, |input, cx| input.focus(window, cx));
            });
        });
        let focused = f32::from(height_after_draw(&mut visual));
        assert!(
            focused > folded + 12.0,
            "focused, the composer opens up: {focused}px vs {folded}px"
        );

        // 下書きを残してフォーカスを外す｡
        visual.update(|window, cx| {
            timeline.update(cx, |view, cx| {
                view.compose_input
                    .update(cx, |input, cx| input.set_value("a draft", window, cx));
            });
            window.dispatch_action(Box::new(crate::menu::BlurComposer), cx);
        });
        let drafted = f32::from(height_after_draw(&mut visual));
        assert!(
            drafted > folded + 12.0,
            "a draft keeps the composer open even unfocused: {drafted}px vs {folded}px"
        );
        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(
                    view.compose.text(),
                    "a draft",
                    "folding never touches the draft"
                );
            });
        });

        // 空に戻してフォーカスを外せば畳まれる｡
        visual.update(|window, cx| {
            timeline.update(cx, |view, cx| {
                view.compose_input
                    .update(cx, |input, cx| input.set_value("", window, cx));
            });
            window.dispatch_action(Box::new(crate::menu::BlurComposer), cx);
        });
        let refolded = f32::from(height_after_draw(&mut visual));
        assert!(
            (refolded - folded).abs() < 1.0,
            "emptied and unfocused, it folds again: {refolded}px vs {folded}px"
        );
    }

    /// `name` の要素が置かれた bounds｡置かれていなければ panic — 「無い」を
    /// 確かめるテストはこれを使わない｡
    pub(super) fn laid_out(
        visual: &mut gpui::VisualTestContext,
        name: &'static str,
    ) -> gpui::Bounds<gpui::Pixels> {
        visual
            .debug_bounds(name)
            .unwrap_or_else(|| panic!("{name} has to be laid out"))
    }

    /// `(url, 幅, 高さ)` の写真を添付に持つ [`item_with`]｡
    pub(super) fn item_with_media(id: &str, photos: &[(&str, u32, u32)]) -> TimelineItem {
        TimelineItem {
            media: photos
                .iter()
                .map(|&(url, width, height)| PostMedia {
                    url: url.to_string(),
                    kind: Some("photo".to_string()),
                    width: Some(width),
                    height: Some(height),
                    alt_text: None,
                })
                .collect(),
            ..item_with(id, "someone", None)
        }
    }

    /// 1 フレーム描き､background に頼んだ仕事 (画像のデコード) を終わらせる｡
    pub(super) fn draw_until_parked(
        visual: &mut gpui::VisualTestContext,
        cx: &mut gpui::TestAppContext,
    ) {
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.run_until_parked();
    }

    /// #256: 1 枚の写真は自分の寸法で本文列の左端に座る｡
    ///
    /// 枠の寸法を決めるのは行の幅ではなく API が返した `width` / `height`
    /// で､横長は `MEDIA_MAX_WIDTH` まで､縦長は `MEDIA_MAX_HEIGHT` まで｡
    /// 画像が届く前も後も同じ寸法なので､行は組み直されない｡
    #[gpui::test]
    fn a_lone_photo_sits_left_at_its_own_size(cx: &mut gpui::TestAppContext) {
        let fixture = Fixture {
            items: vec![
                item_with_media("2", &[("media/e.png", 128, 72)]),
                item_with_media("1", &[("media/f.png", 180, 320)]),
            ],
            ..fixture_with(&[], &[])
        };
        let (window, timeline) = fixture_window(cx, fixture);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);

        // 届く前の placeholder の寸法を控えておく｡
        draw_until_parked(&mut visual, cx);
        let placeholder = laid_out(&mut visual, "media-frame-media/f.png");

        // 1 枚は届いている扱いにして､`img` の枝も同じ寸法になることを見る｡
        // 中身は何でもよい — layout は画像のデコードを待たない｡
        let arrived = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/AppIcon.png");
        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                view.media_paths.insert("media/f.png".to_string(), arrived);
                cx.notify();
            });
        });
        // 2 回描く: 最初の draw が画像のデコードを background に頼み､
        // `run_until_parked` がそれを終わらせ､2 回目の draw で gpui が画像の
        // 縦横比を layout に持ち込む｡1 回目の bounds では縦横比の影響が
        // まだ無く､画面で起きる突き抜けを見られない｡
        for _ in 0..2 {
            draw_until_parked(&mut visual, cx);
        }

        // `Pixels` の四則は `arithmetic_side_effects` に弾かれるので､素の
        // f32 で比べる (`rust-lint-gauntlet`)｡
        let close = |left: f32, right: f32| (left - right).abs() < 1.0;
        let max_width = f32::from(super::render::post::MEDIA_MAX_WIDTH);
        let max_height = f32::from(super::render::post::MEDIA_MAX_HEIGHT);
        let timeline = laid_out(&mut visual, "timeline");

        // 小さい横長 1 枚: 幅の最大値までふくらみ､中央ではなく左に座る｡
        let small = laid_out(&mut visual, "media-frame-media/e.png");
        assert!(
            close(f32::from(small.size.width), max_width),
            "a small photo grows to the max width: {}",
            small.size.width
        );
        assert!(
            close(f32::from(small.size.height), max_width * 72.0 / 128.0),
            "and keeps its aspect: {}",
            small.size.height
        );
        assert!(
            small.center().x < timeline.center().x,
            "the photo sits left of the timeline's center: {} vs {}",
            small.center().x,
            timeline.center().x
        );

        // 縦長 1 枚: 高さの最大値で止まり､届いた画像は枠と同じ寸法｡
        let frame = laid_out(&mut visual, "media-frame-media/f.png");
        assert!(
            close(f32::from(frame.size.height), max_height),
            "a portrait stops at the max height: {}",
            frame.size.height
        );
        assert!(
            close(f32::from(frame.size.width), max_height * 180.0 / 320.0),
            "and keeps its aspect: {}",
            frame.size.width
        );
        assert!(
            close(
                f32::from(frame.size.width),
                f32::from(placeholder.size.width)
            ) && close(
                f32::from(frame.size.height),
                f32::from(placeholder.size.height)
            ),
            "the frame keeps the placeholder's size once the image arrives: {:?} vs {:?}",
            frame.size,
            placeholder.size
        );
        let image = laid_out(&mut visual, "media-image-media/f.png");
        assert!(
            close(f32::from(image.size.width), f32::from(frame.size.width))
                && close(f32::from(image.size.height), f32::from(frame.size.height)),
            "an image that arrived is exactly its frame, not what its own aspect says: {:?} vs {:?}",
            image.size,
            frame.size
        );
        assert!(
            close(f32::from(frame.left()), f32::from(small.left())),
            "every photo starts at the same left edge: {} vs {}",
            frame.left(),
            small.left()
        );
    }

    /// #188: ディスク上に無い media の URL は `media_failed` に残る｡
    /// viewer が「開けない」と言うための種で､取れた URL とは別の set に
    /// 分けてある — `media_paths` は「持っている」だけを言う｡
    #[gpui::test]
    fn a_media_fetch_that_finds_no_file_is_recorded_as_failed(cx: &mut gpui::TestAppContext) {
        let ok_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/AppIcon.png");
        let ok_url = ok_path.to_string_lossy().into_owned();
        let fixture = Fixture {
            items: vec![item_with_media(
                "1",
                &[("media/missing.png", 10, 10), (ok_url.as_str(), 10, 10)],
            )],
            ..fixture_with(&[], &[])
        };
        let (_window, timeline) = fixture_window(cx, fixture);
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert!(
                    view.media_failed.contains("media/missing.png"),
                    "a url that is not a file on disk lands in media_failed"
                );
                assert!(
                    !view.media_failed.contains(ok_url.as_str()),
                    "a url that resolved successfully must not stay in media_failed"
                );
                assert!(
                    view.media_paths.contains_key(ok_url.as_str()),
                    "a url that resolved successfully lands in media_paths"
                );
            });
        });
    }

    /// #188: サムネイルをクリックした本物の経路で行き先が分かれる｡写真は
    /// viewer を開き､クリックした 1 枚を渡す — 同じ post に動画が混ざって
    /// いても viewer には入らない (`media_click_target` の判断どおり)｡
    #[gpui::test]
    fn clicking_a_photo_thumbnail_opens_the_viewer_at_that_photo(cx: &mut gpui::TestAppContext) {
        let mut item =
            item_with_media("1", &[("media/a.png", 100, 100), ("media/b.png", 100, 100)]);
        item.media.push(PostMedia {
            url: "media/v.mp4".to_string(),
            kind: Some("video".to_string()),
            width: Some(100),
            height: Some(100),
            alt_text: None,
        });
        let fixture = Fixture {
            items: vec![item],
            ..fixture_with(&[], &[])
        };
        let (window, _timeline) = fixture_window(cx, fixture);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        draw_until_parked(&mut visual, cx);

        let cell = laid_out(&mut visual, "media-media/a.png");
        visual.simulate_click(cell.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        assert_eq!(
            cx.update(|cx| cx.windows().len()),
            2,
            "clicking a photo opens the viewer"
        );
        let viewer = cx
            .update(|cx| {
                cx.windows()
                    .into_iter()
                    .find_map(|window| window.downcast::<ImageViewer>())
            })
            .expect("the viewer window has to be open");
        cx.update(|cx| {
            let view = viewer.read(cx).expect("the viewer is open");
            assert_eq!(
                view.photos.len(),
                2,
                "only the photos, not the video, reach the viewer"
            );
            assert_eq!(view.index, 0, "it opens at the photo that was clicked");
        });
    }

    /// #188: `a.png` (0 枚目) をクリックしたときの `index == 0` だけでは
    /// `post_row::media_cell` の `unwrap_or(0)` フォールバックと区別が
    /// つかない｡`b.png` (1 枚目) をクリックし､`position` が実際に効いて
    /// いることを別に押さえる｡
    #[gpui::test]
    fn clicking_the_second_photo_opens_the_viewer_at_index_one(cx: &mut gpui::TestAppContext) {
        let fixture = Fixture {
            items: vec![item_with_media(
                "1",
                &[("media/a.png", 100, 100), ("media/b.png", 100, 100)],
            )],
            ..fixture_with(&[], &[])
        };
        let (window, _timeline) = fixture_window(cx, fixture);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        draw_until_parked(&mut visual, cx);

        let cell = laid_out(&mut visual, "media-media/b.png");
        visual.simulate_click(cell.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        let viewer = cx
            .update(|cx| {
                cx.windows()
                    .into_iter()
                    .find_map(|window| window.downcast::<ImageViewer>())
            })
            .expect("the viewer window has to be open");
        cx.update(|cx| {
            let view = viewer.read(cx).expect("the viewer is open");
            assert_eq!(view.index, 1, "it opens at the second photo, not the first");
        });
    }

    /// #188: 動画のサムネイルは viewer を開かず､ブラウザへ飛ばす task が
    /// 走るだけ｡`run_until_parked` を挟まない — 挟むと spawn した task が
    /// 本物のブラウザを開こうとする (テストはネットワークも `browser::open`
    /// も叩かない)｡
    #[gpui::test]
    fn clicking_a_video_thumbnail_stays_in_the_browser(cx: &mut gpui::TestAppContext) {
        let mut item = item_with_media("1", &[("media/a.png", 100, 100)]);
        item.media.push(PostMedia {
            url: "media/v.mp4".to_string(),
            kind: Some("video".to_string()),
            width: Some(100),
            height: Some(100),
            alt_text: None,
        });
        let fixture = Fixture {
            items: vec![item],
            ..fixture_with(&[], &[])
        };
        let (window, timeline) = fixture_window(cx, fixture);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        draw_until_parked(&mut visual, cx);

        let cell = laid_out(&mut visual, "media-media/v.mp4");
        visual.simulate_click(cell.center(), gpui::Modifiers::none());

        assert_eq!(
            cx.update(|cx| cx.windows().len()),
            1,
            "a video does not open the viewer"
        );
        cx.update(|cx| {
            assert!(
                timeline.read(cx).open_task.is_some(),
                "the click still spawns the browser-open task"
            );
        });
    }

    /// #256: 縦長の写真どうしは横 1 段に並び､高さを揃え､幅は縦横比に比例
    /// する (Tumblr の photoset)｡段の全体は `MEDIA_MAX_WIDTH` に収まる｡
    #[gpui::test]
    fn portrait_photos_share_one_row(cx: &mut gpui::TestAppContext) {
        let fixture = Fixture {
            items: vec![item_with_media(
                "3",
                &[
                    ("media/a.png", 180, 320),
                    ("media/b.png", 160, 320),
                    ("media/c.png", 180, 320),
                    ("media/d.png", 240, 320),
                ],
            )],
            ..fixture_with(&[], &[])
        };
        let (window, _timeline) = fixture_window(cx, fixture);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        draw_until_parked(&mut visual, cx);

        let close = |left: f32, right: f32| (left - right).abs() < 1.0;
        let max_width = f32::from(super::render::post::MEDIA_MAX_WIDTH);
        let a = laid_out(&mut visual, "media-frame-media/a.png");
        let b = laid_out(&mut visual, "media-frame-media/b.png");
        let c = laid_out(&mut visual, "media-frame-media/c.png");
        let d = laid_out(&mut visual, "media-frame-media/d.png");
        for (name, frame) in [("b", &b), ("c", &c), ("d", &d)] {
            assert!(
                close(f32::from(frame.top()), f32::from(a.top())),
                "{name} shares the row with a: {} vs {}",
                frame.top(),
                a.top()
            );
            assert!(
                close(f32::from(frame.size.height), f32::from(a.size.height)),
                "{name} shares a's height: {} vs {}",
                frame.size.height,
                a.size.height
            );
        }
        assert!(
            a.right() < b.left() && b.right() < c.left() && c.right() < d.left(),
            "the four sit side by side in order"
        );
        assert!(
            b.size.width < a.size.width && a.size.width < d.size.width,
            "widths follow aspect: {} < {} < {}",
            b.size.width,
            a.size.width,
            d.size.width
        );
        assert!(
            close(f32::from(a.size.width), f32::from(c.size.width)),
            "two portraits of one aspect share a width: {} vs {}",
            a.size.width,
            c.size.width
        );
        assert!(
            f32::from(d.right()) - f32::from(a.left()) <= max_width + 1.0,
            "together they fit the max width: {} to {}",
            a.left(),
            d.right()
        );
    }

    /// #256: 横長の写真どうしは縦に積む — 横に並べると 1 枚 1 枚が読めない
    /// ほど小さくなる｡積まれた各枚は 1 枚のときと同じ寸法を取る｡
    #[gpui::test]
    fn landscape_photos_stack_below_one_another(cx: &mut gpui::TestAppContext) {
        let fixture = Fixture {
            items: vec![item_with_media(
                "3",
                &[("media/a.png", 320, 180), ("media/b.png", 128, 72)],
            )],
            ..fixture_with(&[], &[])
        };
        let (window, _timeline) = fixture_window(cx, fixture);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        draw_until_parked(&mut visual, cx);

        let close = |left: f32, right: f32| (left - right).abs() < 1.0;
        let max_width = f32::from(super::render::post::MEDIA_MAX_WIDTH);
        let a = laid_out(&mut visual, "media-frame-media/a.png");
        let b = laid_out(&mut visual, "media-frame-media/b.png");
        assert!(
            b.top() >= a.bottom(),
            "the second landscape sits below the first: {} vs {}",
            b.top(),
            a.bottom()
        );
        assert!(
            close(f32::from(b.left()), f32::from(a.left())),
            "both start at the column's left edge: {} vs {}",
            b.left(),
            a.left()
        );
        for (name, frame) in [("a", &a), ("b", &b)] {
            assert!(
                close(f32::from(frame.size.width), max_width),
                "{name} takes the lone photo's width: {}",
                frame.size.width
            );
            assert!(
                close(f32::from(frame.size.height), max_width * 9.0 / 16.0),
                "{name} keeps 16:9: {}",
                frame.size.height
            );
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
            f32::from(body.bottom()) - f32::from(toast.bottom()) < 48.,
            "the toast sits near the bottom edge, not floating mid-screen: {toast:?} vs {body:?}"
        );
        assert!(
            toast.top() > body.center().y,
            "the toast overlaps the bottom of the timeline, not its middle: {toast:?}"
        );
        assert!(
            f32::from(toast.size.width) < f32::from(body.size.width) / 2.,
            "a capsule, not a bar: {toast:?} vs {body:?}"
        );
        assert!(
            (f32::from(toast.center().x) - f32::from(body.center().x)).abs() < 1.,
            "centered: {toast:?} vs {body:?}"
        );
    }

    /// #206: toast は viewport に貼りつく｡一覧が scroll しても動かない｡
    #[gpui::test]
    fn the_toast_stays_put_while_the_timeline_scrolls(cx: &mut gpui::TestAppContext) {
        let ids: Vec<String> = (1..=40).map(|n| n.to_string()).collect();
        let shown: Vec<&str> = ids.iter().map(String::as_str).collect();
        let (mut visual, timeline) = drawn(cx, fixture_with(&shown, &["99"]));
        // 出てくる途中は持ち上がりで動くので､先に着かせる｡
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
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
            gpui::point(gpui::px(f32::from(body.left()) + 10.), toast.center().y),
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

    /// #206: 押した瞬間には消えない — バッファは glide へ合流するので､
    /// トーストは `unseen` が視界の上に残っている間は点いたままで数え続け､
    /// glide が終わってから初めて薄くなって外れる｡
    #[gpui::test]
    fn the_toast_fades_out_once_the_glide_it_started_finishes(cx: &mut gpui::TestAppContext) {
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
            "the toast keeps counting rather than vanishing in the same frame"
        );
        cx.update(|cx| {
            let view = timeline.read(cx);
            assert!(
                view.pending.is_none(),
                "the click moved the buffer onto the glide"
            );
            assert!(
                view.glide.is_some(),
                "the click starts the same glide follow uses"
            );
            assert_eq!(
                view.toast.fade,
                Fade::Shown,
                "rows are still above the viewport, so the toast has not started falling yet"
            );
        });

        // 2 行分の glide (`GLIDE_MIN_S` = 0.6 秒前後) と､それに続くフェード
        // の 180ms (`FADE_STEPS` × `FADE_STEP_MILLIS`) を両方まかなう長さ｡
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(2));
        cx.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        // `debug_bounds` では「無い」を言えない — gpui の `Frame::clear` は
        // その map を空けないので､一度描いた名前は消えた後も残る｡代わりに
        // 要素を組む側に尋ねる｡
        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                assert_eq!(view.unseen, 0, "the glide has finished");
                assert_eq!(view.toast.fade, Fade::Hidden);
                assert!(view.toast_fade_task.is_none());
                assert!(
                    view.toast(cx).is_none(),
                    "a hidden toast is out of the tree, not a transparent capsule"
                );
            });
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
        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                assert_eq!(view.toast.fade, Fade::Hidden);
                assert!(
                    view.toast(cx).is_none(),
                    "with nothing left above, the toast is gone"
                );
            });
        });
    }

    /// トーストのクリックがバッファを最上部へ跳ばすのではなく follow と
    /// 同じ glide を始めることを確かめる｡読み手が下へスクロールした位置
    /// から適用するのが要: `settle_from` が無いままだと､`start_glide` の
    /// settle ループがスクロール済みの offset をそれだけで「anchor が
    /// 着地した」と誤読して 1 フレーム目で抜けてしまい､その後 anchor が
    /// 実際に着地した瞬間を読み手がホイールを握ったと誤認して glide を
    /// 諦める｡この誤読が直っているかどうかを､anchor が着地するはずの
    /// フレームの直後で `glide.is_some()` を見て押さえる｡
    #[gpui::test]
    fn clicking_the_toast_resumes_the_glide_from_a_scrolled_down_position(
        cx: &mut gpui::TestAppContext,
    ) {
        let ids: Vec<String> = (1..=40).map(|n| n.to_string()).collect();
        let shown: Vec<&str> = ids.iter().map(String::as_str).collect();
        let (mut visual, timeline) = drawn(cx, fixture_with(&shown, &["99"]));

        let body = visual
            .debug_bounds("timeline")
            .expect("the timeline has to be laid out before a wheel can reach it");
        visual.simulate_event(wheel_event(body.center(), -10.));
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(2));
        cx.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            offset_y(cx, &timeline) < -10.,
            "the reader must be scrolled down before clicking the toast"
        );

        let toast = visual
            .debug_bounds("new-posts")
            .expect("a post is waiting, so the toast is laid out");
        visual.simulate_click(toast.center(), gpui::Modifiers::none());

        // ここが 1 フレーム目: follow の `scroll_to_top_of_item` が置いた
        // anchor がまさにこのフレームで着地する｡
        cx.executor()
            .advance_clock(std::time::Duration::from_secs_f32(super::scroll::FRAME_S));
        cx.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.update(|cx| {
            assert!(
                timeline.read(cx).glide.is_some(),
                "the anchor landing on a scrolled-down offset must not read as the \
                 reader grabbing the scrollbar"
            );
        });

        // #208 の glide は長くても `GLIDE_MAX_S` (5 秒) で終わる｡`unseen` は
        // 境界を跨ぐたびに減るので途中で 0 になりうる — それだけで抜けると
        // offset がまだ着いていないうちに読んでしまうので､ここは一気に
        // 2 秒進めて glide 自体を終わらせてから読む｡
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(2));
        cx.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        cx.update(|cx| {
            let view = timeline.read(cx);
            assert_eq!(view.unseen, 0, "the glide must finish rather than hang");
            assert!(
                f32::from(view.list_scroll.offset().y).abs() < 1.,
                "the glide ends at the top: {:?}",
                view.list_scroll.offset()
            );
        });
    }

    /// #206: follow が流し込んでいる途中でホイールに触れると glide は止まる
    /// (#175)｡そこで toast を押すと､最上部へ跳ぶのではなく同じ glide を
    /// 続きから再開する — リストを置き換え直すことはない｡
    #[gpui::test]
    fn clicking_the_toast_resumes_a_glide_the_wheel_had_stopped(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline, body) = scrollable_window(cx);

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

        visual.simulate_event(wheel_event(body.center(), -1.));
        let unseen_when_stopped = cx.update(|cx| {
            let view = timeline.read(cx);
            assert!(view.glide.is_none(), "the reader's hand stopped the glide");
            view.unseen
        });
        assert!(
            unseen_when_stopped > 0,
            "rows are still above the viewport while stopped"
        );

        let toast = visual
            .debug_bounds("new-posts")
            .expect("rows are above the viewport, so the toast is laid out");
        visual.simulate_click(toast.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        cx.update(|cx| {
            let view = timeline.read(cx);
            assert!(
                view.glide.is_some(),
                "the click resumes the glide instead of jumping"
            );
            assert_eq!(
                view.unseen, unseen_when_stopped,
                "resuming does not itself reveal any rows"
            );
            assert_eq!(
                shown_ids(view).len(),
                43,
                "the list is not replaced, only the glide restarts"
            );
        });

        cx.executor()
            .advance_clock(std::time::Duration::from_secs(2));
        cx.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        cx.update(|cx| {
            let view = timeline.read(cx);
            assert_eq!(view.unseen, 0, "the resumed glide eventually finishes");
            assert!(view.pending.is_none());
            assert!(view.client.is_none());
            assert!(
                view.last_reload_at.is_none(),
                "resuming a glide must not count as a fetch"
            );
        });
    }

    /// #22: `ScrollToTop` はトーストのクリックと違って glide を挟まず､
    /// 1 フレームで先頭へ着く｡
    #[gpui::test]
    fn scroll_to_top_still_jumps_instantly(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline, body) = scrollable_window(cx);

        visual.simulate_event(wheel_event(body.center(), -10.));
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(2));
        cx.run_until_parked();
        assert!(
            offset_y(cx, &timeline) < -10.,
            "the reader must be scrolled down before the jump"
        );

        visual.update(|window, cx| {
            window.dispatch_action(Box::new(crate::menu::ScrollToTop), cx);
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let view = timeline.read(cx);
            assert!(view.glide.is_none(), "ScrollToTop must not start a glide");
            assert_eq!(view.unseen, 0);
        });

        // anchor の着地に要るのは 1 フレームだけ — glide のような数十フレーム
        // の advance_clock は要らない｡
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            offset_y(cx, &timeline).abs() < 1.,
            "one frame is enough to land at the top, with no glide in between"
        );
    }

    /// `apply_pending` は `reveal_new_posts` を経由せず `⌘⇧R` / View → Show
    /// New Posts から直接呼ばれる (`layout.rs`)｡ホイールの spring loop が
    /// まだ動いているところへその経路で `follow` が glide を始めても､
    /// 手放しが `follow` 自身にあるので取り合いにならないことを確かめる｡
    #[gpui::test]
    fn showing_new_posts_releases_a_wheel_still_settling(cx: &mut gpui::TestAppContext) {
        let ids: Vec<String> = (1..=40).map(|n| n.to_string()).collect();
        let shown: Vec<&str> = ids.iter().map(String::as_str).collect();
        let (window, timeline) = fixture_window(cx, fixture_with(&shown, &["99"]));
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let body = visual
            .debug_bounds("timeline")
            .expect("the timeline has to be laid out before a wheel can reach it");

        visual.simulate_event(wheel_event(body.center(), -3.));
        cx.update(|cx| {
            assert!(
                timeline.read(cx).scroll_motion.is_some(),
                "the wheel's spring loop must still be settling"
            );
        });

        visual.update(|window, cx| {
            window.dispatch_action(Box::new(crate::menu::ShowNewPosts), cx);
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(
                timeline.read(cx).scroll_motion.is_none(),
                "follow must release the wheel's spring before the glide owns the offset"
            );
        });

        cx.executor()
            .advance_clock(std::time::Duration::from_secs(2));
        cx.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.update(|cx| {
            let view = timeline.read(cx);
            assert_eq!(
                view.unseen, 0,
                "the glide reaches the top despite the wheel"
            );
            assert!(
                f32::from(view.list_scroll.offset().y).abs() < 1.,
                "ends at the top: {:?}",
                view.list_scroll.offset()
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

                view.refresh_situation = Some(counting_situation());

                assert_eq!(view.apply_poll(Err(denied), cx), Poll::Halt);
                assert!(
                    view.refresh_situation.is_none(),
                    "#214: a halted loop has no next poll for the footer to count down to"
                );
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

    /// #267: Window メニューの Translucent は透過を反転させ､今どちら向きかを
    /// 言う — follow のトグルと同じ理由で､新しい状態が見えるのはバナーだけだ｡
    /// fixture のウィンドウには覚える先が無いが､切り替えそのものは効く｡
    #[gpui::test]
    fn toggling_translucent_flips_the_switch_and_reports_itself(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        let (window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &[]));

        cx.update_window(window.into(), |_, window, cx| {
            let _ = window.draw(cx);
            window.dispatch_action(Box::new(crate::menu::ToggleTranslucent), cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert!(
                    view.window_state.translucent,
                    "a window starts opaque, so one toggle makes it translucent"
                );
                assert!(
                    matches!(view.reload_notice, Some(ReloadNotice::Outcome(_))),
                    "the flip must say which way it went, got {:?}",
                    view.reload_notice
                );
            });
        });
    }

    /// #267: fixture が `translucent` と言えば､窓は透過の状態で立ち上がる｡
    /// fixture の窓は window state ファイルを読まないので､透過の見た目を
    /// 撮るにはこれしか道が無い — `Fixture::picker_open` と同じ例外だ｡
    #[gpui::test]
    fn a_fixture_can_ask_for_a_translucent_window(cx: &mut gpui::TestAppContext) {
        let fixture = Fixture {
            translucent: true,
            ..fixture_with(&["1"], &[])
        };
        let (_window, timeline) = fixture_window(cx, fixture);
        cx.update(|cx| {
            assert!(
                timeline.read(cx).window_state.translucent,
                "a fixture-declared translucent window must start translucent"
            );
        });
    }

    /// #267: Window メニューの Float on Top も同じ形 — 反転させ､言い､覚える｡
    /// window の level そのものは gpui の fork の patch が触るので､テスト
    /// プラットフォームでは no-op｡ここで見えるのは view 側の配線だけだ｡
    #[gpui::test]
    fn toggling_float_on_top_flips_the_switch_and_reports_itself(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        let (window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &[]));

        cx.update_window(window.into(), |_, window, cx| {
            let _ = window.draw(cx);
            window.dispatch_action(Box::new(crate::menu::ToggleFloatOnTop), cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert!(
                    view.window_state.float_on_top,
                    "a window starts among the others, so one toggle floats it"
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
    ///
    /// #248 で入口はメニューへ移り､リクエスト数の隣に座る裸の span は
    /// 次の sync の時刻 (#214) になった｡危うさは同じで､見る組が変わった｡
    #[gpui::test]
    fn the_status_bars_segments_keep_apart(cx: &mut gpui::TestAppContext) {
        let (window, _timeline) = fixture_window(cx, fixture_with_sync(&["2", "1"], 0));

        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let usage = visual
            .debug_bounds("status-usage")
            .expect("the request count is always shown");
        let next = visual
            .debug_bounds("status-sync-next")
            .expect("an idle sync has a next time to show");

        assert!(
            next.left() > usage.right(),
            "the two segments run together, which reads as `11 reqNext sync` \
             on screen: usage ends at {:?}, next sync starts at {:?}",
            usage.right(),
            next.left()
        );
    }

    /// #162: `the_status_bars_segments_keep_apart` の続き — footer が常時
    /// 見積り金額を出すようになり (`~$X.XX`)､旧 `Today: N req · Total: M req`
    /// より長くなった｡5 桁の today/total という最悪ケースでもまだ次の区画に
    /// 触れないことを確かめる｡
    #[gpui::test]
    fn the_status_usage_segment_survives_worst_case_numbers(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline) = drawn(cx, fixture_with_sync(&["2", "1"], 0));

        visual.update(|window, cx| {
            timeline.update(cx, |view, _cx| {
                view.usage_totals = usage::Totals {
                    today: 9_999,
                    total: 99_999,
                };
            });
            let _ = window.draw(cx);
        });

        let usage = visual
            .debug_bounds("status-usage")
            .expect("the usage summary is always shown");
        let next = visual
            .debug_bounds("status-sync-next")
            .expect("an idle sync has a next time to show");

        assert!(
            next.left() > usage.right(),
            "large usage numbers must not run into the next segment: \
             usage ends at {:?}, next sync starts at {:?}",
            usage.right(),
            next.left()
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

    /// `ids` を Home のキャッシュ済み timeline として smoke 用のディレクトリ
    /// へ書く (`cache_list` の Home 版)｡#43 のトグルは Home を含めた集合を
    /// 都度再合成するので､Home にもキャッシュが無いと `missing_sources` が
    /// 埋めようとして client 無しの reload に落ち (`NotAuthenticated`)、
    /// 複数 source を行き来するテストが成立しない｡
    fn cache_home(ids: &[&str]) {
        let paths = smoke_paths();
        paths.ensure_dirs().unwrap();
        let items: Vec<TimelineItem> = ids
            .iter()
            .map(|id| item_with(id, "someone", None))
            .collect();
        crate::cache::save_primary_timeline(
            &paths,
            &crate::cache::TimelineSource::Home,
            "5685672",
            &items,
            0,
        )
        .unwrap();
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

    /// #156: post の操作は文字ラベルではなく記号 — その矩形は正方形で､
    /// 記号 (`theme::ICON_SIZE`) より小さくならない｡文字ラベルのままなら
    /// 横長になり落ちる｡
    #[gpui::test]
    fn an_action_is_a_square_button_around_its_icon(cx: &mut gpui::TestAppContext) {
        let (mut visual, _timeline) = drawn(cx, fixture_with_metrics("1"));
        let like = visual
            .debug_bounds("like-1")
            .expect("a shown post always offers Like");
        assert_eq!(
            like.size.width, like.size.height,
            "the like button must be a square around its icon, not a text label: {like:?}"
        );
        assert!(
            like.size.width >= theme::ICON_SIZE,
            "the button must be at least as big as the icon it holds: {:?} < {:?}",
            like.size.width,
            theme::ICON_SIZE
        );
    }

    /// #156: 件数は記号の矩形の外に座る — 重ならず､右に 4px 前後空く｡
    /// `with_count` に名前を渡していない今は `like-1-count` が存在しない
    /// ので落ちる｡
    #[gpui::test]
    fn a_count_sits_beside_its_action_without_overlapping(cx: &mut gpui::TestAppContext) {
        let (mut visual, _timeline) = drawn(cx, fixture_with_metrics("1"));
        let like = visual
            .debug_bounds("like-1")
            .expect("a shown post always offers Like");
        let count = visual
            .debug_bounds("like-1-count")
            .expect("a post with a like count must name that count");
        assert!(
            count.left() >= like.right(),
            "the count overlaps the button: count starts at {:?}, button ends at {:?}",
            count.left(),
            like.right()
        );
        let gap = f32::from(count.left()) - f32::from(like.right());
        assert!(
            (0.0..10.0).contains(&gap),
            "the gap between the button and its count should be about 4px, got {gap}"
        );
    }

    /// #156: hover の塗りは padding の内側に収まり､要素の bounds を変えない｡
    /// 色そのものはテストから見えないので､寸法だけ押さえる｡
    #[gpui::test]
    fn hovering_an_action_does_not_move_it(cx: &mut gpui::TestAppContext) {
        let (mut visual, _timeline) = drawn(cx, fixture_with_metrics("1"));
        let before = visual
            .debug_bounds("like-1")
            .expect("a shown post always offers Like");
        visual.simulate_mouse_move(before.center(), None, gpui::Modifiers::none());
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let after = visual
            .debug_bounds("like-1")
            .expect("the button is still shown after hovering it");
        assert_eq!(before, after, "hovering the button must not move it");
    }

    /// #156: Delete は他の 5 つから離して行の右端に置く｡`justify_between`
    /// が外れれば delete は open のすぐ右へ戻り､この余白が消える｡
    #[gpui::test]
    fn delete_sits_apart_at_the_row_end(cx: &mut gpui::TestAppContext) {
        let fixture = Fixture {
            items: vec![item_with("1", "usadamasa", None)],
            ..fixture_with(&[], &[])
        };
        let (mut visual, _timeline) = drawn(cx, fixture);
        let open = visual
            .debug_bounds("open-1")
            .expect("a shown post always offers Open in X");
        let delete = visual
            .debug_bounds("delete-1")
            .expect("one's own post always offers Delete");
        let gap = f32::from(delete.left()) - f32::from(open.right());
        // 通常の隣接 gap (`gap_3`) は 12px｡`justify_between` がこの
        // テスト窓の右端まで開くと 1000px を超える｡50px はその両方から
        // 十分離れているので､この assert は `justify_between` が実際に
        // 効いているときだけ通る｡
        assert!(
            gap > 50.0,
            "delete must sit apart at the row's end, not right beside open \
             with just the ordinary gap_3: open ends at {:?}, delete starts \
             at {:?} (gap {gap})",
            open.right(),
            delete.left()
        );
    }

    /// #266: 自分の post の行にも repost の toggle が出る｡述語だけでなく
    /// 行の組み立てまで届いていることを押さえる — fixture の
    /// `signed_in_as` が `home_username` を埋めるので､この行は自分の
    /// post として描かれる｡
    #[gpui::test]
    fn ones_own_post_offers_repost(cx: &mut gpui::TestAppContext) {
        let fixture = Fixture {
            items: vec![item_with("1", "usadamasa", None)],
            ..fixture_with(&[], &[])
        };
        let (mut visual, _timeline) = drawn(cx, fixture);
        assert!(
            visual.debug_bounds("repost-1").is_some(),
            "one's own post must offer Repost too"
        );
    }

    /// #156: 自分の post の行 (末尾に `justify_between` で
    /// 開かれる delete) でも like の件数とその右の quote は `gap_3`
    /// (12px) 以上離れているべきで､右端寄せの実装のせいで隣の gap が
    /// つぶれてはいけない｡
    #[gpui::test]
    fn a_count_keeps_its_gap_next_to_the_following_action_on_ones_own_post(
        cx: &mut gpui::TestAppContext,
    ) {
        let fixture = Fixture {
            items: vec![TimelineItem {
                id: "1".to_string(),
                text: String::new(),
                created_at: None,
                author_name: String::new(),
                author_username: "usadamasa".to_string(),
                reposted_by: None,
                quoted: None,
                replied_to: None,
                metrics: Some(PostMetrics {
                    replies: 0,
                    reposts: 0,
                    likes: 1,
                }),
                links: Vec::new(),
                author_avatar_url: None,
                original_post_id: None,
                media: Vec::new(),
            }],
            ..fixture_with(&[], &[])
        };
        let (mut visual, _timeline) = drawn(cx, fixture);
        let count = visual
            .debug_bounds("like-1-count")
            .expect("a like count of 1 must be shown and named");
        let quote = visual
            .debug_bounds("quote-1")
            .expect("one's own post always offers Quote");
        let gap = f32::from(quote.left()) - f32::from(count.right());
        assert!(
            gap >= 12.0,
            "the like count must keep the row's gap_3 before quote: \
             count ends at {:?}, quote starts at {:?} (gap {gap})",
            count.right(),
            quote.left()
        );
    }

    /// 件数の有無で action の縦の並びが動かない｡件数を持つ post と持たない
    /// post で open の左端が一致する｡`with_count` が件数の無いときに
    /// action だけ返すと､下の行の open が左へ寄って落ちる｡
    #[gpui::test]
    fn actions_line_up_across_posts_with_and_without_counts(cx: &mut gpui::TestAppContext) {
        let mut fixture = fixture_with_metrics("1");
        fixture.items.push(item_with("2", "someone", None));
        let (mut visual, _timeline) = drawn(cx, fixture);
        let with = visual
            .debug_bounds("open-1")
            .expect("a shown post always offers Open in X");
        let without = visual
            .debug_bounds("open-2")
            .expect("a shown post always offers Open in X");
        assert_eq!(
            with.left(),
            without.left(),
            "open must sit at the same x whether or not the post shows counts"
        );
    }

    /// auto-refresh のループが最初の起床で写すもの (#214)｡`smoke_config` は
    /// auto-refresh を切っていて fixture のウィンドウは client を持たない
    /// ので､ループは決して始まらず､テストはこれを直接置く｡
    fn counting_situation() -> Situation {
        Situation {
            last_reload_at: None,
            started_at: crate::oauth::unix_now(),
            interval_seconds: 300,
            busy: false,
            activity: crate::activity::Activity::Present,
            resumed_at: None,
        }
    }

    /// #214: auto-refresh のカウントダウンはループが写した期限から出る｡
    /// 写しが無ければ出ない — off のウィンドウに "Auto-refresh in" が出れば
    /// 嘘になる｡出たときは reload のアイコンの左に座り､接しない (#184 の
    /// `the_status_bars_segments_keep_apart` と同じ主張)｡
    #[gpui::test]
    fn the_refresh_countdown_sits_by_the_reload_icon_only_while_a_loop_is_counting(
        cx: &mut gpui::TestAppContext,
    ) {
        let (mut visual, timeline) = drawn(cx, fixture_with(&["2", "1"], &[]));

        visual.update(|_window, cx| {
            timeline.update(cx, |view, _cx| view.refresh_situation = None);
        });
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            visual.debug_bounds("auto-refresh-countdown").is_none(),
            "a window with no polling loop must not promise a next poll"
        );

        visual.update(|_window, cx| {
            timeline.update(cx, |view, _cx| {
                view.refresh_situation = Some(counting_situation());
            });
        });
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let countdown = visual
            .debug_bounds("auto-refresh-countdown")
            .expect("a counting loop has to reach the toolbar");
        let reload = visual
            .debug_bounds("primary-action")
            .expect("the reload icon is always shown");
        assert!(
            reload.left() > countdown.right(),
            "the countdown runs into the reload icon: countdown ends at {:?}, icon starts at {:?}",
            countdown.right(),
            reload.left()
        );
    }

    /// footer の主要な区画の bounds を､ウィンドウを `width` にしてから読む
    /// (#214)｡順に: 帯､次の sync､post の数｡
    fn footer_bounds_at(
        visual: &mut gpui::VisualTestContext,
        width: f32,
    ) -> (
        gpui::Bounds<gpui::Pixels>,
        gpui::Bounds<gpui::Pixels>,
        gpui::Bounds<gpui::Pixels>,
    ) {
        visual.simulate_resize(gpui::size(gpui::px(width), gpui::px(700.)));
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (
            visual
                .debug_bounds("status-bar")
                .expect("the footer is always shown"),
            visual
                .debug_bounds("status-sync-next")
                .expect("an idle sync has a next time to show"),
            visual
                .debug_bounds("status-kept")
                .expect("a loaded timeline always says how many posts it keeps"),
        )
    }

    /// #214: 幅が足りなくなると footer はまず文言を詰め､それでも足りなければ
    /// 次の sync を "…" で切る｡post の数は決して右端から落ちない｡
    ///
    /// 最初の実装は両方のカウントダウンを footer に置き､550px ですら
    /// "posts kept" が右端から落ちた｡2 つの幅で見る: `COMPACT_BELOW` を
    /// わずかに下回る幅では詰めた文言がそのまま入り (同じ密度でもっと広い
    /// 幅と同じ寸法)､本番の 429px では詰めた文言も切られるが､post の数は
    /// 帯の中に残る｡テスト環境のフォントは本番より広いので､本番なら 429px
    /// で入る文言がここでは切られる — このテストが見ているのは寸法ではなく
    /// 譲る順番だ｡
    #[gpui::test]
    fn the_footer_shortens_first_and_truncates_last(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline) = drawn(cx, fixture_with_sync(&["2", "1"], 0));
        visual.update(|_window, cx| {
            timeline.update(cx, |view, _cx| {
                view.refresh_situation = Some(counting_situation());
            });
        });

        // 詰めた文言の本来の幅は､詰める側でいちばん広い幅で読む｡それより
        // 狭い幅で同じ寸法なら､そこでも丸ごと入っている｡
        let roomy = f32::from(countdown::COMPACT_BELOW) - 1.;
        let (_, unsqueezed, _) = footer_bounds_at(&mut visual, roomy);
        let (bar, next, kept) = footer_bounds_at(&mut visual, roomy - 20.);
        assert_eq!(
            next.size.width, unsqueezed.size.width,
            "a little under the threshold the shortened wording has to fit whole"
        );
        assert!(
            kept.right() <= bar.right(),
            "the post count falls off the window: count ends at {:?}, window ends at {:?}",
            kept.right(),
            bar.right()
        );

        let (bar, _, kept) = footer_bounds_at(&mut visual, 429.);
        assert!(
            kept.right() <= bar.right(),
            "at 429px the post count falls off the window: count ends at {:?}, window ends at {:?}",
            kept.right(),
            bar.right()
        );

        // 詰めた文言すら入らない幅は寸法から逆算する: 余っている幅を使い
        // 切り､さらに文言の半分ぶん狭める｡
        let slack = f32::from(bar.right()) - f32::from(kept.right());
        let cramped = 429. - slack - f32::from(unsqueezed.size.width) / 2.;
        let (bar, next, kept) = footer_bounds_at(&mut visual, cramped);
        assert!(
            kept.right() <= bar.right(),
            "cramped, the post count falls off the window: count ends at {:?}, window ends at {:?}",
            kept.right(),
            bar.right()
        );
        assert!(
            next.size.width < unsqueezed.size.width,
            "cramped, the next sync time has to be the segment that gives way"
        );
        assert!(
            next.right() <= kept.left(),
            "the next sync time runs into the post count: time ends at {:?}, count starts at {:?}",
            next.right(),
            kept.left()
        );
    }

    /// #214: toolbar のカウントダウンは 429px でも reload のアイコンを
    /// ウィンドウの外へ押し出さない｡
    #[gpui::test]
    fn the_toolbar_countdown_keeps_the_reload_icon_in_the_window(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline) = drawn(cx, fixture_with(&["2", "1"], &[]));
        visual.update(|_window, cx| {
            timeline.update(cx, |view, _cx| {
                view.refresh_situation = Some(counting_situation());
            });
        });
        visual.simulate_resize(gpui::size(gpui::px(429.), gpui::px(700.)));
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let viewport = visual.update(|window, _| window.viewport_size());
        let reload = visual
            .debug_bounds("primary-action")
            .expect("the reload icon is always shown");
        let countdown = visual
            .debug_bounds("auto-refresh-countdown")
            .expect("a counting loop has to reach the toolbar");
        assert!(
            reload.right() <= viewport.width,
            "the reload icon falls off the window: icon ends at {:?}, window ends at {:?}",
            reload.right(),
            viewport.width
        );
        assert!(
            countdown.right() < reload.left(),
            "the countdown runs into the reload icon"
        );
    }

    /// #192, #43: 開いたメニューでは､どの項目も配置され､Home が先頭で､
    /// どれも重ならない — segmented control (#164) が横一列だったのに
    /// 対し､ドロップダウンは縦に積む｡`the_status_bars_segments_keep_apart`
    /// がステータスバーについて述べるのと同じ主張を､同じ理由で述べている｡
    #[gpui::test]
    fn the_open_menu_lists_home_and_every_list_top_to_bottom(cx: &mut gpui::TestAppContext) {
        let (mut visual, _timeline) = drawn(
            cx,
            fixture_with_lists(&["1"], &[("9101", "Following mirror"), ("9102", "Rust")]),
        );

        let trigger = visual
            .debug_bounds("source-picker")
            .expect("the trigger is always shown");
        visual.simulate_click(trigger.center(), gpui::Modifiers::none());
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let home = visual
            .debug_bounds("tab-home")
            .expect("Home is always a segment");
        let first = visual
            .debug_bounds("tab-list-9101")
            .expect("the first fixture list is a segment");
        let second = visual
            .debug_bounds("tab-list-9102")
            .expect("the second fixture list is a segment");
        assert!(first.top() >= home.bottom(), "{home:?} then {first:?}");
        assert!(second.top() >= first.bottom(), "{first:?} then {second:?}");

        // 1 段上でまた #182: ツールバーの行の `gap` はタイトルを溝に密着させた
        // ままにするので､`List@usadamasa` と読めてしまう｡トリガー自体は
        // 固定幅なので､メニューが開いていてもツールバー行の並びは変わらない｡
        let title = visual
            .debug_bounds("header-title")
            .expect("the title is always shown");
        assert!(
            title.left() > trigger.right(),
            "the title runs into the trigger: trigger ends at {:?}, title starts at {:?}",
            trigger.right(),
            title.left()
        );
    }

    /// #43: fixture が `sources` (複数) と `picker_open` を宣言できること｡
    /// `--fixture` の窓はクリックを合成できないので (`fixture-visual-check`)、
    /// 開いた状態・複数選択の画面を撮るにはこの経路しかない｡
    #[gpui::test]
    fn a_fixture_can_declare_multiple_sources_and_start_the_menu_open(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut fixture = fixture_with_lists(&["1"], &[("9101", "rust")]);
        fixture.sources = vec![
            super::source_picker::Selection::Home,
            super::source_picker::Selection::List {
                id: "9101".to_string(),
            },
        ];
        fixture.list_items = std::collections::BTreeMap::from([(
            "9101".to_string(),
            vec![item_with("2", "someone", None)],
        )]);
        fixture.picker_open = true;

        let (mut visual, timeline) = drawn(cx, fixture);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(
                    view.sources,
                    vec![
                        crate::cache::TimelineSource::Home,
                        crate::cache::TimelineSource::List("9101".to_string()),
                    ]
                );
                assert!(view.source_picker_open.is_open());
                assert_eq!(shown_ids(view), ["2", "1"]);
            });
        });
        // メニューが開いた状態で描かれているので､項目に到達できる｡
        assert!(visual.debug_bounds("tab-list-9101").is_some());
    }

    /// `fixtures/lane.json` の撮影で見つかった不具合 (#43): 一覧がウィンドウ
    /// より短いとき､本文が折り返す行の下に約 180px の空白が挟まった｡
    /// 行の高さは中身だけで決まるべきで､一覧の残りの長さに依存してはいけない｡
    /// 同じ post を､短い一覧 (4 行) と長い一覧 (24 行) で描いて高さを比べる｡
    #[gpui::test]
    fn a_wrapping_row_keeps_its_height_when_the_lane_is_short(cx: &mut gpui::TestAppContext) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/lane.json");
        let short = crate::fixture::load(&path).expect("fixtures/lane.json must load");
        let mut long = short.clone();
        let filler = long.items[0].clone();
        for n in 0..20 {
            let mut item = filler.clone();
            item.id = format!("92000000000000001{n:02}");
            long.items.push(item);
        }

        let height_in = |cx: &mut gpui::TestAppContext, fixture: Fixture| {
            let (mut visual, _timeline) = drawn(cx, fixture);
            // 実機の既定ウィンドウ幅 (560px)｡gpui テストの既定幅では本文が
            // 折り返さず再現しない｡
            visual.simulate_resize(gpui::size(gpui::px(560.), gpui::px(852.)));
            visual.update(|window, cx| {
                let _ = window.draw(cx);
            });
            visual
                .debug_bounds("post-row-9200000000000000002")
                .expect("the wrapping row is laid out")
                .size
                .height
        };
        let in_short = height_in(cx, short);
        let in_long = height_in(cx, long);
        // `Pixels` の四則は `arithmetic_side_effects` に弾かれるので f32 で｡
        // 半ピクセルの丸めは許し､180px の空白だけを捕まえる｡
        let gap = (f32::from(in_short) - f32::from(in_long)).abs();
        assert!(
            gap < 1.0,
            "the row is {in_short:?} in a short lane but {in_long:?} in a long one"
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

    /// #192 の判別テスト: 13 本目 (最後) のリストを選択中にしたとき､本番の
    /// 実寸 429px でその区画に到達できること｡`overflow_hidden` が右側の
    /// コントロールを守る代わりにタブそのものを画面外へ追いやっていた
    /// (`the_toolbar_action_stays_on_screen_under_a_dozen_tabs` はそちらを
    /// 守らない側しか見ていない)｡
    ///
    /// トリガーが無い今の実装では `if let` が素通りし､`tab-list-9113` は
    /// 描かれてはいるが viewport の外にあるので落ちる｡ドロップダウンを
    /// 実装した後は開いて同じ名前の bounds を viewport の内側で見つける｡
    #[gpui::test]
    fn the_thirteenth_list_is_reachable_at_429px(cx: &mut gpui::TestAppContext) {
        let lists: [(&str, &str); 13] = [
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
            ("9113", "Yet another list"),
        ];
        let (mut visual, timeline) = drawn(cx, fixture_with_lists(&["1"], &lists));
        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                view.sources = vec![crate::cache::TimelineSource::List("9113".to_string())];
                cx.notify();
            });
        });
        visual.simulate_resize(gpui::size(gpui::px(429.), gpui::px(700.)));
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        if let Some(trigger) = visual.debug_bounds("source-picker") {
            visual.simulate_click(trigger.center(), gpui::Modifiers::none());
            visual.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }
        let item = visual
            .debug_bounds("tab-list-9113")
            .expect("every list must be addressable by its own name");
        let viewport = visual.update(|window, _| window.viewport_size());
        assert!(
            item.right() <= viewport.width,
            "the selected list is not reachable at 429px: {item:?} in {viewport:?}"
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

    /// #164 (#192/#43 でメニューへ移動): client を持つウィンドウは
    /// list fetch のボタンを出す — ただし今はツールバーではなく開いた
    /// メニューの末尾 (セパレータの後)｡閉じた状態のトリガーは幅を食わない｡
    ///
    /// このボタンが実際に描かれる唯一の場所はサインイン済みの live ウィンドウ
    /// だが､それはどのテストにも構築できない — そこでここでは fixture の
    /// ウィンドウに client を渡し (トークンの文字列｡`XClient::new` は何も
    /// 送らない)､描き直す｡これが無いと､ボタンの最初の描画がユーザーの最初の
    /// 起動になる｡「ボタンが無い」と報告されたのはそういう経緯だ｡
    #[gpui::test]
    fn a_signed_in_menu_offers_the_list_fetch_after_the_segments(cx: &mut gpui::TestAppContext) {
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

        assert!(
            visual.debug_bounds("load-lists").is_none(),
            "the closed trigger must not offer the fetch directly"
        );

        let trigger = visual
            .debug_bounds("source-picker")
            .expect("the trigger is always shown");
        visual.simulate_click(trigger.center(), gpui::Modifiers::none());
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let button = visual
            .debug_bounds("load-lists")
            .expect("a window with a client and a known user offers the fetch in the open menu");
        let last_segment = visual
            .debug_bounds("tab-list-9101")
            .expect("the fixture list is a segment");
        assert!(
            button.top() >= last_segment.bottom(),
            "the button sits after the picker's segments: {last_segment:?} then {button:?}"
        );
        assert!(
            button.size.width > gpui::px(0.0) && button.size.height > gpui::px(0.0),
            "the button has a size: {button:?}"
        );
    }

    /// #164 の 2 つ目の完了条件 (#43 でトグルへ拡張): すでにキャッシュ済みの
    /// source どうしを行き来しても何も送らない｡
    ///
    /// Home と list が 2 つ､すべて前もってキャッシュしてある (Home は空)｡
    /// ウィンドウは Home を外し､list を行き来するようにトグルされ (メニューは
    /// 項目クリックで閉じないので連続でクリックできる)､各クリックの後には
    /// きっかりキャッシュ済みの行を表示する｡client はまだ無く
    /// `last_reload_at` も動いていないので､何も出ていないし試みられても
    /// いない — `showing_new_posts_sends_nothing` が頼るのと同じ証拠だ｡
    #[gpui::test]
    fn toggling_between_cached_sources_sends_nothing(cx: &mut gpui::TestAppContext) {
        cache_home(&[]);
        cache_list("9111", &["12", "11"]);
        cache_list("9112", &["22", "21"]);
        let (mut visual, timeline) = drawn(
            cx,
            fixture_with_lists(&["1"], &[("9111", "first"), ("9112", "second")]),
        );

        let trigger = visual
            .debug_bounds("source-picker")
            .expect("the trigger is always shown");
        visual.simulate_click(trigger.center(), gpui::Modifiers::none());
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        for segment in [
            "tab-list-9111", // sources: Home, 9111
            "tab-home",      // sources: 9111
            "tab-list-9112", // sources: 9111, 9112
            "tab-list-9111", // sources: 9112
        ] {
            let bounds = visual
                .debug_bounds(segment)
                .expect("the segment has to be laid out before a click can reach it");
            visual.simulate_click(bounds.center(), gpui::Modifiers::none());
            cx.run_until_parked();

            cx.update(|cx| {
                timeline.update(cx, |view, _cx| {
                    assert!(view.client.is_none());
                    assert!(
                        view.last_reload_at.is_none(),
                        "a toggle between cached sources must not count as a fetch \
                         (after clicking {segment})"
                    );
                });
            });
            // 次の参照が､前のフレームが置いた場所ではなく今ある場所で区画を
            // 拾えるように描き直す｡
            visual.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(
                    view.sources,
                    vec![crate::cache::TimelineSource::List("9112".to_string())]
                );
                assert_eq!(shown_ids(view), ["22", "21"]);
            });
        });
    }

    /// #164: クリックは区画の上に落ち､切り替えは前の取得元に属していたものを
    /// リセットする — ここでは poll のバッファで､そうしなければ古いリストの
    /// post を新しいリストに被せて出してしまう｡
    /// #43: 区画は「切り替える」ではなく「トグルする」。list を足してから
    /// Home を外し、結局は #164 が確かめていたのと同じ単一選択の終着点
    /// (list だけ) へたどり着くことを確認する — メニューは項目クリックで
    /// 閉じないので、2 回続けてクリックできる。
    #[gpui::test]
    fn toggling_segments_changes_the_source_and_drops_the_old_buffer(
        cx: &mut gpui::TestAppContext,
    ) {
        cache_list("9121", &["32", "31"]);
        let mut fixture = fixture_with_lists(&["2", "1"], &[("9121", "Rust")]);
        fixture.pending = vec![item_with("3", "someone", None)];
        let (mut visual, timeline) = drawn(cx, fixture);

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(view.sources, vec![crate::cache::TimelineSource::Home]);
                assert!(view.pending.is_some(), "the fixture's buffer is waiting");
            });
        });

        let trigger = visual
            .debug_bounds("source-picker")
            .expect("the trigger is always shown");
        visual.simulate_click(trigger.center(), gpui::Modifiers::none());
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let list_item = visual
            .debug_bounds("tab-list-9121")
            .expect("the segment has to be laid out before a click can reach it");
        visual.simulate_click(list_item.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let home_item = visual
            .debug_bounds("tab-home")
            .expect("home stays addressable while the menu is open");
        visual.simulate_click(home_item.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(
                    view.sources,
                    vec![crate::cache::TimelineSource::List("9121".to_string())]
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

    /// #43: source を on/off するトグルは
    /// auto-refresh のループを再起動する｡ループは開始時点の `sources` を
    /// capture するので､再起動しないと off にした source を poll し続け､
    /// #43 の完了条件「オフのソースが API リクエストを消費しない」に
    /// 違反する｡ここでは「トグル後もループが生きている (`auto_refresh` が
    /// `Some`)」ことまでを押さえる — 具体的にどの source を poll するかは
    /// `apply_poll`/`lane::reload_all` の担当で、そちらは別のユニット
    /// テストで押さえてある｡
    #[gpui::test]
    fn toggling_a_source_restarts_the_auto_refresh_loop(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline) = drawn(cx, fixture_with_lists(&["1"], &[("9161", "Rust")]));
        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                view.client = Some(crate::x_api::XClient::new("token".to_string()));
                view.config.auto_refresh = true;
                cx.notify();
            });
        });
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let trigger = visual
            .debug_bounds("source-picker")
            .expect("the trigger is always shown");
        visual.simulate_click(trigger.center(), gpui::Modifiers::none());
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let list_item = visual
            .debug_bounds("tab-list-9161")
            .expect("the segment has to be laid out before a click can reach it");
        visual.simulate_click(list_item.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert!(
                    view.auto_refresh.is_some(),
                    "toggling a source must restart the auto-refresh loop"
                );
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

        let trigger = visual
            .debug_bounds("source-picker")
            .expect("the trigger is always shown");
        visual.simulate_click(trigger.center(), gpui::Modifiers::none());
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let segment = visual
            .debug_bounds("tab-list-9131")
            .expect("the segment has to be laid out before a click can reach it");
        visual.simulate_click(segment.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        let remembered = super::source_picker::load_selection(&paths.selection_file());
        assert_eq!(
            remembered.active,
            vec![
                super::source_picker::Selection::Home,
                super::source_picker::Selection::List {
                    id: "9131".to_string()
                }
            ]
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

        let trigger = visual
            .debug_bounds("source-picker")
            .expect("the trigger is always shown");
        visual.simulate_click(trigger.center(), gpui::Modifiers::none());
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let segment = visual
            .debug_bounds("tab-list-9151")
            .expect("the segment has to be laid out before a click can reach it");
        visual.simulate_click(segment.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert!(
                    shown_ids(view).iter().any(|id| id == "51"),
                    "the toggle itself still happens"
                );
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
    fn clicking_the_only_showing_segment_changes_nothing(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline) =
            drawn(cx, fixture_with_lists(&["2", "1"], &[("9141", "Rust")]));

        let trigger = visual
            .debug_bounds("source-picker")
            .expect("the trigger is always shown");
        visual.simulate_click(trigger.center(), gpui::Modifiers::none());
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let home = visual
            .debug_bounds("tab-home")
            .expect("Home is always a segment");
        visual.simulate_click(home.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                // 非空 invariant: 唯一の source は外せない｡
                assert_eq!(view.sources, vec![crate::cache::TimelineSource::Home]);
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
    /// 入口は #248 でメニューの `SyncList` になったので､メニューと同じ
    /// action を dispatch する (#146 層 3)｡cancel は `simulate_click` で
    /// hit test を通る (#184 の `Addressable`)｡金のための assert は cancel
    /// のあとに status が動いていないことで､`start_sync` は必ず status を
    /// 書き換える (資格情報の無いこのウィンドウでは gate へ)｡
    ///
    /// 閉じたことは `debug_bounds` では見られない｡gpui 0.2.2 の
    /// `Frame::clear` はあの map を消さないので､一度描かれた名前は最後の
    /// bounds を返し続ける｡言えるのは「一度も描かれていない」までなので､
    /// 閉じたことは `pending_sync` で見る｡footer に入口が無いことは､まさに
    /// その「一度も描かれていない」で見る｡
    ///
    /// 下の `a_stopped_sync_opens_a_dialog_that_offers_no_way_to_spend` が
    /// `sync-confirm` の `None` を見られるのは､あちらのウィンドウがそのボタンを
    /// 一度も描いていないから｡
    #[gpui::test]
    fn the_menu_opens_the_dialog_and_cancel_closes_it_without_spending(
        cx: &mut gpui::TestAppContext,
    ) {
        let (window, timeline) = fixture_window(cx, fixture_with_sync(&["2", "1"], 7));

        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert!(
            visual.debug_bounds("sync-open").is_none(),
            "the footer no longer carries the way in (#248)"
        );
        visual.update(|window, cx| {
            window.dispatch_action(Box::new(crate::menu::SyncList), cx);
        });
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
            window.dispatch_action(Box::new(crate::menu::SyncList), cx);
        });
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
            theme: theme::ThemeMode::Light,
            log_level: crate::log::Level::default(),
            post_resource_price: 0.005,
            daily_post_budget: 1000,
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
