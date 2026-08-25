//! ウィンドウが描くアイコン｡バイナリに埋め込んである (#95)｡
//!
//! gpui の [`gpui::svg`] 要素はマークアップを受け取らない — 受け取るのは
//! パスで､それを [`gpui::Application`] の構築に使った [`AssetSource`] 経由で
//! 解決する｡このアプリはそれを登録していなかったので､アイコンの試みは
//! ことごとく何も描かず､UI はテキストラベルのままだった｡
//!
//! ここの source は動く最小のものだ: パスに対する `match` で､各 arm の
//! 裏に [`include_bytes!`] を置く｡ディレクトリを歩かず､実行時にファイルも
//! 読まない — アイコンはバイナリの中で配られる｡`.app` バンドルが求めるのも
//! どのみちそれだし､存在しないアイコンは空の四角ではなくコンパイル
//! エラーになる｡
//!
//! ## アイコンファイルに入れてよいもの
//!
//! gpui は SVG を **単色のマスク** として描く: どのピクセルを塗るかを形が
//! 決め､色は要素の `text_color` が決めるので､`fill`､`stroke`､ファイル中の
//! 色はすべて無視される｡したがってアイコンは､単一のフラットな色で正しく
//! 読める形として描かねばならない — これらが塗り潰したグリフではなく
//! stroke 方式の輪郭なのもそのためだ｡多色のアートワークにはまったく別の
//! 経路が要る (ラスタライズして `img` を使う)｡ここでは求めていない｡

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

/// toolbar のリロードアイコン — SF Symbols 風の `arrow.clockwise` で､
/// 開いた円に矢印の先端を付けた形で描いてある｡
pub(crate) const RELOAD_ICON: &str = "icons/arrow.clockwise.svg";

/// [`RELOAD_ICON`] と､その横に足したものを提供する｡
#[derive(Debug, Clone, Copy)]
pub(crate) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match path {
            RELOAD_ICON => Ok(Some(Cow::Borrowed(
                include_bytes!("../assets/icons/arrow.clockwise.svg").as_slice(),
            ))),
            // エラーではなく `None` を返す: gpui はこのアプリが登録して
            // いないパス (たとえばカーソルのスタイル) を訊いてくるので､
            // それを失敗にすると飾りの欠落が起動失敗に化ける｡
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
        // これが守る失敗は静かだ: パスが解決しないとき `svg()` は何も
        // 描かないので､リネームや移動をしたファイルは toolbar に空白を
        // 残すだけで､エラーはどこにも出ない｡
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
        // gpui はこのアプリが登録していないパスを訊いてくる｡それにエラーで
        // 答えると飾りの欠落がクラッシュに化ける｡
        assert!(
            Assets
                .load("icons/does-not-exist.svg")
                .expect("an unknown path is not an error")
                .is_none()
        );
    }
}
