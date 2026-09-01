//! background sync の 1 回の起床: 支払う側の半分｡
//!
//! [`super::schedule`] が tick の内容を決め､[`super::state`] がその結果を
//! 覚え､この module があいだで実行する｡分割は `run.rs` と同じ理由による
//! もので､判断は純粋関数として隣にある — [`super::schedule::next_step`]､
//! [`super::schedule::next_batch`]､[`super::schedule::apply_outcome`]､
//! [`super::state::settle`]｡
//!
//! ここの分岐はどれも request かファイルだが､request の相手は
//! [`super::api::ListSyncApi`] で､ファイルは `env::temp_dir()` の下に置ける｡
//! だから tick が何を買い何を書いたかはテストから見える: 期限前に何も
//! 呼ばないこと､read が失敗しても打刻が残ること､別の list の plan を
//! 捨てること､保留された plan がファイルに残ることは､どれも assert して
//! ある｡
//!
//! # tick が使ってよい費用
//!
//! `Diff` が高くつく方だ: 両側を丸ごと読み､1 アカウントにつき 1 課金
//! resource｡`config.sync_interval_seconds` と [`super::SyncState`] が
//! ペースを決め､後者は起動をまたいで残るので､アプリを再起動しても同じ
//! 答えを買い直さずに済む｡
//!
//! `Apply` は [`Pacing::writes_per_batch`] 件の write に制限され､loop は
//! batch のあいだ `state::apply_pause_seconds` が引いた長さだけ待つ｡この
//! 二つで持続的な write レートが決まり､その既定値は意図的に低くしてある
//! — #197 が実測したロックと､それが何のあとに起きたかは [`super::state`] を､
//! refusal が出ない実行のあとに引き上げるためのつまみは
//! `config::DEFAULT_SYNC_WRITES_PER_BATCH` を見よ｡
//!
//! 待ちが固定値ではなく範囲なのは､速度を落としても拒否が止まらなかった
//! ため｡理由と二つの層は [`super::state`] の module doc を見よ｡揺らぎを
//! 引くのはこの module で､[`super::state::settle`] は引かれた長さしか
//! 見ない｡
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

use super::api::ListSyncApi;
use super::schedule::Outcome;
use super::{Action, SyncState, load_plan, load_state, save_plan, save_state, schedule, state};
use crate::paths::Paths;

