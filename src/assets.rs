//! The icons the window draws, compiled into the binary (#95).
//!
//! gpui's [`gpui::svg`] element does not take markup — it takes a path,
//! which it resolves through the [`AssetSource`] the [`gpui::Application`]
//! was built with. This app had never registered one, so every attempt at
//! an icon rendered nothing at all and the UI stayed on text labels.
//!
//! The source here is the smallest thing that works: a `match` over paths
//! with [`include_bytes!`] behind each arm. No directory walking, no
//! runtime file reads — the icons ship inside the binary, which is what a
//! `.app` bundle wants anyway, and an icon that does not exist is a
//! compile error rather than a blank square.
//!
//! ## What an icon file may contain
//!
//! gpui renders an SVG as a **single-color mask**: the shape decides which
//! pixels are painted and the element's `text_color` decides the color, so
//! `fill`, `stroke` and any color in the file are ignored. Icons therefore
//! have to be drawn as shapes that read correctly in one flat color —
//! which is also why these are stroke-style outlines and not filled
//! glyphs. Multi-color artwork needs a different path entirely (rasterize
//! it and use `img`), and none is wanted here.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

/// The reload icon in the toolbar — an SF Symbols-shaped
/// `arrow.clockwise`, drawn as an open circle with an arrowhead.
pub(crate) const RELOAD_ICON: &str = "icons/arrow.clockwise.svg";

/// Serves [`RELOAD_ICON`] and anything else added beside it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match path {
            RELOAD_ICON => Ok(Some(Cow::Borrowed(
                include_bytes!("../assets/icons/arrow.clockwise.svg").as_slice(),
            ))),
            // `None` rather than an error: gpui asks for paths this app
            // never registered (a cursor style, say), and failing those
            // would turn a missing decoration into a startup failure.
            _ => Ok(None),
        }
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![SharedString::from(RELOAD_ICON)])
    }
}

#[cfg(test)]
mod tests {
    use super::{Assets, RELOAD_ICON};
    use gpui::AssetSource;

    #[test]
    fn the_reload_icon_is_in_the_binary() {
        // The failure this guards is silent: `svg()` renders nothing at
        // all when its path does not resolve, so a renamed or moved file
        // would leave a blank gap in the toolbar with no error anywhere.
        let bytes = Assets
            .load(RELOAD_ICON)
            .expect("loading a registered asset cannot fail")
            .expect("the reload icon is registered");
        assert!(!bytes.is_empty());
        assert!(
            bytes.starts_with(b"<svg"),
            "the reload icon must be SVG markup"
        );
    }

    #[test]
    fn an_unregistered_path_is_absent_rather_than_an_error() {
        // gpui asks for paths this app never registered. Answering those
        // with an error would turn a missing decoration into a crash.
        assert!(
            Assets
                .load("icons/does-not-exist.svg")
                .expect("an unknown path is not an error")
                .is_none()
        );
    }
}
