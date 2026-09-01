//! post の写真を開く別ウィンドウ (#188)｡
//!
//! サムネイルはクリックしてもブラウザへ飛ばす先しか持っていなかった｡
//! X のページを開くのは､画像を見たいだけの読み手には遠回りになる｡ここは
//! 手元に落ちている画像をそのまま出すだけのウィンドウで､`←` / `→` で
//! 同じ post の写真のあいだを動く｡
//!
//! `TimelineView` からは `media_paths` と `media_failed` を毎 render で
//! 読む｡開いた後に画像が届いても描き直るようにするためで､viewer 自身は
//! ダウンロードを始めない｡

#[cfg(test)]
mod tests {
    use crate::ui::TimelineState;
    use crate::ui::tests::{draw_until_parked, fixture_window, fixture_with, laid_out};
    use crate::x_api::PostMedia;

    /// `open` へ渡す 1 枚｡
    fn photo(url: &str, width: u32, height: u32) -> PostMedia {
        PostMedia {
            url: url.to_string(),
            kind: Some("photo".to_string()),
            width: Some(width),
            height: Some(height),
            alt_text: None,
        }
    }

    /// いま開いているウィンドウの数｡
    fn window_count(cx: &mut gpui::TestAppContext) -> usize {
        cx.update(|cx| cx.windows().len())
    }

    /// 開いている viewer のウィンドウ｡timeline の root は
    /// `gpui_component::Root` なので､`downcast` で取り違えることは無い｡
    fn viewer_window(cx: &mut gpui::TestAppContext) -> gpui::WindowHandle<super::ImageViewer> {
        cx.update(|cx| {
            cx.windows()
                .into_iter()
                .find_map(|window| window.downcast::<super::ImageViewer>())
        })
        .expect("the viewer window has to be open")
    }

    /// viewer がいま見せている写真の位置｡
    fn viewer_index(
        viewer: gpui::WindowHandle<super::ImageViewer>,
        cx: &mut gpui::TestAppContext,
    ) -> usize {
        cx.update(|cx| viewer.read(cx).expect("the viewer is open").index)
    }

    /// viewer が画像の代わりに出している文言｡画像があれば `None`｡
    fn viewer_notice(
        viewer: gpui::WindowHandle<super::ImageViewer>,
        cx: &mut gpui::TestAppContext,
    ) -> Option<gpui::SharedString> {
        cx.update(|cx| viewer.read(cx).expect("the viewer is open").notice(cx))
    }

    /// #188: 写真を開くとウィンドウが 1 枚増える｡渡す写真が無ければ増え
    /// ない — 空のウィンドウは何の役にも立たない｡
    #[gpui::test]
    fn opening_a_photo_adds_a_window(cx: &mut gpui::TestAppContext) {
        let (_window, timeline) = fixture_window(cx, fixture_with(&["1"], &[]));
        assert_eq!(window_count(cx), 1, "the timeline is the only window");

        cx.update(|cx| super::open(timeline.clone(), Vec::new(), 0, cx));
        assert_eq!(window_count(cx), 1, "no photo means no window");

        cx.update(|cx| {
            super::open(
                timeline.clone(),
                vec![photo("media/a.png", 800, 600)],
                0,
                cx,
            );
        });
        assert_eq!(window_count(cx), 2, "a photo opens a window of its own");
    }

