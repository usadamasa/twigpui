//! 複数 source を 1 本のレーンへ合成する (#43)｡
//!
//! source ごとのキャッシュファイルはそのまま (`cache::load_primary_timeline`)｡
//! 合成はここでメモリ上だけで行う: `cache::splice` を id 重複除去のために
//! fold するだけで、新しい dedupe/sort ロジックは要らない｡
//!
//! N-source の reload もここに置く。`auto_refresh.rs` (実装の天井 800 行に
//! 対して余裕 22 行) と `tasks/fetch.rs` (余裕 97 行) を太らせないためだ。

use std::collections::HashMap;

use anyhow::Result;

use super::source_picker;
use crate::cache::{self, MeEntry, Side, TimelineSource, splice};
use crate::paths::Paths;
use crate::x_api::{ListSummary, TimelineItem, XClient};

/// 複数 source のキャッシュ済み timeline を 1 本に混ぜる (#43)｡id で重複除去し
/// `created_at` 降順を保つ｡`splice` の fold は結果を変えない (ある段で切り
/// 捨てられた item は、その段で残った物より必ず古いので、後続の source が
/// 混ざっても全体の上位には入り得ない) — 順序も結果を左右しない｡
pub(super) fn compose(per_source: Vec<Vec<TimelineItem>>) -> Vec<TimelineItem> {
    per_source
        .into_iter()
        .fold(Vec::new(), |acc, items| splice(acc, items, Side::Ahead))
}

/// `load_composite_timeline` が返すもの: 合成した timeline と、投稿ごとの
/// 出自 (post id → 表示順で最初に載っていた source)｡
///
/// 出自は表示専用の派生値であり、削除の真実の情報源にはしない —
/// `TimelineView::confirm_delete` は `sources` を全部回して `forget_post` する
/// (`tasks/act.rs` を見よ)。複数 list に載っている post を出自だけで消すと、
/// 載っていないもう片方の list のキャッシュに残ったままになり、次の再合成で
/// 復活する。
pub(super) struct Composed {
    pub items: Vec<TimelineItem>,
    pub provenance: HashMap<String, TimelineSource>,
}

/// `sources` のキャッシュ済み timeline を読み、合成する (#43)｡1 つもキャッシュが
/// 無ければ空 (`Composed::items` が空、`Loading` ではなく「まだ何も無い」)｡
pub(super) fn load_composite_timeline(
    paths: &Paths,
    sources: &[TimelineSource],
    user_id: &str,
) -> Composed {
    let per_source = sources
        .iter()
        .map(|source| {
            let items = cache::load_primary_timeline(paths, source, user_id)
                .unwrap_or_else(|error| {
                    crate::log::warn(&format!("could not read the cached timeline: {error:#}"));
                    None
                })
                .unwrap_or_default();
            (source.clone(), items)
        })
        .collect();
    compose_with_provenance(per_source)
}

/// per-source の `(source, items)` の組から [`Composed`] を作る (#43)｡
/// ディスクには触れない — `load_composite_timeline` から核だけを切り出した
/// もので、fixture (`TimelineView::show_fixture`) がメモリ上のデータから
/// 同じ形を組むのにも使う。
pub(super) fn compose_with_provenance(
    per_source: Vec<(TimelineSource, Vec<TimelineItem>)>,
) -> Composed {
    let mut provenance = HashMap::new();
    for (source, items) in &per_source {
        for item in items {
            provenance
                .entry(item.id.clone())
                .or_insert_with(|| source.clone());
        }
    }
    let items = compose(per_source.into_iter().map(|(_, items)| items).collect());
    Composed { items, provenance }
}

/// `sources` の全キャッシュから `post_id` を消す
/// (`TimelineView::confirm_delete` から呼ぶ)｡出自の表示用 map は見ない —
/// 複数 source に同じ post が載っていても、表示中の source だけから消すと
/// 載っていない方に残ったままになり、次の再合成で復活する (#72 が名指しで
/// 潰した失敗と同じ形、opus-advisor A-1)。`cache::forget_post` はキャッシュ
/// ファイルが無い source にも post が入っていない source にも安全な no-op
/// なので、全部回すのが最短かつ正しい。ネットワークには触れない。
///
/// 再合成はここでは行わない (opus-advisor 指摘): 呼び出し側が spawn 時に
/// 捕獲した `sources` ではなく、`update` クロージャの中で完了時点の
/// `this.sources` を使って `load_composite_timeline` を呼ぶこと —
/// `reload_sources` の完了ハンドラ (opus-advisor A-4) と同じ理由で、削除が
/// 飛んでいる間にトグルされても古い集合でレーンを組み直さないようにする。
/// キャッシュから消す側は捕獲した `sources` のままでよい — 余分に回しても
/// 上のno-opの理由により安全。
pub(super) fn forget_post_everywhere(
    paths: &Paths,
    sources: &[TimelineSource],
    user_id: &str,
    post_id: &str,
    now: i64,
) -> Result<()> {
    for source in sources {
        cache::forget_post(paths, source, user_id, post_id, now)?;
    }
    Ok(())
}

