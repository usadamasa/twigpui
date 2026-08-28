//! リモート画像のダウンロードとキャッシュ (#64, #65)｡
//!
//! アバター (`avatar.rs`) も post のメディア (`ui.rs` のメディアグリッド) も
//! 必要なものは同じだ: URL を一度 fetch し､バイト列をディスクに置き､
//! `gpui::img` が描けるパスを返す｡どちらも X API のトラフィックではなく
//! — 両方とも `pbs.twimg.com` へ行く — **quota も credit も使わない**｡
//! それでも HTTP リクエストではあるので､UI スレッドの外で､URL ごとに
//! 一度だけ行う: ファイルがすでにあれば [`ensure_cached`] は即座に返る｡
//!
//! 2 つの呼び出し元で違うのはファイルが落ちる *ディレクトリ* だけだ｡
//! だからそれをここのパラメータにしてあり､このモジュールを 2 つ複製して
//! いない｡
//!
//! カバレッジを担っている純粋な継ぎ目は [`cache_key`] だ｡[`ensure_cached`]
//! はネットワークとディスクに触るので unit test しない｡`cache::reload` と
//! `repost::create` がすでに従っている慣習で — このプロジェクトのテストは
//! ネットワークリクエストを一切しない｡

use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use sha2::{Digest as _, Sha256};

/// これまでに届かない画像は諦める｡意図的に短くしてある: 依存するものが
/// 何も無く､その間 row はプレースホルダを描き､固まった接続が background
/// task を無期限に生かしつづけてはならないからだ｡
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// これより大きいものは拒否する｡プロフィール画像は数十キロバイト､
/// timeline の写真は数百キロバイトだ｡それをはるかに超えるレスポンスは
/// 別物へのリダイレクトか､サーバのエラーページか､このアプリがディスクに
/// 書く筋合いの無いリソースだ｡
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// 1 つの画像 URL がキャッシュされるファイル名｡
///
/// URL そのものではなく URL 全体のハッシュだ: これらの URL は `/` と
/// クエリ文字列を含むし､X は同じ basename (`image_normal.jpg`) を別々の
/// アカウントで使い回すので､これより短いものは不正なファイル名になるか
/// ユーザー間で衝突するかのどちらかになる｡URL がもっともらしい拡張子を
/// 持つときはそれを引き継ぐ｡キャッシュディレクトリを手で開いたものから
/// ファイルが見分けられるままになるようにだ｡想定外のものはパス要素として
/// 信用せず､`.img` へ落とす｡
pub(crate) fn cache_key(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    // 容量のヒントにすぎない: 1 バイトあたり 16 進 2 文字､それにドットと
    // 短い拡張子｡saturating なのはこれが最適化であって境界ではないからだ
    // — ここでの overflow は誤った確保サイズであり､誤った key ではない
    // (#47)｡
    let mut key = String::with_capacity(digest.len().saturating_mul(2).saturating_add(5));
    for byte in digest {
        // 失敗しない: String への書き込みは決して失敗しない｡
        let _ = write!(key, "{byte:02x}");
    }
    key.push('.');
    key.push_str(extension_of(url));
    key
}

/// キャッシュした画像に与える拡張子: URL 自身のものが短く純粋に英数字
/// (`jpg`, `png`, `webp`) ならそれを､そうでなければ `img` を使う｡そのまま
/// 信用することは決してない — 拡張子は､このコードがこの後書き込む
/// ファイル名の一部だからだ｡
fn extension_of(url: &str) -> &str {
    let candidate = url
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, extension)| extension)
        .unwrap_or_default();
    let plausible = !candidate.is_empty()
        && candidate.len() <= 5
        && candidate.chars().all(|c| c.is_ascii_alphanumeric());
    if plausible { candidate } else { "img" }
}

/// 1 つの画像 URL が `dir` の中のどこに住むか｡まだそこに無くても答える｡
pub(crate) fn cached_path(dir: &Path, url: &str) -> PathBuf {
    dir.join(cache_key(url))
}