    /// #188: `→` / `←` は 1 枚ずつ動き､端で止まる｡巻き戻らないのは､
    /// 3 枚目の次が 1 枚目に戻ると何枚目を見ているのか分からなくなるからだ｡
    #[gpui::test]
    fn the_arrows_move_between_photos_and_stop_at_the_ends(cx: &mut gpui::TestAppContext) {
        let (_window, timeline) = fixture_window(cx, fixture_with(&["1"], &[]));
        let photos = vec![
            photo("media/a.png", 800, 600),
            photo("media/b.png", 800, 600),
            photo("media/c.png", 800, 600),
        ];
        cx.update(|cx| super::open(timeline, photos, 0, cx));
        let viewer = viewer_window(cx);
        let mut visual = gpui::VisualTestContext::from_window(viewer.into(), cx);
        draw_until_parked(&mut visual, cx);
        assert_eq!(viewer_index(viewer, cx), 0, "it opens at the photo clicked");

        for expected in [1, 2, 2] {
            visual.update(|window, cx| window.dispatch_action(Box::new(super::NextPhoto), cx));
            draw_until_parked(&mut visual, cx);
            assert_eq!(
                viewer_index(viewer, cx),
                expected,
                "the right arrow walks forward and stops at the last photo"
            );
        }
        for expected in [1, 0, 0] {
            visual.update(|window, cx| window.dispatch_action(Box::new(super::PreviousPhoto), cx));
            draw_until_parked(&mut visual, cx);
            assert_eq!(
                viewer_index(viewer, cx),
                expected,
                "the left arrow walks back and stops at the first photo"
            );
        }
    }

    /// #188: viewer を閉じても timeline は何も失わない｡`cmd-w` は
    /// timeline のウィンドウでは (それが最後の 1 枚なので) アプリを終える
    /// 鍵で､viewer ではその手前の 1 枚だけを閉じる｡
    #[gpui::test]
    fn closing_the_viewer_leaves_the_timeline_alone(cx: &mut gpui::TestAppContext) {
        let (_window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &[]));
        let scrolled = cx.update(|cx| f32::from(timeline.read(cx).list_scroll.offset().y));

        cx.update(|cx| {
            super::open(
                timeline.clone(),
                vec![photo("media/a.png", 800, 600)],
                0,
                cx,
            );
        });
        assert_eq!(window_count(cx), 2, "the viewer opened");

        let viewer = viewer_window(cx);
        let mut visual = gpui::VisualTestContext::from_window(viewer.into(), cx);
        draw_until_parked(&mut visual, cx);
        visual.update(|window, cx| window.dispatch_action(Box::new(crate::menu::CloseWindow), cx));
        cx.run_until_parked();

