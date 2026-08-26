//! #163 の sync のうち支払う側の半分: ページングした read､apply の loop､
//! そして `--sync-list` の入口｡
//!
//! `mod.rs` の末尾ではなく別ファイルにしてあるのは､ここが金を使う側だから
//! だ｡以下の関数はどれも `cache` の reload 経路と同じく実際の request を
//! 投げる — ただし投げる相手は [`super::api::ListSyncApi`] で､テストは
//! そこにページと write の結果を仕込む｡だから read の連結も apply の
//! 中断と再開も HTTP を張らずに確かめられる｡transport の側は
//! フィクスチャ JSON を通した `x_api::client` のテストが見ている｡

use anyhow::{Context as _, Result};

use super::api::ListSyncApi;
use super::schedule::Outcome;
use super::{Action, Plan, load_plan, load_state, plan, report, save_plan, save_state};
use crate::cache;
use crate::config::Config;
use crate::oauth;
use crate::paths::Paths;
use crate::x_api::XClient;
use crate::x_api::model::User;

/// #163 の二つの read の片方を cursor が尽きるまでページングし､全アカウント
/// を返すか､さもなくば何も返さない｡
///
/// **意図しての all-or-nothing｡** [`super::plan`] は集合差なので､途中で
/// 切れた read は小さい答えではなく誤った答えだ: 読まれなかった follow は
/// unfollow に見えて削除を得るし､読まれなかった member は再追加される｡
/// どのページの失敗にも `Err` を返すことが､半分しか読めていない側を diff に
/// 到達させないための仕組みだ｡
///
/// `MAX_PAGES` は終わらない cursor への歯止めであって､誰かが当たるべき
/// 上限ではない: 1 ページ 100 アカウントなら 20,000 まで許し､X 自身の
/// following 上限をはるかに超える｡ここに当たったら黙って切り詰めずに
/// エラーにするのは､ページの失敗と同じ理由による｡
///
/// `fetch_page` が継ぎ目だ｡呼び出し側は [`super::api::ListSyncApi`] の
/// ページ取得を渡し､テストは仕込んだページの列を渡す｡
fn read_all(
    what: &str,
    mut fetch_page: impl FnMut(Option<&str>) -> Result<(Vec<User>, Option<String>)>,
) -> Result<Vec<User>> {
    const MAX_PAGES: usize = 200;

    let mut all = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let (page, next) = fetch_page(cursor.as_deref())
            .with_context(|| format!("could not read the whole {what} — nothing was changed"))?;
        all.extend(page);
        match next {
            Some(token) => cursor = Some(token),
            None => return Ok(all),
        }
    }
    anyhow::bail!("the {what} did not finish paging after {MAX_PAGES} pages — nothing was changed")
}

/// 両側を丸ごと読んで diff する (#163 の dry-run)｡
///
/// sync の read 費用をすべて使う: 両側のどのアカウントも課金 resource だ｡
/// ここでは X に何も書かない — 結果は [`apply`] が消費する [`super::Plan`]
/// だ｡
pub(super) fn plan_sync(
    paths: &Paths,
    client: &dyn ListSyncApi,
    user_id: &str,
    list_id: &str,
    now: i64,
) -> Result<Plan> {
    let following = match paths.profile().sync_seed_usernames() {
        None => read_all("follow list", |cursor| {
            client.following_page(paths, user_id, cursor, now)
        })?,
        Some(usernames) => seed_users(paths, client, usernames, now)?,
    };
    let members = read_all("list members", |cursor| {
        client.list_members_page(paths, list_id, cursor, now)
    })?;
    Ok(plan(list_id, now, &following, &members))
}

/// follow グラフの read を固定の screen name 群で代用する (#169) —
/// development build の sync 元で､#163 の作業がサインイン中のユーザーの
/// follow 全アカウント分の dry run を課金させないためのものだ｡
///
/// `cache::reload` が使うのと同じキャッシュ付き lookup で解決するので､
/// その月の初回だけ名前ごとに 1 課金 request､以降はゼロで済む｡
/// [`super::plan`] に届くのは `id` と `username` だけなので､`name` には
/// 2 回目の lookup 相当の表示名ではなく screen name を入れてある｡
fn seed_users(
    paths: &Paths,
    client: &dyn ListSyncApi,
    usernames: &[&str],
    now: i64,
) -> Result<Vec<User>> {
    usernames
        .iter()
        .map(|username| {
            // `cache::reload` 自身の lookup と同じ形: まずキャッシュ､
            // API に訊かざるをえなかったものは永続化する｡
            let id = if let Some(id) = cache::cached_user_id(paths, username, now)? {
                id
            } else {
                let id = client
                    .lookup_user_id(paths, username, now)
                    .with_context(|| {
                        format!("could not resolve the development sync seed @{username}")
                    })?;
                cache::save_user_id(paths, username, &id, now)?;
                id
            };
            Ok(User {
                id,
                name: (*username).to_string(),
                username: (*username).to_string(),
                profile_image_url: None,
            })
        })
        .collect()
}

