//! reload を走らせてよいか､走らせないときアプリが何を言うか｡
//!
//! [`super::render`] と並んで `ui` から分けた (#126)｡ただし線は別だ:
//! ここにあるのはクリックと課金されるリクエストの間に立つ関数だ｡
//! `reload_gate` と `reload_cooldown` はそもそも金を使うかを決め､
//! `reload_failure_outcome` と `cooldown_tick` は答えが「使わない」の
//! ときに画面へ何が残るかを決める｡両者は render の補助関数の間に散った
//! 数個の関数ではなく､金に面した 1 つのロジックとして読める｡
//!
//! 純粋であり､そのようにテストしてある — これらを呼ぶ非同期の経路は
//! `ui` に残り､ユニットテストしていない｡

use super::{Cooldown, ReloadNotice, ReloadTrigger, TimelineState, cache, rate_limit};

/// #10 のレート制限判断で塞がれている間の reload ボタンのカウントダウン
/// 文言｡描画時点で `reset_at` を (ちょうど) 過ぎていた場合､`remaining` は
/// 負にせず 0 に丸める｡
///
/// 2 つの cooldown が違う読み方になるのは意図的だ: このアプリを実際に
/// レート制限しているのは片方だけであり､自分で課した間隔をレート制限と
/// 報告すれば何が起きたかを取り違えて伝えることになる｡
pub(super) fn cooldown_label(cooldown: Cooldown, reset_at: i64, now: i64) -> String {
    // `saturating_sub` (#47): `reset_at` は API のヘッダから､`now` は
    // クロックから来る｡どちらもこのコードが信用してよいものではない｡
    let remaining = reset_at.saturating_sub(now).max(0);
    match cooldown {
        Cooldown::LocalInterval => format!("Waiting out the fetch interval — {remaining}s"),
        Cooldown::ApiRateLimit => format!("Rate limited by X — retry in {remaining}s"),
    }
}

/// 失敗した reload / load-older のエラーを､上げるべき [`ReloadNotice`] へ
/// 分類する (#57) — 「reset 時刻が分かっているレート制限」と「ただの失敗」を
/// 決める唯一の場所であり､[`map_reload_error`] (他に見せるものが無いときの
/// フォールバック) と [`reload_failure_outcome`] (失敗を生き延びるべき
/// timeline がすでにある通常のケース) が共有する｡
///
/// #10: 送信がブロックされた場合は reset 時刻が分かるのでカウントダウンで
/// 見せる｡それ以外 (429 に使える reset ヘッダが無かったレート制限も含む) は
/// 素のエラーメッセージへ落ちる｡
pub(super) fn reload_notice_for_error(error: &anyhow::Error) -> ReloadNotice {
    match error.downcast_ref::<rate_limit::RateLimited>() {
        Some(rate_limit::RateLimited {
            reset_at: Some(reset_at),
            ..
        }) => ReloadNotice::Cooldown {
            reset_at: *reset_at,
            cooldown: Cooldown::ApiRateLimit,
        },
        _ => ReloadNotice::Failed(format!("{error:#}").into()),
    }
}

/// 失敗した reload のエラーを､それを見せるべき state へ対応づける｡画面に
/// 落ちる先が他に無いとき向けだ｡#57 の時点で残る唯一の呼び出し元は
/// [`reload_failure_outcome`] で､しかもこの失敗が追い出してしまうような
/// 読み込み済みの timeline が無いと確認した後だけだ —
/// `TimelineView::reload` と `TimelineView::load_older` はどちらも今や
/// その経路からのみここへ届き､直接来ることはない｡
pub(super) fn map_reload_error(error: &anyhow::Error) -> TimelineState {
    match reload_notice_for_error(error) {
        ReloadNotice::Cooldown { reset_at, cooldown } => {
            TimelineState::RateLimited { reset_at, cooldown }
        }
        // ここで `Outcome` には到達しない: 引数は失敗の variant しか
        // 作らない `reload_notice_for_error` から来る｡ワイルドカードに
        // 任せず並べてあるのは､後で足された variant が黙って `Failed` に
        // 落ちるのではなく､検討を強いられるようにするためだ｡
        ReloadNotice::Failed(message) | ReloadNotice::Outcome(message) => {
            TimelineState::Failed(message)
        }
    }
}

