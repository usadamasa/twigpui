//! このアプリが follow しているアカウントを List へミラーする (#163)｡
//!
//! #161 で List がウィンドウの主たる情報源になり､list の membership が
//! そのまま timeline の中身になった｡follow の一覧を手で打ち直すのは誰も
//! 二度とやらないし､アカウントを follow しても list はついてこない — なので
//! 両側をここで diff し､その差分を適用する｡
//!
//! # 一方向
//!
//! follow が真であり､list がそれに従う｡逆向きは list を編集して x.com 上で
//! アカウントを follow するということで､ミラーの目的ではない｡
//!
//! # 利便性に優先する二つの規則
//!
//! **部分的な read は決して plan にならない｡** 両側ともページングされ､
//! diff は集合差だ: 途中で切れた follow list に無いアカウントは unfollow と
//! 読まれて削除を得るし､途中で切れた member list は既にいるアカウントを
//! 再追加する｡だから [`read_all`] の失敗は続行の材料ではなく sync 全体に
//! とって致命的だ — ここに使える部分的な答えなど存在しない｡
//!
//! **CLI では removal は opt-in｡** list は手で入れたアカウントを持ちうるし､
//! plan は送るよう求められたかどうかによらず removal を常に *列挙* するので､
//! `--prune` が無ければ `--sync-list --apply` はそれらに手を触れない｡
//!
//! background sync の方は prune する (2026-08-23､オーナーの決定による)｡
//! 「list とはこのアプリが follow しているものだ」がそれの提供する契約
//! すべてだからだ｡手で list に足したアカウントはそれに消される｡これは
//! 事故ではなく意図した挙動であり､上の all-or-nothing の規則が以前より
//! 重くなった理由でもある: 途中で切れた follow の read に prune が加われば､
//! addition の取りこぼしではなく大量削除になる｡
//!
//! # 費用
//!
//! どちらの read も返した resource ごとに課金される (`x-api-budget`) ので､
//! 数千の follow に対する dry-run はセントではなくドルの話だ — plan を disk
//! に書くのはそのためだ｡失敗後に `--apply` を再実行すれば両側を読み直す
//! 支払いをせずにファイルから再開するし､entry は届くたびに印が付くので
//! 二度送られるものは無い｡
//!
//! # タイマー駆動
//!
//! #163 はその理由で自動ポーリングを退けた｡それは覆されたが
//! (2026-08-23)､理由の方は捨てずに残した: diff は長く設定可能な interval で
//! 走り､その最後の *試行* が [`SyncState`] に永続化されるので､アプリを
//! 再起動しても同じ答えを買い直さずに済む｡判断は [`schedule`] が持ち､
//! [`auto::tick`] が実行し､そのあと記憶を進める唯一のものが
//! [`state::settle`] だ — refusal のあとの期限を含めてで､これが catch-up に
//! 2 分ごとに上限へ送り込ませない (#198)､また丸一日 15 分間隔で同じように
//! 送り込ませない (#197) ための仕組みだ｡

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::x_api::model::User;

mod api;
mod auto;
mod run;
mod schedule;
mod state;

pub(crate) use auto::{Pacing, Tick, tick};
pub(crate) use run::{Request, run_cli};
pub(crate) use schedule::{Outcome, is_finished, notice};
pub(crate) use state::{SyncState, load_state, save_state};

/// entry が diff のどちら側から来たか｡
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Action {
    /// follow しているが list にいない｡
    Add,
    /// list にいるが follow していない｡
    Remove,
}

/// sync が手を加えるアカウント 1 件｡
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PlanEntry {
    pub user_id: String,
    /// report が id ではなくアカウント名を出せるようにするためだけに
    /// 持っている｡照合には使わない: screen name は変わるが id は変わらない｡
    pub username: String,
    pub action: Action,
    /// この entry の request が `Ok` で返ってきたら立てる｡
    /// `#[serde(default)]` なのは､中断された apply より前に書かれた plan
    /// ファイルも読み込めるようにするためだ｡
    #[serde(default)]
    pub applied: bool,
}

