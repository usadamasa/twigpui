//! メニューバーの `Sources` メニューへ並べる項目の組み立て (#282)。
//! `source_picker.rs` から切り出した — あちらは状態の読み書き
//! (`toggle_source` 等) を持ち、こちらは純粋な組み立てだけを持つ。
//!
//! #192/#43 まではここがツールバーの pull-down トリガーとドロップダウン
//! (どちらも gpui の `div` で描いていた) を持っていたが、#282 でメニュー
//! バーへ移った。gpui の `MenuItem` はウィンドウの要素ではなく OS の
//! ネイティブメニューなので、テストからクリックを合成する手段はもう無い
//! — ここのテストはウィンドウを一切組まず、返ってきた `Vec<MenuItem>` を
//! そのまま検査する。

use super::source_picker::{Selection, lists_button_label, segments};
use crate::cache::TimelineSource;
use crate::menu::{LoadOwnedLists, ToggleSource};
use crate::x_api::ListSummary;

/// メニューバーの `Sources` メニューへ並べる項目 (#282)｡[`segments`] が
/// 描画順を決め､ここはそれを `gpui::MenuItem` へ写すだけの純粋関数だ —
/// ウィンドウもクリックも要らないので、テストは [`TimelineView`] を組まずに
/// 直接呼べる｡
///
/// 選択の印はラベル前置の `"✓ "`｡macOS のメニューへチェック状態そのものは
/// 渡せない (`menu::menus` の doc を見よ) ので、これしか手が無い｡
///
/// `offers_fetch` が真なら末尾に区切りと取得ボタンを足す — 偽なら (fixture
/// のウィンドウ、`/me` 未解決など) 項目ごと出さない: gpui の `MenuItem` に
/// disabled が無いので、押しても何もしないボタンを見せるより無い方がよい｡
/// 取得中でもラベルは `lists_button_label` の値段付きのまま — 2 度目の
/// クリックは [`TimelineView::fetch_owned_lists`] が `lists_fetch.is_some()`
/// で早期 return するので無害だが、値段を隠す理由には決してならない
/// (`x-api-budget`)｡
///
/// [`TimelineView`]: super::TimelineView
/// [`TimelineView::fetch_owned_lists`]: super::TimelineView::fetch_owned_lists
pub(super) fn source_menu_items(
    sources: &[TimelineSource],
    owned: &[ListSummary],
    offers_fetch: bool,
    fetching: bool,
) -> Vec<gpui::MenuItem> {
    let mut items: Vec<gpui::MenuItem> = segments(sources, owned)
        .into_iter()
        .map(|segment| {
            let label = if segment.selected {
                format!("✓ {}", segment.label)
            } else {
                segment.label
            };
            gpui::MenuItem::action(
                label,
                ToggleSource {
                    selection: Selection::of(&segment.source),
                },
            )
        })
        .collect();
    if offers_fetch {
        items.push(gpui::MenuItem::separator());
        items.push(gpui::MenuItem::action(
            lists_button_label(!owned.is_empty(), fetching),
            LoadOwnedLists,
        ));
    }
    items
}

#[cfg(test)]
mod tests {
    use gpui::MenuItem;

    use super::source_menu_items;
    use crate::cache::TimelineSource;
    use crate::menu::ToggleSource;
    use crate::ui::source_picker::Selection;
    use crate::x_api::ListSummary;

    fn list(id: &str, name: &str) -> ListSummary {
        ListSummary {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    fn selection_of(item: &MenuItem) -> Option<Selection> {
        match item {
            MenuItem::Action { action, .. } => action
                .as_any()
                .downcast_ref::<ToggleSource>()
                .map(|toggle| toggle.selection.clone()),
            _ => None,
        }
    }

    fn label_of(item: &MenuItem) -> Option<String> {
        match item {
            MenuItem::Action { name, .. } => Some(name.to_string()),
            _ => None,
        }
    }

    #[test]
    fn the_sources_menu_marks_the_shown_timelines() {
        let current = [TimelineSource::Home, TimelineSource::List("1".to_string())];
        let owned = [list("1", "rust"), list("2", "art")];
        let items = source_menu_items(&current, &owned, false, false);

        assert_eq!(selection_of(&items[0]), Some(Selection::Home));
        assert_eq!(
            selection_of(&items[1]),
            Some(Selection::List {
                id: "1".to_string()
            })
        );
        let labels: Vec<Option<String>> = items.iter().map(label_of).collect();
        assert_eq!(
            labels,
            vec![
                Some("✓ Home".to_string()),
                Some("✓ rust".to_string()),
                Some("art".to_string()),
            ],
            "only the shown timelines carry the ✓ prefix"
        );
    }

    #[test]
    fn the_sources_menu_names_the_price_of_naming_the_lists() {
        let current = [TimelineSource::Home];
        let owned = [list("1", "rust")];

        let offered = source_menu_items(&current, &owned, true, false);
        assert_eq!(
            label_of(offered.last().expect("the fetch item")).as_deref(),
            Some("Refresh lists (1 request)"),
            "the price stays on the label even while idle"
        );
        assert!(
            matches!(offered.get(offered.len() - 2), Some(MenuItem::Separator)),
            "a separator sets the fetch item apart from the segments"
        );

        let not_offered = source_menu_items(&current, &owned, false, false);
        assert_eq!(
            not_offered.len(),
            offered.len() - 2,
            "no fetch means no separator either — there is nothing to set apart"
        );
    }

    #[test]
    fn every_list_reaches_the_menu() {
        let owned: Vec<ListSummary> = (1..=13)
            .map(|n| list(&n.to_string(), &format!("list {n}")))
            .collect();
        let current = [TimelineSource::Home];
        let items = source_menu_items(&current, &owned, false, false);
        assert_eq!(items.len(), 14, "Home plus all 13 lists");
    }
}
