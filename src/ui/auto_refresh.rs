//! ウィンドウが自分の timeline をいつポーリングし､返ってきたものを
//! どう扱うか (#21)｡
//!
//! [`super::reload_policy`] と同じく `ui` から切り出した (#126) が､線の
//! 引き方は違う｡あちらは判断を持ち､動くところは `ui` に任せる｡こちらは
//! #21 の全体 — 純粋な判断 *と* それに基づいて動くループ — を､呼び出す
//! 関数の下に `impl TimelineView` ブロックとして持つ｡タイマーと､バッファ
//! と､バッファが空になる 2 通りの経路は 1 つの機構であり､その半分が
//! `ui` の他の 3000 行の下に綴じられていたら､それは誰も見つけない半分
//! だ｡
//!
//! 分割が今も買っているのは､そもそもの狙いだ: `impl` より上はすべて純粋
//! なので､auto-refresh を安くも高くもする判断を gpui 抜きでユニット
//! テストできる｡
//!
//! #22 は､ポーリングの post が画面に届く 3 つ目の経路を足した: 読み手が
//! すでに最上部にいるとき､[`follows`] はバッファを飛ばしてそのまま滑り
//! 込ませる — [`TimelineView::follow`] を見よ｡それ以外の読み手にとっては､
//! バッファと pill が引き続き経路のままだ｡
//!
//! # なぜこれが `since_id` ポーリングではないのか
//!
//! #21 は home timeline 向けに書かれた｡あちらなら差分取得は `since_id`
//! ひとつで済む｡#161 がウィンドウを List に載せ替え､
//! `GET /2/lists/:id/tweets` は `since_id` をまったく受け付けない —
//! `XClient::list_timeline` を見よ｡これより安いリクエストは無い:
//! ポーリングは先頭ページを読み直すか､走らないかのどちらかだ｡
//!
//! 聞こえるほど悪くはない｡read は返った resource ごとに課金され､UTC の
//! 1 日の中で重複排除される (`x-api-budget` スキルを見よ)｡だから午後中
//! 同じ先頭ページを読み直しても､課金されるのは本当に新しかった post
//! だけで､それはどう届こうと読むのにかかる分と変わらない｡繰り返し課金
//! されるのは UTC の各深夜のあとの先頭ページ 1 回分で､`max_results` が
//! 上限になる｡
//!
//! そこでここの設計は､リクエストではなく別のところに気を遣う: リクエスト
//! が持ち帰ったもので読み手を邪魔しないことに｡ポーリングは読み手が読んで
//! いる途中のものを決して置き換えない｡マージ済みの timeline を [`Pending`]
//! バッファに預け､ウィンドウはそれを読み手が押せる件数として差し出す —
//! #21 自身の言い回しだ — ただし読み手が follow を入れたまま最上部に座って
//! いる場合 (#22) は別だ｡そこでは「読んでいるものを動かすな」と「いちばん
//! 新しいものを見せろ」は同じ指示になる｡

// リストではなく `super::*` を使う｡[`super::render`] に合わせた: 下の
// `impl` ブロックは `ui` が import するものの大半に手を伸ばすし､2 つの
// 子モジュールの前置きを同じ形に保つことのほうが､メソッドが出入りする
// たびに書き換えないといけない正確なリストより価値がある｡
use super::lane;
use super::*;

/// 読み手が始めた fetch がまだ飛んでいるとき､tick が次に見るまで待つ
/// 長さ｡
///
/// 短いのは､これが cadence ではなく再確認だからだ｡走っている fetch は
/// どれであれ `last_reload_at` を自分の開始時刻に動かし済みなので､次の
/// tick はそこから丸ごと 1 interval を計算する｡ここで数秒待った結果として
/// 二重にポーリングすることは無い｡
const BUSY_RECHECK_SECONDS: i64 = 5;

/// 終わった 1 回のポーリングのあと､ループが続くか終わるか (#239)｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Poll {
    Continue,
    /// 繰り返しても同じ答えしか返らない拒否だった｡[`halting_reason`] を見よ｡
    Halt,
}

/// auto-refresh ループの 1 回の起床が何をすべきか｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tick {
    /// まだ期限が来ていない｡この unix 時刻まで眠って判断し直す —
    /// ループはこの期限を信じるのではなく時計を読み直すので､期限を
    /// 寝過ごしたマシンは起きたときに素直にポーリングする｡
    Wait { until: i64 },
    /// 今ポーリングに支払う｡
    Poll,
}

/// [`next_tick`] が判断の材料にするものすべて｡
#[derive(Debug, Clone, Copy)]
pub(super) struct Situation {
    /// 種類を問わず最後の fetch が出ていった時刻 — ボタン､ショートカット､
    /// 前回のポーリング｡このセッションで何も取っていなければ `None` で､
    /// 起動時にキャッシュが当たった場合がそれだ: なぜ即ポーリングせず
    /// `started_at` に落とすかは [`next_tick`] を見よ｡
    pub last_reload_at: Option<i64>,
    /// ループが始まった時刻｡最初のポーリングの起点にする｡
    ///
    /// 起床ごとに計算する「今から 1 interval 後」ではなく､固定の
    /// timestamp にする: 後者は時計とともに動くので､期限はループが
    /// 近づくのとまったく同じ速さで遠ざかり､最初のポーリングは永遠に
    /// 来ない｡
    pub started_at: i64,
    pub interval_seconds: u32,
    /// すでに fetch が飛んでいるかどうか — [`BUSY_RECHECK_SECONDS`] を見よ｡
    pub busy: bool,
    /// 読み手が画面の前にいるかどうか (#204)｡ロックされた画面の向こうに
    /// 届く post には誰も気づかないので､この 1 ビットが他のすべてに
    /// 優先する｡どう知るかは [`crate::activity`] を見よ｡
    pub activity: Activity,
    /// 読み手が戻ってきたと分かった時刻 (#204)｡ロックが解けたことに
    /// 気づいた瞬間か､マシンが sleep から戻った瞬間で､まだ一度も
    /// 離れていなければ `None`｡
    ///
    /// なぜこれが anchor に混ざるのかは
    /// [`crate::activity::Presence::resumed_at`] を見よ｡
    pub resumed_at: Option<i64>,
}

