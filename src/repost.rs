//! repost の記録 (#15) — 本当に repost に固有の部分だけ｡
//!
//! この機能が乗っている 2 つの仕組み､すなわち post id のローカル記録と
//! 楽観更新/ロールバックのボタン状態は､いいね (#68) と共有していて
//! [`crate::toggle`] にある｡ここに残っているのは純粋に repost 固有の部分だ:
//! どのエンドポイントを呼ぶか､どのファイルに記録するか､そして X が返す
//! エラー文言のうちどれが「これは失敗した」ではなく
//! 「ローカル記録が古い」を意味するか｡
//!
//! **他のクライアントから行った repost はここに反映されない** — ローカル
//! 記録が twigpui の持つ唯一の source of truth であり､リクエストコスト
//! ゼロで使えるボタン状態を得るためのトレードオフとして受け入れている｡
//! README と [`crate::paths::Paths::reposted_posts_file`] の doc を参照｡
//!
//! [`create`]/[`remove`] は実際にネットワーク (`XClient` 経由) とディスクを
//! 触る薄いオーケストレーションで､ユニットテストは無い —
//! `cache::reload` の「直接のユニットテストは無い」という慣習に倣っている｡
//! 組み合わせている要素はすべて単体でテストされているためだ｡

use std::collections::HashSet;

use anyhow::Result;

use crate::paths::Paths;
use crate::toggle;
use crate::x_api::XClient;

/// repost 済みとして記録されている post id の全体 — [`toggle::load_all`] を参照｡
pub(crate) fn load_all(paths: &Paths) -> Result<HashSet<String>> {
    toggle::load_all(&paths.reposted_posts_file())
}

/// repost の作成/削除の失敗レスポンスを､本物の失敗ではなくローカル記録への
/// 訂正として解釈する (記録が現実から乖離したあとの､#15 における唯一の
/// 回復経路だ): `creating: true` は "you already retweeted this" を認識し
/// (作成しようとしたら状態がすでに true だった)､`creating: false` は
/// "you have not retweeted this" を認識する (削除しようとしたら状態が
/// すでに false だった)｡認識できたときは永続化すべき訂正後の値を返し､
/// それ以外の失敗ではすべて `None` を返す — 呼び出し側は `None` を
/// 通常のエラーとして伝播させる｡
///
/// **確度: 実際の API に対しては未検証｡** この 2 つの衝突に対して X が返す
/// 正確なエラーの形は､この変更では確認できていない — 実装レポートを参照｡
/// 一致判定は大文字小文字を無視して行い､対象は `x_api::client::check_status`
/// が (`ApiProblem` 経由で) 他のあらゆるエラーについてすでに取り出している
/// のと同じ人間向けの `title`/`detail`/`reason` テキストだ｡`check_status` が
/// 付ける固定の "403 Forbidden — …" プレフィックスに関わらず､そのテキストは
/// 文字列化された `anyhow::Error` を通じてこの関数まで届く — ステータス
/// コードだけで判定するより X の実際の言い回しに対して頑健だ｡素の 403 は
/// 無関係な権限エラーでも返るためである｡
pub(crate) fn reconcile_from_error(creating: bool, message: &str) -> Option<bool> {
    let lower = message.to_lowercase();
    if creating && lower.contains("already retweeted") {
        Some(true)
    } else if !creating
        && (lower.contains("have not retweeted") || lower.contains("haven't retweeted"))
    {
        Some(false)
    } else {
        None
    }
}

/// `user_id` として `post_id` を repost する (#15): API を呼び､成功したら
/// 永続化する｡認識できた "already retweeted" の衝突
/// ([`reconcile_from_error`] を参照) はエラーを伝播させる代わりにローカル
/// 記録を訂正する — 呼び出し側 (`ui.rs`) は `Ok` を「これが現時点の状態だ」
/// として扱い､必ずしも「作成が成功した」とは扱わない｡
///
/// 直接のユニットテストは無い — `client` を通じて実際に HTTP リクエストを
/// 出すためで､`cache::reload` にテストが無いのと同じ理由だ｡この関数の実際の
/// テストカバレッジは [`reconcile_from_error`] と [`toggle::ToggleState`] が
/// 担っている｡
pub(crate) fn create(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    post_id: &str,
    now: i64,
) -> Result<bool> {
    let path = paths.reposted_posts_file();
    match client.create_repost(paths, user_id, post_id, now) {
        Ok(()) => {
            toggle::mark(&path, post_id)?;
            Ok(true)
        }
        Err(error) => match reconcile_from_error(true, &format!("{error:#}")) {
            Some(actual) => {
                toggle::persist(&path, post_id, actual)?;
                Ok(actual)
            }
            None => Err(error),
        },
    }
}

/// `user_id` として `post_id` の repost を取り消す (#15) — [`create`] の
/// 完全な鏡像で､方向だけが逆｡
pub(crate) fn remove(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    post_id: &str,
    now: i64,
) -> Result<bool> {
    let path = paths.reposted_posts_file();
    match client.delete_repost(paths, user_id, post_id, now) {
        Ok(()) => {
            toggle::unmark(&path, post_id)?;
            Ok(false)
        }
        Err(error) => match reconcile_from_error(false, &format!("{error:#}")) {
            Some(actual) => {
                toggle::persist(&path, post_id, actual)?;
                Ok(actual)
            }
            None => Err(error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciles_an_already_reposted_conflict_on_create() {
        let message = "403 Forbidden — this app cannot access the endpoint: Forbidden: \
                        You have already retweeted this Tweet.";
        assert_eq!(reconcile_from_error(true, message), Some(true));
    }

    #[test]
    fn reconciles_a_not_reposted_conflict_on_delete() {
        let message = "403 Forbidden — this app cannot access the endpoint: Forbidden: \
                        You have not retweeted this Tweet.";
        assert_eq!(reconcile_from_error(false, message), Some(false));
    }

    #[test]
    fn reconciliation_is_case_insensitive() {
        assert_eq!(reconcile_from_error(true, "ALREADY RETWEETED"), Some(true));
    }

    #[test]
    fn does_not_reconcile_an_unrelated_failure() {
        assert_eq!(
            reconcile_from_error(true, "401 Unauthorized — the bearer token was rejected"),
            None
        );
    }

    #[test]
    fn a_create_conflict_message_does_not_reconcile_a_delete_attempt() {
        // 2 つの文言は互いの鏡像だ — 逆方向に一致させると､本物の失敗を
        // そのまま通す代わりに､ローカル記録を黙って誤った値へ反転させて
        // しまう｡
        assert_eq!(
            reconcile_from_error(false, "you have already retweeted this tweet"),
            None
        );
    }

    #[test]
    fn a_delete_conflict_message_does_not_reconcile_a_create_attempt() {
        assert_eq!(
            reconcile_from_error(true, "you have not retweeted this tweet"),
            None
        );
    }

    #[test]
    fn load_all_reads_the_reposted_record_under_the_state_dir() {
        // ファイルについてこのモジュールがまだ持っている唯一の責務:
        // *どの* ファイルか｡読み取りの挙動は `toggle` のもので､
        // テストもそちらにある｡
        let root = std::env::temp_dir().join(format!("twigpui-test-repost-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.display().to_string();
        let paths = Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap();
        paths.ensure_dirs().unwrap();

        toggle::mark(&paths.reposted_posts_file(), "1").unwrap();
        assert!(load_all(&paths).unwrap().contains("1"));

        std::fs::remove_dir_all(&root).unwrap();
    }
}
