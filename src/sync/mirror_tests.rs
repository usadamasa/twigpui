#[cfg(test)]
use super::tests::GAP;
use super::*;
use crate::sync::api::fake::{Call, FakeApi, Scratch, page, rate_limited, rejected};

fn write_mirror(paths: &Paths, list: &str, read_at: i64) {
    let json = serde_json::json!({
        "version": 1, "list_id": list, "read_at": read_at,
        "members": [{"id": "2", "username": "bob"}]
    });
    std::fs::write(paths.sync_members_file(), json.to_string()).unwrap();
}

fn saved_members(paths: &Paths) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(paths.sync_members_file()).unwrap()).unwrap()
}

#[test]
fn fresh_mirror_replaces_the_member_read() {
    let scratch = Scratch::new("mirror-fresh");
    write_mirror(scratch.paths(), "7", 100);
    let client = FakeApi::new().following(vec![Ok(page(&[("2", "bob")], None))]);
    let plan = plan_sync(scratch.paths(), &client, "me", "7", None, 101).unwrap();
    assert!(plan.is_complete());
    assert_eq!(plan.members_total, 1);
    assert_eq!(client.calls(), [Call::Following(None)]);
}

#[test]
fn absent_mirror_saves_every_member_page_before_the_follow_read() {
    let scratch = Scratch::new("mirror-pages");
    let client = FakeApi::new()
        .members(vec![
            Ok(page(&[("2", "bob")], Some("next"))),
            Ok(page(&[("3", "carol")], None)),
        ])
        .following(vec![Err(anyhow::anyhow!("following unavailable"))]);
    assert!(plan_sync(scratch.paths(), &client, "me", "7", None, 100).is_err());
    let saved = saved_members(scratch.paths());
    assert_eq!(saved["version"], 1);
    assert_eq!(saved["list_id"], "7");
    assert_eq!(saved["read_at"], 100);
    assert_eq!(saved["members"].as_array().unwrap().len(), 2);
    assert_eq!(
        client.calls(),
        [
            Call::Members(None),
            Call::Members(Some("next".to_string())),
            Call::Following(None)
        ]
    );
}

#[test]
fn a_mirror_for_another_list_requires_a_full_member_read() {
    let scratch = Scratch::new("mirror-other");
    write_mirror(scratch.paths(), "8", 3_000_000);
    let client = FakeApi::new()
        .members(vec![Ok(page(&[("3", "carol")], None))])
        .following(vec![Ok(page(&[], None))]);
    let plan = plan_sync(scratch.paths(), &client, "me", "7", None, 3_000_000).unwrap();
    assert_eq!(plan.members_total, 1);
    assert!(client.calls().contains(&Call::Members(None)));
    let saved = saved_members(scratch.paths());
    assert_eq!(saved["read_at"], 3_000_000);
    assert_eq!(saved["members"][0]["id"], "3");
}

#[test]
fn age_alone_never_buys_a_member_read() {
    // member の全件取得は following の 10 倍高く､残高が足りなければ途中の
    // 402 で何も残らない｡このファイルはアプリが list に入れた相手の台帳で､
    // 古さは読み直す理由にならない｡読み直すのはファイルを消した人だけ｡
    for (label, read_at) in [("old", 0), ("future", 3_000_001)] {
        let scratch = Scratch::new(&format!("mirror-{label}"));
        write_mirror(scratch.paths(), "7", read_at);
        let client = FakeApi::new().following(vec![Ok(page(&[("2", "bob")], None))]);
        let plan = plan_sync(scratch.paths(), &client, "me", "7", None, 3_000_000).unwrap();
        assert!(plan.is_complete(), "{label}");
        assert_eq!(client.calls(), [Call::Following(None)], "{label}");
        assert_eq!(
            saved_members(scratch.paths())["read_at"],
            read_at,
            "{label}"
        );
    }
}

#[test]
fn corrupt_or_unknown_version_mirrors_are_replaced() {
    for (label, contents) in [
        ("corrupt", "broken"),
        (
            "version",
            r#"{"version":2,"list_id":"7","read_at":100,"members":[]}"#,
        ),
    ] {
        let scratch = Scratch::new(&format!("mirror-{label}"));
        std::fs::write(scratch.paths().sync_members_file(), contents).unwrap();
        let client = FakeApi::new()
            .members(vec![Ok(page(&[], None))])
            .following(vec![Ok(page(&[], None))]);
        plan_sync(scratch.paths(), &client, "me", "7", None, 100).unwrap();
        assert!(client.calls().contains(&Call::Members(None)));
        assert_eq!(saved_members(scratch.paths())["version"], 1);
    }
}