/// この起床が何をすべきか｡
///
/// 起点は `last_reload_at`､無ければ `started_at`｡これが auto-refresh を
/// *cadence* に留め､アプリがどちらの端で支払う額をも変えないようにして
/// いる:
///
/// - 手動の reload は次のポーリングを丸ごと 1 interval 先へ押しやるので､
///   ボタンを押すことが数秒後のポーリングまで買うことにはならない｡
/// - キャッシュが答えたので何も支払わなかった起動 (#9) は､その後も
///   1 interval は何も支払わない｡auto-refresh は開けっぱなしのウィンドウ
///   にリズムを足すもので､起動時の判断への second opinion ではない｡
///
/// ロックされた画面は他のすべてに優先する (#204)｡そこには開けっぱなしの
/// ウィンドウが無いので､足すリズムも無い｡`busy` より先に見るのは､
/// 飛んでいる fetch を待つ理由がそもそも無いからだ — 待った先で
/// ポーリングするわけではない｡
pub(super) fn next_tick(situation: &Situation, now: i64) -> Tick {
    if matches!(situation.activity, Activity::Away) {
        return Tick::Wait {
            until: now.saturating_add(activity::AWAY_RECHECK_SECONDS),
        };
    }
    if situation.busy {
        return Tick::Wait {
            until: now.saturating_add(BUSY_RECHECK_SECONDS),
        };
    }
    let due = poll_due_at(situation);
    if due > now {
        Tick::Wait { until: due }
    } else {
        Tick::Poll
    }
}

/// 次のポーリングが期限を迎える時刻 — [`next_tick`] が `Poll` と答え
/// 始める瞬間で､footer のカウントダウン (#214) が数えるのもこれだ｡
///
/// 起点は `last_reload_at`､無ければ `started_at`｡どちらより後でも読み手が
/// 戻ってきた時刻 (#204) が勝つ｡規則の理由は [`next_tick`] の doc にある｡
/// ここに切り出したのは､ループと footer が別々の計算を持つと､数字が 0 に
/// なってもポーリングが来ないか､来たのに数字が残るかのどちらかになる
/// からだ｡
pub(super) fn poll_due_at(situation: &Situation) -> i64 {
    situation
        .last_reload_at
        .unwrap_or(situation.started_at)
        .max(situation.resumed_at.unwrap_or(i64::MIN))
        .saturating_add(i64::from(situation.interval_seconds))
}

/// ポーリングが取ってきた､まだ読み手に見せていない post (#21)｡
#[derive(Debug)]
pub(super) struct Pending {
    /// ポーリングが返してきたマージ済み timeline の全体で､新しい行だけ
    /// ではない — `cache::reload_primary` はキャッシュと新しいバッチを
    /// 継ぎ合わせて返すし､読み手が求めたときに表示すべきなのはその結合
    /// 済みのリストだ｡新しい行だけを持っていたら､"Load older" が足した
    /// ものをすべて落としてしまう｡
    pub items: Vec<TimelineItem>,
    /// そのうち画面にあるものと比べて新しいのが何件か｡pill が数えるのは
    /// これで､ゼロには決してならない — [`pending_after_poll`] を見よ｡
    pub count: usize,
}

/// 終わったポーリングが読み手のために残すもの｡
///
/// `None` はポーリングが新着を見つけなかったということで､これが普通の
/// 結果であり､画面をまったく触らないでいなければならない: pill も
/// バナーも scroll も無し｡数分おきに "no new posts" と報告するポーリング
/// は読み手が頼んでいないノイズだ｡自分で押した reload (#141) はそう言う
/// が､それはまさに答えを待っているからで､こちらとは違う｡
///
/// 数え方は [`newly_arrived`] — 手動 reload 自身の件数と scroll の補正が
/// 使うのと同じ先頭連続の規則なので､pill が押して実際に現れるより多くの
/// post を約束することは決してない｡
pub(super) fn pending_after_poll(
    displayed: &[&str],
    incoming: Vec<TimelineItem>,
) -> Option<Pending> {
    let incoming_ids: Vec<&str> = incoming.iter().map(|item| item.id.as_str()).collect();
    let count = newly_arrived(displayed, &incoming_ids);
    if count == 0 {
        return None;
    }
    Some(Pending {
        items: incoming,
        count,
    })
}

/// 失敗したポーリングを繰り返す意味があるか (#239)｡`Some` を返したら
/// ループはそこで終わり､返した文言が常設バナーになる｡
///
/// #239 のログはこれが無かった姿だ: 3 分ごとの 403 が 30 行､続いて 401 が
/// 100 行以上｡どれも同じ理由で拒まれ続けていて､次の 1 回が違う答えを持ち
/// 帰る見込みはどこにも無かった｡
///
/// 止めるのは **X が答えたうえで断った** 場合だけだ｡ネットワークの瞬断も
/// 5xx も普通の rate limit も `None` を返す — そのどれも次の tick には
/// 直っていておかしくないし､ポーリングの失敗は黙って捨てるという
/// [`TimelineView::apply_poll`] の約束は､そちらにはそのまま当てはまる｡
pub(super) fn halting_reason(error: &anyhow::Error) -> Option<String> {
    if let Some(denied) = error.downcast_ref::<Denied>() {
        return Some(match denied.denial {
            // 401 が生き残るのは､更新した token まで拒まれたときだけだ
            // (#239 の Session が期限前に更新する)｡つまりセッションそのもの
            // が死んでいる｡
            Denial::Rejected => "X no longer accepts this sign-in session, so auto-refresh \
                 has stopped. Click \"Sign in with X\" to start a new session."
                .to_string(),
            Denial::Forbidden => format!(
                "X refused the auto-refresh poll ({}), so it has stopped: {}. Check the \
                 monthly spend cap and this app's permissions in the X developer portal, \
                 then restart twigpui.",
                denied.endpoint.key(),
                denied.detail
            ),
        });
    }
    if let Some(expired) = error.downcast_ref::<oauth::SessionExpired>() {
        return Some(format!(
            "auto-refresh has stopped because the X sign-in session could not be renewed \
             ({}). Click \"Sign in with X\" to start a new session.",
            expired.detail
        ));
    }
    if let Some(cap) = error.downcast_ref::<rate_limit::UsageCapExceeded>() {
        return Some(format!(
            "auto-refresh has stopped because the X API credit is used up: {}. Top up the \
             balance, then restart twigpui.",
            cap.detail
        ));
    }
    None
}

/// pill が言うこと｡
///
/// "(s)" ではなく単数形と複数形を書き分ける｡これは
/// `reload_policy::reload_outcome_label` に合わせたもので､意図的に
/// それと同じように読めるようにしてある: 2 つは post が届きうる 2 つの
/// 方向から同じ事実を報告しているので､読み手はいま自分がどちらを見て
/// いるのかに気づかされる必要が無い｡
pub(super) fn pending_label(count: usize) -> String {
    match count {
        1 => "1 new post".to_string(),
        n => format!("{n} new posts"),
    }
}

/// 厳密な最上部からどれだけ離れていても「最上部」と読めるか (#22)､
/// 単位は pixel｡ゼロではない: トラックパッドの弾きは offset をわずかに
/// 届かないところに残しうるし､その読み手は自分が最上部にいると思って
/// いる — 半 pixel で pill が出たら follow が壊れて見える｡
const AT_TOP_TOLERANCE_PX: f32 = 2.0;