/// `item_id` の出自ラベル (#43、`post_row.rs` から呼ぶ): `sources_len` が
/// 複数のときだけ、list 由来の post に list 名を返す｡Home にしか無い post や
/// 単一選択時は `None`｡`provenance` (`load_composite_timeline` が合成のたびに
/// 作り直す表示専用の派生値) を引くだけで、削除の真実の情報源にはしない —
/// `TimelineView::confirm_delete` はこれを見ない (`forget_post_everywhere`)。
pub(super) fn provenance_label(
    sources_len: usize,
    provenance: &HashMap<String, TimelineSource>,
    owned_lists: &[ListSummary],
    item_id: &str,
) -> Option<String> {
    if sources_len <= 1 {
        return None;
    }
    match provenance.get(item_id)? {
        TimelineSource::Home => None,
        TimelineSource::List(id) => Some(
            owned_lists
                .iter()
                .find(|list| &list.id == id)
                .map_or_else(|| id.clone(), source_picker::segment_label),
        ),
    }
}

/// `sources` のうち、まだ一度もキャッシュされていないものだけを返す (#43 の
/// 「on にする」規則、起動時にも使う)｡
pub(super) fn missing_sources(
    paths: &Paths,
    sources: &[TimelineSource],
    user_id: &str,
) -> Vec<TimelineSource> {
    sources
        .iter()
        .filter(|source| {
            cache::load_primary_timeline(paths, source, user_id)
                .ok()
                .flatten()
                .is_none()
        })
        .cloned()
        .collect()
}

/// N-source reload が使ったもの: 成功・失敗の本数、`sources.len() == 1` の
/// ときだけ意味を持つ `next_token`、解決した `/me`｡`me` は `Option` ではなく
/// `MeEntry` そのもの — `successes == 0` は下の `reload_all` が `Err` で
/// 弾くので、`Ok` に載る時点で必ず解決済みだからだ (opus-advisor 指摘)。
/// 呼び出し側に「成功したのに `None`」という到達しない分岐を書かせない。
pub(super) struct ReloadOutcome {
    pub successes: usize,
    pub failures: usize,
    pub next_token: Option<String>,
    pub me: MeEntry,
}

