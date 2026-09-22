//! follow list の台帳: 最後の全件読みを API が返した順のまま持ち､次の diff
//! はその先頭に当たるまでしか読まない (#289)｡
//!
//! `GET /2/users/:id/following` は新しく follow した順に返す (2026-09-22 の
//! 実測､`x-api-endpoints`)｡follow した時刻の field は無いので､時刻では
//! 絞れない｡代わりに手元の先頭 id を cursor として使う: count の probe
//! (`preflight::probe`) が増えていたら､増えた分だけの小さいページを読み､
//! 台帳が知っている id に当たったところで止める｡そこまでが新しい follow だ｡
//!
//! # 検算
//!
//! 順序の前提は 1 標本の実測に乗っているので､結果を信じる前に 2 つ確かめる｡
//!
//! - 最初に当たった既知の id が台帳の先頭と同じであること｡違えば先頭が
//!   unfollow されたか､X が順序を変えた
//! - 台帳の count + 新規の件数 == 今回の count であること｡違えば数えて
//!   いない unfollow がある
//!
//! どちらかが外れたら全件読みに落ちる — つまり #289 より前の挙動に戻るだけで､
//! 誤った diff にはならない｡count が減ったとき (unfollow) も全件読み:
//! どれが消えたかは先頭からは知れない｡
//!
//! [`super::mirror`] と同じく古さでは読み直さない｡読み直したいときは
//! `sync_following.json` を消す｡

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::api::ListSyncApi;
use crate::paths::Paths;
use crate::x_api::model::User;

/// 先頭読みが頼む最小のページ｡`max_results=5` が通ることは 2026-09-22 に
/// 実測した｡1〜4 は未計測なので､増えた分がそれより少なくてもここまでは読む｡
const HEAD_PAGE_MIN: u32 = 5;

/// 先頭読みが頼む最大のページ｡spec の上限で､全件読みの `USER_PAGE_SIZE` と
/// 同じ値｡
const HEAD_PAGE_MAX: u32 = 100;

#[derive(Debug, Serialize, Deserialize)]
struct Follow {
    id: String,
    username: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Ledger {
    version: u32,
    /// 読んだアカウント自身の id｡別のアカウントで sign in し直した台帳は
    /// 使わない｡
    user_id: String,
    /// 全件読みの直前に probe した `following_count`｡検算の基準になる｡
    /// probe が失敗していれば `None` で､その台帳からは先頭読みを始めない｡
    count: Option<u64>,
    read_at: i64,
    /// API が返した順のまま｡先頭が最も新しい follow｡
    follows: Vec<Follow>,
}

impl Ledger {
    fn of(user_id: &str, count: Option<u64>, now: i64, users: &[User]) -> Self {
        Self {
            version: 1,
            user_id: user_id.to_string(),
            count,
            read_at: now,
            follows: users
                .iter()
                .map(|user| Follow {
                    id: user.id.clone(),
                    username: user.username.clone(),
                })
                .collect(),
        }
    }

    fn users(&self) -> Vec<User> {
        self.follows
            .iter()
            .map(|follow| User {
                id: follow.id.clone(),
                name: follow.username.clone(),
                username: follow.username.clone(),
                profile_image_url: None,
            })
            .collect()
    }

    fn save(&self, paths: &Paths) -> Result<()> {
        let path = paths.sync_following_file();
        let json =
            serde_json::to_string_pretty(self).context("could not serialize the follow ledger")?;
        std::fs::write(&path, json).with_context(|| format!("could not write {}", path.display()))
    }
}

/// 無いファイルは黙って扱い､壊れた記録は理由を残す｡
fn load(paths: &Paths) -> Option<Ledger> {
    let contents = match std::fs::read_to_string(paths.sync_following_file()) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            crate::log::warn(&format!(
                "list sync: could not read the follow ledger: {error}; a full follow read is needed"
            ));
            return None;
        }
    };
    match serde_json::from_str::<Ledger>(&contents) {
        Ok(ledger) if ledger.version == 1 => Some(ledger),
        Ok(ledger) => {
            crate::log::warn(&format!(
                "list sync: unsupported follow ledger version {}; a full follow read is needed",
                ledger.version
            ));
            None
        }
        Err(error) => {
            crate::log::warn(&format!(
                "list sync: corrupt follow ledger: {error}; a full follow read is needed"
            ));
            None
        }
    }
}

