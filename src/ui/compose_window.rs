//! composer を `⌘N` の別ウィンドウへ移したもの (#282)｡
//!
//! 先例は `image_viewer.rs` だが 1 点だけ決定的に違う: composer は
//! `gpui_component` のテキスト入力を使うので､root を素の [`ComposeWindow`]
//! ではなく `gpui_component::Root` で包む必要がある (`main.rs` の doc を見よ)｡
//!
//! 状態は `TimelineView` に置いたまま動かさない｡[`ComposeWindow`] は
//! `WeakEntity<TimelineView>` を持つ薄い殻で､`render` の中で
//! `TimelineView::composer` をそのまま描くだけ｡下書き ([`crate::compose::ComposeState`])
//! を決して失わないという #14 の約束は､それが動かないことで保たれる｡
//!
//! `compose_input` (`gpui_component::input::InputState`) だけは別だ｡
//! `InputState::new` はカーソルの点滅と blur を渡された window へ束ねる
//! (focus/blur の購読が window 単位のため) ので､timeline の window で
//! 作ったものを別の window で描いても点滅も blur も届かない｡だから
//! compose window を開くたびに [`TimelineView::rebind_compose_input`] で
//! 作り直す — 下書きの *本文* は移らない (`compose.text()` が正本のまま)
//! ので､失うものは無い｡

use gpui::{
    App, Bounds, Context, Entity, FocusHandle, Subscription, TitlebarOptions, WeakEntity, Window,
    WindowBounds, WindowOptions, div, prelude::*, px, size,
};

use super::TimelineView;
use crate::log;
use crate::menu::CloseWindow;
use crate::profile::Profile;

/// compose window の root 要素が担うキーコンテキスト｡timeline の
/// [`crate::menu::KEY_CONTEXT`] とは別にしてある — 開いている間に timeline
/// 側の裸のキー (`n`/`j`/`k`/…) が発火してはならない｡
const KEY_CONTEXT: &str = "Composer";

/// 開く大きさ｡本文 280 字とカウンタ､Post ボタンが収まればよいので
/// `image_viewer` のような画面依存の計算はしない｡
const WIDTH: f32 = 420.0;
const HEIGHT: f32 = 280.0;

/// compose window のキーバインドを登録する (#282)｡`main` が `menu::init` /
/// `image_viewer::init` の隣で一度だけ呼ぶ｡
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        gpui::KeyBinding::new("cmd-w", CloseWindow, Some(KEY_CONTEXT)),
        gpui::KeyBinding::new("escape", CloseWindow, Some(KEY_CONTEXT)),
    ]);
}

/// compose window を開く｡サインインしていない session では何もしない —
/// これまでインラインの composer が出なかった条件と同じ｡
///
/// 既に開いていれば新しく開かず前面へ出す｡
///
/// `cx.defer` で開く: `⌘N` のハンドラも行の Reply/Quote のクリックハンドラも
/// `TimelineView` を lease した中から呼ぶので､その場で `open_window` すると
/// "cannot read `TimelineView` while it is already being updated" で落ちる
/// (`image_viewer::open` の doc に実際の panic の記録がある)｡
pub(in crate::ui) fn open(timeline: &Entity<TimelineView>, cx: &mut App) {
    let timeline = timeline.clone();
    cx.defer(move |cx| {
        if !timeline.read(cx).signed_in_with_oauth {
            return;
        }
        let already_open = timeline.read(cx).compose_window.is_some_and(|handle| {
            handle
                .update(cx, |_, window, _cx| window.activate_window())
                .is_ok()
        });
        if already_open {
            return;
        }

        let bounds = Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx);
        let mut options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        let profile = timeline.read(cx).paths.profile();
        options.titlebar = Some(TitlebarOptions {
            title: Some(title(profile).into()),
            ..Default::default()
        });

        let opened = cx.open_window(options, |window, cx| {
            timeline.update(cx, |view, cx| view.rebind_compose_input(window, cx));
            let compose_window = cx.new(|cx| ComposeWindow::new(&timeline, window, cx));
            cx.new(|cx| gpui_component::Root::new(compose_window, window, cx))
        });
        match opened {
            Ok(handle) => {
                timeline.update(cx, |view, cx| {
                    view.compose_window = Some(handle);
                    cx.notify();
                });
                // 開いた直後から打てるように､入力欄へフォーカスを入れ直す
                // (`ComposeWindow::new` はいったんウィンドウ自身へ置く —
                // `Self::focus_handle` の doc を見よ)｡
                let _ = handle.update(cx, |_, window, cx| {
                    timeline.update(cx, |view, cx| {
                        view.compose_input
                            .update(cx, |state, cx| state.focus(window, cx));
                    });
                });
            }
            Err(error) => {
                log::error(&format!("could not open the composer window: {error:#}"));
            }
        }
    });
}