/// `sources` を順に reload する (#43、直列 — 並列化は ponytail の天井)｡
///
/// 部分失敗を許容する: 1 本でも成功すれば `Ok`、全滅なら最後のエラーで
/// `Err`｡`Endpoint::ListTimeline` は全 list id で 1 バケット共有なので、1 本が
/// レート制限に当たれば以降の list もローカルゲートで弾かれやすく、「取れた
/// 分を出す」がこの下で実質的な既定動作になる。
pub(super) fn reload_all(
    paths: &Paths,
    client: &XClient,
    sources: &[TimelineSource],
    max_results: u32,
    now: i64,
) -> Result<ReloadOutcome> {
    let mut successes: usize = 0;
    let mut failures: usize = 0;
    let mut next_token = None;
    let mut me = None;
    let mut last_error = None;
    for source in sources {
        match cache::reload_primary(paths, client, source, max_results, now) {
            Ok(reloaded) => {
                // `sources` は高々数十件 (所有リスト + Home) で、この値が
                // それを超えて溢れることは実質無い。溢れても失敗として
                // 扱う必要は無いので、上限で止まる `saturating_add`。
                successes = successes.saturating_add(1);
                next_token = reloaded.next_token;
                me = Some(reloaded.me);
                // `items` は使わない: 呼び出し側は完了時点の `sources` で
                // `load_composite_timeline` を通して読み直す (opus-advisor
                // A-4)。ここでのアキュムレータには使えない。
                let _ = reloaded.items;
            }
            Err(error) => {
                failures = failures.saturating_add(1);
                last_error = Some(error);
            }
        }
    }
    // `successes == 0` (`sources` が空の場合も含む — 呼び出し側が非空
    // invariant を守っていれば起きない) なら `me` は `None` のままなので、
    // ここで一緒に弾く。これで下の `Ok` に載る `me` は必ず解決済みになる。
    let Some(me) = me else {
        return Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no sources to reload")));
    };
    Ok(ReloadOutcome {
        successes,
        failures,
        next_token,
        me,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, created_at: &str) -> TimelineItem {
        TimelineItem {
            id: id.to_string(),
            text: String::new(),
            created_at: Some(created_at.to_string()),
            author_name: "someone".to_string(),
            author_username: "someone".to_string(),
            reposted_by: None,
            quoted: None,
            replied_to: None,
            metrics: None,
            links: Vec::new(),
            author_avatar_url: None,
            original_post_id: None,
            media: Vec::new(),
        }
    }

    #[test]
    fn duplicate_ids_across_sources_collapse_to_one() {
        let a = vec![item("1", "2026-01-01T00:00:02.000Z")];
        let b = vec![item("1", "2026-01-01T00:00:02.000Z")];
        let composed = compose(vec![a, b]);
        assert_eq!(composed.len(), 1);
    }

    #[test]
    fn composed_items_stay_newest_first_across_sources() {
        let older = vec![item("1", "2026-01-01T00:00:01.000Z")];
        let newer = vec![item("2", "2026-01-01T00:00:02.000Z")];
        let composed = compose(vec![older, newer]);
        assert_eq!(
            composed
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["2", "1"]
        );
    }

    #[test]
    fn an_empty_source_does_not_break_the_merge() {
        let composed = compose(vec![
            Vec::new(),
            vec![item("1", "2026-01-01T00:00:01.000Z")],
        ]);
        assert_eq!(composed.len(), 1);
    }

    #[test]
    fn no_sources_compose_to_nothing() {
        assert!(compose(Vec::new()).is_empty());
    }

    // --- provenance (出自、§3.8) ---

    #[test]
    fn provenance_names_the_first_source_a_post_was_found_in() {
        let root = temp_root("provenance-first");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let list_a = TimelineSource::List("1".to_string());
        let list_b = TimelineSource::List("2".to_string());
        // 同じ id を両方の source のキャッシュに書く｡
        cache::save_primary_timeline(
            &paths,
            &list_a,
            "me",
            &[item("1", "2026-01-01T00:00:02.000Z")],
            0,
        )
        .unwrap();
        cache::save_primary_timeline(
            &paths,
            &list_b,
            "me",
            &[item("1", "2026-01-01T00:00:02.000Z")],
            0,
        )
        .unwrap();

        let composed = load_composite_timeline(&paths, &[list_a.clone(), list_b], "me");
        assert_eq!(composed.provenance.get("1"), Some(&list_a));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_source_with_no_cache_names_nothing() {
        let root = temp_root("provenance-empty");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let composed = load_composite_timeline(&paths, &[TimelineSource::Home], "me");
        assert!(composed.items.is_empty());
        assert!(composed.provenance.is_empty());

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- forget_post_everywhere (opus-advisor A-1) ---

    #[test]
    fn deleting_a_post_present_in_two_sources_removes_it_from_both() {
        let root = temp_root("forget-everywhere");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let list_a = TimelineSource::List("1".to_string());
        let list_b = TimelineSource::List("2".to_string());
        cache::save_primary_timeline(
            &paths,
            &list_a,
            "me",
            &[
                item("1", "2026-01-01T00:00:02.000Z"),
                item("2", "2026-01-01T00:00:01.000Z"),
            ],
            0,
        )
        .unwrap();
        cache::save_primary_timeline(
            &paths,
            &list_b,
            "me",
            &[item("1", "2026-01-01T00:00:02.000Z")],
            0,
        )
        .unwrap();

        forget_post_everywhere(&paths, &[list_a.clone(), list_b.clone()], "me", "1", 1).unwrap();

        // 消えた post は再合成の結果に残らない (呼び出し側と同じく、削除の
        // 後で改めて load_composite_timeline を呼ぶ — opus-advisor 指摘)。
        let composed = load_composite_timeline(&paths, &[list_a.clone(), list_b.clone()], "me");
        assert_eq!(
            composed
                .items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["2"]
        );
        // 個別のキャッシュファイルからも消えている — 表示していない方の
        // source に残ったままだと、次にそちらを on にしたときに復活する
        // (opus-advisor A-1)。
        let remaining_a = cache::load_primary_timeline(&paths, &list_a, "me")
            .unwrap()
            .unwrap();
        assert!(!remaining_a.iter().any(|item| item.id == "1"));
        let remaining_b = cache::load_primary_timeline(&paths, &list_b, "me")
            .unwrap()
            .unwrap();
        assert!(!remaining_b.iter().any(|item| item.id == "1"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- missing_sources (#43 の「on にする」規則) ---

    #[test]
    fn a_cached_source_is_not_missing() {
        let root = temp_root("missing-hit");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let list = TimelineSource::List("1".to_string());
        cache::save_primary_timeline(
            &paths,
            &list,
            "me",
            &[item("1", "2026-01-01T00:00:01.000Z")],
            0,
        )
        .unwrap();

        assert_eq!(missing_sources(&paths, &[list], "me"), Vec::new());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_uncached_source_is_missing() {
        let root = temp_root("missing-miss");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let list = TimelineSource::List("1".to_string());
        assert_eq!(
            missing_sources(&paths, std::slice::from_ref(&list), "me"),
            vec![list]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    fn test_paths(root: &std::path::Path) -> Paths {
        let home = root.display().to_string();
        Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap()
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("twigpui-test-lane-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }
}