/// `plan` の残り entry を適用し､届くたびに印を付けて永続化する (#163)｡
///
/// 最後に一度ではなく entry ごとに保存する: plan ファイルの意義はまさに､
/// 途中で中断した apply — rate limit､crash､`^C` — がどちらの側も読み直さず､
/// 既に通ったものを再送もせずに再開できることにある｡最後に一度だけ保存
/// したのでは､再開が必要とする情報をちょうど失う｡
///
/// `prune` が門番をするのは removal だけだ｡addition こそミラーの目的で
/// あって､誰かが手で list に入れたアカウントを消すことは #163 が未決の
/// ままにした部分なので､求められない限り起きない｡
///
/// 最初の失敗で止めてそれを返し､disk 上の plan には届いたものがすべて
/// 反映される｡エラーを越えて続ければ､受け付けられないと今しがた証明した
/// credential や list に対して write を使い続けることになる｡
fn apply(
    paths: &Paths,
    client: &dyn ListSyncApi,
    plan: &mut Plan,
    prune: bool,
    now: i64,
) -> (usize, Result<()>) {
    apply_some(paths, client, plan, prune, now, usize::MAX)
}

/// [`apply`] と同じだが､最大 `limit` 件送ったところで返る — background
/// sync の仕事の単位だ｡実際に通った件数を､batch を止めた失敗があれば
/// それと **並べて** 返す: この件数があるおかげで `sync::state` は､write が
/// 届いた直後の refusal と refusal に続く refusal を見分けられる｡
/// `Result<usize>` では一方を報告するのにもう一方を捨てるしかない｡
///
/// CLI に上限は要らない: `--apply` は終わらせることが仕事の前景コマンドだ｡
/// loop には要る｡rate limit とは無関係の二つの理由による (追跡している
/// window が既に自力で止めるからだ): 2,000 件の request を送る tick は
/// その間ずっと background executor を占有し､途中できれいに落とせない｡
/// それに addition をすべて送ってから最初の removal に行くので､ひどく
/// 古びた list では stale な member が消える何時間も前に addition だけが
/// 見えてしまう｡
///
/// removal を交互に混ぜるのはその二つ目の理由による — `limit` は addition
/// に先に使い切らず､両方の action に振り分ける｡
///
/// write と write のあいだの間は [`super::api::ListSyncApi::pause_between_writes`]
/// に頼む｡本番はそこで眠り､テストは渡された長さを記録するだけなので､
/// 「1 件目の前には待たない」を suite を止めずに確かめられる｡
pub(super) fn apply_some(
    paths: &Paths,
    client: &dyn ListSyncApi,
    plan: &mut Plan,
    prune: bool,
    now: i64,
    limit: usize,
) -> (usize, Result<()>) {
    let mut sent = 0usize;
    let batch = super::schedule::next_batch(plan, prune, limit);
    let batch_size = batch.len();
    for (action, user_id) in batch {
        // batch の中を散らす｡これが無いと batch は同じ秒のうちに全件を
        // 投げる — #197 のロックの直前にしていた形｡1 件目の前には置かない｡
        // tick は既に batch と batch の間を待って来ている｡
        if sent > 0 {
            let gap = super::state::write_gap(crate::rate_limit::random_jitter_fraction());
            // 眠る前に書く (#231)｡行のタイムスタンプと待つ秒数が sleep を
            // 挟むので､次の 1 回はログだけで間が本当に空いたかを読める｡
            // batch サマリだけでは 1 件ずつ送ったのか同じ秒に投げたのかが
            // 見分けられなかった｡
            crate::log::info(&format!(
                "list sync: waiting {}s before write {} of {batch_size}",
                gap.as_secs(),
                sent.saturating_add(1)
            ));
            client.pause_between_writes(gap);
        }
        let result = match action {
            Action::Add => client.add_member(paths, &plan.list_id, &user_id, now),
            Action::Remove => client.remove_member(paths, &plan.list_id, &user_id, now),
        };
        if let Err(error) = result {
            return (sent, Err(error));
        }
        plan.mark_applied(&user_id, action);
        sent = sent.saturating_add(1);
        if let Err(error) = save_plan(&paths.sync_plan_file(), plan) {
            return (sent, Err(error));
        }
    }
    (sent, Ok(()))
}