/// 失敗した fetch が `state` に何をすべきか､どの notice を (もしあれば)
/// 上げるべきか (#57) — [`TimelineView::reload`]
/// (`TimelineView::apply_reload_failure` 経由) と
/// `TimelineView::load_older` が共有する｡「すでにある timeline は失敗した
/// fetch を生き延びる」を gpui 無しでユニットテストできるよう純粋な関数に
/// してある｡refresh の失敗は､すでに読み込まれているものが誤りだという証拠
/// ではない｡だから `state` がすでに post を持つときは手を触れずに返し､
/// 失敗は [`reload_notice_for_error`] を通じて notice になる — `Some` を
/// 返すのはこの分岐だけだ｡まだ何も表示されていないときは､失敗が state
/// そのものになる — このファイルの他のあらゆる失敗した fetch が使うのと
/// 同じ [`map_reload_error`] の対応づけだ — そして notice は `None` で
/// 返る: `state` (`Failed`/`RateLimited`) がすでに本文へ何が起きたかを
/// 伝えているので､同じ文言を繰り返すバナーは画面上の失敗の重複でしかない｡
pub(super) fn reload_failure_outcome(
    state: TimelineState,
    error: &anyhow::Error,
) -> (TimelineState, Option<ReloadNotice>) {
    match state {
        TimelineState::Loaded(items) => (
            TimelineState::Loaded(items),
            Some(reload_notice_for_error(error)),
        ),
        _ => (map_reload_error(error), None),
    }
}

/// ヘッダが "Load older" ボタンを出すべきか (#11): レスポンスが実際に
/// 再開用の `meta.next_token` を運んできた後で､かつ timeline がそれを
/// 押す意味のある状態にある間だけだ｡
///
/// post の上限では出さない｡そこが金に効く部分だ｡`cache::splice` は
/// `MAX_CACHED_POSTS` まで切り詰めるので､上限でクリックすると本物の API
/// リクエストを消費したうえで買った post をすべて捨てることになる — 金を
/// 払った no-op であり､キャッシュ全体がまさにそれを避けるために存在する
/// このプロジェクトでは筋が通らない｡ボタンが黙って消えないよう､
/// [`at_the_post_cap`] がその場所に説明を描く｡
///
/// `single_source` は #43 の天井: 複数 source を同時にページングし結果を
/// どう合成するかは解いていない (`sources.len() != 1` のときは
/// `next_page_token` 自体が常に `None` になるので実質この条件だけで足りる
/// が､呼び出し側の意図を隠さないよう明示的に取る)｡
pub(super) fn offers_load_older(
    next_page_token: Option<&str>,
    state: &TimelineState,
    single_source: bool,
) -> bool {
    match state {
        TimelineState::Loaded(items) => {
            single_source && next_page_token.is_some() && items.len() < cache::MAX_CACHED_POSTS
        }
        _ => false,
    }
}

/// 読み込み済みの timeline が [`offers_load_older`] の止まる上限に達したか｡
/// 本文がこれ以上遡れない理由を言えるようにするためのものだ｡
pub(super) fn at_the_post_cap(state: &TimelineState) -> bool {
    matches!(state, TimelineState::Loaded(items) if items.len() >= cache::MAX_CACHED_POSTS)
}

/// `config.min_fetch_interval_seconds` に照らして [`TimelineView::reload`]
/// が今の実行を拒むべきか (#10)｡`None` は「進めてよい」 — reload がまだ
/// 一度も無いか､前回からの間隔がすでに経過している｡`Some(reset_at)` は
/// 「まだだ」で､いつ再び許されるかを [`cooldown_label`] が期待するのと
/// 同じ形で運ぶ｡
pub(super) fn reload_cooldown(
    last_reload_at: Option<i64>,
    min_interval_seconds: u32,
    now: i64,
) -> Option<i64> {
    let last = last_reload_at?;
    let reset_at = last.saturating_add(i64::from(min_interval_seconds));
    (reset_at > now).then_some(reset_at)
}