#[test]
fn partial_member_read_does_not_replace_the_old_mirror_or_read_following() {
    let scratch = Scratch::new("mirror-partial");
    // 別の list の台帳は使えないので､全件取得が始まる｡
    write_mirror(scratch.paths(), "8", 0);
    let original = saved_members(scratch.paths());
    let client = FakeApi::new().members(vec![
        Ok(page(&[("3", "carol")], Some("next"))),
        Err(anyhow::anyhow!("402")),
    ]);
    assert!(plan_sync(scratch.paths(), &client, "me", "7", None, 3_000_000).is_err());
    assert_eq!(saved_members(scratch.paths()), original);
    assert!(
        !client
            .calls()
            .iter()
            .any(|call| matches!(call, Call::Following(_)))
    );
}

#[test]
fn successful_writes_update_the_mirror_and_the_next_diff_is_empty() {
    let scratch = Scratch::new("mirror-applied");
    write_mirror(scratch.paths(), "7", 100);
    let mut plan = tests::plan_of("7", &["1"], &["2"]);
    let client = FakeApi::new().writes(vec![Ok(()), Ok(())]);
    let (sent, result) = apply_some(scratch.paths(), &client, &mut plan, true, 101, 5, GAP);
    result.unwrap();
    assert_eq!(sent, 2);
    let saved = saved_members(scratch.paths());
    assert_eq!(saved["read_at"], 100);
    assert_eq!(
        saved["members"],
        serde_json::json!([{"id":"1","username":"user1"}])
    );
    let client = FakeApi::new().following(vec![Ok(page(&[("1", "user1")], None))]);
    let next = plan_sync(scratch.paths(), &client, "me", "7", None, 102).unwrap();
    assert!(next.is_complete());
    assert_eq!(client.calls(), [Call::Following(None)]);
}

#[test]
fn rejected_or_failed_writes_leave_the_mirror_intact() {
    for (label, error) in [
        ("rejected", rejected("bad user")),
        ("limited", rate_limited(500, false)),
        ("payment", anyhow::anyhow!("HTTP 402")),
        ("server", anyhow::anyhow!("HTTP 503")),
    ] {
        let scratch = Scratch::new(&format!("mirror-write-{label}"));
        write_mirror(scratch.paths(), "7", 100);
        let original = saved_members(scratch.paths());
        let mut plan = tests::plan_of("7", &["1"], &[]);
        let client = FakeApi::new().writes(vec![Err(error)]);
        let (sent, _) = apply_some(scratch.paths(), &client, &mut plan, false, 101, 5, GAP);
        assert_eq!(sent, 0);
        assert_eq!(saved_members(scratch.paths()), original);
    }
}

#[test]
fn legacy_and_other_list_plans_do_not_create_or_change_a_mirror() {
    let scratch = Scratch::new("mirror-legacy");
    let mut plan = tests::plan_of("7", &["1"], &[]);
    let client = FakeApi::new().writes(vec![Ok(())]);
    apply_some(scratch.paths(), &client, &mut plan, false, 100, 5, GAP)
        .1
        .unwrap();
    assert!(!scratch.paths().sync_members_file().exists());
    write_mirror(scratch.paths(), "8", 100);
    let original = saved_members(scratch.paths());
    let mut plan = tests::plan_of("7", &["2"], &[]);
    let client = FakeApi::new().writes(vec![Ok(())]);
    apply_some(scratch.paths(), &client, &mut plan, false, 101, 5, GAP)
        .1
        .unwrap();
    assert_eq!(saved_members(scratch.paths()), original);
}

#[test]
fn a_mirror_save_failure_stops_after_recording_the_landed_write() {
    use std::os::unix::fs::PermissionsExt as _;

    let scratch = Scratch::new("mirror-save-fails");
    write_mirror(scratch.paths(), "7", 100);
    let path = scratch.paths().sync_members_file();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o444);
    std::fs::set_permissions(&path, permissions).unwrap();
    let mut plan = tests::plan_of("7", &["1", "3"], &[]);
    let client = FakeApi::new().writes(vec![Ok(()), Ok(())]);
    let (sent, result) = apply_some(scratch.paths(), &client, &mut plan, false, 101, 5, GAP);
    assert_eq!(sent, 1);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("sync_members.json")
    );
    assert_eq!(client.calls(), [Call::Add("1".to_string())]);
    let saved = load_plan(&scratch.paths().sync_plan_file())
        .unwrap()
        .unwrap();
    assert!(saved.entries[0].applied);
    assert!(!saved.entries[1].applied);
}

#[test]
fn adding_an_existing_member_does_not_duplicate_it() {
    let scratch = Scratch::new("mirror-duplicate");
    write_mirror(scratch.paths(), "7", 100);
    let mut plan = tests::plan_of("7", &["2"], &[]);
    let client = FakeApi::new().writes(vec![Ok(())]);
    apply_some(scratch.paths(), &client, &mut plan, false, 101, 5, GAP)
        .1
        .unwrap();
    assert_eq!(
        saved_members(scratch.paths())["members"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