/// `--sync-list` が何をするよう求められたか｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Request {
    /// plan の write を送る｡これが無ければ実行は dry-run になる: 両側を
    /// 読み､plan ファイルを書き､report を印字して止まる｡既定を dry-run に
    /// することが #163 の「適用前に確認する」そのものだ — 対話的な
    /// プロンプトは無い｡ドルのかかる実行が shell の履歴からキー 1 つで
    /// 届く場所にあってはならないからだ｡
    pub apply: bool,
    /// removal も送る｡既定では off — [`apply`] を見よ｡
    pub prune: bool,
}

/// `--sync-list` (#163)｡プロセスの exit code を返す｡
///
/// ここでの失敗はどれも中途半端な実行ではなく､支出の拒否だ: list が
/// 設定されていない､session が無い､scope の無い session､別 list 向けの
/// plan｡そのうち最も安く済む検査から先に置いてある｡
pub(crate) fn run_cli(config: &Config, paths: &Paths, request: Request) -> i32 {
    let Some(list_id) = config.list_id.clone() else {
        eprintln!(
            "--sync-list needs a list to sync into. Set X_LIST_ID, or add \
             list_id to config.toml."
        );
        return 1;
    };

    let resolution = match oauth::resolve_credential(config, paths, oauth::unix_now()) {
        Ok(resolution) => resolution,
        Err(error) => {
            eprintln!("could not resolve a credential: {error:#}");
            return 1;
        }
    };
    if let Some(demotion) = &resolution.demotion {
        eprintln!("{}", oauth::describe_demotion(demotion));
    }
    let Some(credential) = resolution.credential else {
        eprintln!(
            "no signed-in session is available. Run twigpui without --sync-list and click \
             \"Sign in with X\" once; this flag reuses the session that leaves behind."
        );
        return 1;
    };

    // 何かを使う前に: #163 は `SCOPES` に `follows.read` と `list.write` を
    // 足したので､それ以前に認可された session は follow list を丸ごと
    // ページングした挙げ句に最初の write で拒否される — あるいは `/me` の
    // 分を既に払ったうえで最初の read で拒否される｡
    if let Some(missing) = super::missing_scope(credential.scope.as_deref()) {
        eprintln!(
            "this session was authorized before --sync-list existed and does not carry \
             {missing}. Launch twigpui and click \"Re-authorize\" once, then run this again."
        );
        return 1;
    }

    let client = XClient::renewing(credential.session);
    let user_id = match resolve_own_id(paths, &client) {
        Ok(user_id) => user_id,
        Err(error) => {
            eprintln!("could not resolve the signed-in account: {error:#}");
            return 1;
        }
    };

    match run(
        paths,
        &client,
        &user_id,
        &list_id,
        request,
        config.sync_interval_seconds,
    ) {
        Ok(report) => {
            println!("{report}");
            0
        }
        Err(error) => {
            eprintln!("sync failed: {error:#}");
            1
        }
    }
}

/// サインイン中のアカウント自身の id｡`/me` のキャッシュが新しければ
/// (30 日 — `cache::cached_me` を見よ) そこから､でなければ API から取る｡
/// [`run_cli`] が拒否の並びとして読めるように関数を分けてある｡
fn resolve_own_id(paths: &Paths, client: &dyn ListSyncApi) -> Result<String> {
    let now = oauth::unix_now();
    if let Some(entry) = cache::cached_me(paths, now)? {
        return Ok(entry.id);
    }
    let user = client.signed_in_user(paths, now)?;
    cache::save_me(paths, &user.id, &user.username, now)?;
    Ok(user.id)
}