/// `trigger` を踏まえて [`TimelineView::reload`] が今の実行を拒むべきか
/// (#57)｡`ReloadTrigger::UserAction` は [`reload_cooldown`] を丸ごと
/// 迂回して常に `None` を返す — post の送信やサインイン後の reload が､
/// polling を抑えるために存在する間隔で塞がれてはならない理由は
/// [`ReloadTrigger`] の doc にある｡その間隔は､ユーザーが今やったことへの
/// 直接の応答を制限するためのものではない｡`ReloadTrigger::Polling` は
/// `reload_cooldown` にそのまま委ねる｡
pub(super) fn reload_gate(
    trigger: ReloadTrigger,
    last_reload_at: Option<i64>,
    min_interval_seconds: u32,
    now: i64,
) -> Option<i64> {
    match trigger {
        ReloadTrigger::Polling => reload_cooldown(last_reload_at, min_interval_seconds, now),
        ReloadTrigger::UserAction => None,
    }
}

/// fetch を spawn する直前に `state` が何になるべきか (#57) —
/// [`TimelineView::reload`] と `TimelineView::load_older` が共有する｡
/// 「すでにある timeline は進行中の fetch を生き延びる」を gpui 無しで
/// ユニットテストできるよう純粋な関数にしてある｡新しいコピーを取りに行く
/// ことは､前のものが古いとか誤っているとかの証拠ではない｡だから
/// `previous` がすでに post を持つならそのまま残す｡ヘッダの busy 表示は
/// 代わりに `TimelineView::reloading` から来る｡これは `state` に畳み込まず
/// 並べて設定してある (そのフィールドの doc を参照)｡まだ何も読み込まれて
/// いないときだけ `TimelineState::Loading` へ落ちる｡失うものが無い唯一の
/// ケースについて､#57 より前の挙動に合わせている｡
pub(super) fn reload_start_state(previous: TimelineState) -> TimelineState {
    match previous {
        TimelineState::Loaded(items) => TimelineState::Loaded(items),
        _ => TimelineState::Loading,
    }
}

/// 現在の `reload_notice` と時刻を踏まえて､
/// [`TimelineView::start_cooldown_ticker`] のループの 1 回の起床が何を
/// すべきか (#57 の項目 3) — そのループの背後にある純粋な判断で､gpui の
/// タイマー無しでユニットテストできるよう切り出してある｡ループ自身は
/// これに match して､続けるか返るかするだけだ｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CooldownTick {
    /// 刻むものが無い: `reload_notice` が `None` か､進めるカウントダウンの
    /// 無い `Failed` の notice を持っている — もともと cooldown ではなかった
    /// か､ticker が始まってから他の何か (成功､ただの失敗) がすでに置き換えた
    /// かのどちらかだ｡ループは `reload_notice` に触れず止まるべきだ｡
    NotTicking,
    /// まだ cooldown の窓の中だ: バナーのカウントダウンが進むよう再通知し､
    /// もう 1 秒待つ｡
    StillWaiting,
    /// `reset_at` を過ぎた｡ループは `reload_notice` を消して止まるべきだ —
    /// バナーが消え "Reload" が再び押せるようになることが､ユーザーから
    /// 見える「待ち終わった」の合図だ｡
    Elapsed,
}

/// [`CooldownTick`] の判断の純粋な中核 — 各 variant がループに何をさせる
/// 意味なのかは､その doc を参照｡
pub(super) fn cooldown_tick(notice: Option<&ReloadNotice>, now: i64) -> CooldownTick {
    match notice {
        Some(ReloadNotice::Cooldown { reset_at, .. }) if *reset_at > now => {
            CooldownTick::StillWaiting
        }
        Some(ReloadNotice::Cooldown { .. }) => CooldownTick::Elapsed,
        // `Outcome` は ticker がまだ動いている間に reload が終わったという
        // ことで､それはまさに `NotTicking` が説明する「他の何かが置き換えた」
        // ケースだ (#141)｡
        Some(ReloadNotice::Failed(_) | ReloadNotice::Outcome(_)) | None => CooldownTick::NotTicking,
    }
}

