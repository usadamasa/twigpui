//! 開いている macOS の window を最前面に留めるかどうかだけを切り替える (#267)｡
//!
//! gpui-pre 0.3.5 には window を開いた後に level を動かす口が無い｡
//! `WindowKind` は `open_window` のときに決まるもので､しかも macOS の
//! `WindowKind::Floating` は `NSPanel` で開くため
//! `MacWindow::active_window` の class 判定から外れ､浮かせている間
//! メニューバーが `Quit` 以外効かなくなる (`NSPanel` の
//! `hidesOnDeactivate` も既定のままで､別のアプリを前面にすると窓ごと
//! 消える)｡だから window の種類は `Normal` のままにして､`NSWindow` の
//! level だけを外から差し替える｡
//!
//! この crate が本体から分かれているのは `unsafe` のためだけだ｡twigpui は
//! `unsafe_code = "forbid"` のままで､生ポインタを触るのは下の 1 か所に
//! 閉じてある｡[`raw_window_handle`] しか見ないので gpui には依存しない｡

use core::fmt;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSFloatingWindowLevel, NSNormalWindowLevel, NSView};
use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle};

/// [`set_floating`] が `NSWindow` に届かなかった理由｡
///
/// どれも呼び出し側にとっては「見た目が変わらない」でしかない — level は
/// 描画にも状態にも関わらないので､呼び出し側は報告して続けてよい｡
#[derive(Debug)]
pub enum Error {
    /// window が raw handle を返さなかった｡gpui のテスト platform は常に
    /// これで (`HandleError::NotSupported`)､テストからの呼び出しはここで
    /// 止まる｡
    NoHandle(HandleError),
    /// `AppKit` 以外の raw handle だった｡macOS 専用のこのアプリでは起きない
    /// が､`RawWindowHandle` は platform ごとの variant を持つので潰す先が
    /// 要る｡
    NotAppKit,
    /// main thread の外から呼ばれた｡`AppKit` の window は main thread から
    /// しか触れない｡
    NotMainThread,
    /// `NSView` がまだどの `NSWindow` にも載っていない｡
    NoWindow,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHandle(error) => write!(f, "the window has no raw handle: {error}"),
            Self::NotAppKit => f.write_str("the window is not an AppKit window"),
            Self::NotMainThread => f.write_str("a window level can only be set on the main thread"),
            Self::NoWindow => f.write_str("the view is not in a window yet"),
        }
    }
}

impl std::error::Error for Error {}

/// `window` を他のアプリの窓より上に留めるかどうかを決める (#267)｡
///
/// 真なら `NSFloatingWindowLevel`､偽なら `NSNormalWindowLevel`｡触るのは
/// level だけで､window の class も style mask も `hidesOnDeactivate` も
/// そのままなので､浮かせている間も普通の window として振る舞う｡
///
/// # Errors
///
/// [`Error`] の 4 つ｡どれも level が変わらなかったというだけで､window は
/// 無事である｡
pub fn set_floating(window: &impl HasWindowHandle, floating: bool) -> Result<(), Error> {
    let handle = window.window_handle().map_err(Error::NoHandle)?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return Err(Error::NotAppKit);
    };
    // 下の unsafe の前提の半分｡`MainThreadMarker` 自体は使わないが､
    // 「main thread に居る」を安全に確かめられるのはこれだけだ｡
    if MainThreadMarker::new().is_none() {
        return Err(Error::NotMainThread);
    }
    // SAFETY: `appkit.ns_view` は gpui がこの window のために持っている
    // `NSView` へのポインタで (`gpui-pre-macos` の `MacWindow` が
    // `AppKitWindowHandle::new` に渡したもの)､`handle` が借りている
    // `&impl HasWindowHandle` が生きているあいだは解放されない｡
    // AppKit の型を触ってよい thread に居ることは直上で確かめてある｡
    let view: &NSView = unsafe { appkit.ns_view.cast::<NSView>().as_ref() };
    let Some(ns_window) = view.window() else {
        return Err(Error::NoWindow);
    };
    ns_window.setLevel(if floating {
        NSFloatingWindowLevel
    } else {
        NSNormalWindowLevel
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// raw handle を返さない window — gpui のテスト platform と同じ形｡
    #[derive(Debug)]
    struct Headless;

    impl HasWindowHandle for Headless {
        fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, HandleError> {
            Err(HandleError::NotSupported)
        }
    }

    /// handle を返せない window は panic ではなく [`Error::NoHandle`]｡
    /// 呼び出し側 (`TimelineView::apply_floating`) はこれを warn 1 行に
    /// 落とすので､テストの中でトグルを押しても落ちない｡
    #[test]
    fn a_window_without_a_raw_handle_reports_instead_of_panicking() {
        let outcome = set_floating(&Headless, true);
        assert!(
            matches!(outcome, Err(Error::NoHandle(HandleError::NotSupported))),
            "expected NoHandle, got {outcome:?}"
        );
    }

    /// 呼び出し側はこれをログへ流すので､理由が読める文でなければならない｡
    #[test]
    fn the_reasons_say_what_went_wrong() {
        assert!(Error::NotAppKit.to_string().contains("AppKit"));
        assert!(Error::NotMainThread.to_string().contains("main thread"));
        assert!(Error::NoWindow.to_string().contains("window"));
    }
}