/// [`run_cli`] のうち credential と list が揃っている部分｡上のエラーが
/// すべて素の拒否になり､下がすべて一つの `Result` になるように切り出して
/// ある｡
///
/// `--apply` は background sync の記憶 ([`super::SyncState`]) を共有する:
/// backoff を読み､そう告げたうえで **それでも送る** — 端末の前にいる人が
/// 上限が明けたか見るために batch を一つ投げてみるのは #197 が持つ最も
/// 安い実測であり､拒否すればそれを取り上げることになる｡返ってきたものは
/// 同じ state へ settle されるので､届いた write は loop にとっても連続を
/// 終わらせるし､refusal はそれを伸ばす｡
fn run(
    paths: &Paths,
    client: &dyn ListSyncApi,
    user_id: &str,
    list_id: &str,
    request: Request,
    interval_seconds: u32,
) -> Result<String> {
    let plan_path = paths.sync_plan_file();
    let now = oauth::unix_now();

    if !request.apply {
        let plan = plan_sync(paths, client, user_id, list_id, now)?;
        save_plan(&plan_path, &plan)?;
        return Ok(format!(
            "{}\n\nnothing was changed. Re-run with --apply to send these.",
            report(&plan)
        ));
    }

    let Some(mut plan) = load_plan(&plan_path)? else {
        anyhow::bail!(
            "no sync plan on file. Run --sync-list without --apply first: the dry-run is \
             what reads both sides and writes the plan this consumes."
        );
    };
    // plan が意味を持つのは､それが diff された list に対してだけだ｡
    // `list_id` が変わったあとに適用すれば､別の membership から計算した
    // diff で､誰も頼んでいない list を書き換えることになる｡
    anyhow::ensure!(
        plan.list_id == list_id,
        "the plan on file is for list {}, but list {list_id} is configured. Re-run \
         --sync-list without --apply to diff the configured list.",
        plan.list_id
    );

    // write と write のあいだが揺らぐようになって､CLI の apply は数分から
    // 時間単位へ移った｡黙って止まって見えるので最悪ケースを先に出す —
    // `x-api-budget` の「押す前に最悪ケースを出す」と同じ規則｡
    let pending = super::schedule::sendable(&plan, request.prune);
    if pending > 0 {
        let worst_minutes = pending
            .saturating_mul(
                usize::try_from(
                    super::state::WRITE_GAP_FLOOR_SECONDS + super::state::WRITE_GAP_SPREAD_SECONDS,
                )
                .unwrap_or(0),
            )
            .saturating_div(60);
        eprintln!(
            "note: sending {pending} write(s), pausing 3-20s between each so the run does not \
             look like a script. Worst case about {worst_minutes} minute(s)."
        );
    }

    let state_path = paths.sync_state_file();
    let state = load_state(&state_path);
    if state.is_blocked(now) {
        eprintln!(
            "note: the background sync is backing off until unix time {} after {} consecutive \
             refusal(s); sending anyway, and recording what happens for it",
            state.blocked_until.unwrap_or(now),
            state.refusals
        );
    }

    let (sent, result) = apply(paths, client, &mut plan, request.prune, now);
    let remaining = super::schedule::sendable(&plan, request.prune);
    let outcome = super::schedule::apply_outcome(sent, remaining, result);
    let spacing = super::state::Spacing {
        interval_seconds,
        apply_pause_seconds: super::state::apply_pause_seconds(
            crate::rate_limit::random_jitter_fraction(),
        ),
    };
    let settled = super::state::settle(state, outcome.as_ref().ok(), now, spacing);
    save_state(&state_path, &settled.state)?;

    let finished = plan.is_complete() || (!request.prune && plan.pending_count(Action::Add) == 0);
    if matches!(outcome, Ok(Outcome::Applied { .. })) && finished {
        // 再開すべきものは残っておらず､置いたままにすると次の --apply に
        // 残務があるように見えてしまう｡
        std::fs::remove_file(&plan_path)
            .with_context(|| format!("could not remove {}", plan_path.display()))?;
    }
    match outcome? {
        Outcome::RateLimited { opaque, .. } => anyhow::bail!(
            "rate limited after {sent} write(s) landed{}; the plan on file records them. \
             Backing off until unix time {} (refusal #{}); re-run --apply after that.",
            if opaque {
                " — by a cap the x-rate-limit headers do not describe"
            } else {
                ""
            },
            settled.wake_at,
            settled.state.refusals
        ),
        _ => Ok(report(&plan)),
    }
}

#[cfg(test)]
mod tests {
    use super::super::api::fake::{Call, FakeApi, Scratch, page, rate_limited, user};
    use super::*;
    use crate::sync::{Action, PlanEntry};

    /// 未適用の entry だけを持つ plan｡`members_total` は removal を測る
    /// 分母なので､prune の判定が絡むテストが自分で上書きする｡
    fn plan_of(list_id: &str, adds: &[&str], removals: &[&str]) -> Plan {
        let entry = |user_id: &str, action| PlanEntry {
            user_id: user_id.to_string(),
            username: format!("user{user_id}"),
            action,
            applied: false,
        };
        Plan {
            list_id: list_id.to_string(),
            created_at: 0,
            members_total: removals.len(),
            entries: adds
                .iter()
                .map(|id| entry(id, Action::Add))
                .chain(removals.iter().map(|id| entry(id, Action::Remove)))
                .collect(),
        }
    }