        assert_eq!(window_count(cx), 1, "only the timeline is left");
        cx.update(|cx| {
            let view = timeline.read(cx);
            assert!(
                matches!(view.state, TimelineState::Loaded(_)),
                "the timeline still holds the posts it had"
            );
            let after = f32::from(view.list_scroll.offset().y);
            assert!(
                (after - scrolled).abs() < 1.0,
                "the timeline did not scroll: {after} vs {scrolled}"
            );
        });
    }

    /// #188: 画像はウィンドウからはみ出さない — 横長も縦長も｡
    ///
    /// `object_fit` を外すか幅か高さの片方だけを与えると､縦長の画像が
    /// 枠を突き抜ける (#256 が timeline で踏んだのと同じ罠)｡
    #[gpui::test]
    fn the_photo_stays_inside_the_window(cx: &mut gpui::TestAppContext) {
        let arrived = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/AppIcon.png");
        let (_window, timeline) = fixture_window(cx, fixture_with(&["1"], &[]));
        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                view.media_paths
                    .insert("media/wide.png".to_string(), arrived.clone());
                view.media_paths
                    .insert("media/tall.png".to_string(), arrived.clone());
            });
        });
        let photos = vec![
            photo("media/wide.png", 1600, 400),
            photo("media/tall.png", 400, 1600),
        ];
        cx.update(|cx| super::open(timeline, photos, 0, cx));
        let viewer = viewer_window(cx);
        let mut visual = gpui::VisualTestContext::from_window(viewer.into(), cx);

        // 2 回描く: 1 回目が画像のデコードを background に頼み､2 回目で
        // gpui が画像の縦横比を layout に持ち込む (#256 の `a_lone_photo_…`
        // と同じ理由)｡
        for _ in 0..2 {
            draw_until_parked(&mut visual, cx);
        }
        assert_photo_fits(&mut visual, "a landscape photo");

        visual.update(|window, cx| window.dispatch_action(Box::new(super::NextPhoto), cx));
        for _ in 0..2 {
            draw_until_parked(&mut visual, cx);
        }
        assert_photo_fits(&mut visual, "a portrait photo");
    }

    /// 画像が viewer の枠に収まっていること｡`what` は失敗の文言に混ぜる｡
    fn assert_photo_fits(visual: &mut gpui::VisualTestContext, what: &str) {
        let root = laid_out(visual, "image-viewer");
        let image = laid_out(visual, "image-viewer-image");
        // layout の丸めのぶんだけ緩める｡
        let slack = 1.0;
        assert!(
            f32::from(image.left()) >= f32::from(root.left()) - slack
                && f32::from(image.right()) <= f32::from(root.right()) + slack
                && f32::from(image.top()) >= f32::from(root.top()) - slack
                && f32::from(image.bottom()) <= f32::from(root.bottom()) + slack,
            "{what} stays inside the window: {:?} in {:?}",
            image,
            root
        );
    }

    /// #188: 画像がまだ無いときは､なぜ無いのかを出す｡取りそこねたものは
    /// 待っても着かないので､「読み込み中」と言い続けてはいけない｡
    #[gpui::test]
    fn a_photo_without_a_file_says_why(cx: &mut gpui::TestAppContext) {
        let (_window, timeline) = fixture_window(cx, fixture_with(&["1"], &[]));
        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                view.media_failed.insert("media/failed.png".to_string());
            });
        });
        let photos = vec![
            photo("media/failed.png", 800, 600),
            photo("media/waiting.png", 800, 600),
        ];
        cx.update(|cx| super::open(timeline, photos, 0, cx));
        let viewer = viewer_window(cx);
        let mut visual = gpui::VisualTestContext::from_window(viewer.into(), cx);
        draw_until_parked(&mut visual, cx);

        assert!(
            visual.debug_bounds("image-viewer-notice").is_some(),
            "a photo that failed shows a notice instead of an image"
        );
        assert_eq!(
            viewer_notice(viewer, cx),
            Some("Could not load this image".into()),
            "a url in media_failed says the download failed"
        );

        visual.update(|window, cx| window.dispatch_action(Box::new(super::NextPhoto), cx));
        draw_until_parked(&mut visual, cx);
        assert!(
            visual.debug_bounds("image-viewer-notice").is_some(),
            "a photo that has not arrived shows a notice too"
        );
        assert_eq!(
            viewer_notice(viewer, cx),
            Some("Loading…".into()),
            "a url in neither set is still on its way"
        );
    }

    /// #188: 開く大きさ｡API の寸法があれば画面に収まるまで縮め､無ければ
    /// (画面が分からないときも) 決め打ちの大きさで開く｡
    #[test]
    fn the_window_opens_at_a_size_that_fits_the_screen() {
        let wide = photo("media/wide.png", 1600, 400);
        let display = gpui::size(gpui::px(1000.0), gpui::px(1000.0));
        let fitted = super::initial_size(Some(&wide), Some(display));
        assert!(
            (f32::from(fitted.width) - 900.0).abs() < 1.0
                && (f32::from(fitted.height) - 225.0).abs() < 1.0,
            "a photo wider than the screen shrinks and keeps its aspect: {fitted:?}"
        );

        let small = photo("media/small.png", 320, 240);
        let kept = super::initial_size(Some(&small), Some(display));
        assert!(
            (f32::from(kept.width) - 320.0).abs() < 1.0
                && (f32::from(kept.height) - 240.0).abs() < 1.0,
            "a photo that already fits opens at its own size: {kept:?}"
        );

        let sizeless = PostMedia {
            width: None,
            height: None,
            ..photo("media/unknown.png", 0, 0)
        };
        for (photo, display, what) in [
            (Some(&sizeless), Some(display), "a photo without a size"),
            (Some(&wide), None, "a screen we cannot measure"),
            (None, Some(display), "no photo at all"),
        ] {
            let fallback = super::initial_size(photo, display);
            assert!(
                (f32::from(fallback.width) - 800.0).abs() < 1.0
                    && (f32::from(fallback.height) - 600.0).abs() < 1.0,
                "{what} falls back to 800x600: {fallback:?}"
            );
        }
    }
}