/// 読み手が timeline の最上部にいるかどうか (#22)｡
/// `ScrollHandle::logical_scroll_top` の 2 つ組の答え — viewport の上端の
/// 下にある行の index と､その行のどこまで上端が入り込んでいるか — から
/// 決める｡
pub(super) fn at_top(top_item: usize, offset_in_item: gpui::Pixels) -> bool {
    top_item == 0 && f32::from(offset_in_item).abs() <= AT_TOP_TOLERANCE_PX
}

/// 最上部に貼り付く follow のための `TimelineView` の実行時スイッチ
/// (#22): `config.follow_new_posts` を種にし､View メニューで反転し､
/// ファイルへ書き戻すことは決してない — config が常設の設定で､こちらは
/// 今日の分だ｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FollowMode {
    /// 読み手が最上部にいるとき､ポーリングの新着 post がそのまま流れ込む｡
    Follow,
    /// scroll 位置に関わらず､どのポーリングも pill の後ろで待つ｡
    Pill,
}

impl FollowMode {
    /// `config.follow_new_posts` が種にするモード｡
    pub(super) fn from_config(follow_new_posts: bool) -> Self {
        if follow_new_posts {
            Self::Follow
        } else {
            Self::Pill
        }
    }

    /// View メニューのトグルがすること｡
    pub(super) fn flipped(self) -> Self {
        match self {
            Self::Follow => Self::Pill,
            Self::Pill => Self::Follow,
        }
    }

    /// これがスイッチの [`Self::Follow`] 側かどうか｡
    pub(super) fn is_following(self) -> bool {
        matches!(self, Self::Follow)
    }
}

/// ポーリングの新着 post が pill の後ろで待つのではなく､そのまま画面へ
/// 流れ込むべきかどうか (#22, #177)｡
///
/// 3 つ揃うか､さもなくば無しだ｡モードは読み手の常設の指示｡`loaded` は
/// `Failed`/`Loading` の画面が､誰も見たいと頼んでいないポーリングに黙って
/// 置き換えられるのを防ぐ｡そして `at_top` が「いちばん新しいものを見せろ」
/// と「ここを読んでいる」を分ける — `preserved_scroll_target` が反対側から
/// 引くのと同じ線だ｡
pub(super) fn follows(mode: FollowMode, loaded: bool, at_top: bool) -> bool {
    mode.is_following() && loaded && at_top
}

/// glide が新しい行を流し込む速さ (#208)､px/s｡
///
/// #22 の最初の glide は毎フレーム残り距離の 15% を進んでいた｡画面 1 枚
/// ぶんが 1 秒足らずで通り過ぎ､流れてくる行を目で追えなかった｡これは
/// 「読みながら流れる」ための速さで､post 1 件 (150px 前後) が 0.6 秒ほど
/// かけて視界へ降りてくる｡
const GLIDE_SPEED_PX_PER_S: f32 = 240.;

/// glide の最短時間 (#208)､秒｡数十 px の小さな到着でも一瞬で済ませず､
/// 動いたと分かるだけの時間をかける｡
const GLIDE_MIN_S: f32 = 0.6;

/// glide の最長時間 (#208)､秒｡何十件も一度に来たときに速さの計算どおり
/// 十数秒も歩かせない — その先は読み手が握って止めるより先に終わるべき
/// 長さだ｡
const GLIDE_MAX_S: f32 = 5.;

/// glide をやめて最後の 1 pixel 未満を吸着させてよいだけ最上部に近い
/// 距離 (#22)｡
const GLIDE_DONE_PX: f32 = 1.0;

/// `distance` px を歩く glide にかける時間 (#208)､秒｡距離に比例させ､
/// [`GLIDE_MIN_S`] と [`GLIDE_MAX_S`] で挟む｡向きは問わない｡
pub(super) fn glide_duration_s(distance: f32) -> f32 {
    (distance.abs() / GLIDE_SPEED_PX_PER_S).clamp(GLIDE_MIN_S, GLIDE_MAX_S)
}

/// `start` から歩き始めて `elapsed_s` 秒後に glide が置く scroll offset､
/// または glide が終わっていれば `None` (#22, #208)｡offset は gpui が
/// 持っているもので､最上部で 0､読み手が下へ行くほど負の方向に大きくなる｡
///
/// フレームの回数ではなく経過時間の関数なので､timer が遅れても位置が
/// 飛ぶだけで終点は変わらない (#175 の「実行環境によって終了位置が
/// 変わらない」)｡両端を緩める smoothstep で､動き出しも着地も急がない｡
pub(super) fn glide_y(start: f32, elapsed_s: f32) -> Option<f32> {
    if start.abs() <= GLIDE_DONE_PX {
        return None;
    }
    let duration = glide_duration_s(start);
    if elapsed_s >= duration {
        return None;
    }
    let t = elapsed_s / duration;
    let eased = t * t * (3. - 2. * t);
    Some(start * (1. - eased))
}

