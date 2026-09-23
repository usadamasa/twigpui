//! 読み取り前の件数確認と CLI の説明｡

use anyhow::Result;

use super::api::ListSyncApi;
use super::{load_state, mirror, report, save_plan, save_state};
use crate::paths::Paths;

/// 最適化の probe が失敗しても､残高と rate limit 以外は通常の diff に戻す｡
pub(super) fn probe(paths: &Paths, client: &dyn ListSyncApi, now: i64) -> Result<Option<u64>> {
    match client.following_count(paths, now) {
        Ok(count) => Ok(Some(count)),
        Err(error)
            if error.is::<crate::rate_limit::RateLimited>()
                || error.is::<crate::rate_limit::UsageCapExceeded>()
                || error.is::<crate::x_api::PaymentRequired>() =>
        {
            Err(error)
        }
        Err(error) => {
            crate::log::warn(&format!(
                "list sync: following count probe failed; continuing with the diff: {error:#}"
            ));
            Ok(None)
        }
    }
}

/// count が同じでも､members を信頼できなければ diff を省略しない｡
pub(super) fn unchanged(
    paths: &Paths,
    list_id: &str,
    old: Option<u64>,
    count: Option<u64>,
) -> bool {
    // ponytail: 確認間に 1 follow と 1 unfollow が相殺すると､次の count 変化か
    // 強制実行まで見逃す｡dev の同期元は固定 seed なので count と比較しない｡
    paths.profile().sync_seed_usernames().is_none()
        && count.is_some()
        && count == old
        && mirror::load(paths).is_some_and(|mirror| mirror.usable(list_id))
}

/// 台帳が無いまま plan を送ると､種になる members の全件取得がその分だけ
/// 大きくなる｡先に読めば list が小さいうちに済み､以後は読まない｡
/// 台帳があれば空文字｡
pub(super) fn seed_first_note(paths: &Paths, list_id: &str, additions: usize) -> String {
    if mirror::load(paths).is_some_and(|mirror| mirror.usable(list_id)) {
        return String::new();
    }
    format!(
        "\n\nThere is no members ledger (sync_members.json) for this list yet, so the next diff \
         after these are sent reads every list member (Users — 10x the Owned price) from a list \
         that is {additions} account(s) larger. Run --sync-list --reread first to seed the \
         ledger while the list is small; after that the members are not read again."
    )
}

/// CLI の diff は probe と見込み表示を済ませてから読み始める｡
pub(super) fn dry_run(
    paths: &Paths,
    client: &dyn ListSyncApi,
    user_id: &str,
    list_id: &str,
    reread: bool,
    now: i64,
) -> Result<String> {
    let mut state = load_state(&paths.sync_state_file());
    let count = probe(paths, client, now)?;
    if !reread && unchanged(paths, list_id, state.following_count, count) {
        return Ok(format!(
            "no follow list or list members were read because the following count ({}) has \
             not changed since the last diff (the count probe read 1 Owned resource). \
             Pass --reread to pay for a fresh diff.",
            count.unwrap_or_default()
        ));
    }
    eprintln!("{}", note_before_reading(paths, user_id, list_id, count));
    // ミラーだけが新しくなった失敗を､古い count で完了扱いしない｡
    state.following_count = None;
    save_state(&paths.sync_state_file(), &state)?;
    let plan = super::run::plan_sync(paths, client, user_id, list_id, count, now)?;
    save_plan(&paths.sync_plan_file(), &plan)?;
    state.following_count = count;
    save_state(&paths.sync_state_file(), &state)?;
    Ok(format!(
        "{}\n\nnothing was changed. Re-run with --apply to send these.",
        report(&plan)
    ))
}

