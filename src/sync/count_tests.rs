#[cfg(test)]
use super::*;
use crate::sync::api::fake::{Call, FakeApi, Scratch, page, rate_limited};

const NOW: i64 = 3_000_000;
const INTERVAL: u32 = 21_600;

fn prepare(paths: &Paths, count: Option<u64>, read_at: i64) {
    save_state(
        &paths.sync_state_file(),
        &SyncState {
            following_count: count,
            last_diff_at: Some(1),
            ..SyncState::default()
        },
    )
    .unwrap();
    let json = serde_json::json!({"version":1,"list_id":"7","read_at":read_at,"members":[]});
    std::fs::write(paths.sync_members_file(), json.to_string()).unwrap();
}

fn check(paths: &Paths, api: &FakeApi, forced: bool) -> Tick {
    tick(
        paths,
        api,
        "me",
        "7",
        Pacing {
            interval_seconds: INTERVAL,
            writes: crate::sync::WritePacing::DEFAULT,
            forced,
        },
        10,
        NOW,
    )
}

#[test]
fn unchanged_count_skips_the_diff_and_schedules_one_interval_later() {
    let scratch = Scratch::new("count-unchanged");
    prepare(scratch.paths(), Some(4), NOW);
    let api = FakeApi::new().counts(vec![Ok(4)]);
    let tick = check(scratch.paths(), &api, false);
    let outcome = tick.outcome.unwrap();
    assert!(schedule::notice(&outcome).is_none());
    assert!(schedule::is_finished(Some(&outcome)));
    assert_eq!(tick.wake_at, NOW.saturating_add(i64::from(INTERVAL)));
    assert_eq!(api.calls(), [Call::FollowingCount]);
    let state = load_state(&scratch.paths().sync_state_file());
    assert_eq!(state.last_diff_at, Some(NOW));
    assert_eq!(state.following_count, Some(4));
}

#[test]
fn changed_first_and_forced_counts_run_the_diff_and_store_the_probe() {
    for (label, count, forced) in [
        ("changed", Some(3), false),
        ("first", None, false),
        ("forced", Some(4), true),
    ] {
        let scratch = Scratch::new(&format!("count-{label}"));
        prepare(scratch.paths(), count, NOW);
        let api = FakeApi::new()
            .counts(vec![Ok(4)])
            .following(vec![Ok(page(&[("1", "alice")], None))]);
        let tick = check(scratch.paths(), &api, forced);
        assert!(
            matches!(tick.outcome, Ok(Outcome::Diffed { adds: 1, .. })),
            "{:?}",
            tick.outcome
        );
        assert_eq!(
            load_state(&scratch.paths().sync_state_file()).following_count,
            Some(4)
        );
        assert_eq!(api.calls(), [Call::FollowingCount, Call::Following(None)]);
    }
}

#[test]
fn a_changed_count_with_a_follow_ledger_reads_only_the_head() {
    // #289: loop 側も同じ台帳を通る｡1 人 follow した tick は先頭の 1 ページで
    // diff を作り､全件は買わない｡
    let scratch = Scratch::new("count-follow-head");
    prepare(scratch.paths(), Some(3), NOW);
    std::fs::write(
        scratch.paths().sync_following_file(),
        r#"{"version":1,"user_id":"me","count":3,"read_at":100,"follows":[
            {"id":"3","username":"c"},{"id":"2","username":"b"},{"id":"1","username":"a"}]}"#,
    )
    .unwrap();
    let api = FakeApi::new()
        .counts(vec![Ok(4)])
        .heads(vec![Ok(page(&[("9", "new"), ("3", "c")], None))]);
    let tick = check(scratch.paths(), &api, false);
    assert!(
        matches!(tick.outcome, Ok(Outcome::Diffed { adds: 4, .. })),
        "{:?}",
        tick.outcome
    );
    assert_eq!(
        api.calls(),
        [Call::FollowingCount, Call::FollowingHead(5, None)]
    );
    assert_eq!(
        load_state(&scratch.paths().sync_state_file()).following_count,
        Some(4)
    );
}

