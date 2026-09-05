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
