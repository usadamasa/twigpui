//! 裸の 1 文字で timeline を読み進める (#148): `j` / `k` で選択を送り､
//! `l` / `r` で選択中の post に反応し､`n` で composer へ移る｡
//!
//! 鍵とアクションの対応は `menu.rs`､アクションを受ける `on_action` は
//! `layout.rs`､選択行の印は `post_row.rs`｡ここにあるのは選択そのものを
//! 動かすメソッドで､`ui/mod.rs` を太らせないために独立したファイルに
//! している｡
//!
//! 選択は index ではなく post id で持つ｡timeline は reload・source の
//! 切り替え・follow の流し込みで並びごと入れ替わるので､index を覚えると
//! 読み手の知らないうちに別の post を指す｡id なら､消えた post は
//! 「選択なし」として扱えばよい｡

#[cfg(test)]
mod tests {
    use crate::fixture::Fixture;
    use crate::ui::TimelineView;
    use crate::ui::tests::{
        draw_until_parked, fixture_window, fixture_with, item_with, laid_out, repost_row_item,
    };

    /// `keys` を打って 1 フレーム描く｡
    ///
    /// `scroll_to_item` は要求を積むだけで､実際に offset が動くのは次の
    /// prepaint だ (`div.rs` の `scroll_to_active_item`)｡描かずに続けて打つと
    /// `top_item` が古い bounds を答えるので､1 打ごとに描く｡
    fn press(
        visual: &mut gpui::VisualTestContext,
        cx: &mut gpui::TestAppContext,
        keys: &'static str,
    ) {
        visual.simulate_keystrokes(keys);
        draw_until_parked(visual, cx);
    }

    /// 今選ばれている post id｡
    fn selected_id(
        timeline: &gpui::Entity<TimelineView>,
        cx: &mut gpui::TestAppContext,
    ) -> Option<String> {
        cx.update(|cx| timeline.read(cx).selected.clone())
    }

    // --- #148: j / k で読み進める ---

    #[gpui::test]
    fn the_first_j_selects_the_row_at_the_top_of_the_viewport(cx: &mut gpui::TestAppContext) {
        // 「1 つ進む」ではなく「今そこにある行を選ぶ」｡選択が無い状態で
        // `j` を押した人は､目の前の行が選ばれることを期待する｡
        let (window, timeline) = fixture_window(cx, fixture_with(&["1", "2", "3"], &[]));
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        draw_until_parked(&mut visual, cx);

        assert_eq!(selected_id(&timeline, cx), None, "nothing is selected yet");

        press(&mut visual, cx, "j");
        assert_eq!(
            selected_id(&timeline, cx).as_deref(),
            Some("1"),
            "the first j takes the row the reader is already looking at"
        );
    }

    #[gpui::test]
    fn j_and_k_stop_at_both_ends(cx: &mut gpui::TestAppContext) {
        // 巻き戻さない｡末尾で `j` を押し続けた人が最新の post へ跳ばされる
        // のは事故にしかならない｡
        let (window, timeline) = fixture_window(cx, fixture_with(&["1", "2", "3"], &[]));
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        draw_until_parked(&mut visual, cx);

        press(&mut visual, cx, "j");
        press(&mut visual, cx, "j");
        press(&mut visual, cx, "j");
        assert_eq!(selected_id(&timeline, cx).as_deref(), Some("3"));

        press(&mut visual, cx, "j");
        assert_eq!(
            selected_id(&timeline, cx).as_deref(),
            Some("3"),
            "the last row is where j stops"
        );

        press(&mut visual, cx, "k");
        press(&mut visual, cx, "k");
        assert_eq!(selected_id(&timeline, cx).as_deref(), Some("1"));

        press(&mut visual, cx, "k");
        assert_eq!(
            selected_id(&timeline, cx).as_deref(),
            Some("1"),
            "the first row is where k stops"
        );
    }