/// sync が何をするか｡[`crate::paths::Paths::sync_plan_file`] に書かれる｡
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Plan {
    /// この plan を計算した相手の list｡適用前に検査する: plan が意味を
    /// 持つのは diff された list に対してだけで､dry-run と apply のあいだに
    /// `list_id` を別の場所へ向ければ､そうしなければ誤った list の
    /// membership を書き換えてしまう｡
    pub list_id: String,
    pub created_at: i64,
    /// この plan が diff された時点で list が持っていたアカウント数 —
    /// #176 の prune 上限が `Remove` entry を測る分母だ｡
    /// `#[serde(default)]` なのは上限が入る前の plan ファイルも読み込める
    /// ようにするためで､0 と読まれる｡これを `schedule::prune_allowed` は
    /// 空の list ではなく「removal をすべて保留」として扱う｡
    #[serde(default)]
    pub members_total: usize,
    pub entries: Vec<PlanEntry>,
}

impl Plan {
    /// まだ適用されていない `action` の entry｡
    pub(crate) fn pending(&self, action: Action) -> impl Iterator<Item = &PlanEntry> {
        self.entries
            .iter()
            .filter(move |entry| entry.action == action && !entry.applied)
    }

    /// `action` の entry が何件残っているか｡
    pub(crate) fn pending_count(&self, action: Action) -> usize {
        self.pending(action).count()
    }

    /// `user_id` の `action` が通ったことを記録する｡plan が持たない id には
    /// 何もしない｡`apply` からは起こりえないが､万一起きても panic には
    /// しないためだ｡
    pub(crate) fn mark_applied(&mut self, user_id: &str, action: Action) {
        for entry in &mut self.entries {
            if entry.user_id == user_id && entry.action == action {
                entry.applied = true;
            }
        }
    }

    /// すべての entry が適用済みかどうか — plan ファイルにもう言うことが
    /// 無く､捨ててよくなる地点だ｡
    pub(crate) fn is_complete(&self) -> bool {
        self.entries.iter().all(|entry| entry.applied)
    }
}

/// `--sync-list` に必要な scope のうち `granted` が持たない最初のもの｡
/// session が仕事を丸ごとこなせるなら `None`｡
///
/// 最初の request で気づくのではなく､その前に検査する: follow list の
/// read はアカウントごとに 1 課金 resource かかるので､最初の *write* で
/// 拒否されるような session は read を支払う前に追い返さなければならない｡
/// どちらの scope も #163 で新しく入ったので､それ以前に認可された session は
/// すべてここで落ちる｡
pub(crate) fn missing_scope(granted: Option<&str>) -> Option<&'static str> {
    [
        crate::oauth::tokens::FOLLOWS_READ_SCOPE,
        crate::oauth::tokens::LIST_WRITE_SCOPE,
    ]
    .into_iter()
    .find(|required| !crate::oauth::tokens::has_scope(granted, required))
}

/// 両側を diff して plan にする (#163 の核心)｡
///
/// 照合は終始 user id で行う｡screen name は同一性ではない: 二つの read の
/// あいだに改名したアカウントは､そうしなければ削除して再追加され､何も
/// 変えないのに write を 2 回使うことになる｡
///
/// 順序は addition が `following` の順､removal が `members` の順なので､
/// report は hash set が反復する任意の順ではなく､API がアカウントを
/// 渡してきた順のまま読める｡
pub(crate) fn plan(list_id: &str, now: i64, following: &[User], members: &[User]) -> Plan {
    let member_ids: std::collections::HashSet<&str> =
        members.iter().map(|user| user.id.as_str()).collect();
    let following_ids: std::collections::HashSet<&str> =
        following.iter().map(|user| user.id.as_str()).collect();

    let adds = following
        .iter()
        .filter(|user| !member_ids.contains(user.id.as_str()))
        .map(|user| entry(user, Action::Add));
    let removals = members
        .iter()
        .filter(|user| !following_ids.contains(user.id.as_str()))
        .map(|user| entry(user, Action::Remove));

    Plan {
        list_id: list_id.to_string(),
        created_at: now,
        members_total: members.len(),
        entries: adds.chain(removals).collect(),
    }
}

fn entry(user: &User, action: Action) -> PlanEntry {
    PlanEntry {
        user_id: user.id.clone(),
        username: user.username.clone(),
        action,
        applied: false,
    }
}

