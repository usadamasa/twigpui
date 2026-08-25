//! background sync の 1 回の起床: 支払う側の半分｡
//!
//! [`super::schedule`] が tick の内容を決め､[`super::state`] がその結果を
//! 覚え､この module があいだで実行する｡分割は `run.rs` と同じ理由による
//! もので､ここは一つも unit test されていない｡どの分岐も HTTP request か､
//! その結果を書き込むファイルだからだ｡カバレッジを担っているのは
//! [`super::schedule::next_step`]､[`super::schedule::next_batch`]､
//! [`super::schedule::apply_outcome`]､[`super::state::settle`] で､
//! いずれも純粋関数として隣に置いてある｡
//!
//! # tick が使ってよい費用
//!
//! `Diff` が高くつく方だ: 両側を丸ごと読み､1 アカウントにつき 1 課金
//! resource｡`config.sync_interval_seconds` と [`super::SyncState`] が
//! ペースを決め､後者は起動をまたいで残るので､アプリを再起動しても同じ
//! 答えを買い直さずに済む｡
//!
//! `Apply` は [`Pacing::writes_per_minute`] 件の write に制限され､loop は
//! batch のあいだ `state::APPLY_PAUSE_SECONDS` 待つ｡この二つで持続的な
//! write レートが決まり､その既定値は意図的に低くしてある — #197 が実測した
//! ロックと､それが何のあとに起きたかは [`super::state`] を､refusal が
//! 出ない実行のあとに引き上げるためのつまみは
//! `config::DEFAULT_SYNC_WRITES_PER_MINUTE` を見よ｡
//!
//! # tick が消してよいもの
//!
//! ここでの prune は誰にも確認しないという意味では無条件だが､上限は
//! ある (#176)｡removal が `config.sync_prune_limit_percent` の許す割合を
//! 超える plan は､addition だけを流し切り､removal は未送信のまま plan
//! ファイルに残して `--sync-list --apply --prune` の確認に委ねる｡判定は
//! [`schedule::prune_allowed`] にあり､理由もそこが持つ｡この module が
//! 足す規則は､保留された plan は完了した仕事では *ない* ということだ:
//! ここでの `pending` は [`schedule::sendable`] のことなので､送れるものが
//! 残っていない plan は loop を縛らず､次の diff を期限どおりに来させる｡
//!
//! # tick が log に残すもの (#199)
//!
//! 何かをした tick と拒否された tick には 1 行ずつ､待っただけの tick には
//! 何も出さない — loop は毎分起きるので､起床ごとに 1 行出せば同じ文で
//! ファイルが埋まる｡refusal は毎回 log する｡#198 のあとでは refusal は
//! 起床ごとではなく backoff ごとに 1 回しか起きないからだ｡

use anyhow::Result;

use super::schedule::Outcome;
use super::{Action, SyncState, load_plan, load_state, save_plan, save_state, schedule, state};
use crate::paths::Paths;
use crate::x_api::XClient;

/// 呼び出し側がこの tick に望むペース配分 — *何を* ではなく *いつ* に
/// 関わるもののうち､[`SyncState`] にまだ無いものすべて｡
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pacing {
    /// `config.sync_interval_seconds`｡
    pub interval_seconds: u32,
    /// `config.sync_writes_per_minute` (#197): `Apply` の tick ごとに送る
    /// write 数で､`state::APPLY_PAUSE_SECONDS` と合わせて持続レートに
    /// なる｡既定値の根拠と､引き上げが実測を伴う意図的な行為である理由は
    /// config 側の定数に置いてある｡
    pub writes_per_minute: u8,
    /// #174 の手動起動: この 1 tick だけ interval と､失敗した tick が
    /// 得た block を落とす｡
    ///
    /// やり方は､[`schedule::next_step`] に last-diff の時刻をまったく
    /// 渡さず ([`schedule::last_diff_for`])､これは「diff が一度も走って
    /// いない」として既に読まれる値だ､かつ refusal 由来でない限り block も
    /// 渡さない ([`schedule::blocked_for`])｡判断のそれ以外は何も変わらず､
    /// interval を短くしたり 4 つ目の [`schedule::Step`] を足したりせずに
    /// こうしてあるのはそのためだ: 優先順位はそのままなので､refusal の
    /// backoff は今も tick を拒み､流し切っていない plan は両側を読み直す
    /// 前に流し切られる｡catch-up が残っているあいだにボタンを押しても
    /// read には何も使わない — 支払い済みの plan を再開するだけだ｡
    ///
    /// 呼び出し側は待っただけの tick をまたいでこれを立てたままにするので､
    /// backoff 中の押下は backoff に食われず､明けたときに効く｡
    pub forced: bool,
}

