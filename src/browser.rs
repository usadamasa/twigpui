//! URL をユーザーのブラウザで開く (#70)｡
//!
//! ここで唯一自明でない規則は **シェルを一切経由しない** ことだ｡
//! `open(1)` は [`std::process::Command`] から直接呼び､引数ベクタで
//! `exec` する — `sh -c` を挟まないので､元をたどれば post のテキスト
//! から来た URL に対してクォート・グロブ・単語分割が起きる余地がない｡
//! このクレートは `unsafe_code` を forbid しているので `NSWorkspace` は
//! どのみち選べない｡これが issue の求めるまっすぐな経路だ｡
//!
//! [`is_openable`] はこのモジュールのテストカバレッジを担う純粋な継ぎ目で､
//! そもそも何を `open` に到達させてよいかを決める｡[`open`] 自体は
//! プロセスを spawn するだけだ｡

use std::process::Command;

use anyhow::{Result, bail};

/// `url` を `open(1)` に渡してよいかどうか｡
///
/// 通るのは `http://` と `https://` だけだ｡これは飾りではない: `open` は
/// ローカルのパスも､他のアプリが登録した任意の `x-…://` スキームも､
/// そして何より先頭の `-` も平気で受け取る — `-` は対象ではなく `open`
/// 自身のフラグとして読まれる｡post のテキストも API から来る
/// `expanded_url` もどちらも *リモート入力* なので､許可する集合を
/// 肯定形で (この 2 つのスキーム) 述べる｡拒否するものを並べる形は取らない｡
///
/// スキームの一致は RFC 3986 §3.1 に従って大文字小文字を区別せず､
/// スキームの後ろに何かが続いていなければならない — 素の `https://` は
/// 何も有用なものを開かず､意図というよりパースの事故である公算が高い｡
pub(crate) fn is_openable(url: &str) -> bool {
    let Some(rest) = strip_scheme(url) else {
        return false;
    };
    !rest.is_empty()
}

/// `url` の `http://` または `https://` より後ろの部分｡どちらのスキームも
/// 持たなければ `None`｡
/// 比較も分割も `str` の範囲ではなく **バイト** で行う｡`url[..8]` はバイト
/// 範囲で､8 がたまたま文字境界でなければ panic する｡`"https:/\u{3042}"` が
/// まさにその入力で､リモート入力が panic に届いてしまう (#47,
/// `clippy::string_slice` が発見)｡先にプレフィックスのバイトを比べておくと､
/// 続く `get` も実質的に失敗しなくなる: 先頭 `scheme.len()` バイトが ASCII と
/// 分かった時点で､その位置は文字境界だからだ｡
fn strip_scheme(url: &str) -> Option<&str> {
    for scheme in ["https://", "http://"] {
        // `?` ではなく `continue`: 試しているスキームより短い URL は
        // 次のスキームへ落ちるべきで､探索全体を終わらせてはいけない —
        // さもないと結果がこのリストの順序に左右される｡
        let Some(prefix) = url.as_bytes().get(..scheme.len()) else {
            continue;
        };
        if prefix.eq_ignore_ascii_case(scheme.as_bytes()) {
            return url.get(scheme.len()..);
        }
    }
    None
}

/// [`is_openable`] が許可した後で､`url` を `open(1)` 経由でシステムの
/// ブラウザに渡す｡
///
/// 待たずに spawn する: アプリは `open` の終了ステータスに用がないし､
/// gpui のクリックハンドラを別プロセスで止めるのは､#57 が他の箇所で
/// 手間をかけて取り除いたのと同じ種類の停滞だ｡拒否した URL は黙って
/// 何もしないのではなくエラーにする｡そうすればクリックが無反応に見える
/// 代わりに `ui.rs` が何かを言える｡
///
/// ユニットテストはしない — 実プロセスを起こすからだ｡カバレッジは
/// [`is_openable`] が担う｡`cache::reload` や `repost::create` が
/// すでに従っている慣習に倣っている｡
pub(crate) fn open(url: &str) -> Result<()> {
    if !is_openable(url) {
        bail!("refusing to open a non-http(s) URL: {url}");
    }
    // シェル文字列ではなく `arg`: `open` はこれをそのまま argv[1] として
    // exec されるので､URL の中身が構文として解釈されることはない｡
    Command::new("open")
        .arg(url)
        .spawn()
        .map(|_child| ())
        .map_err(|error| anyhow::anyhow!("could not launch the browser: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_an_ordinary_https_url() {
        assert!(is_openable(
            "https://x.com/XDevelopers/status/1700000000000000001"
        ));
    }

    #[test]
    fn allows_http_as_well_as_https() {
        assert!(is_openable("http://example.com/"));
    }

    #[test]
    fn the_scheme_match_is_case_insensitive() {
        assert!(is_openable("HTTPS://example.com/"));
    }

    #[test]
    fn refuses_a_bare_scheme_with_nothing_after_it() {
        assert!(!is_openable("https://"));
    }

    #[test]
    fn refuses_a_leading_dash_that_open_would_read_as_a_flag() {
        // そもそもこの検査がある理由: `open -a Calculator` は URL ではなく
        // コマンドで､post のテキストはリモート入力だ｡
        assert!(!is_openable("-a Calculator"));
    }

    #[test]
    fn refuses_a_local_path() {
        assert!(!is_openable("/etc/passwd"));
        assert!(!is_openable("file:///etc/passwd"));
    }

    #[test]
    fn refuses_an_unregistered_or_app_specific_scheme() {
        assert!(!is_openable("javascript:alert(1)"));
        assert!(!is_openable("x-apple-something://do-a-thing"));
    }

    #[test]
    fn a_multi_byte_character_inside_the_scheme_does_not_panic() {
        // バグ報告ではなく `clippy::string_slice` (#47) が見つけた:
        // `url[..8]` はバイト範囲で､"https:/あ" は 10 バイト､文字境界は
        // 7 と 10 — なので 8 で切ると以前は panic していた｡
        // post のテキストはリモート入力なので､ここには到達しうる｡
        assert!(!is_openable("https:/\u{3042}"));
        assert!(!is_openable("http:/\u{3042}"));
    }

    #[test]
    fn a_url_shorter_than_the_longest_scheme_still_matches_its_own() {
        // "http://a" は 8 バイトで "https://" も 8 バイトなので､
        // スキームの探索は､長さすら足りない最初のスキームで止まらずに
        // 次へ落ちなければならない｡
        assert!(is_openable("http://a"));
    }

    #[test]
    fn refuses_empty_input() {
        assert!(!is_openable(""));
    }

    #[test]
    fn refuses_a_scheme_that_only_appears_later_in_the_string() {
        // 先頭で固定して照合するので､これはたまたま URL に言及している
        // だけの文字列であって URL ではない｡
        assert!(!is_openable("not-a-url https://example.com"));
    }
}