/// `url` が fetch を要するものか｡そうでなければディスク上のパスとして
/// 読む (#234)｡
///
/// 境界は `http(s)://` の有無｡fixture が画像を相対パスで書き､
/// [`crate::fixture::load`] がそれを絶対パスにして届ける｡X の URL は
/// どれも `https://pbs.twimg.com/…` なので､本番の経路がこちらへ落ちる
/// ことはない｡
pub(crate) fn is_remote(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// `url` のローカルパス｡まだキャッシュされていなければ先に `dir` へ
/// ダウンロードする｡
///
/// [`is_remote`] でない `url` はディスク上のファイルとして扱い､キャッシュ
/// へ複製せずそのまま返す (#234)｡無ければ error — 見つからないファイルを
/// URL として fetch しにいくと､どのみち失敗するうえに WARN がファイル名
/// ではなく `could not fetch` を言う｡
///
/// fetch の経路は unit test しない — 本物の HTTP リクエストを出すからだ｡
/// カバレッジは [`cache_key`] とローカルの分岐が担う｡
pub(crate) fn ensure_cached(dir: &Path, url: &str) -> Result<PathBuf> {
    if !is_remote(url) {
        let local = Path::new(url);
        if !local.is_file() {
            bail!("{url} is not a file on disk");
        }
        return Ok(local.to_path_buf());
    }

    let path = cached_path(dir, url);
    if path.exists() {
        return Ok(path);
    }

    std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(FETCH_TIMEOUT))
        .build()
        .into();
    let mut response = agent
        .get(url)
        .call()
        .with_context(|| format!("could not fetch {url}"))?;

    let status = response.status().as_u16();
    if status != 200 {
        bail!("{url} answered {status}");
    }

    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_BYTES)
        .read_to_end(&mut bytes)
        .with_context(|| format!("could not read the image body from {url}"))?;
    if bytes.is_empty() {
        bail!("{url} returned an empty body");
    }

    std::fs::write(&path, &bytes).with_context(|| format!("could not write {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_key_is_stable_for_one_url() {
        let url = "https://pbs.twimg.com/profile_images/1/abc_normal.jpg";
        assert_eq!(cache_key(url), cache_key(url));
    }

    #[test]
    fn different_urls_get_different_cache_keys() {
        // X はアカウントをまたいで同じ basename を使い回すので､2 つの画像を
        // 分けているのは URL 全体をハッシュすることだ｡
        assert_ne!(
            cache_key("https://pbs.twimg.com/profile_images/1/image_normal.jpg"),
            cache_key("https://pbs.twimg.com/profile_images/2/image_normal.jpg")
        );
    }

    #[test]
    fn the_cache_key_is_a_single_path_component() {
        let key = cache_key("https://pbs.twimg.com/profile_images/1/abc_normal.jpg");
        assert!(!key.contains('/'), "{key} must not be a nested path");
        assert!(!key.contains(".."), "{key} must not escape the cache dir");
    }

    #[test]
    fn the_cache_key_keeps_a_plausible_extension() {
        assert_eq!(
            cache_key("https://pbs.twimg.com/a/b_normal.jpg")
                .rsplit_once('.')
                .map(|(_, extension)| extension),
            Some("jpg")
        );
        assert_eq!(
            cache_key("https://pbs.twimg.com/a/b_normal.png")
                .rsplit_once('.')
                .map(|(_, extension)| extension),
            Some("png")
        );
    }

    #[test]
    fn an_implausible_extension_falls_back_to_img() {
        // そのまま信用することは決してない: これはこの後書き込まれる
        // ファイル名の一部になる｡
        for url in [
            "https://example.com/a/b.php?x=/../../etc",
            "https://example.com/avatar",
        ] {
            assert_eq!(
                cache_key(url).rsplit_once('.').map(|(_, ext)| ext),
                Some("img"),
                "{url}"
            );
        }
    }

    #[test]
    fn a_cached_image_lives_in_the_directory_it_was_given() {
        let dir = Path::new("/tmp/twigpui-images");
        assert_eq!(
            cached_path(dir, "https://pbs.twimg.com/a/b.jpg").parent(),
            Some(dir)
        );
    }

    #[test]
    fn a_local_file_is_returned_as_is_without_touching_the_cache() {
        // #234: fixture は画像をディスクから持ち込む｡それをキャッシュへ
        // 複製もせず､ましてネットワークへも出ない — 触った形跡として
        // キャッシュディレクトリが作られていないことを見る｡
        let file = std::env::temp_dir().join("twigpui-image-cache-local.png");
        std::fs::write(&file, b"not really a png").unwrap();
        let dir = std::env::temp_dir().join("twigpui-image-cache-untouched");
        let _ = std::fs::remove_dir_all(&dir);

        let resolved = ensure_cached(&dir, file.to_str().unwrap()).unwrap();

        assert_eq!(resolved, file);
        assert!(!dir.exists(), "a local file must not create the cache dir");
        std::fs::remove_file(&file).unwrap();
    }

    #[test]
    fn a_missing_local_file_is_an_error_naming_the_path() {
        // 無いローカルファイルを URL として fetch しにいってはならない:
        // 失敗の契約は今までどおり (WARN 1 行と枠のまま) で､その 1 行が
        // どのファイルかを言う｡
        let file = std::env::temp_dir().join("twigpui-image-cache-nope.png");
        let _ = std::fs::remove_file(&file);
        let dir = std::env::temp_dir().join("twigpui-image-cache-untouched-2");

        let error = ensure_cached(&dir, file.to_str().unwrap()).expect_err("no file, no path");

        assert!(
            format!("{error:#}").contains("twigpui-image-cache-nope.png"),
            "{error:#}"
        );
        assert!(!dir.exists());
    }

    #[test]
    fn only_http_urls_are_remote() {
        assert!(is_remote("https://pbs.twimg.com/a/b.jpg"));
        assert!(is_remote("http://example.com/a.png"));
        assert!(!is_remote("/Users/someone/fixtures/media/one.png"));
        assert!(!is_remote("fixtures/media/one.png"));
    }
}
