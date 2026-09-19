//! source picker のツールバー側の見た目 (#156): 閉じたトリガーと開いた
//! ドロップダウン本体｡`source_picker.rs` から切り出した — あちらが
//! サイズの天井に達したので､状態の読み書き (`toggle_source` 等) とは
//! 別に､ここは描画だけを持つ｡純粋な移動で振る舞いは変えていない｡

use gpui::{Context, InteractiveElement as _, IntoElement as _, ParentElement as _, Styled as _};
use gpui::{StatefulInteractiveElement as _, anchored, deferred, point, px, rgb, rgba};

use super::render::{Addressable as _, tab_segment};
use super::source_picker::{
    SourcePickerVisibility, lists_button_label, offers_list_fetch, segments, trigger_label,
};
use super::{AnyElement, TimelineView, div, theme};

impl TimelineView {
    /// ツールバーの pull-down トリガー (#192, #43)｡ラベルは [`trigger_label`]
    /// (1 件ならその名前､複数なら先頭の名前 + `+N`)｡クリックでドロップ
    /// ダウンの開閉をトグルするだけで､選択そのものはメニュー側の項目が担う｡
    pub(super) fn source_picker_trigger(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = self.theme;
        let label = format!("{} ⌄", trigger_label(&self.sources, &self.owned_lists));
        tab_segment(&label, true, theme)
            .addressable("source-picker")
            .max_w(px(160.0))
            .truncate()
            .cursor_pointer()
            // #156: `tab_segment(.., selected: true, ..)` の `bg(theme.bg)`
            // は不透明なので､`hover()` の置き換えではなく `blend` で合成後の
            // 色をその場で作る — `tab_segment` 自身はメニュー項目とも
            // 共有するのでここでは付けない｡
            .hover(|style| style.bg(rgb(theme.bg).blend(rgba(theme.control_hover_overlay))))
            .active(|style| style.bg(rgb(theme.bg).blend(rgba(theme.control_pressed_overlay))))
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.source_picker_open = this.source_picker_open.toggled();
                cx.notify();
            }))
            .into_any_element()
    }

    /// ドロップダウンのメニュー本体 (#192, #43)｡開いていなければ `None`｡
    ///
    /// `anchored()` + `deferred()` でツールバーの `overflow_hidden` の外へ
    /// 描画する (`sync_row.rs::sync_dialog` の `absolute()` + `inset_0()` は
    /// 全画面中央のモーダル向けで､ここには使わない — トリガー直下に
    /// 左詰めで出す)｡`on_mouse_down_out` で外側クリックを検知して閉じる｡
    /// Escape も閉じる経路の一つだが､それは `layout.rs` の `BlurComposer`
    /// ハンドラが既存の escape バインディングへ相乗りして担う｡
    ///
    /// 項目クリックではメニューを閉じない: チェックを
    /// 複数付け外しする操作なので､1 回ごとに閉じると #43 の「任意の
    /// タイミングでオン・オフ」が面倒になる｡macOS のメニューは選択で
    /// 閉じるのが標準だが､ここは意図的に逸脱する｡
    pub(super) fn source_picker_menu(
        &self,
        bg_alpha: u8,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        if !self.source_picker_open.is_open() {
            return None;
        }
        let theme = self.theme;
        let mut menu = div()
            .addressable("source-menu")
            .w(px(220.0))
            .flex()
            .flex_col()
            // #267: 本体と同じ不透明度で — 帯だけ不透明に残さない｡
            .bg(rgba(theme::with_alpha(theme.bg_header, bg_alpha)))
            .border_1()
            .border_color(rgb(theme.border))
            .rounded(theme::RADIUS_MENU)
            // #156: 項目は全幅なので､hover の塗りが角の 8px の外へ
            // はみ出さないよう切る｡`shadow_md` は外側なので消えない｡
            .overflow_hidden()
            .shadow_md()
            .on_mouse_down_out(cx.listener(|this, _event, _window, cx| {
                this.source_picker_open = SourcePickerVisibility::Closed;
                cx.notify();
            }));

        for segment in segments(&self.sources, &self.owned_lists) {
            let source = segment.source;
            let mark = if segment.selected { "✓" } else { "" };
            menu = menu.child(
                div()
                    .addressable(segment.name)
                    .flex()
                    .items_center()
                    .h(px(24.0))
                    .px_2()
                    .gap_2()
                    .text_size(theme::TEXT_BODY)
                    .cursor_pointer()
                    // #156: macOS のメニューは hover した項目を accent
                    // で塗り文字を白くする｡下地は menu 自身の `bg_header`
                    // で不透明だが､accent で完全に置き換わればよいだけなので
                    // `blend` は要らない｡
                    .hover(|style| {
                        style
                            .bg(rgb(theme.accent))
                            .text_color(rgb(theme.button_label))
                    })
                    .child(div().w(px(20.0)).child(mark))
                    .child(div().min_w(px(0.0)).truncate().child(segment.label))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.toggle_source(&source, cx);
                    })),
            );
        }

        if let Some(control) = self.lists_control(cx) {
            menu = menu
                .child(div().h(px(1.0)).bg(rgb(theme.border)))
                .child(div().px_2().py_1().child(control));
        }

        Some(
            deferred(
                anchored()
                    .position(point(theme::ROW_PAD_X, theme::TOOLBAR_HEIGHT))
                    .child(menu),
            )
            .into_any_element(),
        )
    }

    /// list の名前を取得するボタン (#164)｡取得する手立てが無いときは
    /// `None` — [`offers_list_fetch`] を参照｡取得が飛んでいる間はただの
    /// テキストになる: 2 度目のクリックは同じページを 2 回買うだけだ｡
    /// メニュー末尾に移した (#192): ツールバーの閉じた
    /// トリガーは固定幅なので､ここに居ては閉じた状態の幅を食うだけだった｡
    ///
    /// #282: メニューバーへの移行後は [`source_menu_items`] がこの役目を
    /// 引き継ぐ｡この関数と呼び出し元 ([`Self::source_picker_menu`]) は
    /// ツールバーのドロップダウンと一緒に消える｡
    pub(super) fn lists_control(&self, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        if !offers_list_fetch(self.client.is_some(), self.home_user_id.is_some()) {
            return None;
        }
        let fetching = self.lists_fetch.is_some();
        let theme = self.theme;
        let control = div()
            .text_size(theme::TEXT_META)
            .text_color(rgb(theme.text_muted))
            .child(lists_button_label(!self.owned_lists.is_empty(), fetching));
        if fetching {
            return Some(control.into_any_element());
        }
        Some(
            control
                .addressable("load-lists")
                .px_1()
                .rounded(theme::RADIUS_CONTROL)
                .cursor_pointer()
                // #156: メニュー項目 (`segment.name`) と同じ hover —
                // 取得中は addressable ですらないテキストになるので､
                // クリックできるこの枝にだけ付ける｡
                .hover(|style| {
                    style
                        .bg(rgb(theme.accent))
                        .text_color(rgb(theme.button_label))
                })
                .on_click(cx.listener(|this, _event, _window, cx| this.fetch_owned_lists(cx)))
                .into_any_element(),
        )
    }
}

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
pub(super) fn source_menu_items(
    sources: &[crate::cache::TimelineSource],
    owned: &[crate::x_api::ListSummary],
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
                crate::menu::ToggleSource {
                    selection: super::source_picker::Selection::of(&segment.source),
                },
            )
        })
        .collect();
    if offers_fetch {
        items.push(gpui::MenuItem::separator());
        items.push(gpui::MenuItem::action(
            lists_button_label(!owned.is_empty(), fetching),
            crate::menu::LoadOwnedLists,
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
