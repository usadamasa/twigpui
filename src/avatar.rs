//! 著者のアバター (#64): サイズを選び､URL を共有の画像キャッシュへ渡す｡
//!
//! ダウンロード､キャッシュ､ファイル名付けはすべて [`crate::image_cache`]
//! にあり､post のメディア (#65) と共有している｡アバター固有のもの — つまり
//! ここに残るもの — は [`preferred_url`]､X にどのサイズの variant を求めるか
//! の選択､そしてその推測が外れたときの fallback だ｡

use std::path::PathBuf;

use anyhow::Result;

use crate::image_cache;
use crate::paths::Paths;

/// X の既定の `_normal` (48x48) アバター URL を 400x400 の variant へ
/// 上げる｡Retina ディスプレイが 44pt の円をぼやけずに描くのに要るのが
/// それだ｡
///
/// この接尾辞の慣習は X のものであって､文書化された API の保証ではない｡
/// だから書き換えるのは実際に `_normal.<ext>` で終わる URL だけで､それ
/// 以外は来たままにする｡書き換えた URL が存在しなかった場合は 1 つ上で
/// 扱う: [`ensure_cached`] が row をアバター無しにする代わりに元の URL へ
/// fallback する｡
pub(crate) fn preferred_url(url: &str) -> String {
    let Some((stem, extension)) = url.rsplit_once('.') else {
        return url.to_string();
    };
    match stem.strip_suffix("_normal") {
        Some(base) => format!("{base}_400x400.{extension}"),
        None => url.to_string(),
    }
}

/// `url` のアバターのローカルパス｡まだキャッシュされていなければ先に
/// ダウンロードする (#64)｡
///
/// まず [`preferred_url`] の大きい variant を試し､API がくれたままの URL へ
/// fallback する｡サイズ接尾辞の慣習は X 自身のもので､何にも約束されて
/// いないからだ｡どちらも自分の key でキャッシュされるので､fallback の
/// 代償は追加リクエスト 1 回きりであって､描画ごとに 1 回ではない｡
pub(crate) fn ensure_cached(paths: &Paths, url: &str) -> Result<PathBuf> {
    let dir = paths.avatar_dir();
    let preferred = preferred_url(url);
    match image_cache::ensure_cached(&dir, &preferred) {
        Ok(path) => Ok(path),
        Err(error) if preferred != url => {
            // 大きい variant は推測だが､API 自身の URL はそうではない｡
            image_cache::ensure_cached(&dir, url).map_err(|fallback_error| {
                fallback_error.context(format!("the {preferred} variant also failed: {error:#}"))
            })
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrades_the_normal_suffix_to_the_larger_variant() {
        assert_eq!(
            preferred_url("https://pbs.twimg.com/profile_images/1/abc_normal.jpg"),
            "https://pbs.twimg.com/profile_images/1/abc_400x400.jpg"
        );
    }

    #[test]
    fn leaves_a_url_without_the_normal_suffix_alone() {
        // 接尾辞の慣習は X のものであって文書化された保証ではないので､
        // 見慣れないものは触らずそのまま通す｡
        assert_eq!(
            preferred_url("https://pbs.twimg.com/profile_images/1/abc.jpg"),
            "https://pbs.twimg.com/profile_images/1/abc.jpg"
        );
        assert_eq!(
            preferred_url("https://pbs.twimg.com/profile_images/1/abc_bigger.png"),
            "https://pbs.twimg.com/profile_images/1/abc_bigger.png"
        );
    }

    #[test]
    fn leaves_a_url_with_no_extension_alone() {
        assert_eq!(
            preferred_url("https://example.com/avatar"),
            "https://example.com/avatar"
        );
    }
}