/// auto-refresh のうち純粋になれない半分: リクエストに支払うループと､
/// その答えをウィンドウがどう扱うか (#21)｡
///
/// [`super::reload_policy`] や [`super::render`] がデータ上の自由関数で
/// あるのと違い､子モジュールに置いた `impl` ブロックだ｡理由は #21 が
/// 1 つの機構 — タイマー､バッファ､そしてバッファが空になる 2 通りの
/// 経路 — であり､それをここの純粋なファイルと `ui` の 4 つのメソッドに
/// 割ったら､どちらの半分も単独では読めなくなるからだ｡子モジュールは親の
/// 非公開項目を見られるので､`TimelineView` のフィールドは `ui` に閉じた
/// ままで､これを可能にするために何かを広げることも無い｡
impl TimelineView {
    /// ウィンドウが開いている間､タイマーで timeline に新着 post を
    /// ポーリングする (#21)｡
    ///
    /// `config.auto_refresh` が off か､取得に使う client が無いときは､
    /// 何も spawn せずに返る｡この早期 return が #21 の「切ればアプリは
    /// 何も送らない」条件のすべてだ: このメソッドの他の部分には到達
    /// できないので､発火しないと信じるべき生き残ったタイマーは存在しない｡
    ///
    /// [`Self::start`] から､そして [`Self::sign_in`] からも始まる｡
    /// `start_auto_sync` と同じ 2 か所で､理由も同じ — client はその
    /// どちらかの後にしか存在しない｡代入し直すと走っていたループは
    /// キャンセルされるので､サインインし直してもループは 2 つでなく 1 つ｡
    ///
    /// tick が何を決めるかは [`auto_refresh::next_tick`] の担当､その結果を
    /// どう扱うかは [`pending_after_poll`] の担当で､どちらも純粋で隣で
    /// テストされている｡ここに残るのは純粋になれない部分だ:
    /// リクエストに支払い､答えをどこかに置くこと｡
    pub(super) fn start_auto_refresh(&mut self, cx: &mut Context<'_, Self>) {
        /// `timer` 1 回が待つ最長時間｡マシンが眠る前に計算した期限を
        /// 信じるのではなくループが時計を読み直すため —
        /// `start_auto_sync` の定数と同じで､理由も同じ｡
        const MAX_SLEEP_SECONDS: i64 = 60;
        /// 起床の最短間隔｡ループがキャンセル可能なままでいられるように｡
        const MIN_SLEEP_SECONDS: i64 = 1;

        // #214: 前のループの期限も同じく｡早期 return より前に消すのは､
        // 止まったループの下でカウントダウンだけが残らないようにするため｡
        self.refresh_situation = None;
        if !self.config.auto_refresh {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };

        // #239: 前のループが止まった理由は､新しいループより長生きできない｡
        // ここへ来る 2 つ目の経路は再サインインで､それはまさに 401 で
        // 止まったループへの答えだ｡
        self.auto_refresh_notice = None;

        let paths = self.paths.clone();
        // #43: N ソースなら 1 tick で N request になる (`lane::reload_all`
        // が直列に呼ぶ)。`Endpoint::ListTimeline` は全 list id で 1
        // バケット共有なので、on にする本数が多いほどそのバケットを速く
        // 消費する — 上限は設けていない (ponytail、opus-advisor B-8)。
        let sources = self.sources.clone();
        let max_results = self.config.max_results;
        let interval_seconds = self.config.auto_refresh_interval_seconds;
        let started_at = oauth::unix_now();
        log::info(&format!(
            "auto-refresh is on, polling every {interval_seconds}s"
        ));

        self.auto_refresh = Some(cx.spawn(async move |this, cx| {
            let mut presence = activity::Presence::present();

            loop {
                // 画面がロックされているかを尋ねるのに `ioreg` を spawn
                // するので､main thread ではなく background で待つ (#204)｡
                let probed = cx
                    .background_executor()
                    .spawn(async { activity::probe() })
                    .await;
                let now = oauth::unix_now();
                let activity = presence.observe(probed, now, interval_seconds);

                // `Err` はウィンドウが消えたということで､このループが
                // 終わる唯一の理由だ — `start_auto_sync` の約束｡
                let Ok(situation) = this.update(cx, |this, _| {
                    let situation = Situation {
                        last_reload_at: this.last_reload_at,
                        started_at,
                        interval_seconds,
                        busy: this.reloading,
                        activity,
                        resumed_at: presence.resumed_at(),
                    };
                    // #214: footer がこの判断を数え直せるように写す｡
                    // `notify` はしない — 数字を進めるのは countdown の
                    // ticker で､こちらは 1 分に 1 回しか起きない｡
                    this.refresh_situation = Some(situation);
                    situation
                }) else {
                    return;
                };

                let sleep_until = match next_tick(&situation, now) {
                    Tick::Wait { until } => until,
                    Tick::Poll => {
                        // `reload` とまったく同じく､リクエストが出ていく
                        // 前に記録する: fetch はもう決まったので､返って
                        // くるかどうかに関わらず､それが次の interval を
                        // 測る起点になる｡
                        let _ = this.update(cx, |this, _| this.last_reload_at = Some(now));

                        let result = {
                            let (paths, client, sources) =
                                (paths.clone(), client.clone(), sources.clone());
                            cx.background_executor()
                                .spawn(async move {
                                    lane::reload_all(
                                        &paths,
                                        &client,
                                        &sources,
                                        max_results,
                                        oauth::unix_now(),
                                    )
                                })
                                .await
                        };

                        // #239: `Err` はウィンドウが消えたということで､
                        // 下の `Halt` と同じくループを終える｡
                        let Ok(poll) = this.update(cx, |this, cx| this.apply_poll(result, cx))
                        else {
                            return;
                        };
                        if poll == Poll::Halt {
                            return;
                        }
                        now.saturating_add(i64::from(interval_seconds))
                    }
                };

                let wait = sleep_until
                    .saturating_sub(oauth::unix_now())
                    .clamp(MIN_SLEEP_SECONDS, MAX_SLEEP_SECONDS);
                // 期限は `sleep_until` ではなくここで読み直した時計から
                // 測る (#204)｡上の clamp は待つ長さを切り詰めるので､
                // 2 つは普段から食い違っている｡
                let expected_wake_at = oauth::unix_now().saturating_add(wait);
                cx.background_executor()
                    .timer(Duration::from_secs(u64::try_from(wait).unwrap_or(1)))
                    .await;
                presence.woke(expected_wake_at, oauth::unix_now(), interval_seconds);
            }
        }));
        // #214: ループが最初に起きた瞬間から footer が数えられるように｡
        self.start_countdown_ticker(cx);
    }