/// `user_id` の follow list｡台帳と `count` (今回の probe) で済むなら先頭だけ
/// 読み､でなければ `read_all` で全件読んで台帳を作り直す｡
///
/// どちらの経路でも返す前に台帳を保存する｡先頭読みの結果も台帳になる —
/// 次の diff はそれの先頭を見る｡
pub(super) fn read(
    paths: &Paths,
    client: &dyn ListSyncApi,
    user_id: &str,
    count: Option<u64>,
    now: i64,
    read_all: impl FnOnce() -> Result<Vec<User>>,
) -> Result<Vec<User>> {
    if let Some(ledger) = load(paths).filter(|ledger| ledger.user_id == user_id)
        && let Some(count) = count
        && let Some(users) = head(paths, client, user_id, &ledger, count, now)?
    {
        Ledger::of(user_id, Some(count), ledger.read_at, &users).save(paths)?;
        return Ok(users);
    }
    let users = read_all()?;
    Ledger::of(user_id, count, now, &users).save(paths)?;
    Ok(users)
}

/// 先頭だけ読んで台帳に継ぎ足す｡`None` は「全件読みが要る」で､理由は
/// log に残す｡
fn head(
    paths: &Paths,
    client: &dyn ListSyncApi,
    user_id: &str,
    ledger: &Ledger,
    count: u64,
    now: i64,
) -> Result<Option<Vec<User>>> {
    let Some(old) = ledger.count else {
        crate::log::info(
            "list sync: the follow ledger has no count; reading the whole follow list",
        );
        return Ok(None);
    };
    let Some(first) = ledger.follows.first() else {
        return Ok(None);
    };
    let Some(delta) = count.checked_sub(old) else {
        crate::log::info(&format!(
            "list sync: following count fell from {old} to {count}; reading the whole follow list"
        ));
        return Ok(None);
    };
    // 新しい follow が delta 件と､台帳の先頭が 1 件｡それより先は読まない｡
    let wanted = delta.saturating_add(1);
    let page_size = u32::try_from(wanted)
        .unwrap_or(u32::MAX)
        .clamp(HEAD_PAGE_MIN, HEAD_PAGE_MAX);
    let pages = wanted.div_ceil(u64::from(page_size));
    let known: std::collections::HashSet<&str> = ledger
        .follows
        .iter()
        .map(|follow| follow.id.as_str())
        .collect();

    let mut new = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..pages {
        let (page, next) = client
            .following_head(paths, user_id, page_size, cursor.as_deref(), now)
            .context("could not read the head of the follow list — nothing was changed")?;
        for user in page {
            if !known.contains(user.id.as_str()) {
                new.push(user);
                continue;
            }
            if user.id != first.id {
                crate::log::info(&format!(
                    "list sync: the follow list no longer starts where the ledger does (hit {} \
                     before {}); reading the whole follow list",
                    user.id, first.id
                ));
                return Ok(None);
            }
            let found = u64::try_from(new.len()).unwrap_or(u64::MAX);
            if old.saturating_add(found) != count {
                crate::log::info(&format!(
                    "list sync: {found} new follow(s) do not account for the count ({old} -> \
                     {count}); reading the whole follow list"
                ));
                return Ok(None);
            }
            crate::log::info(&format!(
                "list sync: {found} new follow(s) since the last full read; the rest came from \
                 the ledger"
            ));
            new.extend(ledger.users());
            return Ok(Some(new));
        }
        match next {
            Some(token) => cursor = Some(token),
            None => break,
        }
    }
    crate::log::info(&format!(
        "list sync: no account from the ledger within the first {} follow(s); reading the whole \
         follow list",
        new.len()
    ));
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::api::fake::{Call, FakeApi, Scratch, page};

    fn write_ledger(paths: &Paths, user_id: &str, count: Option<u64>, ids: &[&str]) {
        let follows: Vec<serde_json::Value> = ids
            .iter()
            .map(|id| serde_json::json!({"id": id, "username": format!("user{id}")}))
            .collect();
        let json = serde_json::json!({
            "version": 1, "user_id": user_id, "count": count, "read_at": 100, "follows": follows
        });
        std::fs::write(paths.sync_following_file(), json.to_string()).unwrap();
    }

    fn saved(paths: &Paths) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(paths.sync_following_file()).unwrap())
            .unwrap()
    }

    fn ids(users: &[User]) -> Vec<&str> {
        users.iter().map(|user| user.id.as_str()).collect()
    }

    fn read_with(scratch: &Scratch, client: &FakeApi, count: Option<u64>) -> Result<Vec<User>> {
        read(scratch.paths(), client, "me", count, 200, || {
            let mut all = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let (page, next) =
                    client.following_page(scratch.paths(), "me", cursor.as_deref(), 200)?;
                all.extend(page);
                match next {
                    Some(token) => cursor = Some(token),
                    None => return Ok(all),
                }
            }
        })
    }

    #[test]
    fn without_a_ledger_the_whole_list_is_read_and_becomes_the_ledger() {
        let scratch = Scratch::new("follow-fresh");
        let client = FakeApi::new().following(vec![
            Ok(page(&[("2", "b")], Some("next"))),
            Ok(page(&[("1", "a")], None)),
        ]);
        let users = read_with(&scratch, &client, Some(2)).unwrap();
        assert_eq!(ids(&users), ["2", "1"]);
        assert_eq!(
            client.calls(),
            [
                Call::Following(None),
                Call::Following(Some("next".to_string()))
            ]
        );
        let ledger = saved(scratch.paths());
        assert_eq!(ledger["version"], 1);
        assert_eq!(ledger["user_id"], "me");
        assert_eq!(ledger["count"], 2);
        assert_eq!(ledger["read_at"], 200);
        assert_eq!(ledger["follows"][0]["id"], "2");
        assert_eq!(ledger["follows"][1]["username"], "a");
    }

    #[test]
    fn one_new_follow_is_read_from_the_head_and_prepended() {
        // #289 の本題: 1 人 follow しても全件 (数千件) は読み直さない｡
        let scratch = Scratch::new("follow-head");
        write_ledger(scratch.paths(), "me", Some(3), &["3", "2", "1"]);
        let client = FakeApi::new().heads(vec![Ok(page(
            &[("9", "new"), ("3", "c"), ("2", "b")],
            None,
        ))]);
        let users = read_with(&scratch, &client, Some(4)).unwrap();
        assert_eq!(ids(&users), ["9", "3", "2", "1"]);
        // 増えた分 1 + 先頭 1 = 2 だが､実測した下限の 5 までは読む｡
        assert_eq!(client.calls(), [Call::FollowingHead(5, None)]);
        let ledger = saved(scratch.paths());
        assert_eq!(ledger["count"], 4);
        assert_eq!(ledger["follows"].as_array().unwrap().len(), 4);
        assert_eq!(ledger["follows"][0]["id"], "9");
        // 全件読みの時刻は保つ (mirror と同じ)｡
        assert_eq!(ledger["read_at"], 100);
    }

    #[test]
    fn the_page_grows_with_the_delta_and_is_capped_at_the_spec_maximum() {
        let scratch = Scratch::new("follow-page-size");
        let new = |range: std::ops::Range<usize>| -> Vec<User> {
            range
                .map(|n| crate::sync::api::fake::user(&format!("n{n}"), "new"))
                .collect()
        };
        // 7 人増えた: 7 + 1 = 8 件のページ｡
        write_ledger(scratch.paths(), "me", Some(10), &["1"]);
        let mut first = new(0..7);
        first.push(crate::sync::api::fake::user("1", "a"));
        let client = FakeApi::new().heads(vec![Ok((first, None))]);
        let users = read_with(&scratch, &client, Some(17)).unwrap();
        assert_eq!(client.calls(), [Call::FollowingHead(8, None)]);
        assert_eq!(users.len(), 8);
        // 150 人増えた: 100 件のページを 2 枚まで｡
        write_ledger(scratch.paths(), "me", Some(10), &["1"]);
        let mut second = new(100..150);
        second.push(crate::sync::api::fake::user("1", "a"));
        let client = FakeApi::new().heads(vec![
            Ok((new(0..100), Some("p2".to_string()))),
            Ok((second, None)),
        ]);
        let users = read_with(&scratch, &client, Some(160)).unwrap();
        assert_eq!(
            client.calls(),
            [
                Call::FollowingHead(100, None),
                Call::FollowingHead(100, Some("p2".to_string()))
            ]
        );
        assert_eq!(users.len(), 151);
        assert_eq!(users.last().unwrap().id, "1");
    }

    #[test]
    fn a_head_that_starts_elsewhere_falls_back_to_the_whole_list() {
        // X が順序を変えた: 1 人 follow して count は合うが､最初に当たった
        // 既知の id が台帳の先頭ではない｡順序の前提が外れたら信じない｡
        let scratch = Scratch::new("follow-shifted");
        write_ledger(scratch.paths(), "me", Some(3), &["3", "2", "1"]);
        let client = FakeApi::new()
            .heads(vec![Ok(page(&[("9", "n"), ("2", "b"), ("3", "c")], None))])
            .following(vec![Ok(page(
                &[("9", "n"), ("2", "b"), ("3", "c"), ("1", "a")],
                None,
            ))]);
        let users = read_with(&scratch, &client, Some(4)).unwrap();
        assert_eq!(ids(&users), ["9", "2", "3", "1"]);
        assert_eq!(
            client.calls(),
            [Call::FollowingHead(5, None), Call::Following(None)]
        );
        assert_eq!(
            saved(scratch.paths())["follows"].as_array().unwrap().len(),
            4
        );
    }

    #[test]
    fn a_head_that_does_not_account_for_the_count_falls_back_to_the_whole_list() {
        // 台帳の先頭には当たったが､新規が 2 人で count は +1: どこかで 1 人
        // unfollow している｡
        let scratch = Scratch::new("follow-miscount");
        write_ledger(scratch.paths(), "me", Some(3), &["3", "2", "1"]);
        let client = FakeApi::new()
            .heads(vec![Ok(page(&[("9", "n"), ("8", "m"), ("3", "c")], None))])
            .following(vec![Ok(page(
                &[("9", "n"), ("8", "m"), ("3", "c"), ("1", "a")],
                None,
            ))]);
        let users = read_with(&scratch, &client, Some(4)).unwrap();
        assert_eq!(ids(&users), ["9", "8", "3", "1"]);
        assert!(client.calls().contains(&Call::Following(None)));
    }

    #[test]
    fn no_known_account_within_the_head_falls_back_to_the_whole_list() {
        let scratch = Scratch::new("follow-unknown");
        write_ledger(scratch.paths(), "me", Some(3), &["3", "2", "1"]);
        let client = FakeApi::new()
            .heads(vec![Ok(page(&[("9", "n"), ("8", "m")], Some("more")))])
            .following(vec![Ok(page(&[("9", "n")], None))]);
        read_with(&scratch, &client, Some(4)).unwrap();
        // 1 ページで足りるはずの読みは 2 ページ目へ進まない｡
        assert_eq!(
            client.calls(),
            [Call::FollowingHead(5, None), Call::Following(None)]
        );
    }

    #[test]
    fn a_fallen_count_a_missing_count_or_another_account_reads_the_whole_list() {
        for (label, user, ledger_count, count) in [
            ("unfollow", "me", Some(3), Some(2)),
            ("no-probe", "me", Some(3), None),
            ("no-ledger-count", "me", None, Some(4)),
            ("other-account", "you", Some(3), Some(4)),
        ] {
            let scratch = Scratch::new(&format!("follow-{label}"));
            write_ledger(scratch.paths(), user, ledger_count, &["3", "2", "1"]);
            let client = FakeApi::new().following(vec![Ok(page(&[("3", "c")], None))]);
            let users = read_with(&scratch, &client, count).unwrap();
            assert_eq!(ids(&users), ["3"], "{label}");
            assert_eq!(client.calls(), [Call::Following(None)], "{label}");
            let ledger = saved(scratch.paths());
            assert_eq!(ledger["user_id"], "me", "{label}");
            assert_eq!(ledger["count"], serde_json::json!(count), "{label}");
        }
    }

    #[test]
    fn an_unchanged_count_costs_one_small_page_and_no_new_follows() {
        // 手動起動 (#174) と --reread は count の省略判定を越えるが､台帳は
        // 通る: 先頭 5 件で「変わっていない」が確かめられ､全件は読まない｡
        let scratch = Scratch::new("follow-same");
        write_ledger(scratch.paths(), "me", Some(3), &["3", "2", "1"]);
        let client = FakeApi::new().heads(vec![Ok(page(&[("3", "c"), ("2", "b")], None))]);
        let users = read_with(&scratch, &client, Some(3)).unwrap();
        assert_eq!(ids(&users), ["3", "2", "1"]);
        assert_eq!(client.calls(), [Call::FollowingHead(5, None)]);
    }

    #[test]
    fn a_failed_head_read_is_an_error_not_a_full_read() {
        // 残高切れや rate limit の 402 / 429 を全件読みで上書きしない｡
        let scratch = Scratch::new("follow-head-fails");
        write_ledger(scratch.paths(), "me", Some(3), &["3", "2", "1"]);
        let client = FakeApi::new().heads(vec![Err(anyhow::anyhow!("HTTP 402"))]);
        let error = read_with(&scratch, &client, Some(4))
            .unwrap_err()
            .to_string();
        assert!(error.contains("nothing was changed"), "{error}");
        assert_eq!(client.calls(), [Call::FollowingHead(5, None)]);
        assert_eq!(saved(scratch.paths())["count"], 3);
    }

    #[test]
    fn corrupt_or_unknown_version_ledgers_are_replaced() {
        for (label, contents) in [
            ("corrupt", "broken"),
            (
                "version",
                r#"{"version":2,"user_id":"me","count":3,"read_at":100,"follows":[]}"#,
            ),
        ] {
            let scratch = Scratch::new(&format!("follow-{label}"));
            std::fs::write(scratch.paths().sync_following_file(), contents).unwrap();
            let client = FakeApi::new().following(vec![Ok(page(&[], None))]);
            read_with(&scratch, &client, Some(0)).unwrap();
            assert_eq!(client.calls(), [Call::Following(None)], "{label}");
            assert_eq!(saved(scratch.paths())["version"], 1, "{label}");
        }
    }

    #[test]
    fn a_partial_full_read_leaves_the_old_ledger_in_place() {
        let scratch = Scratch::new("follow-partial");
        write_ledger(scratch.paths(), "me", Some(3), &["3", "2", "1"]);
        let client = FakeApi::new().following(vec![
            Ok(page(&[("3", "c")], Some("next"))),
            Err(anyhow::anyhow!("HTTP 503")),
        ]);
        assert!(read_with(&scratch, &client, Some(2)).is_err());
        assert_eq!(saved(scratch.paths())["count"], 3);
    }
}