    fn applied_ids(plan: &Plan) -> Vec<&str> {
        plan.entries
            .iter()
            .filter(|entry| entry.applied)
            .map(|entry| entry.user_id.as_str())
            .collect()
    }

    // --- read_all: 部分的な read は決して plan にならない ---

    #[test]
    fn every_page_is_joined_in_the_order_it_arrived() {
        let pages = std::cell::RefCell::new(vec![
            Ok(page(&[("1", "a")], Some("next"))),
            Ok(page(&[("2", "b")], None)),
        ]);
        let read = read_all("follow list", |_| pages.borrow_mut().remove(0)).unwrap();
        assert_eq!(
            read.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(),
            ["1", "2"]
        );
    }

    #[test]
    fn the_cursor_of_one_page_is_what_the_next_page_is_asked_for() {
        // ここを取り違えると同じ 1 ページ目を課金しながら読み続ける｡
        let asked = std::cell::RefCell::new(Vec::new());
        let pages = std::cell::RefCell::new(vec![
            Ok(page(&[("1", "a")], Some("cursor-2"))),
            Ok(page(&[("2", "b")], None)),
        ]);
        read_all("follow list", |cursor| {
            asked.borrow_mut().push(cursor.map(str::to_string));
            pages.borrow_mut().remove(0)
        })
        .unwrap();
        assert_eq!(
            asked.into_inner(),
            [None, Some("cursor-2".to_string())],
            "the second page must be asked for with the first page's cursor"
        );
    }

    #[test]
    fn a_page_that_fails_halfway_fails_the_whole_read() {
        // 半分読めた follow list は小さい答えではなく誤った答えだ｡
        let pages = std::cell::RefCell::new(vec![
            Ok(page(&[("1", "a")], Some("next"))),
            Err(anyhow::anyhow!("the API said 503")),
        ]);
        let error = read_all("follow list", |_| pages.borrow_mut().remove(0))
            .unwrap_err()
            .to_string();
        assert!(error.contains("nothing was changed"), "{error}");
    }

    #[test]
    fn a_cursor_that_never_ends_is_an_error_rather_than_a_truncated_read() {
        let error = read_all("follow list", |_| Ok(page(&[("1", "a")], Some("forever"))))
            .unwrap_err()
            .to_string();
        assert!(error.contains("did not finish paging"), "{error}");
        assert!(error.contains("nothing was changed"), "{error}");
    }

    // --- plan_sync: 両側を読んで diff へ渡す ---

    #[test]
    fn the_diff_reads_both_sides_and_plans_from_what_came_back() {
        let scratch = Scratch::new("plan-sync");
        let client = FakeApi::new()
            .following(vec![
                Ok(page(&[("1", "alice")], Some("page-2"))),
                Ok(page(&[("2", "bob")], None)),
            ])
            .members(vec![Ok(page(&[("2", "bob"), ("3", "carol")], None))]);

        let plan = plan_sync(scratch.paths(), &client, "me", "7", 100).unwrap();

        assert_eq!(plan.pending_count(Action::Add), 1);
        assert_eq!(plan.pending_count(Action::Remove), 1);
        assert_eq!(plan.members_total, 2);
        assert_eq!(
            client.calls(),
            [
                Call::Following(None),
                Call::Following(Some("page-2".to_string())),
                Call::Members(None),
            ]
        );
    }

    #[test]
    fn a_failed_follow_read_never_reaches_the_member_read() {
        // 続ければ diff の片側だけに金を払ったうえで捨てることになる｡
        let scratch = Scratch::new("plan-sync-fail");
        let client = FakeApi::new().following(vec![Err(anyhow::anyhow!("the API said 401"))]);

        let error = plan_sync(scratch.paths(), &client, "me", "7", 100)
            .unwrap_err()
            .to_string();

        assert!(error.contains("nothing was changed"), "{error}");
        assert_eq!(client.calls(), [Call::Following(None)]);
    }