/// 呼び出し側がこの tick に望むペース配分 — *何を* ではなく *いつ* に
/// 関わるもののうち､[`SyncState`] にまだ無いものすべて｡
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pacing {
    /// `config.sync_interval_seconds`｡
    pub interval_seconds: u32,
    /// `config.sync_writes_per_batch` (#197): `Apply` の tick ごとに送る
    /// write 数で､`state::apply_pause_seconds` が引く間と合わせて持続
    /// レートになる｡既定値の根拠と､引き上げが実測を伴う意図的な行為で
    /// ある理由は config 側の定数に置いてある｡
    pub writes_per_batch: u8,
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
    client: &dyn ListSyncApi,
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
    // 揺らぎを引くのはここ 1 回きり｡settle は渡された長さしか見ないので
    // 純粋なままでいられる — `rate_limit::backoff_delay` と同じ継ぎ目｡
    let spacing = state::Spacing {
        interval_seconds: pacing.interval_seconds,
        apply_pause_seconds: state::apply_pause_seconds(crate::rate_limit::random_jitter_fraction()),
    };
    let settled = state::settle(state, outcome.as_ref().ok(), now, spacing);
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
    client: &dyn ListSyncApi,
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
        paused_until: schedule::paused_for(pacing.forced, state),
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
                usize::from(pacing.writes_per_batch),
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
    client: &dyn ListSyncApi,
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

/// plan に残っている write を最大 `limit` 件 ([`Pacing::writes_per_batch`])
/// 送る｡
///
/// `prune` は [`schedule::prune_allowed`] から得た [`perform`] の判定だ｡
/// false なら batch は addition のみで､`remaining` は未適用の entry すべて
/// ではなく､まだ送ってよいものを数える — `pending` と同じ「残り」の
/// 読み方なので､保留された removal が残っていても addition を流し切れば
/// 完了通知が出る｡
fn apply(
    paths: &Paths,
    client: &dyn ListSyncApi,
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

#[cfg(test)]
mod tests {
    use super::super::api::fake::{Call, FakeApi, Scratch, page, rate_limited};
    use super::*;
    use crate::sync::{Plan, PlanEntry, save_plan};

    const INTERVAL: u32 = 21_600;
    /// 上限は list の 10% (config の既定)｡
    const PRUNE_LIMIT: u8 = 10;
    const NOW: i64 = 1_800_000_000;

    fn pacing(writes_per_batch: u8, forced: bool) -> Pacing {
        Pacing {
            interval_seconds: INTERVAL,
            writes_per_batch,
            forced,
        }
    }

    fn plan_of(list_id: &str, adds: &[&str], removals: &[&str], members_total: usize) -> Plan {
        let entry = |user_id: &str, action| PlanEntry {
            user_id: user_id.to_string(),
            username: format!("user{user_id}"),
            action,
            applied: false,
            rejected: None,
        };
        Plan {
            list_id: list_id.to_string(),
            created_at: 0,
            members_total,
            entries: adds
                .iter()
                .map(|id| entry(id, Action::Add))
                .chain(removals.iter().map(|id| entry(id, Action::Remove)))
                .collect(),
        }
    }

    /// 何も送らずに済む fake — 呼ばれた時点で答えを持たず落ちるので､
    /// 「API を使わない」を assert するのはこれで足りる｡
    fn silent() -> FakeApi {
        FakeApi::new()
    }

    // --- interval: 期限が来るまで何も買わない ---

    #[test]
    fn a_tick_inside_the_interval_spends_nothing() {
        let scratch = Scratch::new("auto-idle");
        save_state(
            &scratch.paths().sync_state_file(),
            &SyncState {
                last_diff_at: Some(NOW),
                ..SyncState::default()
            },
        )
        .unwrap();
        let client = silent();

        let tick = tick(
            scratch.paths(),
            &client,
            "me",
            "7",
            pacing(5, false),
            PRUNE_LIMIT,
            NOW,
        );

        assert!(
            matches!(tick.outcome, Ok(Outcome::Idle { .. })),
            "{:?}",
            tick.outcome
        );
        assert!(client.calls().is_empty(), "an idle tick reads nothing");
        assert_eq!(tick.wake_at, NOW.saturating_add(i64::from(INTERVAL)));
    }

    #[test]
    fn a_forced_tick_does_not_wait_out_the_interval() {
        // #174 のボタン｡interval を縮めるのではなく last-diff の時刻を捨てる｡
        let scratch = Scratch::new("auto-forced");
        save_state(
            &scratch.paths().sync_state_file(),
            &SyncState {
                last_diff_at: Some(NOW),
                ..SyncState::default()
            },
        )
        .unwrap();
        let client = FakeApi::new()
            .following(vec![Ok(page(&[("1", "alice")], None))])
            .members(vec![Ok(page(&[], None))]);

        let tick = tick(
            scratch.paths(),
            &client,
            "me",
            "7",
            pacing(5, true),
            PRUNE_LIMIT,
            NOW,
        );

        assert!(
            matches!(tick.outcome, Ok(Outcome::Diffed { adds: 1, .. })),
            "{:?}",
            tick.outcome
        );
    }

    // --- diff: 読む前に打刻する ---

    #[test]
    fn a_due_tick_diffs_both_sides_and_writes_the_plan() {
        let scratch = Scratch::new("auto-diff");
        let client = FakeApi::new()
            .following(vec![Ok(page(&[("1", "alice")], None))])
            .members(vec![Ok(page(&[("2", "bob")], None))]);

        let tick = tick(
            scratch.paths(),
            &client,
            "me",
            "7",
            pacing(5, false),
            PRUNE_LIMIT,
            NOW,
        );

        assert!(
            matches!(
                tick.outcome,
                Ok(Outcome::Diffed {
                    adds: 1,
                    removals: 1,
                    members_total: 1,
                    held: true
                })
            ),
            "{:?}",
            tick.outcome
        );
        let plan = load_plan(&scratch.paths().sync_plan_file())
            .unwrap()
            .unwrap();
        assert_eq!(plan.list_id, "7");
    }

    #[test]
    fn the_time_of_a_diff_is_stamped_even_when_the_read_fails() {
        // 届いたページは課金されている｡毎回落ちる diff を起床ごとに
        // 再試行させないための打刻でもある｡
        let scratch = Scratch::new("auto-diff-fails");
        let client = FakeApi::new().following(vec![Err(anyhow::anyhow!("the API said 401"))]);

        let tick = tick(
            scratch.paths(),
            &client,
            "me",
            "7",
            pacing(5, false),
            PRUNE_LIMIT,
            NOW,
        );

        assert!(tick.outcome.is_err(), "{:?}", tick.outcome);
        let state = load_state(&scratch.paths().sync_state_file());
        assert_eq!(state.last_diff_at, Some(NOW));
        // 失敗した tick は interval を丸ごと得る｡
        assert_eq!(state.blocked_until, Some(NOW + i64::from(INTERVAL)));
    }

    #[test]
    fn removals_over_the_cap_are_held_rather_than_trimmed_to_fit() {
        // #176: 疑っているのは read の方なので､悪い read の最初の N 件は
        // 最後の N 件よりましではない｡
        let scratch = Scratch::new("auto-held");
        let members: Vec<(&str, &str)> = vec![
            ("21", "m1"),
            ("22", "m2"),
            ("23", "m3"),
            ("24", "m4"),
            ("25", "m5"),
        ];
        let client = FakeApi::new()
            .following(vec![Ok(page(&[], None))])
            .members(vec![Ok(page(&members, None))]);

        let tick = tick(
            scratch.paths(),
            &client,
            "me",
            "7",
            pacing(5, false),
            PRUNE_LIMIT,
            NOW,
        );

        assert!(
            matches!(tick.outcome, Ok(Outcome::Diffed { held: true, .. })),
            "{:?}",
            tick.outcome
        );
        // 保留された plan はファイルに残る — 支払い済みで､
        // `--sync-list --apply --prune` がそこから送る｡
        assert!(
            load_plan(&scratch.paths().sync_plan_file())
                .unwrap()
                .is_some()
        );
    }

    // --- apply: 流し切りが diff のやり直しに優先する ---

    #[test]
    fn a_plan_on_file_is_sent_a_batch_at_a_time() {
        let scratch = Scratch::new("auto-apply");
        save_plan(
            &scratch.paths().sync_plan_file(),
            &plan_of("7", &["1", "2", "3"], &[], 0),
        )
        .unwrap();
        let client = FakeApi::new().writes(vec![Ok(()), Ok(())]);

        let tick = tick(
            scratch.paths(),
            &client,
            "me",
            "7",
            pacing(2, false),
            PRUNE_LIMIT,
            NOW,
        );

        assert!(
            matches!(
                tick.outcome,
                Ok(Outcome::Applied {
                    sent: 2,
                    remaining: 1
                })
            ),
            "{:?}",
            tick.outcome
        );
        assert_eq!(
            client.calls(),
            [Call::Add("1".to_string()), Call::Add("2".to_string())],
            "the batch never reaches the read"
        );
        // 続きがあるので次の batch までの間が state に残る｡
        assert!(tick.state.paused_until.is_some(), "{:?}", tick.state);
    }

    #[test]
    fn a_plan_that_was_sent_through_leaves_no_file_behind() {
        // 置いたままにすると次の tick が残務として読み､diff を飛ばす｡
        let scratch = Scratch::new("auto-apply-done");
        save_plan(
            &scratch.paths().sync_plan_file(),
            &plan_of("7", &["1"], &[], 0),
        )
        .unwrap();
        let client = FakeApi::new().writes(vec![Ok(())]);

        tick(
            scratch.paths(),
            &client,
            "me",
            "7",
            pacing(5, false),
            PRUNE_LIMIT,
            NOW,
        );

        assert_eq!(load_plan(&scratch.paths().sync_plan_file()).unwrap(), None);
    }

    #[test]
    fn a_plan_whose_removals_are_held_keeps_its_file_after_the_additions_land() {
        // CLI と分かれる一点｡保留された removal は支払い済みなので､
        // `--sync-list --apply --prune` がここから送る｡
        let scratch = Scratch::new("auto-apply-held");
        save_plan(
            &scratch.paths().sync_plan_file(),
            &plan_of("7", &["1"], &["8", "9"], 2),
        )
        .unwrap();
        let client = FakeApi::new().writes(vec![Ok(())]);

        let tick = tick(
            scratch.paths(),
            &client,
            "me",
            "7",
            pacing(5, false),
            PRUNE_LIMIT,
            NOW,
        );

        assert!(
            matches!(
                tick.outcome,
                Ok(Outcome::Applied {
                    sent: 1,
                    remaining: 0
                })
            ),
            "{:?}",
            tick.outcome
        );
        assert_eq!(client.calls(), [Call::Add("1".to_string())]);
        let left = load_plan(&scratch.paths().sync_plan_file())
            .unwrap()
            .unwrap();
        assert_eq!(left.pending_count(Action::Remove), 2);
    }

    #[test]
    fn a_plan_diffed_against_another_list_is_dropped_and_the_diff_runs_instead() {
        // loop には拒否を伝える相手がいないので､`run.rs` のように
        // エラーにはせず捨てる｡
        let scratch = Scratch::new("auto-other-list");
        save_plan(
            &scratch.paths().sync_plan_file(),
            &plan_of("other", &["1"], &[], 0),
        )
        .unwrap();
        let client = FakeApi::new()
            .following(vec![Ok(page(&[], None))])
            .members(vec![Ok(page(&[], None))]);

        let tick = tick(
            scratch.paths(),
            &client,
            "me",
            "7",
            pacing(5, false),
            PRUNE_LIMIT,
            NOW,
        );

        assert!(
            matches!(tick.outcome, Ok(Outcome::Diffed { .. })),
            "{:?}",
            tick.outcome
        );
        assert!(
            !client
                .calls()
                .iter()
                .any(|call| matches!(call, Call::Add(_) | Call::Remove(_))),
            "nothing from another list's plan may be sent"
        );
    }

    #[test]
    fn a_refused_write_backs_the_loop_off_instead_of_failing_the_tick() {
        // rate limit はエラーではなく outcome だ｡loop がそれについて
        // できることは待つことだけだからだ｡
        let scratch = Scratch::new("auto-refused");
        save_plan(
            &scratch.paths().sync_plan_file(),
            &plan_of("7", &["1", "2"], &[], 0),
        )
        .unwrap();
        let client = FakeApi::new().writes(vec![Err(rate_limited(NOW + 900, true))]);

        let tick = tick(
            scratch.paths(),
            &client,
            "me",
            "7",
            pacing(5, false),
            PRUNE_LIMIT,
            NOW,
        );

        assert!(
            matches!(
                tick.outcome,
                Ok(Outcome::RateLimited {
                    sent: 0,
                    remaining: 2,
                    ..
                })
            ),
            "{:?}",
            tick.outcome
        );
        assert_eq!(tick.state.refusals, 1);
        assert!(tick.wake_at > NOW, "the loop must not come straight back");
    }

    // --- state を保存できない tick ---

    #[test]
    fn a_state_that_cannot_be_saved_still_gives_the_loop_a_deadline() {
        // 走っている loop は少なくとも次の保存までペース配分できなければ
        // ならない — 保存の失敗は log であって tick の中断ではない｡
        let scratch = Scratch::new("auto-readonly");
        let state_dir = scratch
            .paths()
            .sync_state_file()
            .parent()
            .unwrap()
            .to_owned();
        let mut permissions = std::fs::metadata(&state_dir).unwrap().permissions();
        {
            use std::os::unix::fs::PermissionsExt as _;
            permissions.set_mode(0o500);
        }
        std::fs::set_permissions(&state_dir, permissions).unwrap();
        let client = silent();

        let tick = tick(
            scratch.paths(),
            &client,
            "me",
            "7",
            pacing(5, false),
            PRUNE_LIMIT,
            NOW,
        );

        assert!(tick.outcome.is_err(), "{:?}", tick.outcome);
        assert_eq!(tick.wake_at, NOW.saturating_add(i64::from(INTERVAL)));
        assert_eq!(tick.state.blocked_until, Some(tick.wake_at));

        // Scratch が片付けられるよう書き込みを戻す｡
        let mut permissions = std::fs::metadata(&state_dir).unwrap().permissions();
        {
            use std::os::unix::fs::PermissionsExt as _;
            permissions.set_mode(0o700);
        }
        std::fs::set_permissions(&state_dir, permissions).unwrap();
    }
}