#[test]
fn unchanged_count_requires_a_usable_mirror() {
    for label in ["absent", "other", "corrupt"] {
        let scratch = Scratch::new(&format!("count-mirror-{label}"));
        prepare(scratch.paths(), Some(4), NOW);
        match label {
            "absent" => std::fs::remove_file(scratch.paths().sync_members_file()).unwrap(),
            "other" => std::fs::write(
                scratch.paths().sync_members_file(),
                r#"{"version":1,"list_id":"8","read_at":3000000,"members":[]}"#,
            )
            .unwrap(),
            _ => std::fs::write(scratch.paths().sync_members_file(), "broken").unwrap(),
        }
        let api = FakeApi::new()
            .counts(vec![Ok(4)])
            .members(vec![Ok(page(&[], None))])
            .following(vec![Ok(page(&[], None))]);
        assert!(check(scratch.paths(), &api, false).outcome.is_ok());
        assert_eq!(
            api.calls(),
            [
                Call::FollowingCount,
                Call::Members(None),
                Call::Following(None)
            ]
        );
    }
}

#[test]
fn probe_failure_falls_back_without_remembering_an_unverified_count() {
    let scratch = Scratch::new("count-probe-failed");
    prepare(scratch.paths(), Some(4), NOW);
    let api = FakeApi::new()
        .counts(vec![Err(anyhow::anyhow!("metrics missing"))])
        .following(vec![Ok(page(&[], None))]);
    assert!(check(scratch.paths(), &api, false).outcome.is_ok());
    assert_eq!(api.calls(), [Call::FollowingCount, Call::Following(None)]);
    assert_eq!(
        load_state(&scratch.paths().sync_state_file()).following_count,
        None
    );
}

#[test]
fn failed_diff_never_stores_the_new_probe_count() {
    let scratch = Scratch::new("count-diff-failed");
    prepare(scratch.paths(), Some(3), NOW);
    let api = FakeApi::new()
        .counts(vec![Ok(4)])
        .following(vec![Err(anyhow::anyhow!("read failed"))]);
    assert!(check(scratch.paths(), &api, false).outcome.is_err());
    assert_ne!(
        load_state(&scratch.paths().sync_state_file()).following_count,
        Some(4)
    );
}

#[test]
fn exhausted_credit_and_rate_limits_stop_before_the_full_diff() {
    for (label, error) in [
        ("rate", rate_limited(NOW.saturating_add(500), false)),
        (
            "cap",
            anyhow::Error::new(crate::rate_limit::UsageCapExceeded {
                detail: "cap".to_string(),
            }),
        ),
        (
            "payment",
            anyhow::Error::new(crate::x_api::PaymentRequired {
                endpoint: crate::rate_limit::Endpoint::Me,
                detail: "credits exhausted".to_string(),
            }),
        ),
    ] {
        let scratch = Scratch::new(&format!("count-stop-{label}"));
        let api = FakeApi::new().counts(vec![Err(error)]);
        assert!(check(scratch.paths(), &api, false).outcome.is_err());
        assert_eq!(api.calls(), [Call::FollowingCount]);
    }
}

#[test]
fn old_state_files_default_the_following_count() {
    let state: SyncState = serde_json::from_str(r#"{"last_diff_at":100}"#).unwrap();
    assert_eq!(state.following_count, None);
}

#[test]
fn a_refreshed_mirror_after_a_failed_follow_read_cannot_skip_the_retry() {
    let scratch = Scratch::new("count-refreshed-failure");
    prepare(scratch.paths(), Some(4), NOW);
    // 台帳が無い状態から始める｡members の全件取得が走る唯一の入口｡
    std::fs::remove_file(scratch.paths().sync_members_file()).unwrap();
    let api = FakeApi::new()
        .counts(vec![Ok(4)])
        .members(vec![Ok(page(&[("2", "bob")], None))])
        .following(vec![Err(anyhow::anyhow!("following unavailable"))]);
    assert!(check(scratch.paths(), &api, false).outcome.is_err());
    let api = FakeApi::new()
        .counts(vec![Ok(4)])
        .following(vec![Ok(page(&[], None))]);
    let tick = tick(
        scratch.paths(),
        &api,
        "me",
        "7",
        Pacing {
            interval_seconds: INTERVAL,
            writes: crate::sync::WritePacing::DEFAULT,
            forced: false,
        },
        10,
        NOW.saturating_add(i64::from(INTERVAL)),
    );
    assert!(
        matches!(tick.outcome, Ok(Outcome::Diffed { removals: 1, .. })),
        "{:?}",
        tick.outcome
    );
    assert_eq!(api.calls(), [Call::FollowingCount, Call::Following(None)]);
}