/// 1 回の tick の帰結: 何をしたか､disk に残した state､そして loop が
/// いつ戻ってくるべきか｡
///
/// state は保存するだけでなく返しもする｡ウィンドウがそこから sync の
/// 様子 — 連続回数､期限 — を語れるようにするためで､ファイルを読み直す
/// ことも自前の写しを持つこともしない｡#198 で失われたのがその写しだ｡
#[derive(Debug)]
pub(crate) struct Tick {
    /// `Err` は完全に失敗した tick で､`state` は既にそれが得た interval を
    /// 持っている｡
    pub outcome: Result<Outcome>,
    pub state: SyncState,
    /// 次の tick を走らせてよい最も早い時刻｡
    pub wake_at: i64,
}

/// 1 回の tick を走らせる: 決め､実行し､settle し､保存し､log する｡
///
/// ここでの prune は `--sync-list` と違って opt-in ではない (あちらは
/// `--prune` の後ろに置いたままだ — 二つの経路が分かれる理由はこの module
/// の親を見よ) が､list の `prune_limit_percent` を上限とする (#176)｡
/// module doc を見よ｡
///
/// tick が成功したかどうかによらず state は保存する: 失敗は interval を
/// 得るし､その interval は再起動を越えて残らなければならない｡保存自体が
/// 失敗した場合は log に出したうえで tick の state をそのまま返す｡走って
/// いる loop が少なくとも次の保存までは正しくペース配分できるようにだ｡
pub(crate) fn tick(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    list_id: &str,
    pacing: Pacing,
    prune_limit_percent: u8,
    now: i64,
) -> Tick {
    let state_path = paths.sync_state_file();
    let mut state = load_state(&state_path);
    let outcome = perform(
        paths,
        client,
        user_id,
        list_id,
        pacing,
        prune_limit_percent,
        &mut state,
        now,
    );
    let settled = state::settle(state, outcome.as_ref().ok(), now, pacing.interval_seconds);
    if let Err(error) = save_state(&state_path, &settled.state) {
        crate::log::error(&format!("list sync: could not save its state: {error:#}"));
    }
    log_outcome(&outcome, settled.state, settled.wake_at);
    Tick {
        outcome,
        state: settled.state,
        wake_at: settled.wake_at,
    }
}

/// tick の仕事: [`schedule::next_step`] が言うことを､実行する｡
///
/// `state` が変わるのはただ一つの場合だけ — diff が読む前に
/// `last_diff_at` を打刻する — で､出てきたものは呼び出し側が settle して
/// 保存する｡
#[allow(clippy::too_many_arguments)]
fn perform(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    list_id: &str,
    pacing: Pacing,
    prune_limit_percent: u8,
    state: &mut SyncState,
    now: i64,
) -> Result<Outcome> {
    // 別の list に対して diff された plan は､この list の仕事ではない｡
    // 適用せずに捨てる: 同じ状況で `run.rs` は拒否するが､loop には拒否を
    // 伝える相手がいない｡
    let plan = load_plan(&paths.sync_plan_file())?.filter(|plan| plan.list_id == list_id);
    // diff のときに一度きりではなく､今の plan に対してここで決める:
    // 上限は設定なので二つのあいだで変わりうるし､上限が入る前の plan
    // ファイルはそもそも一度も判定されていない｡
    let prune = plan
        .as_ref()
        .is_none_or(|plan| schedule::prune_allowed(plan, prune_limit_percent));
    let pending = plan
        .as_ref()
        .map_or(0, |plan| schedule::sendable(plan, prune));

    let situation = schedule::Situation {
        last_diff_at: schedule::last_diff_for(pacing.forced, state.last_diff_at),
        interval_seconds: pacing.interval_seconds,
        pending,
        blocked_until: schedule::blocked_for(pacing.forced, state),
    };

    match schedule::next_step(&situation, now) {
        // `pending` を連れて回る (#174) のは､この arm に流し切った plan
        // でも､数百の write を残したまま rate limit に当たった plan でも
        // 到達するからだ — その二つを見分けなければならない唯一の
        // 呼び出し側である [`schedule::is_finished`] を見よ｡
        schedule::Step::Wait { until } => Ok(Outcome::Idle { until, pending }),
        schedule::Step::Diff => diff(
            paths,
            client,
            user_id,
            list_id,
            prune_limit_percent,
            state,
            now,
        ),
        // この step を生んだのは `pending > 0` なので plan は `Some` だ｡
        // unwrap せずに列挙してあるのは､あとで優先順位を変えてもここが
        // panic に化けないようにするためだ｡
        schedule::Step::Apply => match plan {
            Some(plan) => apply(
                paths,
                client,
                plan,
                prune,
                now,
                usize::from(pacing.writes_per_minute),
            ),
            None => Ok(Outcome::Idle {
                until: now,
                pending: 0,
            }),
        },
    }
}