/// compose window の title｡`profile.rs` の `compose_window_title` を素通し
/// する以上のことはしない (`image_viewer::title` と同じ理由)｡
fn title(profile: Profile) -> String {
    profile.compose_window_title()
}

/// composer を描くだけの薄い殻 (#282)｡状態は持たない — `TimelineView` の
/// `compose`/`compose_input` を読むだけ｡
pub(in crate::ui) struct ComposeWindow {
    /// 弱い handle｡強く持つと timeline のウィンドウを閉じても
    /// `TimelineView` が生き残り､`auto_refresh` のループが回りつづける
    /// (`image_viewer::ImageViewer::timeline` の doc と同じ理由)｡
    timeline: WeakEntity<TimelineView>,
    /// これが無いとフォーカスの経路に compose window が乗らず､
    /// [`KEY_CONTEXT`] へ bind したキーがどれも届かない (#118 と同じ罠)｡
    /// 開いた直後は [`open`] が入力欄へフォーカスを移すので､ここに残るのは
    /// 「何もフォーカスされていない」瞬間を作らないための初期値でしかない｡
    focus_handle: FocusHandle,
    /// timeline が変わったら描き直すための購読 (下書きの文字数やボタンの
    /// 有効/無効が変わる)｡
    _timeline_changed: Subscription,
    /// timeline が消えたら compose window も一緒に閉じる (#139)｡timeline の
    /// ウィンドウを先に閉じても､この窓だけが取り残されてプロセスを
    /// 生かし続けることが無いようにする｡
    _timeline_released: Subscription,
}

impl ComposeWindow {
    fn new(
        timeline: &Entity<TimelineView>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let timeline_changed = cx.observe(timeline, |_this, _timeline, cx| cx.notify());
        let timeline_released =
            cx.observe_release_in(timeline, window, |_this, _timeline, window, _cx| {
                window.remove_window();
            });
        let this = Self {
            timeline: timeline.downgrade(),
            focus_handle: cx.focus_handle(),
            _timeline_changed: timeline_changed,
            _timeline_released: timeline_released,
        };
        window.focus(&this.focus_handle);
        this
    }
}