/// reload が新しい post を先頭に足した後､読み手をどこに置くべきか (#22):
/// 直前まで viewport の先頭にあった行の *新しい* 一覧での index か､
/// スクロール位置に手を触れないなら `None`｡
///
/// `None` は「そこに留まる」2 つのケースの両方を意味する｡状況は違っても
/// 呼び出し元への指示は同じだ: 読み手がすでに一番上にいたので､新しい
/// post はその上に何も無いまま現れて目に入ればよい｡あるいは先頭に何も
/// 足されなかったので､埋め合わせるものが無い｡
///
/// それ以外は読み手をずらす｡20 行下にいる人のところへ新しい post を 6 件
/// 運んでくる reload は､その人が読んでいたものを一覧の 26 行下へ動かす｡
/// viewport は元の場所に留まるので､何も触っていないのに目の下の文章が
/// 変わる｡手元に無かった先頭の id を数えることが､それを取り消すのに
/// ちょうど必要なスクロール量だ｡
///
/// 何が変わったかに対する純粋な関数のままでいられるよう､item ではなく
/// id を取る｡そして *先頭の* 連なりだけを数える: それより下に現れる id は
/// 到着した post ではなく移動した post であり､そのために viewport を
/// 動かすのは誤りだ｡
pub(super) fn preserved_scroll_target(
    previous_ids: &[&str],
    new_ids: &[&str],
    top_item: usize,
) -> Option<usize> {
    if top_item == 0 {
        return None;
    }
    let prepended = newly_arrived(previous_ids, new_ids);
    if prepended == 0 {
        return None;
    }
    Some(top_item.saturating_add(prepended))
}

/// reload が何件の post を運んできたか: 手元に無かった id の先頭の連なりだ｡
///
/// *先頭の* 連なりなのは､それより下の id が到着した post ではなく移動した
/// post だからだ｡呼び出し元は 2 つともこの読み方に依存している —
/// [`preserved_scroll_target`] は読み手を見ていた場所より先へ押しやって
/// しまうし､[`reload_outcome_label`] はすでにそこにあった post を新着だと
/// 言ってしまう — だからルールはそれぞれではなく 1 箇所に置いてある｡
pub(super) fn newly_arrived(previous_ids: &[&str], new_ids: &[&str]) -> usize {
    let previous: std::collections::HashSet<&str> = previous_ids.iter().copied().collect();
    new_ids
        .iter()
        .take_while(|id| !previous.contains(*id))
        .count()
}

/// 終わった reload が自分について何を言うか (#141)｡
///
/// かつて reload は失敗しか報告しなかった｡成功時はヘッダのボタンが
/// `Loading…` へ切り替わって戻るだけで､応答が速ければ 1､2 フレームだ —
/// しかもボタンではなく `cmd-r` やメニューを使ったとき､読み手が見ている
/// 場所では決してない｡
///
/// だから結果はほのめかさず明言する｡「何も来なかった」も明言する: それは
/// 前後で画面が他は同一になるケースであり､押下が届かなかったと読み手が
/// 最も思いやすいケースだ｡
pub(super) fn reload_outcome_label(new_posts: usize) -> String {
    match new_posts {
        0 => "No new posts.".to_string(),
        1 => "1 new post.".to_string(),
        n => format!("{n} new posts."),
    }
}

/// N-source reload (#43) が一部失敗したとき [`reload_outcome_label`] へ足す
/// 一言｡`failures` が 0 なら `base` をそのまま返す — 全部成功した reload は
/// 今までどおり静かだ｡`Endpoint::ListTimeline` が全 list id で 1 バケットを
/// 共有するため､1 本が rate limit に当たれば以降も弾かれやすく (`x-api-budget`)､
/// 「取れた分を出す」がこの下で実質的な既定動作になる — この一言はその事実を
/// 読み手にも見せる｡
pub(super) fn partial_failure_label(base: String, failures: usize, successes: usize) -> String {
    if failures == 0 {
        base
    } else {
        // `lane::reload_all` の `sources` は高々数十件で溢れる現実的な
        // 経路が無いが、これも呼び出し元が制御しない値の合算なので
        // `saturating_add` で止める。
        format!(
            "{base} ({failures} of {} sources failed)",
            failures.saturating_add(successes)
        )
    }
}