    #[test]
    fn the_dev_profile_builds_its_follow_side_from_the_seed_instead_of_the_graph() {
        // #169 の要点: 開発中の diff が本物の follow グラフを課金しない｡
        let scratch = Scratch::dev("seed");
        let client = FakeApi::new()
            .lookups(vec![
                Ok("11".to_string()),
                Ok("12".to_string()),
                Ok("13".to_string()),
                Ok("14".to_string()),
            ])
            .members(vec![Ok(page(&[], None))]);

        let plan = plan_sync(scratch.paths(), &client, "me", "7", 100).unwrap();

        assert_eq!(plan.pending_count(Action::Add), 4);
        assert!(
            !client
                .calls()
                .iter()
                .any(|call| matches!(call, Call::Following(_))),
            "the seed replaces the follow read outright: {:?}",
            client.calls()
        );
    }

    #[test]
    fn the_second_diff_resolves_the_seed_from_the_cache() {
        // 名前ごとに月 1 回の lookup しか払わない (`cache::reload` と同じ形)｡
        // 2 回目に lookup を求めれば fake は答えを持たず落ちる｡
        let scratch = Scratch::dev("seed-cached");
        let client = FakeApi::new()
            .lookups(vec![
                Ok("11".to_string()),
                Ok("12".to_string()),
                Ok("13".to_string()),
                Ok("14".to_string()),
            ])
            .members(vec![Ok(page(&[], None)), Ok(page(&[], None))]);

        plan_sync(scratch.paths(), &client, "me", "7", 100).unwrap();
        let again = plan_sync(scratch.paths(), &client, "me", "7", 100).unwrap();

        assert_eq!(again.pending_count(Action::Add), 4);
        assert_eq!(
            client
                .calls()
                .iter()
                .filter(|call| matches!(call, Call::Lookup(_)))
                .count(),
            4,
            "the second diff must not pay for the same names again"
        );
    }

    #[test]
    fn a_seed_name_that_will_not_resolve_names_itself() {
        let scratch = Scratch::dev("seed-fail");
        let client = FakeApi::new().lookups(vec![Err(anyhow::anyhow!("the API said 404"))]);

        let error = plan_sync(scratch.paths(), &client, "me", "7", 100)
            .unwrap_err()
            .to_string();

        assert!(error.contains("development sync seed @"), "{error}");
    }

    // --- apply_some: batch 1 回分の write ---

    #[test]
    fn a_batch_stops_at_its_limit_and_leaves_the_rest_on_file() {
        let scratch = Scratch::new("apply-limit");
        let client = FakeApi::new().writes(vec![Ok(()), Ok(())]);
        let mut plan = plan_of("7", &["1", "2", "3"], &[]);

        let (sent, result) = apply_some(scratch.paths(), &client, &mut plan, false, 0, 2);

        assert_eq!(sent, 2);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(plan.pending_count(Action::Add), 1);
        let on_file = load_plan(&scratch.paths().sync_plan_file())
            .unwrap()
            .unwrap();
        assert_eq!(applied_ids(&on_file), ["1", "2"]);
    }

    #[test]
    fn additions_and_removals_alternate_so_a_stopped_catch_up_is_not_all_one_sided() {
        let scratch = Scratch::new("apply-alternate");
        let client = FakeApi::new().writes(vec![Ok(()), Ok(()), Ok(()), Ok(())]);
        let mut plan = plan_of("7", &["1", "2"], &["8", "9"]);

        let (_, result) = apply_some(scratch.paths(), &client, &mut plan, true, 0, 4);
        assert!(result.is_ok(), "{result:?}");

        assert_eq!(
            client.calls(),
            [
                Call::Add("1".to_string()),
                Call::Remove("8".to_string()),
                Call::Add("2".to_string()),
                Call::Remove("9".to_string()),
            ]
        );
    }

    #[test]
    fn without_prune_the_batch_never_reaches_a_removal() {
        let scratch = Scratch::new("apply-no-prune");
        let client = FakeApi::new().writes(vec![Ok(())]);
        let mut plan = plan_of("7", &["1"], &["8"]);

        let (sent, _) = apply_some(scratch.paths(), &client, &mut plan, false, 0, usize::MAX);

        assert_eq!(sent, 1);
        assert_eq!(client.calls(), [Call::Add("1".to_string())]);
        assert_eq!(plan.pending_count(Action::Remove), 1);
    }