impl Render for ComposeWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let Some(timeline) = self.timeline.upgrade() else {
            // timeline が既に無い: `_timeline_released` がこのフレームの
            // 後で窓を閉じるので､それまでの空の 1 フレームでしかない｡
            return div().into_any_element();
        };
        div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_this, _: &CloseWindow, window, _cx| {
                window.remove_window();
            }))
            .size_full()
            // 255: compose window は常に不透明 (#267 の透過は timeline の
            // ウィンドウだけの話)｡`into_any_element` で外へ持ち出せる形に
            // 変える — `composer()` の戻り値は `view`/`cx` の借用を
            // 引きずっているので､`timeline.update` の外へそのままは
            // 出られない｡
            .child(timeline.update(cx, |view, cx| view.composer(255, cx).into_any_element()))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use crate::ui::TimelineState;
    use crate::ui::tests::{draw_until_parked, fixture_window, fixture_with};

    /// いま開いているウィンドウの数｡
    fn window_count(cx: &mut gpui::TestAppContext) -> usize {
        cx.update(|cx| cx.windows().len())
    }

    /// #282: `open` はウィンドウを 1 枚増やし､サインインしていなければ
    /// 何もしない｡
    #[gpui::test]
    fn opening_the_composer_adds_a_window(cx: &mut gpui::TestAppContext) {
        let (_window, timeline) = fixture_window(cx, fixture_with(&["1"], &[]));
        cx.update(|cx| {
            timeline.update(cx, |view, _cx| view.signed_in_with_oauth = false);
        });
        cx.update(|cx| super::open(&timeline, cx));
        cx.run_until_parked();
        assert_eq!(window_count(cx), 1, "a signed-out session opens nothing");

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| view.signed_in_with_oauth = true);
        });
        cx.update(|cx| super::open(&timeline, cx));
        cx.run_until_parked();
        assert_eq!(window_count(cx), 2, "a signed-in session opens a window");
    }

    /// #282: 既に開いていれば新しく開かない｡
    #[gpui::test]
    fn opening_the_composer_twice_does_not_double_the_windows(cx: &mut gpui::TestAppContext) {
        let (_window, timeline) = fixture_window(cx, fixture_with(&["1"], &[]));
        cx.update(|cx| super::open(&timeline, cx));
        cx.run_until_parked();
        cx.update(|cx| super::open(&timeline, cx));
        cx.run_until_parked();
        assert_eq!(window_count(cx), 2, "the second open re-uses the window");
    }

    /// #282: 開いた compose window は本物の `cmd-w` / `escape` で閉じられる｡
    #[gpui::test]
    fn cmd_w_closes_the_composer(cx: &mut gpui::TestAppContext) {
        cx.update(super::super::image_viewer::init);
        cx.update(super::init);
        let (_window, timeline) = fixture_window(cx, fixture_with(&["1"], &[]));
        cx.update(|cx| super::open(&timeline, cx));
        cx.run_until_parked();
        assert_eq!(window_count(cx), 2);

        let handle = cx
            .update(|cx| timeline.read(cx).compose_window)
            .expect("the composer window opened");
        let mut visual = gpui::VisualTestContext::from_window(handle.into(), cx);
        draw_until_parked(&mut visual, cx);

        visual.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        assert_eq!(window_count(cx), 1, "cmd-w closes the composer window");
        cx.update(|cx| {
            let view = timeline.read(cx);
            assert!(
                matches!(view.state, TimelineState::Loaded(_)),
                "the timeline itself stays open"
            );
        });
    }

    /// #139, #282: timeline のウィンドウを閉じると compose window も残らない｡
    #[gpui::test]
    fn closing_the_timeline_closes_the_composer_too(cx: &mut gpui::TestAppContext) {
        let (window, timeline) = fixture_window(cx, fixture_with(&["1"], &[]));
        cx.update(|cx| super::open(&timeline, cx));
        cx.run_until_parked();
        assert_eq!(window_count(cx), 2);

        drop(timeline);
        let mut behind = gpui::VisualTestContext::from_window(window.into(), cx);
        behind.update(|window, _cx| window.remove_window());
        cx.run_until_parked();

        assert_eq!(
            window_count(cx),
            0,
            "the composer window must not outlive the timeline"
        );
    }

    /// #282: `rebind_compose_input` が作り直した `InputState` は本物の
    /// `compose_input` として動く — 既存の下書きを写した状態で開き､打鍵は
    /// `on_compose_input_event` を経て `compose.text()` まで届く｡timeline
    /// のウィンドウで打つことを見ていた `the_bare_keys_type_into_a_focused_composer`
    /// (#282 で撤去) の後継で、compose window 側でも同じ経路が生きている
    /// ことを確かめる｡コンパイルが通ることは動作の証拠にならない｡
    #[gpui::test]
    fn typing_reaches_the_draft(cx: &mut gpui::TestAppContext) {
        let (_window, timeline) = fixture_window(cx, fixture_with(&["1"], &[]));
        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                view.compose.set_text("existing".to_string());
            });
            super::open(&timeline, cx);
        });
        cx.run_until_parked();

        let handle = cx
            .update(|cx| timeline.read(cx).compose_window)
            .expect("the composer window opened");
        let mut composer = gpui::VisualTestContext::from_window(handle.into(), cx);
        draw_until_parked(&mut composer, cx);

        cx.update(|cx| {
            assert_eq!(
                timeline.read(cx).compose_input.read(cx).value().to_string(),
                "existing",
                "the rebuilt InputState has to be seeded from the existing draft"
            );
        });

        composer.simulate_keystrokes("a b c");
        cx.run_until_parked();

        cx.update(|cx| {
            let text = timeline.read(cx).compose.text().to_string();
            assert!(
                text.contains("abc") && text.contains("existing"),
                "keystrokes in the reopened composer have to reach the draft: {text:?}"
            );
        });
    }

    /// #282: compose window の title は profile を名乗る (`image_viewer` の
    /// 同種テストと同じ理由)｡
    #[test]
    fn the_title_is_built_from_the_profile_not_a_literal() {
        for profile in [
            crate::profile::Profile::Dev,
            crate::profile::Profile::Release,
        ] {
            assert_eq!(super::title(profile), profile.compose_window_title());
        }
    }
}
