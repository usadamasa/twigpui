//! 読み取り前の件数確認と CLI の説明｡

use anyhow::Result;

use super::api::ListSyncApi;
use super::{load_plan, load_state, mirror, report, save_plan, save_state};
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
    now: i64,
) -> bool {
    // ponytail: 確認間に 1 follow と 1 unfollow が相殺すると､次の count 変化か
    // 強制実行まで見逃す｡dev の同期元は固定 seed なので count と比較しない｡
    paths.profile().sync_seed_usernames().is_none()
        && count.is_some()
        && count == old
        && mirror::load(paths).is_some_and(|mirror| mirror.usable(list_id, now))
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
    if !reread && unchanged(paths, list_id, state.following_count, count, now) {
        return Ok(format!(
            "no follow list or list members were read because the following count ({}) has \
             not changed since the last diff (the count probe read 1 Owned resource). \
             Pass --reread to pay for a fresh diff.",
            count.unwrap_or_default()
        ));
    }
    let mirror = mirror::load(paths);
    let mirrored = mirror
        .as_ref()
        .is_some_and(|mirror| mirror.usable(list_id, now));
    let members = mirror
        .as_ref()
        .and_then(|mirror| mirror.members_total(list_id))
        .or(load_plan(&paths.sync_plan_file())?
            .filter(|plan| plan.list_id == list_id)
            .map(|plan| plan.members_total));
    eprintln!(
        "{}",
        read_note(
            count,
            members,
            mirrored,
            paths.profile().sync_seed_usernames().map(<[&str]>::len)
        )
    );
    // ミラーだけが新しくなった失敗を､古い count で完了扱いしない｡
    state.following_count = None;
    save_state(&paths.sync_state_file(), &state)?;
    let plan = super::run::plan_sync(paths, client, user_id, list_id, now)?;
    save_plan(&paths.sync_plan_file(), &plan)?;
    state.following_count = count;
    save_state(&paths.sync_state_file(), &state)?;
    Ok(format!(
        "{}\n\nnothing was changed. Re-run with --apply to send these.",
        report(&plan)
    ))
}

/// 価格を固定せず､読み取り件数と resource の種類だけを説明する｡
fn read_note(
    following: Option<u64>,
    members: Option<usize>,
    mirrored: bool,
    seed: Option<usize>,
) -> String {
    let following = seed.map_or_else(
        || format!("the follow list (about {} accounts, Owned Reads)", following.map_or_else(|| "unknown".to_string(), |count| count.to_string())),
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
        let text = read_note(Some(2340), Some(2000), false, None);
        assert!(text.contains("2340 accounts, Owned Reads"), "{text}");
        assert!(text.contains("2000 accounts, Users"), "{text}");
        assert!(text.contains("10x the Owned price"), "{text}");
        let mirrored = read_note(None, Some(2000), true, None);
        assert!(mirrored.contains("unknown"), "{mirrored}");
        assert!(mirrored.contains("local mirror, 0 reads"), "{mirrored}");
        let unknown = read_note(None, None, false, None);
        assert_eq!(unknown.matches("unknown").count(), 2);
        for note in [text, mirrored, unknown] {
            assert!(!note.contains('$'));
        }
    }

    #[test]
    fn development_note_names_the_seed_lookup_cost() {
        let text = read_note(Some(2340), None, false, Some(4));
        assert!(text.contains("development seed (4 accounts"), "{text}");
        assert!(text.contains("Users on cache miss"), "{text}");
        assert!(!text.contains("2340"));
    }
}
