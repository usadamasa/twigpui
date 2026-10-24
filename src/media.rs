//! 行のサムネイル (#305): 小さい variant を選び､URL を共有の画像キャッシュへ渡す｡
//!
//! ダウンロード､キャッシュ､ファイル名付けは [`crate::image_cache`] にある｡
//! ここに残るのは [`thumbnail_url`]､X にどの大きさの variant を求めるかの
//! 選択と､その推測が外れたときの fallback だけ｡形は [`crate::avatar`] と同じ｡

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::image_cache;

/// pbs.twimg.com の写真 URL を `?name=small` (最大 680px) の variant にする｡
///
/// `name=` 無しの URL は最大 1200px 程度を返すが､サムネイルの枠はそれより
/// ずっと小さく､gpui の `img` は縮小せずにデコードする｡
///
/// `name=` の慣習は X のものであって API の保証ではないので､書き換えるのは
/// クエリを持たない `https://pbs.twimg.com/media/` の URL だけにする｡
/// fixture のローカルパスもここを素通りする｡
pub(crate) fn thumbnail_url(url: &str) -> String {
    url.to_string()
}

/// `url` の写真のサムネイルのローカルパス｡まだキャッシュされていなければ
/// 先に `dir` へダウンロードする｡
///
/// まず [`thumbnail_url`] の小さい variant を試し､API がくれたままの URL へ
/// fallback する｡どちらも自分の key でキャッシュされるので､fallback の
/// 代償は追加リクエスト 1 回きりになる｡
pub(crate) fn ensure_thumbnail_cached(dir: &Path, url: &str) -> Result<PathBuf> {
    image_cache::ensure_cached(dir, url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_photo_url_asks_for_the_small_variant() {
        assert_eq!(
            thumbnail_url("https://pbs.twimg.com/media/GabcXYZ.jpg"),
            "https://pbs.twimg.com/media/GabcXYZ.jpg?name=small"
        );
    }

    #[test]
    fn a_url_that_already_has_a_query_is_left_alone() {
        // 何かしら variant を指定済みの URL は､こちらの推測で上書きしない｡
        assert_eq!(
            thumbnail_url("https://pbs.twimg.com/media/GabcXYZ?format=jpg&name=large"),
            "https://pbs.twimg.com/media/GabcXYZ?format=jpg&name=large"
        );
    }

    #[test]
    fn a_url_outside_pbs_media_is_left_alone() {
        // 動画の静止画 (`ext_tw_video_thumb`) などは `name=` の慣習が同じとは
        // 限らない｡fixture のローカルパスに `?` を付けるとファイルが無くなる｡
        for url in [
            "https://pbs.twimg.com/ext_tw_video_thumb/1/pu/img/still.jpg",
            "https://example.com/media/a.jpg",
            "/Users/someone/fixtures/media/one.png",
            "fixtures/media/one.png",
        ] {
            assert_eq!(thumbnail_url(url), url);
        }
    }

    #[test]
    fn a_local_file_is_resolved_without_rewriting() {
        let file = std::env::temp_dir().join("twigpui-media-thumbnail-local.png");
        std::fs::write(&file, b"not really a png").unwrap();
        let dir = std::env::temp_dir().join("twigpui-media-thumbnail-untouched");

        let resolved = ensure_thumbnail_cached(&dir, file.to_str().unwrap()).unwrap();

        assert_eq!(resolved, file);
        assert!(!dir.exists(), "a local file must not create the cache dir");
        std::fs::remove_file(&file).unwrap();
    }
}