/// 読む前に出す見込み｡members の件数は台帳からだけ取る｡
///
/// plan の `members_total` には戻らない (#289): #176 より前の plan は
/// `#[serde(default)]` で 0 と読まれ､それは「空の list」ではなく「不明」だ｡
/// #288 以降､plan は台帳と一緒に作られるので､台帳が無いのに plan だけが
/// あるのはその古い plan のときだけ — つまり fallback に届く値は信用できない
/// ものしか無い｡
fn note_before_reading(paths: &Paths, user_id: &str, list_id: &str, count: Option<u64>) -> String {
    let mirror = mirror::load(paths);
    let mirrored = mirror.as_ref().is_some_and(|mirror| mirror.usable(list_id));
    let members = mirror
        .as_ref()
        .and_then(|mirror| mirror.members_total(list_id));
    read_note(
        count,
        super::following::head_estimate(paths, user_id, count),
        members,
        mirrored,
        paths.profile().sync_seed_usernames().map(<[&str]>::len),
    )
}

/// 価格を固定せず､読み取り件数と resource の種類だけを説明する｡
/// `head` は台帳の先頭読みで済む見込みの件数 (#289)｡検算が外れれば
/// `following` の全件になるので､両方を出す｡
fn read_note(
    following: Option<u64>,
    head: Option<u64>,
    members: Option<usize>,
    mirrored: bool,
    seed: Option<usize>,
) -> String {
    let whole = following.map_or_else(|| "unknown".to_string(), |count| count.to_string());
    let following = seed.map_or_else(
        || match head {
            Some(head) => format!(
                "the head of the follow list (about {head} accounts, Owned Reads; the whole \
                 list of about {whole} only if the ledger does not line up)"
            ),
            None => format!("the follow list (about {whole} accounts, Owned Reads)"),
        },
        |count| format!("the development seed ({count} accounts, cached username lookups, Users on cache miss)"),
    );
    let members = if mirrored {
        "from the local mirror, 0 reads".to_string()
    } else {
        format!(
            "about {} accounts, Users — 10x the Owned price",
            members.map_or_else(|| "unknown".to_string(), |count| count.to_string())
        )
    };
    format!("note: reading {following} and the list's members ({members})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_note_names_counts_classes_and_the_mirror_without_prices() {
        let text = read_note(Some(2340), None, Some(2000), false, None);
        assert!(text.contains("2340 accounts, Owned Reads"), "{text}");
        assert!(text.contains("2000 accounts, Users"), "{text}");
        assert!(text.contains("10x the Owned price"), "{text}");
        let mirrored = read_note(None, None, Some(2000), true, None);
        assert!(mirrored.contains("unknown"), "{mirrored}");
        assert!(mirrored.contains("local mirror, 0 reads"), "{mirrored}");
        let unknown = read_note(None, None, None, false, None);
        assert_eq!(unknown.matches("unknown").count(), 2);
        for note in [text, mirrored, unknown] {
            assert!(!note.contains('$'));
        }
    }

    #[test]
    fn the_note_names_the_head_read_when_the_ledger_can_serve_it() {
        // #289: 実機で「about 2343 accounts」と出たまま 5 件で済んでいた｡
        // 先頭読みの見込みと､検算が外れたときの全件の両方を言う｡
        let text = read_note(Some(2343), Some(6), Some(1085), true, None);
        assert!(
            text.contains("head of the follow list (about 6 accounts"),
            "{text}"
        );
        assert!(text.contains("whole list of about 2343"), "{text}");
    }

    #[test]
    fn a_plan_without_a_mirror_leaves_the_member_count_unknown() {
        // #289 の追記: #176 より前の plan は `members_total` が 0 と読まれる｡
        // 台帳が無いときにそれへ戻ると "about 0 accounts" と出て､高い側の
        // 最悪ケースを読む前に見せる意味が無くなる｡
        use crate::sync::api::fake::Scratch;
        let scratch = Scratch::new("note-legacy-plan");
        std::fs::write(
            scratch.paths().sync_plan_file(),
            r#"{"list_id":"7","created_at":0,"entries":[]}"#,
        )
        .unwrap();
        let text = note_before_reading(scratch.paths(), "me", "7", Some(2340));
        assert!(text.contains("about unknown accounts, Users"), "{text}");
        assert!(!text.contains("about 0"), "{text}");
    }

    #[test]
    fn development_note_names_the_seed_lookup_cost() {
        let text = read_note(Some(2340), None, None, false, Some(4));
        assert!(text.contains("development seed (4 accounts"), "{text}");
        assert!(text.contains("Users on cache miss"), "{text}");
        assert!(!text.contains("2340"));
    }
}