/// 両側を読み､新しい plan を書く｡
///
/// 時刻は read の **前** に打刻し､read が成功したかどうかによらず打刻された
/// ままにする｡どちらの側面も効く: 途中で crash しても届いたページ分は既に
/// 課金されているので､再起動が即座にそれを読み直してはならない｡そして
/// 毎回失敗する diff — scope の失効､400 を返し始めた endpoint — は､
/// そうしなければ loop の起床ごとに永久に再試行される｡
///
/// prune の判定もここで取るが outcome のためだけだ — 保留された plan の
/// ことをウィンドウが聞くのは､そこから addition の batch を流し出すたびでは
/// なく､作られた 1 回だけになる｡実際に *強制する* のは [`perform`] が
/// apply 時に取る判定の方だ｡
fn diff(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    list_id: &str,
    prune_limit_percent: u8,
    state: &mut SyncState,
    now: i64,
) -> Result<Outcome> {
    state.last_diff_at = Some(now);
    save_state(&paths.sync_state_file(), state)?;

    let plan = super::run::plan_sync(paths, client, user_id, list_id, now)?;
    let adds = plan.pending_count(Action::Add);
    let removals = plan.pending_count(Action::Remove);
    let held = !schedule::prune_allowed(&plan, prune_limit_percent);
    save_plan(&paths.sync_plan_file(), &plan)?;
    if held {
        crate::log::warn(&format!(
            "list sync: holding {removals} removal(s) against a list of {} members — over \
             sync_prune_limit_percent ({prune_limit_percent}%). They stay in the plan file; \
             confirm them with --sync-list --apply --prune",
            plan.members_total
        ));
    }
    Ok(Outcome::Diffed {
        adds,
        removals,
        members_total: plan.members_total,
        held,
    })
}

/// plan に残っている write を最大 `limit` 件 ([`Pacing::writes_per_minute`])
/// 送る｡
///
/// `prune` は [`schedule::prune_allowed`] から得た [`perform`] の判定だ｡
/// false なら batch は addition のみで､`remaining` は未適用の entry すべて
/// ではなく､まだ送ってよいものを数える — `pending` と同じ「残り」の
/// 読み方なので､保留された removal が残っていても addition を流し切れば
/// 完了通知が出る｡
fn apply(
    paths: &Paths,
    client: &XClient,
    mut plan: super::Plan,
    prune: bool,
    now: i64,
    limit: usize,
) -> Result<Outcome> {
    let (sent, result) = super::run::apply_some(paths, client, &mut plan, prune, now, limit);
    let remaining = schedule::sendable(&plan, prune);

    if plan.is_complete() {
        // 再開すべきものは残っていない｡置いたままにすると次の tick が
        // 残務として読み､diff を飛ばしてしまう｡
        //
        // `remaining == 0` ではなく `is_complete` なのは: addition を
        // 流し切って removal を保留した plan は削除 *しない* からだ｡
        // その removal は支払い済みで､両側を読み直さずに
        // `--sync-list --apply --prune` が送る元がこのファイルだ｡
        // どちらにせよ次の diff が置き換える｡
        let _ = std::fs::remove_file(paths.sync_plan_file());
    }

    schedule::apply_outcome(sent, remaining, result)
}

/// tick が log に書く行 (#199)｡待っただけの tick には何も書かない｡
///
/// 出ていく途中で `log::redact` が走る — API のエラーは request の URL を
/// 引用しうる｡
fn log_outcome(outcome: &Result<Outcome>, state: SyncState, wake_at: i64) {
    match outcome {
        Ok(Outcome::Idle { .. }) => {}
        Ok(Outcome::Diffed {
            adds,
            removals,
            members_total,
            held,
        }) => crate::log::info(&format!(
            "list sync: diffed — {adds} to add, {removals} to remove, list holds \
             {members_total}{}",
            if *held { " (removals held)" } else { "" }
        )),
        Ok(Outcome::Applied { sent, remaining }) => {
            crate::log::info(&format!("list sync: sent {sent}, {remaining} to go"));
        }
        Ok(Outcome::RateLimited {
            opaque,
            sent,
            remaining,
            ..
        }) => crate::log::warn(&format!(
            "list sync: write refused ({}) after {sent} sent this batch; {remaining} to go; \
             refusal #{}, retrying at unix time {wake_at}",
            if *opaque {
                "by a cap the headers do not describe"
            } else {
                "window exhausted"
            },
            state.refusals
        )),
        Err(error) => crate::log::error(&format!(
            "list sync failed: {error:#}; next attempt at unix time {wake_at}"
        )),
    }
}
