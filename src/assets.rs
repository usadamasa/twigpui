//! ウィンドウが描くアイコン｡バイナリに埋め込んである (#95, #156)｡
//!
//! gpui の [`gpui::svg`] 要素はマークアップを受け取らない — 受け取るのは
//! パスで､それを [`gpui::Application`] の構築に使った [`AssetSource`] 経由で
//! 解決する｡このアプリはそれを登録していなかったので､アイコンの試みは
//! ことごとく何も描かず､UI はテキストラベルのままだった｡
//!
//! ここの source は動く最小のものだ: パスと中身の対を [`ICONS`] に並べ､
//! `load` と `list` の双方がその 1 本を読む｡ディレクトリを歩かず､実行時にも
//! ファイルを読まない — アイコンはバイナリの中で配られる｡`.app` バンドルが
//! 求めるのもどのみちそれだし､存在しないアイコンは空の四角ではなくコンパイル
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

/// 一つの post の "Reply" 操作の記号 (#156)｡
pub(crate) const REPLY_ICON: &str = "icons/arrowshape.turn.up.left.svg";

/// 一つの post の repost/un-repost の toggle の記号 (#156)｡on/off は色だけで
/// 示す — SF Symbols も塗り潰しの形を持たない｡
pub(crate) const REPOST_ICON: &str = "icons/arrow.2.squarepath.svg";

/// like/unlike の toggle が off のときの記号 (#156)｡
pub(crate) const LIKE_ICON: &str = "icons/heart.svg";

/// like/unlike の toggle が on のときの記号 (#156) — [`LIKE_ICON`] と同じ
/// 輪郭を塗り潰したもの｡マスクとして描くので別ファイルにしてある｡
pub(crate) const LIKE_ON_ICON: &str = "icons/heart.fill.svg";

/// 一つの post の "Quote" 操作の記号 (#156)｡
pub(crate) const QUOTE_ICON: &str = "icons/quote.bubble.svg";

/// 一つの post を x.com で開く操作の記号 (#156)｡
pub(crate) const OPEN_ICON: &str = "icons/arrow.up.right.square.svg";

/// 一つの post の delete の入口の記号 (#156)｡確認の "Delete permanently" /
/// "Cancel" は文字のまま — 記号にするのはここだけ｡
pub(crate) const DELETE_ICON: &str = "icons/trash.svg";

/// このバイナリに埋め込んだアイコンのすべて: 解決するパスと､その中身｡
///
/// `load` と `list` の双方がこの 1 本を読む｡match の arm と `list` の
/// vec に同じパスを 2 度書くと､片方だけ編集した瞬間に静かにずれる — この
/// テーブルはその二重管理を構造的に無くす｡
const ICONS: &[(&str, &[u8])] = &[
    (
        RELOAD_ICON,
        include_bytes!("../assets/icons/arrow.clockwise.svg"),
    ),
    (
        REPLY_ICON,
        include_bytes!("../assets/icons/arrowshape.turn.up.left.svg"),
    ),
    (
        REPOST_ICON,
        include_bytes!("../assets/icons/arrow.2.squarepath.svg"),
    ),
    (LIKE_ICON, include_bytes!("../assets/icons/heart.svg")),
    (
        LIKE_ON_ICON,
        include_bytes!("../assets/icons/heart.fill.svg"),
    ),
    (
        QUOTE_ICON,
        include_bytes!("../assets/icons/quote.bubble.svg"),
    ),
    (
        OPEN_ICON,
        include_bytes!("../assets/icons/arrow.up.right.square.svg"),
    ),
    (DELETE_ICON, include_bytes!("../assets/icons/trash.svg")),
];

/// [`ICONS`] を提供する｡
#[derive(Debug, Clone, Copy)]
pub(crate) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        // エラーではなく `None` を返す: gpui はこのアプリが登録して
        // いないパス (たとえばカーソルのスタイル) を訊いてくるので､
        // それを失敗にすると飾りの欠落が起動失敗に化ける｡
        Ok(ICONS
            .iter()
            .find(|(registered, _)| *registered == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .map(|(path, _)| SharedString::from(*path))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{Assets, ICONS, RELOAD_ICON};
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

    #[test]
    fn every_listed_icon_is_in_the_binary() {
        // #156: テーブル化で `load` と `list` のずれは構造的に起きなくなる
        // が､ファイルのリネームや移動 (パス定数を変えずに include_bytes!
        // だけ差し替える等) は依然として静かに壊す｡件数も assert する —
        // `list()` が空を返しても `iter().all()` は空で通ってしまうため｡
        assert_eq!(ICONS.len(), 8);
        let names = Assets.list("icons").expect("listing icons cannot fail");
        assert_eq!(names.len(), 8);
        for name in names {
            let bytes = Assets
                .load(&name)
                .expect("loading a listed asset cannot fail")
                .unwrap_or_else(|| panic!("{name} is listed but does not load"));
            assert!(bytes.starts_with(b"<svg"), "{name} must be SVG markup");
        }
    }
}
