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

use std::path::PathBuf;

use gpui::{
    App, Bounds, Context, Entity, FocusHandle, ObjectFit, Pixels, SharedString, Size, Subscription,
    TitlebarOptions, Window, WindowBounds, WindowOptions, div, img, prelude::*, px, rgb, size,
};

use super::TimelineView;
use super::render::Addressable as _;
use crate::log;
use crate::menu::CloseWindow;
use crate::theme;
use crate::x_api::PostMedia;

gpui::actions!(
    image_viewer,
    [
        /// 同じ post の次の写真を見せる (#188)｡`→` に割り当て｡
        NextPhoto,
        /// 同じ post の前の写真を見せる (#188)｡`←` に割り当て｡
        PreviousPhoto,
    ]
);

/// viewer の root 要素が担うキーコンテキスト (#188)｡
///
/// 裸のキー (`←` / `→`) を bind するので､timeline の [`crate::menu::KEY_CONTEXT`]
/// とは別にしてある｡timeline 側で `→` が発火すると､composer に文字を打って
/// いる読み手の下でウィンドウが動くことになる｡
const KEY_CONTEXT: &str = "ImageViewer";

/// 画面か写真の寸法が分からないときに開く大きさ (#188)｡
const FALLBACK_WIDTH: f32 = 800.0;

/// [`FALLBACK_WIDTH`] の対｡
const FALLBACK_HEIGHT: f32 = 600.0;

/// 写真に使ってよい画面の割合 (#188)｡原寸のまま開くと写真が画面より大きい
/// ときにタイトルバーごと外へ出るので､余白を残す｡
const SCREEN_FRACTION: f32 = 0.9;

/// viewer のキーバインドを登録する (#188)｡`main` が `menu::init` の隣で
/// 一度だけ呼ぶ — これらへ dispatch するウィンドウが存在する前に｡
///
/// [`crate::menu::ALL_SHORTCUTS`] には入れない｡あれはメニューバーに出る
/// ショートカットの一覧で､裸のキーを禁じている (timeline の文脈では正しい)｡
/// ここの `←` / `→` はメニューには出ないし､[`KEY_CONTEXT`] の中でしか
/// 生きていない｡
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        gpui::KeyBinding::new("right", NextPhoto, Some(KEY_CONTEXT)),
        gpui::KeyBinding::new("left", PreviousPhoto, Some(KEY_CONTEXT)),
        // 閉じ方は timeline と同じアクションを使う (#188)｡`cmd-w` に加えて
        // `escape` も受けるのは､これが読むためだけに開いた覆いだからだ｡
        gpui::KeyBinding::new("cmd-w", CloseWindow, Some(KEY_CONTEXT)),
        gpui::KeyBinding::new("escape", CloseWindow, Some(KEY_CONTEXT)),
    ]);
}

/// `photos[index]` から開く｡photos が空なら何もしない (#188)｡
///
/// ウィンドウを開けなかったことは記録するだけで､呼び出し元へは返さない｡
/// 写真を出せなかったのは timeline が読めなくなる理由ではない｡
pub(in crate::ui) fn open(
    timeline: Entity<TimelineView>,
    photos: Vec<PostMedia>,
    index: usize,
    cx: &mut App,
) {
    let Some(photo) = photos.get(index) else {
        return;
    };
    let display = cx.primary_display().map(|display| display.bounds().size);
    let bounds = Bounds::centered(None, initial_size(Some(photo), display), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            // URL もパスもクエリも出さない (#188)｡タイトルバーは肩越しに
            // 一番読まれるところで､そこに出して得るものが無い｡
            title: Some("Photo".into()),
            ..Default::default()
        }),
        ..Default::default()
    };

    // root は素の `ImageViewer` (#188)｡`gpui_component::Root` で包むのは
    // そのウィジェットを使うウィンドウだけで､ここは `img` と文字しか
    // 描かない｡
    let opened = cx.open_window(options, |window, cx| {
        cx.new(|cx| ImageViewer::new(timeline, photos, index, window, cx))
    });
    if let Err(error) = opened {
        log::error(&format!("could not open the image viewer: {error:#}"));
    }
}

/// 開く大きさ (#188)｡
///
/// 写真の API 寸法を画面の [`SCREEN_FRACTION`] に収まるまで縮める｡拡大は
/// しない — 小さな画像を引き伸ばしたウィンドウは中身より大きいだけだ｡
/// 写真の寸法が分からないとき､そして画面の寸法が分からないとき (テストの
/// ハーネスにはディスプレイが無いことがある) は決め打ちの大きさで開く｡
fn initial_size(photo: Option<&PostMedia>, display: Option<Size<Pixels>>) -> Size<Pixels> {
    let fallback = size(px(FALLBACK_WIDTH), px(FALLBACK_HEIGHT));
    let (Some(photo), Some(display)) = (photo, display) else {
        return fallback;
    };
    let (Some(width), Some(height)) = (side(photo.width), side(photo.height)) else {
        return fallback;
    };
    let room_width = f32::from(display.width) * SCREEN_FRACTION;
    let room_height = f32::from(display.height) * SCREEN_FRACTION;
    let scale = (room_width / width).min(room_height / height).min(1.0);
    size(px(width * scale), px(height * scale))
}

/// 写真の 1 辺｡0 は寸法を知らないのと同じに扱う｡
///
/// `u32` から `f32` への cast は精度を落とすので通らない｡X の画像は `u16` に
/// 収まるのでそちらを経由する ([`super::render::media_aspect`] と同じ)｡
fn side(pixels: Option<u32>) -> Option<f32> {
    let pixels = pixels.filter(|&pixels| pixels > 0)?;
    Some(f32::from(u16::try_from(pixels).unwrap_or(u16::MAX)))
}

