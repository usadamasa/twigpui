//! twigpui が持つ post ごとの 2 つのトグル､repost (#15) と like (#68) の
//! 共有部分｡
//!
//! この 2 つの機能はまったく同じ形をしている｡X API v2 のタイムライン
//! レスポンスには「サインイン中のユーザーがこの post を repost/いいねした
//! か」を示すフィールドが無く (v1.1 の `retweeted`/`favorited` に相当する
//! ものが v2 には無い)､post ごとに問い合わせると表示 post 1 つにつき
//! 1 リクエストかかる｡キャッシュ全体が課金を避けるために存在している
//! プロジェクトでは論外だ (#9 の module doc を参照)｡そこで各機能は
//! `state_dir` の下に post id のローカル記録を自前で持ち､クリックで反転し
//! 失敗したらロールバックするボタンをそれぞれ駆動している｡
//!
//! この 2 つの仕組みはどちらも repost 固有でも like 固有でもないので､
//! 2 箇所に書く代わりにここへ 1 度だけ置いてある:
//!
//! - [`load_all`]/[`mark`]/[`unmark`]/[`persist`] — id 集合のファイル｡
//!   パスを引数に取るので､`repost.rs` と `like.rs` がそれぞれ自分のものを
//!   渡す ([`Paths::reposted_posts_file`]/[`Paths::liked_posts_file`])｡
//! - [`ToggleState`]/[`ToggleStatus`] — ボタンの描画元になる楽観更新/
//!   ロールバックの状態機械｡`compose.rs` の
//!   `ComposeState`/`ComposeStatus` の慣習に倣っている｡
//!
//! `repost.rs`/`like.rs` に残るのは本当に異なる部分だ: どのエンドポイントを
//! 呼ぶか､どのファイルに記録するか､そして X が返すエラー文言のうちどれが
//! 「これは失敗した」ではなく「ローカル記録が古い」を意味するか｡
//!
//! [`Paths::reposted_posts_file`]: crate::paths::Paths::reposted_posts_file
//! [`Paths::liked_posts_file`]: crate::paths::Paths::liked_posts_file

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

/// トグル 1 つ分の記録ファイルの全内容: このアプリが現在オンにしている
/// post id の全体｡
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct IdSetFile {
    #[serde(default)]
    post_ids: HashSet<String>,
}

/// [`IdSetFile`] をディスクから読む｡ファイルが無いのは
/// 「このアプリからはまだ何も記録していない」という正常なミスだ｡壊れた
/// ファイルや形の違うファイルも *同様に* エラーではなく正常なミスとして
/// 扱う｡`rate_limit::load_file`/`cache::load_json` と共有している規則だ｡
fn load_file(path: &Path) -> Result<IdSetFile> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(IdSetFile::default());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    Ok(serde_json::from_str(&contents).unwrap_or_default())
}

fn save_file(path: &Path, file: &IdSetFile) -> Result<()> {
    let json = serde_json::to_vec_pretty(file)
        .with_context(|| format!("could not serialize {}", path.display()))?;
    std::fs::write(path, json).with_context(|| format!("could not write {}", path.display()))
}

/// ファイルにある post id の全体を 1 度だけ読む — `ui.rs` は表示中の
/// タイムラインが変わるたび (リロード､"Load older"､起動時) にこれを呼び､
/// 各行の既定の [`ToggleState`] を初期化する｡描画のたびに行ごとに
/// ディスクを読むのを避けるためだ｡
pub(crate) fn load_all(path: &Path) -> Result<HashSet<String>> {
    Ok(load_file(path)?.post_ids)
}

/// すでにファイルにあるものはそのままに､`post_id` を記録する｡
pub(crate) fn mark(path: &Path, post_id: &str) -> Result<()> {
    let mut file = load_file(path)?;
    file.post_ids.insert(post_id.to_string());
    save_file(path, &file)
}

/// すでにファイルにあるものはそのままに､`post_id` を記録から取り除く｡
/// もともと無かった id を取り除こうとしてもエラーにはならない｡
pub(crate) fn unmark(path: &Path, post_id: &str) -> Result<()> {
    let mut file = load_file(path)?;
    file.post_ids.remove(post_id);
    save_file(path, &file)
}

/// `on` が求めるほうに応じて [`mark`] または [`unmark`] を呼ぶ —
/// `repost.rs`/`like.rs` のエラー訂正の経路が必要とする形だ｡そこでは
/// 永続化すべき値が､X のエラーを読んで初めて分かる｡
pub(crate) fn persist(path: &Path, post_id: &str, on: bool) -> Result<()> {
    if on {
        mark(path, post_id)
    } else {
        unmark(path, post_id)
    }
}

