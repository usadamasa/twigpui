//! #163 の sync のうち支払う側の半分: ページングした read､apply の loop､
//! そして `--sync-list` の入口｡
//!
//! ここは一つも unit test されておらず､`mod.rs` の末尾ではなく別ファイルに
//! してあるのはそのためだ: 以下の関数はどれも `cache` の reload 経路と同じく
//! 実際の HTTP request を投げる｡テストカバレッジを担っているのは
//! [`super::plan`] と plan ファイルで､どちらも純粋なまま隣にある｡

use anyhow::{Context as _, Result};

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
/// unit test していない — `fetch_page` を通して実際の HTTP request を
/// 投げる｡テストカバレッジを担うのは純粋な [`super::plan`] の方だ｡
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
///
/// unit test していない｡理由は [`read_all`] と同じ｡
pub(super) fn plan_sync(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    list_id: &str,
    now: i64,
) -> Result<Plan> {
    let following = match paths.profile().sync_seed_usernames() {
        None => read_all("follow list", |cursor| {
            client.following(paths, user_id, cursor, now)
        })?,
        Some(usernames) => seed_users(paths, client, usernames, now)?,
    };
    let members = read_all("list members", |cursor| {
        client.list_members(paths, list_id, cursor, now)
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
///
/// unit test していない｡理由は [`read_all`] と同じ｡
fn seed_users(paths: &Paths, client: &XClient, usernames: &[&str], now: i64) -> Result<Vec<User>> {
    usernames
        .iter()
        .map(|username| {
            // `cache::reload` 自身の lookup と同じ形: まずキャッシュ､
            // API に訊かざるをえなかったものは永続化する｡
            let id = if let Some(id) = cache::cached_user_id(paths, username, now)? {
                id
            } else {
                let id = client
                    .user_id_by_username(paths, username, now)
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
///
/// unit test していない｡理由は [`read_all`] と同じ｡
fn apply(
    paths: &Paths,
    client: &XClient,
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
/// unit test していない｡理由は [`read_all`] と同じ｡
pub(super) fn apply_some(
    paths: &Paths,
    client: &XClient,
    plan: &mut Plan,
    prune: bool,
    now: i64,
    limit: usize,
) -> (usize, Result<()>) {
    let mut sent = 0usize;
    for (action, user_id) in super::schedule::next_batch(plan, prune, limit) {
        let result = match action {
            Action::Add => client.add_list_member(paths, &plan.list_id, &user_id, now),
            Action::Remove => client.remove_list_member(paths, &plan.list_id, &user_id, now),
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

    let client = XClient::new(credential.token);
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
fn resolve_own_id(paths: &Paths, client: &XClient) -> Result<String> {
    let now = oauth::unix_now();
    if let Some(entry) = cache::cached_me(paths, now)? {
        return Ok(entry.id);
    }
    let user = client.me(paths, now)?;
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
    client: &XClient,
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
    let settled = super::state::settle(state, outcome.as_ref().ok(), now, interval_seconds);
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