    #[test]
    fn a_refused_write_stops_the_batch_and_comes_back_beside_what_landed() {
        // 件数と失敗が並んで返ることが `state::settle` の見分けの拠りどころだ｡
        let scratch = Scratch::new("apply-refused");
        let client = FakeApi::new().writes(vec![Ok(()), Err(rate_limited(9_000, true))]);
        let mut plan = plan_of("7", &["1", "2", "3"], &[]);

        let (sent, result) = apply_some(scratch.paths(), &client, &mut plan, false, 0, usize::MAX);

        assert_eq!(sent, 1);
        assert!(result.is_err(), "the refusal must come back");
        assert_eq!(client.calls().len(), 2, "the batch stops at the refusal");
        // 届いたものは disk に残る｡再開が再送しないのはこれがあるからだ｡
        let on_file = load_plan(&scratch.paths().sync_plan_file())
            .unwrap()
            .unwrap();
        assert_eq!(applied_ids(&on_file), ["1"]);
    }

    #[test]
    fn the_batch_pauses_before_every_write_but_the_first() {
        // tick は batch と batch の間を待って来ているので､1 件目の前に
        // 待てば batch ごとに二重の間が入る｡
        let scratch = Scratch::new("apply-pause");
        let client = FakeApi::new().writes(vec![Ok(()), Ok(()), Ok(())]);
        let mut plan = plan_of("7", &["1", "2", "3"], &[]);

        let (_, result) = apply_some(scratch.paths(), &client, &mut plan, false, 0, usize::MAX);
        assert!(result.is_ok(), "{result:?}");

        let pauses = client.pauses();
        assert_eq!(pauses.len(), 2, "3 writes take 2 gaps: {pauses:?}");
        let floor = super::super::state::WRITE_GAP_FLOOR_SECONDS;
        let ceiling = floor + super::super::state::WRITE_GAP_SPREAD_SECONDS;
        for gap in pauses {
            assert!(
                (floor..=ceiling).contains(&gap.as_secs()),
                "the gap must stay inside the configured spread: {gap:?}"
            );
        }
    }

    #[test]
    fn the_unlimited_apply_sends_the_whole_plan() {
        let scratch = Scratch::new("apply-all");
        let client = FakeApi::new().writes(vec![Ok(()), Ok(()), Ok(())]);
        let mut plan = plan_of("7", &["1", "2"], &["8"]);

        let (sent, result) = apply(scratch.paths(), &client, &mut plan, true, 0);

        assert_eq!(sent, 3);
        assert!(result.is_ok(), "{result:?}");
        assert!(plan.is_complete());
    }

    // --- run: dry-run と apply の入口 ---

    fn request(apply: bool, prune: bool) -> Request {
        Request { apply, prune }
    }

    #[test]
    fn a_dry_run_writes_the_plan_and_says_nothing_was_changed() {
        let scratch = Scratch::new("run-dry");
        let client = FakeApi::new()
            .following(vec![Ok(page(&[("1", "alice")], None))])
            .members(vec![Ok(page(&[], None))]);

        let report = run(
            scratch.paths(),
            &client,
            "me",
            "7",
            request(false, false),
            21_600,
        )
        .unwrap();

        assert!(report.contains("nothing was changed"), "{report}");
        let on_file = load_plan(&scratch.paths().sync_plan_file())
            .unwrap()
            .unwrap();
        assert_eq!(on_file.pending_count(Action::Add), 1);
    }

    #[test]
    fn applying_without_a_plan_on_file_is_refused() {
        // dry-run こそが両側を読んで plan を書く｡それを飛ばした --apply に
        // 送るものは無い｡
        let scratch = Scratch::new("run-no-plan");
        let client = FakeApi::new();

        let error = run(
            scratch.paths(),
            &client,
            "me",
            "7",
            request(true, false),
            21_600,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("no sync plan on file"), "{error}");
        assert!(client.calls().is_empty(), "nothing may be sent");
    }

    #[test]
    fn a_plan_diffed_against_another_list_is_refused() {
        // 適用すれば誰も頼んでいない list の membership を書き換える｡
        let scratch = Scratch::new("run-other-list");
        save_plan(
            &scratch.paths().sync_plan_file(),
            &plan_of("other", &["1"], &[]),
        )
        .unwrap();
        let client = FakeApi::new();

        let error = run(
            scratch.paths(),
            &client,
            "me",
            "7",
            request(true, false),
            21_600,
        )
        .unwrap_err()
        .to_string();

        assert!(
            error.contains("the plan on file is for list other"),
            "{error}"
        );
        assert!(client.calls().is_empty(), "nothing may be sent");
    }