/// dry-run の report: plan が何をするか､そして再実行なら既に何をしたか｡
///
/// 価格は意図的に載せていない｡`x-api-budget` は read 側を実測値として
/// 記録している (他人の post は $0.005/resource､自分のものは $0.001) が､
/// `/2/lists/:id/members` にも write のどちらにも実測が無いので､ここに
/// 数字を出せば docs を事実として言い直すだけになる｡件数はこの crate が
/// 実際に知っていることだ｡
pub(crate) fn report(plan: &Plan) -> String {
    let adds = plan.pending_count(Action::Add);
    let removals = plan.pending_count(Action::Remove);
    let done = plan.entries.iter().filter(|entry| entry.applied).count();

    let mut lines = vec![format!(
        "list {}: {adds} to add, {removals} to remove",
        plan.list_id
    )];
    if done > 0 {
        lines.push(format!(
            "{done} entr{} already applied by an earlier run — not resent",
            if done == 1 { "y" } else { "ies" }
        ));
    }
    // CLI に prune の上限は無い (#176): この行がその代わりを務める｡
    // 「list の 2,000 members のうち 1,900」と読んだうえで `--prune` と
    // 打つ人は確認を済ませている｡読む人がいない background sync の方は､
    // 同じ plan を保留する｡
    if removals > 0 {
        lines.push(format!(
            "{removals} of the list's {} members would be removed",
            plan.members_total
        ));
    }
    lines.push(format!(
        "applying costs {} write request(s); removals need --prune",
        adds.saturating_add(removals)
    ));
    for entry in plan.entries.iter().filter(|entry| !entry.applied) {
        let verb = match entry.action {
            Action::Add => "+",
            Action::Remove => "-",
        };
        lines.push(format!("  {verb} @{} ({})", entry.username, entry.user_id));
    }
    lines.join("\n")
}

/// `path` から `plan` を読み戻す｡timeline のキャッシュと違い､壊れた
/// ファイルはきれいな miss ではなくエラーだ: キャッシュの miss は避けられた
/// はずの request 1 回で済むが､読めない plan を黙って「plan 無し」として
/// 扱えば､apply は両側を丸ごと読み直すところまで押し戻される｡
pub(crate) fn load_plan(path: &std::path::Path) -> Result<Option<Plan>> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    let plan = serde_json::from_str(&contents)
        .with_context(|| format!("could not parse the sync plan in {}", path.display()))?;
    Ok(Some(plan))
}