/// 1 つの post の写真を 1 枚ずつ見せるウィンドウ (#188)｡
pub(in crate::ui) struct ImageViewer {
    /// `media_paths` と `media_failed` を毎 render で読む先｡自分では
    /// コピーを持たない — 開いた後に画像が届いても描き直るためだ｡
    timeline: Entity<TimelineView>,
    /// この post の写真だけ｡動画とアニメーション GIF は入らない｡
    photos: Vec<PostMedia>,
    /// いま見せている [`Self::photos`] の位置｡
    index: usize,
    /// これが無いとフォーカスの経路に viewer が乗らず､[`KEY_CONTEXT`] へ
    /// bind したキーがどれも届かない (#118 が timeline で踏んだのと同じ)｡
    focus_handle: FocusHandle,
    /// timeline が変わったら描き直すための購読｡drop すると届かなくなる
    /// ので､使わなくても持ちつづける｡
    _timeline_changed: Subscription,
}

impl ImageViewer {
    /// 開いた直後の viewer｡フォーカスは最初のフレームから viewer に置く｡
    fn new(
        timeline: Entity<TimelineView>,
        photos: Vec<PostMedia>,
        index: usize,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let timeline_changed = cx.observe(&timeline, |_this, _timeline, cx| cx.notify());
        let this = Self {
            timeline,
            photos,
            index,
            focus_handle: cx.focus_handle(),
            _timeline_changed: timeline_changed,
        };
        window.focus(&this.focus_handle);
        this
    }

    /// いま見せている写真｡
    fn current(&self) -> Option<&PostMedia> {
        self.photos.get(self.index)
    }

    /// いま見せている写真のファイル｡まだ手元に無ければ `None`｡
    fn path(&self, cx: &App) -> Option<PathBuf> {
        let photo = self.current()?;
        self.timeline.read(cx).media_paths.get(&photo.url).cloned()
    }

    /// 画像の代わりに出す文言 (#188)｡画像があれば `None`｡
    ///
    /// 取りそこねたものと､まだ着いていないものを言い分ける｡失敗した URL に
    /// 「読み込み中」と出しつづけると､読み手は着かないものを待つことになる｡
    fn notice(&self, cx: &App) -> Option<SharedString> {
        let photo = self.current()?;
        let timeline = self.timeline.read(cx);
        if timeline.media_paths.contains_key(&photo.url) {
            return None;
        }
        if timeline.media_failed.contains(&photo.url) {
            return Some("Could not load this image".into());
        }
        Some("Loading…".into())
    }

    /// 何枚目を見ているか (`2 / 3`)｡1 枚しか無ければ出さない｡
    fn counter(&self) -> Option<SharedString> {
        let count = self.photos.len();
        (count > 1)
            .then(|| SharedString::from(format!("{} / {count}", self.index.saturating_add(1))))
    }

    /// 次の写真へ｡最後の 1 枚で止まる (#188)｡
    ///
    /// 巻き戻さない: 3 枚目の次が 1 枚目に戻ると､端に着いたことが分からず
    /// 同じ写真をぐるぐる回ることになる｡
    fn show_next(&mut self, cx: &mut Context<'_, Self>) {
        let last = self.photos.len().saturating_sub(1);
        self.show(self.index.saturating_add(1).min(last), cx);
    }

    /// 前の写真へ｡最初の 1 枚で止まる ([`Self::show_next`] の対)｡
    fn show_previous(&mut self, cx: &mut Context<'_, Self>) {
        self.show(self.index.saturating_sub(1), cx);
    }

    /// `index` の写真を見せる｡端で止まったときは何もしない｡
    fn show(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        if index != self.index {
            self.index = index;
            cx.notify();
        }
    }
}

impl Render for ImageViewer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = self.timeline.read(cx).theme;
        let body = match self.path(cx) {
            // 幅と高さを両方与えたうえで `Contain` に吸収させる (#256)｡
            // 片方だけだと gpui が画像の縦横比を layout に持ち込み､枠を
            // 突き抜ける｡
            Some(path) => img(path)
                .addressable("image-viewer-image")
                .size_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            None => div()
                .addressable("image-viewer-notice")
                .text_color(rgb(theme.text_muted))
                .children(self.notice(cx))
                .into_any_element(),
        };

        div()
            .addressable("image-viewer")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &NextPhoto, _window, cx| this.show_next(cx)))
            .on_action(cx.listener(|this, _: &PreviousPhoto, _window, cx| this.show_previous(cx)))
            .on_action(cx.listener(|_this, _: &CloseWindow, window, _cx| {
                // viewer は timeline の手前に開いた 1 枚でしかないので､
                // 閉じてもアプリは終わらない (`main` の `on_window_closed`
                // が残りを数えている)｡
                window.remove_window();
            }))
            .flex()
            .flex_col()
            .size_full()
            .gap_2()
            .p_2()
            .bg(rgb(theme.bg))
            .text_color(rgb(theme.text))
            .text_size(theme::TEXT_BODY)
            .child(
                // 画像を残りの高さいっぱいに置く区画｡`min_h_0` が無いと
                // flex の既定 (`min-height: auto`) が縮むのを止めて､下の
                // カウンタを枠の外へ押し出す｡
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_center()
                    .justify_center()
                    .child(body),
            )
            .children(self.counter().map(|label| {
                div()
                    .addressable("image-viewer-counter")
                    .w_full()
                    .flex()
                    .justify_center()
                    .text_color(rgb(theme.text_muted))
                    .text_size(theme::TEXT_META)
                    .child(label)
            }))
    }
}

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
            "{what} stays inside the window: {image:?} in {root:?}"
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