/// トグルボタン 1 つの状態｡現在オンかどうかとは独立している —
/// `compose.rs` の `ComposeStatus` が下書きのテキストと分けてあるのと
/// 同じように､[`ToggleState`] が持つ値とは分けてある｡
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToggleStatus {
    Idle,
    /// 作成/削除リクエストが飛んでいる最中 (#15) — #14 の二重送信ガードに
    /// 倣っている｡ただしどちらのトグルも元に戻せるので､余計な 2 回目の
    /// クリックの重みはあちらほどではない｡
    Pending,
    /// 直前のトグルが失敗した｡`ui.rs` が描画するメッセージを持つ｡これ自体は
    /// 次の試行を拒む理由にはならない｡
    Failed(String),
}

/// post 1 つ分のトグル状態 (#15, #68): 現在オンかどうか (このアプリ自身の
/// ローカル記録に基づく｡まだ楽観的な値かもしれない — [`Self::start_toggle`]
/// を参照) と [`ToggleStatus`]｡ここにあるものは gpui にもネットワークにも
/// 時計にも触れない — 遷移はすべて `ui.rs` がクリックか完了したリクエスト
/// から駆動する｡`compose.rs` の `ComposeState` と同じだ｡
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToggleState {
    on: bool,
    status: ToggleStatus,
}

impl ToggleState {
    /// ローカル記録から初期化した新しい状態 (一度も見たことのない post なら
    /// 既定の `false`)｡
    pub(crate) fn new(on: bool) -> Self {
        Self {
            on,
            status: ToggleStatus::Idle,
        }
    }

    pub(crate) fn is_on(&self) -> bool {
        self.on
    }

    pub(crate) fn status(&self) -> &ToggleStatus {
        &self.status
    }

    fn is_pending(&self) -> bool {
        matches!(self.status, ToggleStatus::Pending)
    }

    /// クリックに何かをさせてよいかどうか: この post について飛んでいる
    /// リクエストが無いこと｡
    pub(crate) fn can_toggle(&self) -> bool {
        !self.is_pending()
    }

    /// 楽観的に反対の状態へ反転させ､リクエストが飛んでいる印を付ける
    /// (#15 の「クリックで反転､失敗したら戻す」) — 何かが変わったことを
    /// 見せるのにボタンがネットワークを待つことはない｡呼び出し側は事前に
    /// [`Self::can_toggle`] を確認していなければならない｡ここでは
    /// 再確認しない｡
    pub(crate) fn start_toggle(&mut self) {
        self.on = !self.on;
        self.status = ToggleStatus::Pending;
    }

    /// リクエストを一度も試みずにトグルを拒否する — 例えば #15 の
    /// `tweet.write` スコープ欠落チェックは `start_toggle` の前に走る｡
    /// `ComposeState::refuse` が #14 の同じチェックを扱うのと同じだ｡
    pub(crate) fn refuse(&mut self, message: String) {
        self.status = ToggleStatus::Failed(message);
    }