    #[test]
    fn a_plan_that_was_sent_through_leaves_no_file_behind() {
        // 置いたままにすると次の --apply に残務があるように見える｡
        let scratch = Scratch::new("run-complete");
        save_plan(
            &scratch.paths().sync_plan_file(),
            &plan_of("7", &["1"], &[]),
        )
        .unwrap();
        let client = FakeApi::new().writes(vec![Ok(())]);

        run(
            scratch.paths(),
            &client,
            "me",
            "7",
            request(true, false),
            21_600,
        )
        .unwrap();

        assert_eq!(
            load_plan(&scratch.paths().sync_plan_file()).unwrap(),
            None,
            "the plan file is gone"
        );
    }

    #[test]
    fn a_refused_apply_records_the_backoff_and_fails() {
        let scratch = Scratch::new("run-refused");
        save_plan(
            &scratch.paths().sync_plan_file(),
            &plan_of("7", &["1", "2"], &[]),
        )
        .unwrap();
        let client = FakeApi::new().writes(vec![Ok(()), Err(rate_limited(9_000, true))]);

        let error = run(
            scratch.paths(),
            &client,
            "me",
            "7",
            request(true, false),
            21_600,
        )
        .unwrap_err()
        .to_string();

        assert!(
            error.contains("rate limited after 1 write(s) landed"),
            "{error}"
        );
        // background sync と同じ記憶を共有する: 拒否はそちらの backoff も伸ばす｡
        let state = load_state(&scratch.paths().sync_state_file());
        assert_eq!(state.refusals, 1);
        assert!(state.blocked_until.is_some(), "{state:?}");
        // 届いた 1 件は plan ファイルに残り､再実行が再送しない｡
        let on_file = load_plan(&scratch.paths().sync_plan_file())
            .unwrap()
            .unwrap();
        assert_eq!(applied_ids(&on_file), ["1"]);
    }

    #[test]
    fn a_run_without_prune_is_finished_once_every_addition_landed() {
        // CLI は removal を送るよう求められていないので､残った removal は
        // 残務ではない — plan ファイルは消える｡loop の側の規則は違う
        // (`auto::apply` を見よ)｡
        let scratch = Scratch::new("run-adds-only");
        save_plan(
            &scratch.paths().sync_plan_file(),
            &plan_of("7", &["1"], &["8"]),
        )
        .unwrap();
        let client = FakeApi::new().writes(vec![Ok(())]);

        run(
            scratch.paths(),
            &client,
            "me",
            "7",
            request(true, false),
            21_600,
        )
        .unwrap();

        assert_eq!(load_plan(&scratch.paths().sync_plan_file()).unwrap(), None);
    }

    // --- resolve_own_id: /me は 30 日に 1 回しか払わない ---

    #[test]
    fn the_signed_in_id_is_looked_up_once_and_then_read_from_the_cache() {
        let scratch = Scratch::new("me");
        let client = FakeApi::new().me(Ok(user("42", "alice")));

        assert_eq!(resolve_own_id(scratch.paths(), &client).unwrap(), "42");
        // 2 回目に /me を求めれば fake は答えを持たず落ちる｡
        assert_eq!(resolve_own_id(scratch.paths(), &client).unwrap(), "42");
        assert_eq!(client.calls(), [Call::Me]);
    }

    // --- run_cli: 支出の前に立つ拒否 ---

    /// `--sync-list` が読むフィールドだけを意味のある値にした設定｡
    fn config(list_id: Option<&str>) -> Config {
        Config {
            oauth_client_id: "client".to_string(),
            target_username: "alice".to_string(),
            max_results: 10,
            min_fetch_interval_seconds: 60,
            theme: crate::theme::ThemeMode::Light,
            log_level: crate::log::Level::Info,
            request_price: None,
            list_id: list_id.map(str::to_string),
            daily_request_budget: None,
            auto_sync_list: false,
            sync_interval_seconds: 21_600,
            sync_prune_limit_percent: 10,
            sync_writes_per_batch: 5,
            auto_refresh: false,
            auto_refresh_interval_seconds: 300,
            follow_new_posts: false,
        }
    }

    #[test]
    fn without_a_list_the_cli_refuses_before_it_resolves_anything() {
        let scratch = Scratch::new("cli-no-list");
        assert_eq!(
            run_cli(&config(None), scratch.paths(), request(false, false)),
            1
        );
    }

    #[test]
    fn without_a_signed_in_session_the_cli_refuses_before_it_reads() {
        // read はアカウントごとに課金される｡session の無い実行がそこへ
        // 到達してはならない｡
        let scratch = Scratch::new("cli-no-session");
        assert_eq!(
            run_cli(&config(Some("7")), scratch.paths(), request(false, false)),
            1
        );
    }
}
