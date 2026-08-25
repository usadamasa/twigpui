//! いいねの記録 (#68) — [`crate::repost`] の鏡像｡
//!
//! post id のローカル記録と､楽観更新/ロールバックのボタン状態はどちらも
//! [`crate::toggle`] にあり､repost と共有している｡ここに置いてあるのは
//! 違う部分だけだ: `likes` エンドポイント､[`Paths::liked_posts_file`]､
//! そして「ローカル記録が古い」を意味するエラー文言｡
//!
//! **他のクライアントから付けたいいねはここに反映されない**｡`repost.rs` が
//! 書いているのと同じトレードオフだ: X API v2 のタイムラインレスポンスには
//! 「自分がいいねしたか」を示すフィールドが無く､post ごとに問い合わせると
//! 表示行 1 つにつき 1 リクエストかかる｡
//!
//! [`Paths::liked_posts_file`]: crate::paths::Paths::liked_posts_file

use std::collections::HashSet;

use anyhow::Result;

use crate::paths::Paths;
use crate::toggle;
use crate::x_api::XClient;

/// いいね済みとして記録されている post id の全体 — [`toggle::load_all`] を参照｡
pub(crate) fn load_all(paths: &Paths) -> Result<HashSet<String>> {
    toggle::load_all(&paths.liked_posts_file())
}

/// いいね/いいね解除の失敗レスポンスを､本物の失敗ではなくローカル記録への
/// 訂正として解釈する — `repost::reconcile_from_error` の対応物で､retweet
/// ではなく like に対して X が返す文言に合わせてある: `creating: true` は
/// "you have already liked this Tweet" を､`creating: false` は
/// "you have not liked this Tweet" を認識する｡認識できたときは永続化すべき
/// 訂正後の値を返し､それ以外の失敗ではすべて `None` を返す — 呼び出し側は
/// `None` を通常のエラーとして伝播させる｡
///
/// **確度: 実際の API に対しては未検証**｡repost とまったく同じ状況だ
/// (人間向けのメッセージ本文に一致させるほうが素の 403 に一致させるより
/// 頑健だという理由は `repost::reconcile_from_error` の doc にある)｡
///
/// 2 方向を 1 つの部分文字列チェックにまとめていないのは意図的だ:
/// "already liked" と "not liked" は互いの鏡像で､逆方向に一致させると
/// 本物の失敗をそのまま通す代わりに､ローカル記録を黙って誤った値へ
/// 反転させてしまう｡
pub(crate) fn reconcile_from_error(creating: bool, message: &str) -> Option<bool> {
    let lower = message.to_lowercase();
    if creating && lower.contains("already liked") {
        Some(true)
    } else if !creating && (lower.contains("have not liked") || lower.contains("haven't liked")) {
        Some(false)
    } else {
        None
    }
}

/// `user_id` として `post_id` にいいねする (#68): API を呼び､成功したら
/// 永続化する｡認識できた "already liked" の衝突 ([`reconcile_from_error`]
/// を参照) はエラーを伝播させる代わりにローカル記録を訂正する — 呼び出し側
/// (`ui.rs`) は `Ok` を「これが現時点の状態だ」として扱い､必ずしも
/// 「作成が成功した」とは扱わない｡
///
/// 直接のユニットテストは無い — `client` を通じて実際に HTTP リクエストを
/// 出すためで､`repost::create` と同じだ｡
pub(crate) fn create(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    post_id: &str,
    now: i64,
) -> Result<bool> {
    let path = paths.liked_posts_file();
    match client.create_like(paths, user_id, post_id, now) {
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

/// `user_id` として `post_id` のいいねを解除する (#68) — [`create`] の
/// 完全な鏡像で､方向だけが逆｡
pub(crate) fn remove(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    post_id: &str,
    now: i64,
) -> Result<bool> {
    let path = paths.liked_posts_file();
    match client.delete_like(paths, user_id, post_id, now) {
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
    fn reconciles_an_already_liked_conflict_on_create() {
        let message = "403 Forbidden — this app cannot access the endpoint: Forbidden: \
                        You have already liked this Tweet.";
        assert_eq!(reconcile_from_error(true, message), Some(true));
    }

    #[test]
    fn reconciles_a_not_liked_conflict_on_delete() {
        let message = "403 Forbidden — this app cannot access the endpoint: Forbidden: \
                        You have not liked this Tweet.";
        assert_eq!(reconcile_from_error(false, message), Some(false));
    }

    #[test]
    fn reconciliation_is_case_insensitive() {
        assert_eq!(reconcile_from_error(true, "ALREADY LIKED"), Some(true));
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
        assert_eq!(
            reconcile_from_error(false, "you have already liked this tweet"),
            None
        );
    }

    #[test]
    fn a_delete_conflict_message_does_not_reconcile_a_create_attempt() {
        assert_eq!(
            reconcile_from_error(true, "you have not liked this tweet"),
            None
        );
    }

    #[test]
    fn a_repost_conflict_message_does_not_reconcile_a_like_attempt() {
        // 2 つのモジュールは同じ文字列化されたエラーを読む｡それぞれ自分の
        // エンドポイントの文言だけを認識しなければならない｡
        assert_eq!(
            reconcile_from_error(true, "you have already retweeted this tweet"),
            None
        );
    }

    #[test]
    fn load_all_reads_the_liked_record_under_the_state_dir() {
        let root = std::env::temp_dir().join(format!("twigpui-test-like-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.display().to_string();
        let paths = Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap();
        paths.ensure_dirs().unwrap();

        toggle::mark(&paths.liked_posts_file(), "1").unwrap();
        assert!(load_all(&paths).unwrap().contains("1"));
        // repost の記録は別ファイルなので､空のままでなければならない｡
        assert!(crate::repost::load_all(&paths).unwrap().is_empty());

        std::fs::remove_dir_all(&root).unwrap();
    }
}