    /// 終わったポーリングがウィンドウに対してすること (#21)｡
    ///
    /// 意図的に静かだ｡ポーリングは読み手が頼んだものではないので､画面を
    /// 取ってはいけない: `state` は触らない､scroll 位置も触らない､
    /// そして `reload_notice` — カウントダウンを含め､読み手自身の最後の
    /// reload のものだ — をここで書くことは決してない｡成功したポーリングに
    /// できるのは `pending` を埋めることだけで､それを差し出すのは pill､
    /// 他に動くものは無い｡
    ///
    /// 失敗したポーリングはもっと何もしない: ログに出して捨てる｡reload の
    /// 経路がバナーを上げるのは､答えを聞こうと待っている人がいるからだ｡
    /// こちらを待っている人はいないし､数分前のネットワークの瞬断は､
    /// 問題無い timeline の上に赤い行を出すほどのものではない｡`usage` は
    /// どちらにせよ更新する — parse できたかどうかに関わらず､リクエストは
    /// 送られて課金されている｡
    ///
    /// `next_page_token` は意図的に更新しない｡これは "Load older" の
    /// カーソルで､読み手がどこまで遡ったかを表す｡背後で取った先頭ページ
    /// は､scroll の途中でそれを巻き戻してしまう｡
    ///
    /// 例外は 1 つだけで､#239 が足した: [`halting_reason`] が「次の 1 回も
    /// 同じ答えだ」と言う拒否なら､ループを止めてバナーを出す ([`Poll::Halt`])｡
    /// 上の「黙って捨てる」が守っているのは一時的な失敗で読み手を煩わせない
    /// ことであり､**取得が止まったこと自体を隠すこと** ではない｡
    pub(super) fn apply_poll(
        &mut self,
        result: anyhow::Result<lane::ReloadOutcome>,
        cx: &mut Context<'_, Self>,
    ) -> Poll {
        self.refresh_usage(cx);
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                // `log::redact` は出ていく途中で走る — API のエラーは
                // それを生んだリクエストを引用しうる｡
                log::error(&format!("auto-refresh poll failed: {error:#}"));
                let Some(reason) = halting_reason(&error) else {
                    return Poll::Continue;
                };
                log::warn(&format!("auto-refresh stopped: {reason}"));
                self.auto_refresh_notice = Some(SharedString::from(reason));
                // #214: 来ないポーリングを数え続けない｡
                self.refresh_situation = None;
                cx.notify();
                return Poll::Halt;
            }
        };
        // #43: `outcome.me` は常に解決済み (`ReloadOutcome` の doc を見よ)｡
        // 部分失敗はここでは無視して静かに続ける — `apply_poll` の doc が
        // 言うとおり poll は失敗を画面に出さない｡
        //
        // ヘッダーはサインイン中のアカウントを名指しし､いくつかの操作は
        // その id を必要とする｡ポーリングはどちらもただで解決するので､
        // 起動時の fetch が埋められなかったなら､ここで埋めてしまってよい｡
        self.home_user_id = Some(outcome.me.id.clone());
        self.home_username = Some(outcome.me.username);

        let composed = lane::load_composite_timeline(&self.paths, &self.sources, &outcome.me.id);
        self.item_provenance = composed.provenance;

        let displayed: Vec<&str> = match &self.state {
            TimelineState::Loaded(items) => items.iter().map(|item| item.id.as_str()).collect(),
            _ => Vec::new(),
        };
        let Some(pending) = pending_after_poll(&displayed, composed.items) else {
            // 新着無し｡notice すら出さない — このメソッドの doc を見よ｡
            return Poll::Continue;
        };
        self.present_poll(pending, cx);
        Poll::Continue
    }

    /// ポーリングの新着 post が画面上で何になるか (#21, #22): 流し込みか､
    /// 差し出しか｡どちらかを決めるのは [`follows`] — スイッチを入れたまま
    /// 最上部にいる読み手には [`Self::follow`]､それ以外には pill｡そして
    /// ポーリングは決して画面を取らないという [`Self::apply_poll`] の doc
    /// は､その人たちにはそのまま一言一句当てはまる｡
    ///
    /// pill のバッファのために画像を先読みすることはしない｡
    /// `refresh_avatars`/`refresh_media` は何が足りないかを `self.state`
    /// を読んで決めるし､どちらも代入するとキャンセルされる単一の task
    /// スロットを持つ — バッファの画像を先にダウンロードするなら､別の
    /// ところから読むよう教えるか､表示中の timeline 自身のダウンロードを
    /// タイマーでキャンセルするかのどちらかになる｡[`Self::apply_pending`]
    /// は行が実際に画面に出た瞬間に取る｡手動 reload がすでに持っているのと
    /// 同じ経路､同じ短いプレースホルダーだ｡
    pub(super) fn present_poll(&mut self, pending: Pending, cx: &mut Context<'_, Self>) {
        let (top_item, offset_in_item) = self.list_scroll.logical_scroll_top();
        let loaded = matches!(self.state, TimelineState::Loaded(_));
        if follows(self.follow, loaded, at_top(top_item, offset_in_item)) {
            self.follow(pending, cx);
        } else {
            self.pending = Some(pending);
            cx.notify();
        }
    }

    /// 読み手が最上部にいる画面へ､ポーリングの新着 post を流し込む
    /// (#22) — バッファが空になる 3 つ目の経路であり､バッファを丸ごと
    /// 飛ばす唯一の経路｡
    ///
    /// 置き換えそのものは何も動かさない: viewport の上端の下にあった行は
    /// 新しいリストでは index `count` にあり､それを最上部へ戻して駐める
    /// ことで到着が見えなくなる｡そのあと読み手が見るのは glide — 新しい行
    /// が目で追える速さで視界へ滑り降りてくる｡それが #177 の "always
    /// flowing" の印象で､ポーリングがすでに支払った post でできている｡
    fn follow(&mut self, pending: Pending, cx: &mut Context<'_, Self>) {
        let count = pending.count;
        // 前のポーリングが駐めたバッファはこれより古く､しかも今まさに
        // 置き換えられる timeline を基準に測られている｡
        self.clear_pending();
        let nothing_was_kept = count == pending.items.len();
        self.state = TimelineState::Loaded(pending.items);
        if nothing_was_kept {
            // どの行も新しい — 空の List が初めて埋まるか､重なりの無い
            // 先頭ページか｡その場に留めるべき行が無いので､下の補正は
            // リストの末尾より後ろの index を名指しすることになる｡gpui は
            // 解決できない anchor を *保持* して prepaint のたびに再試行
            // するし､後の "Load older" がリストをその index より伸ばせば
            // viewport が読み手の下で飛ぶ｡代わりに glide 無しで最上部に
            // 着地する: glide は読んでいる行より上の行を見せることであり､
            // ここにはそんな行が無い｡
            self.list_scroll.scroll_to_top_of_item(0);
        } else {
            self.list_scroll.scroll_to_top_of_item(count);
            // #206: 新しい行は全部 viewport の上に駐まっている｡glide が
            // 1 行降ろすたびに `note_scroll_position` が減らす｡
            self.unseen = count;
            self.start_glide(cx);
        }
        self.refresh_images(cx);
        cx.notify();
    }

    /// scroll offset を 1 フレームずつ最上部まで歩いて戻す (#22)｡
    ///
    /// 歩く距離は､これが呼ばれた時点ではまだそこに無い:
    /// [`Self::follow`] の `scroll_to_top_of_item` は次の prepaint で
    /// 着地する｡だからループは最初の数フレームを､offset がゼロから
    /// 動くのを待つのに使う｡回数には上限があり､決して着地しない補正
    /// (空のリスト､描画をやめたウィンドウ) は､ハングではなく pill が
    /// やるのと同じ吸着に落ちる｡
    ///
    /// どのステップも､offset が今どこにあるかを前のステップが置いた
    /// ところと比べる｡差があれば読み手がホイールを回しているということで､
    /// glide はスクロールバーを取り合うのではなく読み手が置いたところで
    /// 止まる — [`Self::apply_poll`] に置き換えではなくバッファを選ばせた
    /// のと同じ譲り方だ｡ホイールの経路 (#175) は glide を drop する
    /// ことでも同じ結果を先に出す; ここの比較はその裏の保険である｡
    ///
    /// 時刻は壁時計ではなくフレームごとに [`scroll::FRAME_S`] を足して
    /// 数える (#208)｡テストの executor は timer の時計だけを進めるので､
    /// `Instant` で測ると 1 フレームが数マイクロ秒になり glide が永遠に
    /// 終わらない｡
    fn start_glide(&mut self, cx: &mut Context<'_, Self>) {
        /// 補正が決して着地しないと結論づけるまでに､何フレーム待つか｡
        const SETTLE_FRAMES: u8 = 10;
        /// glide が置いたところから offset がどれだけ離れていたら読み手の
        /// scroll と読むか､単位は pixel｡
        const GRAB_PX: f32 = 1.0;

        self.glide = Some(cx.spawn(async move |this, cx| {
            let frame = Duration::from_secs_f32(scroll::FRAME_S);
            for _ in 0..SETTLE_FRAMES {
                cx.background_executor().timer(frame).await;
                // `Err` はウィンドウが消えたということ — ここも以下も
                // `start_auto_refresh` の約束｡
                let Ok(settled) = this.update(cx, |this, _| {
                    f32::from(this.list_scroll.offset().y).abs() > GLIDE_DONE_PX
                }) else {
                    return;
                };
                if settled {
                    break;
                }
            }
            let Ok(start) = this.update(cx, |this, _| f32::from(this.list_scroll.offset().y))
            else {
                return;
            };
            let mut elapsed_s = 0.0_f32;
            let mut last_set: Option<f32> = None;
            loop {
                let Ok(done) = this.update(cx, |this, cx| {
                    let offset = this.list_scroll.offset();
                    let y = f32::from(offset.y);
                    if let Some(expected) = last_set
                        && (y - expected).abs() > GRAB_PX
                    {
                        return true;
                    }
                    if let Some(next) = glide_y(start, elapsed_s) {
                        this.list_scroll.set_offset(gpui::point(offset.x, px(next)));
                        this.note_scroll_position();
                        last_set = Some(next);
                        cx.notify();
                        false
                    } else {
                        this.list_scroll.set_offset(gpui::point(offset.x, px(0.)));
                        // 最上部に着いた｡上に残っている行は無い (#206)｡
                        this.unseen = 0;
                        cx.notify();
                        true
                    }
                }) else {
                    return;
                };
                if done {
                    return;
                }
                cx.background_executor().timer(frame).await;
                elapsed_s += scroll::FRAME_S;
            }
        }));
    }

    /// 最後のポーリングが取ってきたものを見せる (#21)｡
    ///
    /// pill がする唯一のこと､そしてそのすべて: timeline をバッファ済みの
    /// リストで置き換え､読み手をその最上部に置く｡いま見せろと言われた
    /// post があるのはそこだ｡
    ///
    /// reload に対して [`Self::keep_the_reader_in_place`] がやるような
    /// 補正ではなく最上部へ scroll するのは､2 つが正反対の要求に答えて
    /// いるからだ｡reload は「ここを読んでいる､これを更新しろ」であり､
    /// 新着件数を数える pill を押すのは「それを見せろ」だ｡読み手を元の
    /// ところにそのまま置いたら､見た目には何もしないボタンになる｡
    ///
    /// `ReloadNotice::Outcome` は上げない: pill がすでに何件あったかを
    /// 言っているし､pill が消えた瞬間にその件数を繰り返すバナーは､同じ
    /// 事実を 2 度言うことになる｡
    pub(super) fn apply_pending(&mut self, cx: &mut Context<'_, Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        // まだ歩いている glide は､これが置き換えるリストを基準に測った
        // offset を狙っている (#22)｡上に残っていた行もこれで視界に入る
        // (#206)｡
        self.glide = None;
        self.unseen = 0;
        self.state = TimelineState::Loaded(pending.items);
        self.list_scroll.scroll_to_top_of_item(0);
        self.refresh_images(cx);
        cx.notify();
    }

    /// ポーリングが待たせていたものを捨てる (#21)｡
    ///
    /// バッファより新しい source から `state` を置き換える経路すべてから
    /// 呼ばれる: 終わった reload､終わった "Load older"､削除､サインイン｡
    /// 古いバッファは単に時代遅れなのではなく､作業を巻き戻す形で誤って
    /// いる — 削除の前に取ったものを適用すれば削除した post が画面へ戻り､
    /// "Load older" の前に取ったものは､いま足したばかりのページを落とす｡
    ///
    /// 件数も誤る: もう表示されていない timeline を基準に測ったものなので､
    /// pill はすでに見えている post を約束することになる｡
    ///
    /// glide も同じ古さのために捨てる (#22): その offset は置き換えられる
    /// 行を基準に測ったものなので､歩かせ続ければ古い距離のぶんだけ新しい
    /// リストを scroll してしまう｡toast の countdown も同じ行を数えたもの
    /// なので一緒に捨てる (#206)｡
    pub(super) fn clear_pending(&mut self) {
        self.pending = None;
        self.glide = None;
        self.unseen = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn situation(last_reload_at: Option<i64>, started_at: i64) -> Situation {
        Situation {
            last_reload_at,
            started_at,
            interval_seconds: 300,
            busy: false,
            activity: Activity::Present,
            resumed_at: None,
        }
    }

    fn item(id: &str) -> TimelineItem {
        TimelineItem {
            id: id.to_string(),
            text: format!("post {id}"),
            created_at: None,
            author_name: String::new(),
            author_username: "someone".to_string(),
            reposted_by: None,
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
    fn the_first_poll_is_one_interval_after_the_window_opened() {
        assert_eq!(
            next_tick(&situation(None, 1_000), 1_000),
            Tick::Wait { until: 1_300 }
        );
    }

    // 期限は固定の瞬間でなければならない｡起床のたびに `now` から計算して
    // いたら､ループが近づくのと同じ速さで遠ざかり､auto-refresh は決して
    // 発火しないタイマーになる｡
    #[test]
    fn the_first_polls_deadline_does_not_move_as_the_loop_waits() {
        assert_eq!(
            next_tick(&situation(None, 1_000), 1_299),
            Tick::Wait { until: 1_300 }
        );
        assert_eq!(next_tick(&situation(None, 1_000), 1_300), Tick::Poll);
    }

    #[test]
    fn a_poll_is_due_once_the_interval_since_the_last_fetch_has_elapsed() {
        assert_eq!(next_tick(&situation(Some(1_000), 500), 1_300), Tick::Poll);
    }

    #[test]
    fn a_poll_is_not_due_before_the_interval_has_elapsed() {
        assert_eq!(
            next_tick(&situation(Some(1_000), 500), 1_299),
            Tick::Wait { until: 1_300 }
        );
    }

    // #10 の interval と #21 の cadence は､reload の値段について一致して
    // いなければならない: ボタンを押すのは fetch なので､直後にポーリング
    // が続くのではなく､次のポーリングを先へ押しやる｡
    #[test]
    fn a_manual_reload_pushes_the_next_poll_a_full_interval_out() {
        let mut situation = situation(Some(2_000), 500);
        situation.interval_seconds = 300;
        assert_eq!(next_tick(&situation, 2_001), Tick::Wait { until: 2_300 });
    }

    #[test]
    fn a_fetch_in_flight_defers_the_decision_rather_than_polling_beside_it() {
        let mut situation = situation(Some(1_000), 500);
        situation.busy = true;
        assert_eq!(
            next_tick(&situation, 9_000),
            Tick::Wait {
                until: 9_000 + BUSY_RECHECK_SECONDS
            }
        );
    }

    // --- #204: ロックされた画面と sleep ---

    // 「ロック中に何度 tick しても request が 0 回」｡期限をどれだけ過ぎて
    // いても `Poll` は返らないし､返らないので何も溜まらない｡
    #[test]
    fn a_locked_screen_never_becomes_due_however_long_it_stays_locked() {
        let mut situation = situation(Some(1_000), 500);
        situation.activity = Activity::Away;

        for now in [1_300, 2_000, 10_000, 1_000_000] {
            assert_eq!(
                next_tick(&situation, now),
                Tick::Wait {
                    until: now + activity::AWAY_RECHECK_SECONDS
                },
                "a locked screen must never poll, and {now} is well past the deadline"
            );
        }
    }

    // ロックは飛んでいる fetch より先に見る｡`busy` の再確認は数秒後に
    // ポーリングするために待つものだが､ロックされた画面にはその先が無い｡
    #[test]
    fn a_locked_screen_outranks_a_fetch_in_flight() {
        let mut situation = situation(Some(1_000), 500);
        situation.activity = Activity::Away;
        situation.busy = true;

        assert_eq!(
            next_tick(&situation, 9_000),
            Tick::Wait {
                until: 9_000 + activity::AWAY_RECHECK_SECONDS
            }
        );
    }

    // 「復帰後は最大 1 回だけ schedule される」｡ロックの間に interval は
    // 100 回ぶん過ぎているが､起点は最後の fetch ではなく復帰した瞬間だ｡
    #[test]
    fn coming_back_schedules_one_poll_an_interval_out_not_the_backlog() {
        let mut situation = situation(Some(1_000), 500);
        situation.resumed_at = Some(31_000);

        assert_eq!(next_tick(&situation, 31_000), Tick::Wait { until: 31_300 });
        assert_eq!(next_tick(&situation, 31_299), Tick::Wait { until: 31_300 });
        assert_eq!(next_tick(&situation, 31_300), Tick::Poll);

        // そしてその 1 回が `last_reload_at` を動かせば､次はまた丸ごと
        // 1 interval 先だ — 溜まっていた tick が続けて発火することは無い｡
        situation.last_reload_at = Some(31_300);
        assert_eq!(next_tick(&situation, 31_301), Tick::Wait { until: 31_600 });
    }

    // 復帰時刻は起点を **遅らせる** だけで､早めることはしない｡復帰した
    // 直後に読み手が自分で reload を押したなら､次のポーリングはその
    // reload から測る｡
    #[test]
    fn a_reload_after_coming_back_still_pushes_the_next_poll_out() {
        let mut situation = situation(Some(31_100), 500);
        situation.resumed_at = Some(31_000);

        assert_eq!(next_tick(&situation, 31_300), Tick::Wait { until: 31_400 });
    }

    // #214: footer が数える期限は､`next_tick` が `Poll` と答え始める時刻と
    // 同じ 1 つの規則から出る｡2 つが食い違えば､カウントダウンが 0 に
    // なってもポーリングが来ないか､来たのに数字が残る｡
    #[test]
    fn the_due_time_is_the_moment_the_tick_turns_into_a_poll() {
        let situation = situation(Some(1_500), 1_000);
        let due = poll_due_at(&situation);

        assert_eq!(due, 1_800);
        assert_eq!(next_tick(&situation, due - 1), Tick::Wait { until: due });
        assert_eq!(next_tick(&situation, due), Tick::Poll);
    }

    #[test]
    fn the_due_time_starts_from_the_loop_when_nothing_has_been_fetched() {
        assert_eq!(poll_due_at(&situation(None, 1_000)), 1_300);
    }

    #[test]
    fn the_due_time_starts_from_the_return_when_that_is_later() {
        let mut situation = situation(Some(31_100), 500);
        situation.resumed_at = Some(31_200);

        assert_eq!(poll_due_at(&situation), 31_500);
    }

    #[test]
    fn a_poll_that_brought_nothing_new_leaves_nothing_waiting() {
        let displayed = ["3", "2", "1"];
        let incoming = vec![item("3"), item("2"), item("1")];

        assert!(pending_after_poll(&displayed, incoming).is_none());
    }

    #[test]
    fn a_poll_that_brought_new_posts_counts_them() {
        let displayed = ["3", "2", "1"];
        let incoming = vec![item("5"), item("4"), item("3"), item("2"), item("1")];

        let pending = pending_after_poll(&displayed, incoming).expect("two posts arrived");
        assert_eq!(pending.count, 2);
    }

    // バッファはマージ済みリストの全体なので､適用してもポーリングが取った
    // 下に "Load older" が足したページを落とすことは無い｡
    #[test]
    fn the_pending_buffer_holds_the_whole_timeline_not_just_the_new_rows() {
        let displayed = ["3", "2", "1"];
        let incoming = vec![item("4"), item("3"), item("2"), item("1")];

        let pending = pending_after_poll(&displayed, incoming).expect("one post arrived");
        assert_eq!(pending.count, 1);
        assert_eq!(pending.items.len(), 4);
    }

    // 数えるのは先頭の連続だけで､手動 reload の数え方とまったく同じだ —
    // もっと下にある id は移動した post であって到着した post ではないし､
    // pill は押しても現れない post を約束してはならない｡
    #[test]
    fn only_the_leading_run_of_new_ids_is_counted() {
        let displayed = ["2", "1"];
        let incoming = vec![item("4"), item("2"), item("3"), item("1")];

        let pending = pending_after_poll(&displayed, incoming).expect("one post arrived");
        assert_eq!(pending.count, 1);
    }

    // まだ画面に何も無いウィンドウ (失敗した起動､空のリスト) は､
    // ポーリングが持ち帰ったものをすべて新着として扱う｡実際そうだからだ｡
    #[test]
    fn everything_is_new_when_nothing_is_displayed_yet() {
        let pending =
            pending_after_poll(&[], vec![item("2"), item("1")]).expect("two posts arrived");
        assert_eq!(pending.count, 2);
    }

    #[test]
    fn one_new_post_is_not_reported_in_the_plural() {
        assert_eq!(pending_label(1), "1 new post");
    }

    #[test]
    fn several_new_posts_are() {
        assert_eq!(pending_label(6), "6 new posts");
    }

    // --- #22: 最上部に貼り付く follow ---

    #[test]
    fn the_reader_at_the_exact_top_is_at_the_top() {
        assert!(at_top(0, px(0.)));
    }

    // 許容量はトラックパッドのためのもので､offset を最上部からわずかに
    // ずらして残す — その読み手は自分が最上部にいると思っているし､半
    // pixel のせいで pill が出たら follow が壊れて見える｡
    #[test]
    fn a_hair_below_the_top_still_counts() {
        assert!(at_top(0, px(-1.5)));
    }

    #[test]
    fn a_reader_scrolled_into_the_first_row_is_not_at_the_top() {
        assert!(!at_top(0, px(-40.)));
    }

    #[test]
    fn a_reader_rows_down_is_not_at_the_top_whatever_the_pixel_says() {
        assert!(!at_top(3, px(0.)));
    }

    // follow には 3 つすべてが要る: スイッチが入っていること､前に足す
    // timeline があること､そして位置が「いちばん新しいものを見せろ」と
    // 言っている読み手｡どれか 1 つでも欠ければ pill に落ちる｡
    #[test]
    fn follow_needs_the_switch_a_loaded_timeline_and_a_reader_at_the_top() {
        assert!(follows(FollowMode::Follow, true, true));
        assert!(
            !follows(FollowMode::Pill, true, true),
            "switched off means the pill"
        );
        assert!(
            !follows(FollowMode::Follow, false, true),
            "nothing loaded means the pill"
        );
        assert!(
            !follows(FollowMode::Follow, true, false),
            "scrolled down means the pill"
        );
    }

    #[test]
    fn the_toggle_flips_between_the_two_modes_and_back() {
        assert_eq!(FollowMode::Follow.flipped(), FollowMode::Pill);
        assert_eq!(FollowMode::Pill.flipped(), FollowMode::Follow);
    }

    // --- #208: glide の速さ ---

    // glide は時刻の関数で､上へしか動かず､最上部を越えない｡フレームを
    // 何回刻んだかではなく経過時間で位置が決まるので､timer の揺れは速さを
    // 乱すだけで終点を動かさない｡
    #[test]
    fn a_glide_moves_monotonically_toward_the_top_without_overshooting() {
        let start = -1_000.0_f32;
        let mut previous = start;
        let mut t = 0.0_f32;
        while let Some(y) = glide_y(start, t) {
            assert!(
                y >= previous,
                "the glide must not turn back at t={t}: {y} < {previous}"
            );
            assert!(
                y <= 0.,
                "the glide must not overshoot the top at t={t}: {y}"
            );
            previous = y;
            t += 0.016;
        }
        assert!(
            previous > -50.,
            "by the time the glide reports done it must be nearly at the top, was {previous}"
        );
    }

    #[test]
    fn a_glide_is_finished_once_its_duration_has_passed() {
        let duration = glide_duration_s(-1_000.);
        assert!(
            glide_y(-1_000., duration).is_none(),
            "at the duration the glide is over"
        );
        assert!(
            glide_y(-1_000., duration * 0.5).is_some(),
            "halfway through it is still walking"
        );
        assert!(glide_y(0., 0.).is_none(), "nothing to walk from the top");
        assert!(
            glide_y(-0.5, 0.).is_none(),
            "half a pixel is not worth a frame"
        );
    }

    // 読める速さ (#208): 1 行ぶん (150px 程度) でも一瞬では済ませず､画面
    // 1 枚ぶんは数秒かけ､どれだけ遠くても上限で打ち切る｡
    #[test]
    fn a_glide_paces_itself_by_distance_between_a_floor_and_a_ceiling() {
        let one_row = glide_duration_s(-150.);
        let a_screenful = glide_duration_s(-800.);
        let far = glide_duration_s(-30_000.);
        assert!(
            one_row >= 0.5,
            "one row must not flash past, took {one_row}s"
        );
        assert!(
            a_screenful > one_row && a_screenful >= 2.,
            "a screenful must take visibly longer than a row, took {a_screenful}s"
        );
        assert!(far <= 6., "a huge batch must still end, took {far}s");
        assert!(
            (glide_duration_s(-800.) - glide_duration_s(800.)).abs() < f32::EPSILON,
            "pace depends on distance, not direction"
        );
    }

    // #175 の要求でもある: フレーム数や実行環境によって終了位置が変わらない｡
    // 60Hz と 30Hz で同じ時刻を刻めば同じ場所にいる｡
    #[test]
    fn a_glide_is_at_the_same_place_regardless_of_frame_rate() {
        let start = -1_000.0_f32;
        let at_60hz = glide_y(start, 0.016 * 30.);
        let at_30hz = glide_y(start, 0.032 * 15.);
        match (at_60hz, at_30hz) {
            (Some(a), Some(b)) => {
                assert!(
                    (a - b).abs() < 0.001,
                    "same elapsed time, same offset: {a} vs {b}"
                );
            }
            other => unreachable!("half a second in, both should still be gliding: {other:?}"),
        }
    }

    // --- #239: 繰り返す意味のない拒否 ---
    //
    // issue のログは同じ拒否を 130 行以上積み上げた｡下の 3 本が止める側で､
    // 続く 3 本が「止めすぎない」側だ｡後者が無いと､夜中のネットワークの
    // 瞬断ひとつで朝まで取得が死ぬ｡

    #[test]
    fn a_spend_cap_403_stops_the_poll_and_says_where_to_look() {
        let error = anyhow::Error::from(Denied {
            endpoint: rate_limit::Endpoint::ListTimeline,
            denial: Denial::Forbidden,
            detail: "Forbidden: Your monthly spend cap has been reached.".to_string(),
        });
        let reason = halting_reason(&error).unwrap();
        assert!(reason.contains("list_timeline"), "{reason}");
        assert!(reason.contains("monthly spend cap"), "{reason}");
    }

    #[test]
    fn a_401_stops_the_poll_and_points_at_signing_in_again() {
        let error = anyhow::Error::from(Denied {
            endpoint: rate_limit::Endpoint::ListTimeline,
            denial: Denial::Rejected,
            detail: "Unauthorized".to_string(),
        });
        let reason = halting_reason(&error).unwrap();
        assert!(reason.contains("Sign in with X"), "{reason}");
    }

    #[test]
    fn an_exhausted_credit_cap_stops_the_poll() {
        let error = anyhow::Error::from(rate_limit::UsageCapExceeded {
            detail: "Usage cap exceeded: Monthly product cap".to_string(),
        });
        assert!(halting_reason(&error).is_some());
    }

    #[test]
    fn a_session_that_cannot_be_renewed_stops_the_poll() {
        let error = anyhow::Error::from(oauth::SessionExpired {
            detail: "invalid_request".to_string(),
        });
        let reason = halting_reason(&error).unwrap();
        assert!(reason.contains("Sign in with X"), "{reason}");
    }

    #[test]
    fn an_ordinary_rate_limit_keeps_polling() {
        // これは待てば直る｡`decision` が次の tick を送らせないので､
        // ループを殺す理由が無い｡
        let error = anyhow::Error::from(rate_limit::RateLimited {
            reset_at: Some(1_700_000_000),
            opaque: false,
        });
        assert_eq!(halting_reason(&error), None);
    }

    #[test]
    fn a_dropped_connection_keeps_polling() {
        let error = anyhow::anyhow!("request to https://api.x.com/2/lists/1/tweets failed");
        assert_eq!(halting_reason(&error), None);
    }

    #[test]
    fn a_5xx_keeps_polling() {
        let error = anyhow::anyhow!("list_timeline: HTTP 503 — upstream unavailable");
        assert_eq!(halting_reason(&error), None);
    }
}