/// `plan` を `path` へ書く｡
pub(crate) fn save_plan(path: &std::path::Path, plan: &Plan) -> Result<()> {
    let json = serde_json::to_string_pretty(plan).context("could not serialize the sync plan")?;
    std::fs::write(path, json).with_context(|| format!("could not write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str, username: &str) -> User {
        User {
            id: id.to_string(),
            name: username.to_string(),
            username: username.to_string(),
            profile_image_url: None,
        }
    }

    fn ids(plan: &Plan, action: Action) -> Vec<&str> {
        plan.pending(action)
            .map(|entry| entry.user_id.as_str())
            .collect()
    }

    /// #163 時点で `SCOPES` が要求するものすべて — 今日認可された session が
    /// 持つもの｡
    const CURRENT_SCOPES: &str = "tweet.read users.read tweet.write like.write list.read list.write follows.read \
         offline.access";

    #[test]
    fn a_current_session_is_missing_no_scope() {
        assert_eq!(missing_scope(Some(CURRENT_SCOPES)), None);
    }

    #[test]
    fn a_session_predating_163_is_turned_away_before_it_reads_anything() {
        // これが防ぐ高くつく失敗: 課金対象のアカウントを数千ページング
        // したあとで最初の write に拒否されること｡
        let pre_163 = "tweet.read users.read tweet.write like.write list.read offline.access";
        assert_eq!(missing_scope(Some(pre_163)), Some("follows.read"));
    }

    #[test]
    fn a_session_that_can_read_follows_but_not_write_the_list_is_still_refused() {
        // 実運用の token は #157 の調査の名残でしばらく `follows.read` を
        // 持っていた｡それで両側を読めば満額課金されたうえ､最初の add で
        // 拒否されていたはずだ｡
        let read_only = "tweet.read users.read tweet.write like.write list.read follows.read \
                         offline.access";
        assert_eq!(missing_scope(Some(read_only)), Some("list.write"));
    }

    #[test]
    fn an_unrecorded_scope_is_treated_as_insufficient() {
        // `has_scope` 自身の規則: 不明は許可ではない｡
        assert_eq!(missing_scope(None), Some("follows.read"));
    }

    #[test]
    fn a_followed_account_missing_from_the_list_is_an_addition() {
        let plan = plan("7", 0, &[user("1", "alice")], &[]);
        assert_eq!(ids(&plan, Action::Add), ["1"]);
        assert!(ids(&plan, Action::Remove).is_empty());
    }

    #[test]
    fn a_member_no_longer_followed_is_a_removal() {
        let plan = plan("7", 0, &[], &[user("1", "alice")]);
        assert_eq!(ids(&plan, Action::Remove), ["1"]);
        assert!(ids(&plan, Action::Add).is_empty());
    }

    #[test]
    fn an_account_on_both_sides_is_left_alone() {
        // 再実行では圧倒的に多い場合｡ここに write を使えば､どの sync も
        // list 全体の大きさ分の費用になってしまう｡
        let plan = plan("7", 0, &[user("1", "alice")], &[user("1", "alice")]);
        assert!(plan.entries.is_empty());
    }

    #[test]
    fn matching_is_by_id_not_by_screen_name() {
        // 二つの read のあいだに改名したアカウントは同じアカウントだ｡
        // 名前で照合すれば削除して再追加してしまう｡
        let plan = plan("7", 0, &[user("1", "newname")], &[user("1", "oldname")]);
        assert!(plan.entries.is_empty());
    }

    #[test]
    fn two_accounts_sharing_a_screen_name_are_still_two_accounts() {
        // 上のテストの裏返し: id が違えば同じ名前でも 1 entry に潰れては
        // ならない｡
        let plan = plan("7", 0, &[user("1", "alice"), user("2", "alice")], &[]);
        assert_eq!(ids(&plan, Action::Add), ["1", "2"]);
    }

    #[test]
    fn additions_keep_the_order_they_were_read_in() {
        let plan = plan(
            "7",
            0,
            &[user("3", "c"), user("1", "a"), user("2", "b")],
            &[],
        );
        assert_eq!(ids(&plan, Action::Add), ["3", "1", "2"]);
    }

    #[test]
    fn the_plan_records_which_list_it_was_diffed_against() {
        // plan を別の list に適用すれば誤った list の membership を書き換える｡
        // apply 経路はこれを比較する｡
        let plan = plan("2091351590695588200", 0, &[user("1", "a")], &[]);
        assert_eq!(plan.list_id, "2091351590695588200");
    }

    #[test]
    fn marking_an_entry_applied_takes_it_out_of_pending() {
        let mut plan = plan("7", 0, &[user("1", "a"), user("2", "b")], &[]);
        plan.mark_applied("1", Action::Add);
        assert_eq!(ids(&plan, Action::Add), ["2"]);
        assert_eq!(plan.pending_count(Action::Add), 1);
    }

    #[test]
    fn marking_an_addition_does_not_mark_a_removal_of_the_same_id() {
        // 同じ id が両側に正当に現れるのはバグ経由でしかないが､万一起きた
        // ときに片方を適用してもう片方を黙って引退させてはならない｡
        let mut plan = plan("7", 0, &[user("1", "a")], &[user("2", "b")]);
        plan.entries.push(PlanEntry {
            user_id: "1".to_string(),
            username: "a".to_string(),
            action: Action::Remove,
            applied: false,
        });
        plan.mark_applied("1", Action::Add);
        assert_eq!(ids(&plan, Action::Remove), ["2", "1"]);
    }

    #[test]
    fn a_plan_is_complete_only_once_every_entry_landed() {
        let mut plan = plan("7", 0, &[user("1", "a"), user("2", "b")], &[]);
        assert!(!plan.is_complete());
        plan.mark_applied("1", Action::Add);
        assert!(!plan.is_complete());
        plan.mark_applied("2", Action::Add);
        assert!(plan.is_complete());
    }

    #[test]
    fn an_empty_plan_is_complete() {
        assert!(plan("7", 0, &[], &[]).is_complete());
    }

    #[test]
    fn the_report_counts_both_sides_and_says_removals_are_opt_in() {
        let plan = plan("7", 0, &[user("1", "alice")], &[user("2", "bob")]);
        let report = report(&plan);
        assert!(report.contains("1 to add, 1 to remove"), "{report}");
        assert!(report.contains("--prune"), "{report}");
        assert!(report.contains("@alice"), "{report}");
        assert!(report.contains("@bob"), "{report}");
    }

    #[test]
    fn the_report_says_what_an_earlier_run_already_applied() {
        // 再実行が黙って小さい数を見せれば､follow list が縮んだように
        // 見えてしまう｡
        let mut plan = plan("7", 0, &[user("1", "alice"), user("2", "bob")], &[]);
        plan.mark_applied("1", Action::Add);
        let report = report(&plan);
        assert!(report.contains("1 entry already applied"), "{report}");
        assert!(report.contains("1 to add"), "{report}");
        assert!(!report.contains("@alice"), "{report}");
    }

    #[test]
    fn the_report_quotes_no_price() {
        // `x-api-budget` は write のどちらにも `/2/lists/:id/members` にも
        // 実測を持たない｡docs の数字を既知であるかのように印字するのは
        // #162 が明示している失敗だ｡
        let report = report(&plan("7", 0, &[user("1", "alice")], &[]));
        assert!(!report.contains('$'), "{report}");
    }

    #[test]
    fn a_plan_survives_a_round_trip_through_the_file() {
        let dir = std::env::temp_dir().join(format!("twigpui-sync-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plan.json");

        let mut written = plan("7", 100, &[user("1", "alice")], &[user("2", "bob")]);
        written.mark_applied("1", Action::Add);
        save_plan(&path, &written).unwrap();

        assert_eq!(load_plan(&path).unwrap(), Some(written));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // --- #176: plan の removal を測る分母となる list の大きさ ---

    #[test]
    fn the_plan_records_how_many_members_the_list_had() {
        // prune 上限の分母｡removal の件数だけでは､それが list のどれだけを
        // 占めるのか何も言えない｡
        let plan = plan(
            "7",
            0,
            &[user("1", "alice")],
            &[user("2", "bob"), user("3", "carol")],
        );
        assert_eq!(plan.members_total, 2);
    }

    #[test]
    fn a_plan_file_written_before_the_cap_reads_with_an_unknown_list_size() {
        // 失敗せずに読み込む — 中の entry は支払い済みだ — が total は 0 に
        // なり､`schedule::prune_allowed` はそれを「removal をすべて保留」と
        // 読む｡
        let json = r#"{"list_id":"7","created_at":0,"entries":[{"user_id":"2","username":"bob","action":"Remove"}]}"#;
        let plan: Plan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.members_total, 0);
        assert_eq!(plan.pending_count(Action::Remove), 1);
    }

    #[test]
    fn the_report_measures_removals_against_the_list() {
        // CLI に上限は無い｡この数字があるから､dry-run を読む人は次の
        // コマンドが `--prune` でよいか判断できる｡
        let plan = plan("7", 0, &[], &[user("2", "bob"), user("3", "carol")]);
        let report = report(&plan);
        assert!(report.contains("2 of the list's 2 members"), "{report}");
    }

    #[test]
    fn the_report_does_not_measure_when_there_is_nothing_to_remove() {
        let report = report(&plan("7", 0, &[user("1", "alice")], &[]));
        assert!(!report.contains("members"), "{report}");
    }

    #[test]
    fn a_missing_plan_file_is_no_plan_rather_than_an_error() {
        let path =
            std::env::temp_dir().join(format!("twigpui-no-plan-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_plan(&path).unwrap(), None);
    }

    #[test]
    fn a_corrupt_plan_file_is_an_error_naming_the_path() {
        // timeline のキャッシュと違い､これを miss として扱えば apply は
        // 両側を丸ごと読む支払いまで押し戻される｡
        let path =
            std::env::temp_dir().join(format!("twigpui-bad-plan-{}.json", std::process::id()));
        std::fs::write(&path, "{ not json").unwrap();

        let error = load_plan(&path).unwrap_err().to_string();
        assert!(error.contains(&path.display().to_string()), "{error}");

        std::fs::remove_file(&path).unwrap();
    }
}