    /// 完了した作成/削除リクエストの結果を適用する: `Ok(actual)` は
    /// サーバー側の結果の状態を確定させる (呼び出し側の訂正処理を経るため､
    /// [`Self::start_toggle`] が楽観的に推測した値と一致しないことがある｡
    /// 実際にはたいてい一致する — `repost::reconcile_from_error` の doc を
    /// 参照)｡`Err` は楽観的な反転を `start_toggle` の直前とまったく同じ値へ
    /// 巻き戻す｡#15 が明示している rollback の保証だ｡
    pub(crate) fn apply_result(&mut self, result: Result<bool, String>) {
        match result {
            Ok(actual) => {
                self.on = actual;
                self.status = ToggleStatus::Idle;
            }
            Err(message) => {
                self.on = !self.on;
                self.status = ToggleStatus::Failed(message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "twigpui-test-toggle-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root.join("toggled.json")
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    // --- load_all / mark / unmark ---

    #[test]
    fn load_all_is_empty_when_nothing_is_on_file() {
        let path = temp_file("load-all-missing");
        assert!(load_all(&path).unwrap().is_empty());
        cleanup(&path);
    }

    #[test]
    fn mark_then_load_all_contains_the_id() {
        let path = temp_file("mark");
        mark(&path, "1700000000000000001").unwrap();
        assert!(load_all(&path).unwrap().contains("1700000000000000001"));
        cleanup(&path);
    }

    #[test]
    fn unmark_removes_a_previously_recorded_id() {
        let path = temp_file("unmark");
        mark(&path, "1700000000000000001").unwrap();
        unmark(&path, "1700000000000000001").unwrap();
        assert!(!load_all(&path).unwrap().contains("1700000000000000001"));
        cleanup(&path);
    }

    #[test]
    fn unmark_on_an_id_never_recorded_is_not_an_error() {
        let path = temp_file("unmark-absent");
        unmark(&path, "nonexistent").unwrap();
        assert!(load_all(&path).unwrap().is_empty());
        cleanup(&path);
    }

    #[test]
    fn mark_preserves_other_already_recorded_ids() {
        let path = temp_file("mark-multi");
        mark(&path, "1").unwrap();
        mark(&path, "2").unwrap();
        let ids = load_all(&path).unwrap();
        assert!(ids.contains("1"));
        assert!(ids.contains("2"));
        cleanup(&path);
    }

    #[test]
    fn persist_marks_when_on_and_unmarks_when_off() {
        let path = temp_file("persist");
        persist(&path, "1", true).unwrap();
        assert!(load_all(&path).unwrap().contains("1"));
        persist(&path, "1", false).unwrap();
        assert!(!load_all(&path).unwrap().contains("1"));
        cleanup(&path);
    }

    #[test]
    fn a_corrupted_file_is_a_clean_miss_not_an_error() {
        let path = temp_file("corrupt");
        std::fs::write(&path, b"not json at all").unwrap();
        assert!(load_all(&path).unwrap().is_empty());
        cleanup(&path);
    }

    #[test]
    fn mark_recovers_cleanly_from_a_corrupted_existing_file() {
        let path = temp_file("save-over-corrupt");
        std::fs::write(&path, b"{ not valid json").unwrap();
        mark(&path, "1").unwrap();
        assert!(load_all(&path).unwrap().contains("1"));
        cleanup(&path);
    }

    #[test]
    fn a_genuine_io_error_reading_the_file_still_propagates() {
        let path = temp_file("io-error");
        // ファイルがあるべき場所にディレクトリがあるのは､破損ではなく
        // 本物の I/O エラーだ — 握り潰さずに表に出さなければならない｡
        std::fs::create_dir(&path).unwrap();
        assert!(load_all(&path).is_err());
        cleanup(&path);
    }

    // --- ToggleState ---

    #[test]
    fn a_fresh_state_is_idle_at_the_seeded_value() {
        let state = ToggleState::new(true);
        assert!(state.is_on());
        assert!(state.can_toggle());
        assert_eq!(state.status(), &ToggleStatus::Idle);
    }

    #[test]
    fn start_toggle_optimistically_flips_from_off_to_on() {
        let mut state = ToggleState::new(false);
        state.start_toggle();
        assert!(state.is_on());
        assert!(
            !state.can_toggle(),
            "a pending toggle must not allow another"
        );
    }

    #[test]
    fn start_toggle_optimistically_flips_from_on_to_off() {
        let mut state = ToggleState::new(true);
        state.start_toggle();
        assert!(!state.is_on());
    }

    #[test]
    fn a_successful_toggle_commits_the_servers_reported_state_and_returns_to_idle() {
        let mut state = ToggleState::new(false);
        state.start_toggle();
        state.apply_result(Ok(true));
        assert!(state.is_on());
        assert!(state.can_toggle());
        assert_eq!(state.status(), &ToggleStatus::Idle);
    }

    #[test]
    fn a_successful_toggle_can_commit_a_state_that_disagrees_with_the_optimistic_guess() {
        // #15 の訂正経路: サーバー側の結果の状態が､`start_toggle` の推測と
        // 食い違っていても優先される｡
        let mut state = ToggleState::new(false);
        state.start_toggle(); // 楽観的な推測: true
        state.apply_result(Ok(false)); // サーバーの答え: 実際にはまだ false
        assert!(!state.is_on());
        assert!(state.can_toggle());
    }

    #[test]
    fn a_failed_create_toggle_rolls_back_to_off() {
        let mut state = ToggleState::new(false);
        state.start_toggle(); // 楽観的に true､pending
        state.apply_result(Err("network error".to_string()));

        assert!(!state.is_on(), "rollback must restore the pre-toggle value");
        assert!(state.can_toggle());
        assert_eq!(
            state.status(),
            &ToggleStatus::Failed("network error".to_string())
        );
    }

    #[test]
    fn a_failed_undo_toggle_rolls_back_to_on() {
        let mut state = ToggleState::new(true);
        state.start_toggle(); // 楽観的に false､pending
        state.apply_result(Err("boom".to_string()));

        assert!(state.is_on(), "rollback must restore the pre-toggle value");
    }

    #[test]
    fn refuse_records_a_message_without_ever_having_toggled() {
        // #15 のスコープ欠落による拒否 — `ComposeState::refuse` と同じ形｡
        let mut state = ToggleState::new(false);
        state.refuse("needs re-authorization".to_string());

        assert!(!state.is_on(), "refuse must not touch the value");
        assert_eq!(
            state.status(),
            &ToggleStatus::Failed("needs re-authorization".to_string())
        );
    }
}
