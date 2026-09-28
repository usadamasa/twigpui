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
use super::pacing::WritePacing;
use super::schedule::Outcome;
use super::{Action, Plan, load_plan, plan, report, save_plan};
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
pub(super) fn read_all(
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

/// members のミラーと following の台帳から diff を作る｡
/// ミラーが無効なら先に members を全件取得し､保存してから following を読む｡
/// following は台帳と `count` (今回の probe) で済むなら先頭だけ読む (#289)｡
/// X への write は行わず､tick ([`write_one`]) が消費する plan を返す｡
pub(super) fn plan_sync(
    paths: &Paths,
    client: &dyn ListSyncApi,
    user_id: &str,
    list_id: &str,
    count: Option<u64>,
    now: i64,
) -> Result<Plan> {
    let members = super::mirror::members(paths, list_id, now, || {
        read_all("list members", |cursor| {
            client.list_members_page(paths, list_id, cursor, now)
        })
    })?;
    let following = match paths.profile().sync_seed_usernames() {
        None => super::following::read(paths, client, user_id, count, now, || {
            read_all("follow list", |cursor| {
                client.following_page(paths, user_id, cursor, now)
            })
        })?,
        Some(usernames) => seed_users(paths, client, usernames, now)?,
    };
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

/// 1 件の write の結末｡`Err` は届きも拒まれもしなかったもの — rate limit､
/// ネットワーク､失効した scope — で､呼び出し側が止まる理由になる｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Written {
    /// `Ok` で返り､plan とミラーに印が付いた｡
    Landed,
    /// X が 400 で拒んだ (#254)｡plan に理由が付き､残務から外れた｡
    Rejected,
}

/// `user_id` への `action` を 1 件送り､結末を plan に書いて返す (#163)｡
///
/// 1 tick に 1 件 (#231): `super::auto::apply` がこれを 1 回だけ呼び､
/// 間は `sync::state` が `paused_until` に置く｡CLI の `--apply` も同じ
/// tick を回す ([`run`]) ので､送る経路はこれ 1 本だ｡
///
/// 届くたびに印を付けて永続化する: plan ファイルの意義はまさに､途中で
/// 中断した apply — rate limit､crash､`^C` — がどちらの側も読み直さず､
/// 既に通ったものを再送もせずに再開できることにある｡
///
/// 400 はこの entry への答えだ (#254)｡印を付けて `Rejected` を返す — 印が
/// 無いと次の tick が同じ entry を先頭に戻し､同じ答えを 6 時間ごとに
/// 受け取り続ける｡拒否が続くかどうかは `SyncState::rejected_in_a_row` が
/// tick をまたいで数える｡
pub(super) fn write_one(
    paths: &Paths,
    client: &dyn ListSyncApi,
    plan: &mut Plan,
    action: Action,
    user_id: &str,
    now: i64,
) -> Result<Written> {
    let result = match action {
        Action::Add => client.add_member(paths, &plan.list_id, user_id, now),
        Action::Remove => client.remove_member(paths, &plan.list_id, user_id, now),
    };
    let written = match result {
        Ok(()) => {
            plan.mark_applied(user_id, action);
            Written::Landed
        }
        Err(error) => match error.downcast_ref::<crate::x_api::InvalidRequest>() {
            Some(refusal) => {
                crate::log::warn(&format!(
                    "list sync: X rejected the {} of {user_id} ({}); marked in the plan \
                     and skipped",
                    match action {
                        Action::Add => "addition",
                        Action::Remove => "removal",
                    },
                    refusal.detail
                ));
                plan.mark_rejected(user_id, action, &refusal.detail);
                Written::Rejected
            }
            None => return Err(error),
        },
    };
    save_progress(paths, plan, user_id, action, written == Written::Landed)?;
    Ok(written)
}

/// plan の送信済み印を先に保存し､成功時だけミラーにも反映する｡
fn save_progress(paths: &Paths, plan: &Plan, id: &str, action: Action, landed: bool) -> Result<()> {
    save_plan(&paths.sync_plan_file(), plan)?;
    if landed {
        super::mirror::applied(paths, plan, id, action)?;
    }
    Ok(())
}

/// `--sync-list` が何をするよう求められたか｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Request {
    /// plan の write を送る｡指定が無ければ dry-run として､既存 plan と
    /// count を確認し､必要なときだけ読み取って plan と report を作る｡
    pub apply: bool,
    /// removal も送る｡既定では off — [`run`] を見よ｡
    pub prune: bool,
    /// 未送信の plan と count の省略判定を越えて diff を買い直す｡
    pub reread: bool,
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
        config.sync_write_pacing,
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
/// `--apply` は background sync と同じ tick ([`super::auto::tick`]) を､
/// plan を流し切るまで前景で回す (#231)｡歩調 — 1 件ずつ､gap､batch､
/// cooldown — も記憶 ([`super::SyncState`]) も loop と共有する: bot 判定は
/// X の側にあり､端末の前に人がいるかどうかを X は知らない｡届いた write は
/// loop にとっても連続を終わらせるし､refusal はそれを伸ばす｡
///
/// loop と違うのは 3 点だけ｡removal は `--prune` の有無で丸ごと決まり
/// (割合の上限は無い — `sync_prune_limit_percent` の代わりに 100 か 0 を
/// 渡す)､最初の tick は前の実行が残した間を待たず､plan を流し切るか
/// 止められたら終わる｡refusal の backoff の途中なら送らずに終わる: 以前は
/// 上限が明けたか見るために送ってみていたが､それは X から見れば拒否の
/// 直後にもう 1 件投げる機械の形だ｡
fn run(
    paths: &Paths,
    client: &dyn ListSyncApi,
    user_id: &str,
    list_id: &str,
    request: Request,
    interval_seconds: u32,
    writes: WritePacing,
) -> Result<String> {
    let plan_path = paths.sync_plan_file();
    let now = oauth::unix_now();

    if !request.apply {
        if !request.reread
            && let Some(plan) = load_plan(&plan_path)?.filter(|plan| plan.list_id == list_id)
        {
            let unsent = super::schedule::sendable(&plan, false);
            if unsent > 0 {
                return Ok(format!(
                    "{}\n\n{unsent} write(s) from the plan on file are still unsent. Re-run \
                     with --apply to send them (no reads needed), or pass --reread to pay \
                     for a fresh diff that replaces the plan.{}",
                    report(&plan),
                    super::preflight::seed_first_note(
                        paths,
                        list_id,
                        plan.pending_count(Action::Add)
                    )
                ));
            }
        }
        return super::preflight::dry_run(paths, client, user_id, list_id, request.reread, now);
    }

    let Some(plan) = load_plan(&plan_path)? else {
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

    // 送るものが無ければ tick を回さない: `--prune` 無しで removal だけが
    // 残った plan (#230) はそのままにし､強制した tick が diff を買いに
    // 行かないようにする｡
    let pending = super::schedule::sendable(&plan, request.prune);
    if pending == 0 {
        return Ok(report(&plan));
    }
    // 1 件ずつ揺らぐので､CLI の apply は分ではなく時間の単位になる｡黙って
    // 止まって見えるので最悪ケースを先に出す — `x-api-budget` の「押す前に
    // 最悪ケースを出す」と同じ規則｡最悪は batch が毎回 1 件で cooldown が
    // 毎回最長のとき｡
    let worst_minutes = pending
        .saturating_mul(usize::try_from(writes.cooldown_seconds.max).unwrap_or(0))
        .saturating_div(60);
    eprintln!(
        "note: sending {pending} write(s) one at a time ({writes}) so the run does not look \
         like a script. Worst case about {worst_minutes} minute(s)."
    );

    let landed = drain(
        paths,
        client,
        user_id,
        list_id,
        request.prune,
        interval_seconds,
        writes,
    )?;
    // 流し切った plan は tick が消している (`auto::apply`)｡`--prune` 無しで
    // removal が残った plan は残る (#230)｡
    Ok(match load_plan(&plan_path)? {
        Some(plan) => report(&plan),
        None => format!("list {list_id}: {landed} write(s) applied, nothing left to send"),
    })
}

/// plan を流し切るか止められるまで tick を回し､届いた件数を返す ([`run`]
/// の `--apply` の本体)｡
///
/// 割合の上限 (#176) は background の規則で､CLI は `prune` が答えだ:
/// 100 は removal を丸ごと許し､0 は addition だけにする｡最初の tick だけ
/// 強制して､前の実行が残した間を待たない｡
fn drain(
    paths: &Paths,
    client: &dyn ListSyncApi,
    user_id: &str,
    list_id: &str,
    prune: bool,
    interval_seconds: u32,
    writes: WritePacing,
) -> Result<usize> {
    let prune_limit_percent = if prune { 100 } else { 0 };
    let mut now = oauth::unix_now();
    let mut landed = 0usize;
    let mut forced = true;
    loop {
        let pacing = super::Pacing {
            interval_seconds,
            writes,
            forced,
        };
        let tick = super::auto::tick(
            paths,
            client,
            user_id,
            list_id,
            pacing,
            prune_limit_percent,
            now,
        );
        forced = false;
        let remaining = match tick.outcome? {
            Outcome::Applied { sent, remaining } => {
                landed = landed.saturating_add(sent);
                remaining
            }
            Outcome::Rejected { remaining } => remaining,
            Outcome::Idle { pending, .. } => pending,
            Outcome::RateLimited { opaque, sent, .. } => anyhow::bail!(
                "rate limited after {} write(s) landed{}; the plan on file records them. \
                 Backing off until unix time {} (refusal #{}); re-run --apply after that.",
                landed.saturating_add(sent),
                if opaque {
                    " — by a cap the x-rate-limit headers do not describe"
                } else {
                    ""
                },
                tick.wake_at,
                tick.state.refusals
            ),
            // `pending > 0` の plan からは起きない｡起きたなら止まる方が安い｡
            Outcome::Diffed { .. } => {
                anyhow::bail!("the plan was replaced mid-run; re-run --apply")
            }
        };
        if remaining == 0 {
            return Ok(landed);
        }
        if tick.state.is_blocked(now) {
            anyhow::bail!(
                "{landed} write(s) landed; the plan on file records them. The sync is backing \
                 off until unix time {} ({}); re-run --apply after that.",
                tick.wake_at,
                if tick.state.refusals > 0 {
                    format!("refusal #{}", tick.state.refusals)
                } else {
                    format!(
                        "{} writes in a row were rejected by X, in case the list or the \
                         request itself is what is being rejected",
                        super::state::REJECTIONS_IN_A_ROW_LIMIT
                    )
                }
            );
        }
        // tick が置いた間 (gap か cooldown) を眠る｡loop は毎分起きて決め直す
        // が､前景のコマンドは期限まで眠ればよい｡
        let wait = tick.wake_at.saturating_sub(now);
        eprintln!("{landed} landed, {remaining} to go; next write in {wait}s");
        client.pause_between_writes(std::time::Duration::from_secs(
            u64::try_from(wait).unwrap_or(0),
        ));
        // fake の sleep は即座に返るので､時計も期限まで進める｡本物は眠った
        // あとの `unix_now()` がそれ以上になっている｡
        now = oauth::unix_now().max(tick.wake_at);
    }
}

#[cfg(test)]
#[path = "mirror_tests.rs"]
mod mirror_tests;

#[cfg(test)]
mod tests {
    use super::super::api::fake::{Call, FakeApi, Scratch, page, rate_limited, rejected, user};
    use super::*;
    use crate::sync::pacing::Span;
    use crate::sync::{Action, PlanEntry, load_state, save_state};

    /// 未適用の entry だけを持つ plan｡`members_total` は removal を測る
    /// 分母なので､prune の判定が絡むテストが自分で上書きする｡
    pub(super) fn plan_of(list_id: &str, adds: &[&str], removals: &[&str]) -> Plan {
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

        let plan = plan_sync(scratch.paths(), &client, "me", "7", None, 100).unwrap();

        assert_eq!(plan.pending_count(Action::Add), 1);
        assert_eq!(plan.pending_count(Action::Remove), 1);
        assert_eq!(plan.members_total, 2);
        assert_eq!(
            client.calls(),
            [
                Call::Members(None),
                Call::Following(None),
                Call::Following(Some("page-2".to_string())),
            ]
        );
    }

    #[test]
    fn a_failed_follow_read_keeps_the_paid_member_mirror() {
        let scratch = Scratch::new("plan-sync-fail");
        let client = FakeApi::new()
            .members(vec![Ok(page(&[("2", "bob")], None))])
            .following(vec![Err(anyhow::anyhow!("the API said 401"))]);

        let error = plan_sync(scratch.paths(), &client, "me", "7", None, 100)
            .unwrap_err()
            .to_string();

        assert!(error.contains("nothing was changed"), "{error}");
        assert_eq!(client.calls(), [Call::Members(None), Call::Following(None)]);
        assert!(scratch.paths().sync_members_file().exists());
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

        let plan = plan_sync(scratch.paths(), &client, "me", "7", None, 100).unwrap();

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

        plan_sync(scratch.paths(), &client, "me", "7", None, 100).unwrap();
        let again = plan_sync(scratch.paths(), &client, "me", "7", None, 100).unwrap();

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
        let client = FakeApi::new()
            .members(vec![Ok(page(&[], None))])
            .lookups(vec![Err(anyhow::anyhow!("the API said 404"))]);

        let error = plan_sync(scratch.paths(), &client, "me", "7", None, 100)
            .unwrap_err()
            .to_string();

        assert!(error.contains("development sync seed @"), "{error}");
    }

    // --- write_one: 1 tick が送る 1 件 (#231) ---

    #[test]
    fn a_write_that_lands_is_marked_on_disk_and_reported_as_landed() {
        let scratch = Scratch::new("write-one-landed");
        let client = FakeApi::new().writes(vec![Ok(())]);
        let mut plan = plan_of("7", &["1", "2"], &[]);

        let written = write_one(scratch.paths(), &client, &mut plan, Action::Add, "1", 0).unwrap();

        assert_eq!(written, Written::Landed);
        assert_eq!(client.calls(), [Call::Add("1".to_string())]);
        let on_file = load_plan(&scratch.paths().sync_plan_file())
            .unwrap()
            .unwrap();
        assert_eq!(applied_ids(&on_file), ["1"]);
    }

    #[test]
    fn a_write_the_api_rejects_is_marked_and_reported_as_rejected() {
        let scratch = Scratch::new("write-one-rejected");
        let client = FakeApi::new().writes(vec![Err(rejected("The user_id is not valid."))]);
        let mut plan = plan_of("7", &["1"], &[]);

        let written = write_one(scratch.paths(), &client, &mut plan, Action::Add, "1", 0).unwrap();

        assert_eq!(written, Written::Rejected);
        let on_file = load_plan(&scratch.paths().sync_plan_file())
            .unwrap()
            .unwrap();
        assert_eq!(
            on_file.entries[0].rejected.as_deref(),
            Some("The user_id is not valid.")
        );
    }

    #[test]
    fn a_refused_write_comes_back_as_the_error_with_nothing_marked() {
        let scratch = Scratch::new("write-one-refused");
        let client = FakeApi::new().writes(vec![Err(rate_limited(9_000, true))]);
        let mut plan = plan_of("7", &["1"], &[]);

        let error =
            write_one(scratch.paths(), &client, &mut plan, Action::Add, "1", 0).unwrap_err();

        assert!(
            error
                .downcast_ref::<crate::rate_limit::RateLimited>()
                .is_some(),
            "{error:#}"
        );
        assert!(!plan.entries[0].applied);
        assert!(plan.entries[0].rejected.is_none());
    }

    // --- run --apply: loop と同じ tick を前景で回す (#231) ---

    #[test]
    fn the_cli_apply_paces_writes_exactly_like_the_loop() {
        // gap 9 秒､batch 2 件､cooldown 50 秒に固定すれば､4 件の間は
        // gap → cooldown → gap と決まる｡交互も tick と同じ (`next_write`)｡
        let scratch = Scratch::new("run-paced");
        save_plan(
            &scratch.paths().sync_plan_file(),
            &plan_of("7", &["1", "2"], &["8", "9"]),
        )
        .unwrap();
        let client = FakeApi::new().writes(vec![Ok(()), Ok(()), Ok(()), Ok(())]);
        let writes = WritePacing {
            gap_seconds: Span::new(9, 9),
            batch_writes: Span::new(2, 2),
            cooldown_seconds: Span::new(50, 50),
        };

        let report = run(
            scratch.paths(),
            &client,
            "me",
            "7",
            request(true, true),
            21_600,
            writes,
        )
        .unwrap();

        assert_eq!(
            client.calls(),
            [
                Call::Add("1".to_string()),
                Call::Remove("8".to_string()),
                Call::Add("2".to_string()),
                Call::Remove("9".to_string()),
            ]
        );
        assert_eq!(
            client.pauses(),
            [9, 50, 9].map(std::time::Duration::from_secs)
        );
        assert!(report.contains("4 write(s) applied"), "{report}");
        assert_eq!(load_plan(&scratch.paths().sync_plan_file()).unwrap(), None);
    }

    #[test]
    fn three_rejections_in_a_row_stop_the_cli_apply() {
        // 400 が list 全体の問題だったときに 2,000 件を撃ち切ってはならない｡
        // loop と同じ規則 (`state::REJECTIONS_IN_A_ROW_LIMIT`) で止まる｡
        let scratch = Scratch::new("run-rejected");
        save_plan(
            &scratch.paths().sync_plan_file(),
            &plan_of("7", &["1", "2", "3", "4", "5"], &[]),
        )
        .unwrap();
        let client = FakeApi::new().writes(vec![
            Ok(()),
            Err(rejected("no")),
            Err(rejected("no")),
            Err(rejected("no")),
            Ok(()),
        ]);

        let error = run(
            scratch.paths(),
            &client,
            "me",
            "7",
            request(true, false),
            21_600,
            WritePacing::DEFAULT,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("3 writes in a row were rejected"), "{error}");
        assert!(error.contains("1 write(s) landed"), "{error}");
        assert_eq!(client.calls().len(), 4, "entry 5 is never sent");
        let on_file = load_plan(&scratch.paths().sync_plan_file())
            .unwrap()
            .unwrap();
        assert!(!on_file.entries[4].applied);
        assert!(on_file.entries[4].rejected.is_none());
        // loop もこの block を守る｡
        let state = load_state(&scratch.paths().sync_state_file());
        assert!(state.blocked_until.is_some(), "{state:?}");
    }

    #[test]
    fn a_cli_apply_during_a_refusal_backoff_sends_nothing() {
        // 以前は「上限が明けたか見るために送ってみる」だったが､X から見れば
        // 拒否の直後にもう 1 件投げる機械の形だ｡
        let scratch = Scratch::new("run-backoff");
        save_plan(
            &scratch.paths().sync_plan_file(),
            &plan_of("7", &["1"], &[]),
        )
        .unwrap();
        save_state(
            &scratch.paths().sync_state_file(),
            &super::super::SyncState {
                blocked_until: Some(i64::MAX),
                refusals: 2,
                ..super::super::SyncState::default()
            },
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
            WritePacing::DEFAULT,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("refusal #2"), "{error}");
        assert!(client.calls().is_empty(), "nothing may be sent");
    }

    // --- run: dry-run と apply の入口 ---

    fn request(apply: bool, prune: bool) -> Request {
        Request {
            apply,
            prune,
            reread: false,
        }
    }

    #[test]
    fn a_dry_run_preserves_unsent_additions_without_any_api_calls() {
        let scratch = Scratch::new("dry-guard");
        let original = plan_of("7", &["1", "2"], &["3"]);
        save_plan(&scratch.paths().sync_plan_file(), &original).unwrap();
        let client = FakeApi::new();
        let text = run(
            scratch.paths(),
            &client,
            "me",
            "7",
            request(false, false),
            21_600,
            WritePacing::DEFAULT,
        )
        .unwrap();
        assert!(client.calls().is_empty());
        assert!(text.starts_with(&report(&original)), "{text}");
        assert!(text.contains("2 write(s)"), "{text}");
        assert!(
            text.contains("--apply") && text.contains("--reread"),
            "{text}"
        );
        assert_eq!(
            load_plan(&scratch.paths().sync_plan_file()).unwrap(),
            Some(original)
        );
    }

    #[test]
    fn the_guard_says_to_seed_the_ledger_before_sending_when_there_is_none() {
        // 台帳が無いまま plan を流し切ると､次の diff は膨らんだ list を全件読む｡
        // 先に読めば小さいうちに済み､以後は二度と読まない｡
        let scratch = Scratch::new("dry-guard-no-ledger");
        save_plan(
            &scratch.paths().sync_plan_file(),
            &plan_of("7", &["1", "2"], &[]),
        )
        .unwrap();
        let client = FakeApi::new();
        let without = run(
            scratch.paths(),
            &client,
            "me",
            "7",
            request(false, false),
            21_600,
            WritePacing::DEFAULT,
        )
        .unwrap();
        assert!(without.contains("sync_members.json"), "{without}");
        assert!(without.contains("--reread first"), "{without}");
        assert!(without.contains("2 account(s) larger"), "{without}");
        assert!(!without.contains('$'), "{without}");

        let ledger = serde_json::json!({"version":1,"list_id":"7","read_at":1,"members":[]});
        std::fs::write(scratch.paths().sync_members_file(), ledger.to_string()).unwrap();
        let with = run(
            scratch.paths(),
            &client,
            "me",
            "7",
            request(false, false),
            21_600,
            WritePacing::DEFAULT,
        )
        .unwrap();
        assert!(!with.contains("--reread first"), "{with}");
        assert!(client.calls().is_empty());
    }

    #[test]
    fn unchanged_count_skips_cli_reads_unless_reread_was_requested() {
        let scratch = Scratch::new("cli-count-skip");
        let now = oauth::unix_now();
        let json = serde_json::json!({"version":1,"list_id":"7","read_at":now,"members":[]});
        std::fs::write(scratch.paths().sync_members_file(), json.to_string()).unwrap();
        save_state(
            &scratch.paths().sync_state_file(),
            &super::super::SyncState {
                following_count: Some(4),
                ..super::super::SyncState::default()
            },
        )
        .unwrap();
        let client = FakeApi::new().counts(vec![Ok(4)]);
        let text = run(
            scratch.paths(),
            &client,
            "me",
            "7",
            request(false, false),
            21_600,
            WritePacing::DEFAULT,
        )
        .unwrap();
        assert!(
            text.contains("following count (4)") && text.contains("--reread"),
            "{text}"
        );
        assert_eq!(client.calls(), [Call::FollowingCount]);
        let client = FakeApi::new()
            .counts(vec![Ok(4)])
            .following(vec![Ok(page(&[], None))]);
        run(
            scratch.paths(),
            &client,
            "me",
            "7",
            Request {
                reread: true,
                ..request(false, false)
            },
            21_600,
            WritePacing::DEFAULT,
        )
        .unwrap();
        assert_eq!(
            client.calls(),
            [Call::FollowingCount, Call::Following(None)]
        );
        assert_eq!(
            load_state(&scratch.paths().sync_state_file()).following_count,
            Some(4)
        );
    }

    #[test]
    fn reread_replaces_an_unsent_plan() {
        let scratch = Scratch::new("dry-reread");
        save_plan(
            &scratch.paths().sync_plan_file(),
            &plan_of("7", &["old"], &[]),
        )
        .unwrap();
        let client = FakeApi::new()
            .following(vec![Ok(page(&[("1", "alice")], None))])
            .members(vec![Ok(page(&[], None))]);
        let request = Request {
            reread: true,
            ..request(false, false)
        };
        run(
            scratch.paths(),
            &client,
            "me",
            "7",
            request,
            21_600,
            WritePacing::DEFAULT,
        )
        .unwrap();
        assert!(client.calls().contains(&Call::Following(None)));
        assert!(client.calls().contains(&Call::Members(None)));
        let saved = load_plan(&scratch.paths().sync_plan_file())
            .unwrap()
            .unwrap();
        assert_eq!(saved.entries[0].user_id, "1");
    }

    #[test]
    fn a_dry_run_with_a_follow_ledger_reads_only_the_head_of_the_follow_list() {
        // #289: probe が 1 増えていれば､全件ではなく先頭の小さいページだけを
        // 買い､新しい follow だけが plan に載る｡
        let scratch = Scratch::new("cli-follow-head");
        std::fs::write(
            scratch.paths().sync_members_file(),
            r#"{"version":1,"list_id":"7","read_at":100,"members":[{"id":"3","username":"c"}]}"#,
        )
        .unwrap();
        std::fs::write(
            scratch.paths().sync_following_file(),
            r#"{"version":1,"user_id":"me","count":1,"read_at":100,"follows":[{"id":"3","username":"c"}]}"#,
        )
        .unwrap();
        save_state(
            &scratch.paths().sync_state_file(),
            &super::super::SyncState {
                following_count: Some(1),
                ..super::super::SyncState::default()
            },
        )
        .unwrap();
        let client = FakeApi::new()
            .counts(vec![Ok(2)])
            .heads(vec![Ok(page(&[("9", "new"), ("3", "c")], None))]);
        let text = run(
            scratch.paths(),
            &client,
            "me",
            "7",
            request(false, false),
            21_600,
            WritePacing::DEFAULT,
        )
        .unwrap();
        assert!(text.contains("1 to add, 0 to remove"), "{text}");
        assert!(text.contains("@new"), "{text}");
        assert_eq!(
            client.calls(),
            [Call::FollowingCount, Call::FollowingHead(5, None)]
        );
        assert_eq!(
            load_state(&scratch.paths().sync_state_file()).following_count,
            Some(2)
        );
    }

    #[test]
    fn cli_retries_a_diff_whose_member_refresh_succeeded_but_follow_read_failed() {
        let scratch = Scratch::new("cli-count-retry");
        save_state(
            &scratch.paths().sync_state_file(),
            &super::super::SyncState {
                following_count: Some(4),
                ..super::super::SyncState::default()
            },
        )
        .unwrap();
        let client = FakeApi::new()
            .counts(vec![Ok(4)])
            .members(vec![Ok(page(&[("2", "bob")], None))])
            .following(vec![Err(anyhow::anyhow!("following unavailable"))]);
        assert!(
            run(
                scratch.paths(),
                &client,
                "me",
                "7",
                request(false, false),
                21_600,
                WritePacing::DEFAULT,
            )
            .is_err()
        );
        let client = FakeApi::new()
            .counts(vec![Ok(4)])
            .following(vec![Ok(page(&[], None))]);
        run(
            scratch.paths(),
            &client,
            "me",
            "7",
            request(false, false),
            21_600,
            WritePacing::DEFAULT,
        )
        .unwrap();
        assert_eq!(
            client.calls(),
            [Call::FollowingCount, Call::Following(None)]
        );
        assert_eq!(
            load_plan(&scratch.paths().sync_plan_file())
                .unwrap()
                .unwrap()
                .pending_count(Action::Remove),
            1
        );
    }

    #[test]
    fn dry_run_reads_when_the_plan_has_no_sendable_additions_for_this_list() {
        for (label, old) in [
            ("other", plan_of("other", &["1"], &[])),
            ("complete", plan_of("7", &[], &[])),
            ("removals", plan_of("7", &[], &["3"])),
        ] {
            let scratch = Scratch::new(&format!("dry-guard-{label}"));
            save_plan(&scratch.paths().sync_plan_file(), &old).unwrap();
            let client = FakeApi::new()
                .following(vec![Ok(page(&[], None))])
                .members(vec![Ok(page(&[], None))]);
            run(
                scratch.paths(),
                &client,
                "me",
                "7",
                request(false, false),
                21_600,
                WritePacing::DEFAULT,
            )
            .unwrap();
            assert!(client.calls().contains(&Call::Following(None)));
        }
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
            WritePacing::DEFAULT,
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
            WritePacing::DEFAULT,
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
            WritePacing::DEFAULT,
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
            WritePacing::DEFAULT,
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
            WritePacing::DEFAULT,
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
    fn a_run_without_prune_keeps_the_plan_while_removals_are_unsent() {
        // #230: removal は支払い済みの diff だ｡`--apply` が addition を
        // 流し切ったあとも plan は残り､`--apply --prune` がそこから送る —
        // loop 側 (`auto::apply`) と同じ規則｡消せば両側を読み直す払いに戻る｡
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
            WritePacing::DEFAULT,
        )
        .unwrap();

        let kept = load_plan(&scratch.paths().sync_plan_file())
            .unwrap()
            .expect("the plan with an unsent removal stays on file");
        assert_eq!(kept.pending_count(Action::Remove), 1);
        assert_eq!(kept.pending_count(Action::Add), 0);

        // 2 回目の `--apply` は何も送らず､plan もまだ消さない｡
        let client = FakeApi::new();
        run(
            scratch.paths(),
            &client,
            "me",
            "7",
            request(true, false),
            21_600,
            WritePacing::DEFAULT,
        )
        .unwrap();
        assert!(client.calls().is_empty());
        assert!(
            load_plan(&scratch.paths().sync_plan_file())
                .unwrap()
                .is_some()
        );

        // `--prune` が removal を送り切ると完了し､そこで消える｡
        let client = FakeApi::new().writes(vec![Ok(())]);
        run(
            scratch.paths(),
            &client,
            "me",
            "7",
            request(true, true),
            21_600,
            WritePacing::DEFAULT,
        )
        .unwrap();
        assert_eq!(client.calls(), [Call::Remove("8".to_string())]);
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
            post_resource_price: 0.005,
            list_id: list_id.map(str::to_string),
            daily_post_budget: 1000,
            auto_sync_list: false,
            sync_interval_seconds: 21_600,
            sync_prune_limit_percent: 10,
            sync_write_pacing: WritePacing::DEFAULT,
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