    #[gpui::test]
    fn a_selection_that_left_the_list_counts_as_no_selection(cx: &mut gpui::TestAppContext) {
        // index ではなく id で持つ理由そのもの｡reload や source の切り替えで
        // 消えた post を指したままにするのではなく､次の `j` が目の前の行から
        // やり直す｡
        let (window, timeline) = fixture_window(cx, fixture_with(&["1", "2", "3"], &[]));
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        draw_until_parked(&mut visual, cx);

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                view.selected = Some("gone".to_string());
            });
        });
        draw_until_parked(&mut visual, cx);

        press(&mut visual, cx, "j");
        assert_eq!(
            selected_id(&timeline, cx).as_deref(),
            Some("1"),
            "a stale id sends j back to the top of the viewport"
        );
    }

    // --- #148: l / r は選択中の post に反応する ---

    #[gpui::test]
    fn l_and_r_act_on_the_original_post_of_a_repost_row(cx: &mut gpui::TestAppContext) {
        // #52: 行の id はリツイートのアクティビティのもので､書き込み系の
        // endpoint が対象にすべきなのは元の post だ｡
        //
        // scope を取り上げて拒否の経路で終わらせる｡`toggle_like` /
        // `toggle_repost` は scope が無いと override に理由を書いて戻る
        // ので､ネットワークを一切叩かずに「どの id に働いたか」だけが残る｡
        let fixture = Fixture {
            items: vec![repost_row_item("activity-id", "original-id", "alice")],
            ..fixture_with(&[], &[])
        };
        let (window, timeline) = fixture_window(cx, fixture);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                view.client = Some(crate::x_api::XClient::new("token".to_string()));
                view.oauth_scope = None;
            });
        });
        draw_until_parked(&mut visual, cx);

        press(&mut visual, cx, "j");
        press(&mut visual, cx, "l");
        press(&mut visual, cx, "r");

        cx.update(|cx| {
            let view = timeline.read(cx);
            assert!(
                view.like_overrides.contains_key("original-id"),
                "l likes the original post, not the retweet activity: {:?}",
                view.like_overrides.keys().collect::<Vec<_>>()
            );
            assert!(
                view.repost_overrides.contains_key("original-id"),
                "r reposts the original post: {:?}",
                view.repost_overrides.keys().collect::<Vec<_>>()
            );
            assert!(
                view.like_tasks.is_empty() && view.repost_tasks.is_empty(),
                "a refusal never reaches the network"
            );
        });
    }

    #[gpui::test]
    fn l_and_r_do_nothing_without_a_selection(cx: &mut gpui::TestAppContext) {
        // 選ばれていない post に反応する先は無い｡
        let (window, timeline) = fixture_window(cx, fixture_with(&["1"], &[]));
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                view.client = Some(crate::x_api::XClient::new("token".to_string()));
                view.oauth_scope = None;
            });
        });
        draw_until_parked(&mut visual, cx);

        press(&mut visual, cx, "l");
        press(&mut visual, cx, "r");

        cx.update(|cx| {
            let view = timeline.read(cx);
            assert!(view.like_overrides.is_empty(), "l needs a selection");
            assert!(view.repost_overrides.is_empty(), "r needs a selection");
        });
    }

    // --- #148: 文字を打っている間は発火しない ---

    #[gpui::test]
    fn the_bare_keys_type_into_a_focused_composer(cx: &mut gpui::TestAppContext) {
        // この issue の中心的な危うさ｡`Timeline && !Input` が働いていなければ
        // `j` は下書きに入らず選択を動かす｡
        let fixture = Fixture {
            items: vec![item_with("1", "alice", None), item_with("2", "alice", None)],
            ..fixture_with(&[], &[])
        };
        let (window, timeline) = fixture_window(cx, fixture);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                view.client = Some(crate::x_api::XClient::new("token".to_string()));
            });
        });
        draw_until_parked(&mut visual, cx);

        visual.update(|window, cx| {
            timeline.update(cx, |view, cx| {
                view.compose_input
                    .update(cx, |input, cx| input.focus(window, cx));
            });
        });
        draw_until_parked(&mut visual, cx);

        press(&mut visual, cx, "j k l r n");

        visual.update(|window, cx| {
            let view = timeline.read(cx);
            assert_eq!(
                view.compose.text(),
                "jklrn",
                "every bare key reaches the draft as a character"
            );
            assert_eq!(view.selected, None, "j and k never moved the selection");
            assert!(view.like_overrides.is_empty(), "l never fired");
            assert!(view.repost_overrides.is_empty(), "r never fired");
            assert!(
                gpui::Focusable::focus_handle(view.compose_input.read(cx), cx).is_focused(window),
                "n never took focus away from the composer"
            );
        });

        // `escape` で timeline へ戻れば､同じ `j` が選ぶ｡
        press(&mut visual, cx, "escape");
        press(&mut visual, cx, "j");
        assert_eq!(
            selected_id(&timeline, cx).as_deref(),
            Some("1"),
            "the same key selects once the composer lets focus go"
        );
    }

    #[gpui::test]
    fn n_moves_to_the_composer_without_touching_the_draft(cx: &mut gpui::TestAppContext) {
        // #14 の「下書きを決して失わない」は `n` にも掛かる｡
        let (window, timeline) = fixture_window(cx, fixture_with(&["1"], &[]));
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        draw_until_parked(&mut visual, cx);

        visual.update(|window, cx| {
            timeline.update(cx, |view, cx| {
                view.compose_input
                    .update(cx, |input, cx| input.set_value("a draft", window, cx));
            });
            window.dispatch_action(Box::new(crate::menu::BlurComposer), cx);
        });
        draw_until_parked(&mut visual, cx);

        press(&mut visual, cx, "n");

        visual.update(|window, cx| {
            let view = timeline.read(cx);
            assert!(
                gpui::Focusable::focus_handle(view.compose_input.read(cx), cx).is_focused(window),
                "n focuses the composer"
            );
            assert_eq!(
                view.compose.text(),
                "a draft",
                "n leaves the draft exactly as it was typed"
            );
        });
    }

    // --- #148: 選んだ行は画面に入る ---

    #[gpui::test]
    fn the_selected_row_is_pulled_into_the_viewport(cx: &mut gpui::TestAppContext) {
        // `debug_bounds` は画面の外に置かれた行にも答えるので､「収まって
        // いる」を assert できる｡
        let ids: Vec<String> = (1..=30).map(|n| n.to_string()).collect();
        let shown: Vec<&str> = ids.iter().map(String::as_str).collect();
        let (window, timeline) = fixture_window(cx, fixture_with(&shown, &[]));
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        draw_until_parked(&mut visual, cx);

        // 前提: 16 行目はまだ画面の下にある｡収まっていればこのテストは
        // 何も見ていない｡
        let viewport = laid_out(&mut visual, "timeline");
        let before = laid_out(&mut visual, "post-row-16");
        assert!(
            before.bottom() > viewport.bottom(),
            "the fixture has to be taller than the window: {before:?} in {viewport:?}"
        );

        for _ in 0..15 {
            press(&mut visual, cx, "j");
        }
        assert_eq!(selected_id(&timeline, cx).as_deref(), Some("16"));

        let after = laid_out(&mut visual, "post-row-16");
        assert!(
            after.top() >= viewport.top() && after.bottom() <= viewport.bottom(),
            "j brought the selected row into view: {after:?} in {viewport:?}"
        );

        // `k` で戻っても同じ｡
        for _ in 0..15 {
            press(&mut visual, cx, "k");
        }
        assert_eq!(selected_id(&timeline, cx).as_deref(), Some("1"));
        let first = laid_out(&mut visual, "post-row-1");
        assert!(
            first.top() >= viewport.top() && first.bottom() <= viewport.bottom(),
            "k brought it back: {first:?} in {viewport:?}"
        );
    }
}
