//! timeline の一覧の仮想化 (#301)｡

#[cfg(test)]
mod tests {
    use crate::ui::tests::{draw_until_parked, fixture_window, fixture_with};

    /// #301: 500 行を持っていても組むのは viewport に入る行だけ｡
    ///
    /// `debug_bounds` は一度描いた名前を消せないが､一度も描いていない名前
    /// には `None` を答える — 末尾の行が `None` なら､その行の要素は組まれて
    /// いない｡先頭の行は同じ frame で組まれているので､「何も描いていない」
    /// を「組まなかった」と読み違えることはない｡
    #[gpui::test]
    fn only_the_rows_in_the_viewport_are_built(cx: &mut gpui::TestAppContext) {
        let ids: Vec<String> = (1..=500).map(|n| n.to_string()).collect();
        let shown: Vec<&str> = ids.iter().map(String::as_str).collect();
        let (window, _timeline) = fixture_window(cx, fixture_with(&shown, &[]));
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        draw_until_parked(&mut visual, cx);

        assert!(
            visual.debug_bounds("post-row-1").is_some(),
            "the first row is on screen and has to be laid out"
        );
        assert!(
            visual.debug_bounds("post-row-500").is_none(),
            "the last row is far below the viewport and must not be built"
        );
    }
}
