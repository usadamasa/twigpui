//! X からではなくファイルから読んだ timeline (#146)｡
//!
//! ウィンドウを埋める手段はこれまで 2 つしか無かった: レスポンス
//! キャッシュからか､課金されるリクエストからか｡どちらも本物のアカウント
//! に依存し､どちらも再現性が無い — 行は実行のたびに変わるので､
//! 「レイアウトは正しいか?」を問う相手が定まらなかった｡UI の確認は
//! ことごとく #115 に､人間が演じるための一文として積み上がった｡
//!
//! フィクスチャが 3 つめの手段だ: どの post を描くかをそのまま書いた
//! ファイル｡認証情報も要らず､リクエストも飛ばず､毎回同じ画面が出る｡
//!
//! ## 意図してそうではないもの
//!
//! API の mock ではない｡運ぶのは [`TimelineItem`] — parser がすでに生成し､
//! renderer がすでに消費する型 — なので､本物の join が作れない timeline を
//! 記述することはできない｡X が返すものから離れたフィクスチャは､renderer を
//! 作り話に対して試すことになる｡
//!
//! `--fetch-only` の代わりにもならない｡あちらは *ネットワーク* の経路が
//! 動くことを示すために存在し､こちらはネットワークを止めておくために存在する｡

use std::path::Path;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::x_api::{ListSummary, TimelineItem};

/// フィクスチャがサインインしていると言っている人物｡
///
/// アプリが自分の id を知るまで差し出されない操作がいくつかあるので必要だ
/// — repost ボタンは自分の post には出ないし (#15)､削除は自分の post に
/// しか出ない (#72)｡これが無いとフィクスチャは誰も見ていない timeline を
/// 描くことになり､まさに閲覧者ごとに変わる行こそが欠ける｡
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct FixtureUser {
    pub id: String,
    pub username: String,
}

/// フィクスチャファイルの全内容｡
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct Fixture {
    pub signed_in_as: FixtureUser,
    /// このクレートの他の場所と同じく､新しい順｡
    pub items: Vec<TimelineItem>,
    /// poll が取得済みでまだ表示していない post (#21)､新しい順 —
    /// "N new posts" のバーが差し出しているものだ｡
    ///
    /// 素の件数ではなく本物の [`TimelineItem`] なのは､このモジュール自身の
    /// 規則による: フィクスチャが記述するのは timeline であって､widget では
    /// 決してない｡だからバーは本物の poll のものと同じようにこれらを数え､
    /// 押せばまさにこれらの行が先頭に付く — 静止状態だけでなく､操作も
    /// 確かめられる｡
    ///
    /// 空 (およびフィクスチャファイルに記載が無い) ならバーは出ない｡この
    /// フィールドができる前に書かれたフィクスチャはすべてそうだ｡
    #[serde(default)]
    pub pending: Vec<TimelineItem>,
    /// toolbar の picker が挙げるリスト (#164)｡所有リストのキャッシュが
    /// 保持するのと同じ形だ｡ここでも本物の [`ListSummary`] なのはこの
    /// モジュールの規則による: フィクスチャはアカウントの所有物を記述し､
    /// セグメントは fetch から描かれるのと同じようにそこから描かれる｡
    ///
    /// 空 (および記載が無い) なら picker には Home しか入らない｡この
    /// フィールドができる前に書かれたフィクスチャはすべてそうだ｡
    #[serde(default)]
    pub lists: Vec<ListSummary>,
}

/// フィクスチャファイルを読んでパースする｡
///
/// `cache::load_json` と違い､fallback せずにエラーにする｡キャッシュミスは
/// 「取り直せ」の意味だし､壊れたキャッシュファイルがアプリを止めては
/// ならない｡一方フィクスチャはコマンドラインで明示的に指定されたものだ｡
/// 代わりに黙って空のウィンドウを開けば､誰も訊いていない問いに答えて
/// しまう｡
pub(crate) fn load(path: &Path) -> Result<Fixture> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("could not read the fixture {}", path.display()))?;
    serde_json::from_str(&contents)
        .with_context(|| format!("could not parse the fixture {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "signed_in_as": { "id": "5685672", "username": "usadamasa" },
      "items": [
        {
          "id": "1",
          "text": "a post",
          "created_at": "2026-08-16T09:00:00.000Z",
          "author_name": "Developers",
          "author_username": "XDevelopers"
        }
      ]
    }"#;

    fn write(name: &str, contents: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("twigpui-fixture-{name}.json"));
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn reads_a_fixture() {
        let path = write("ok", SAMPLE);
        let fixture = load(&path).unwrap();

        assert_eq!(fixture.signed_in_as.username, "usadamasa");
        assert_eq!(fixture.items.len(), 1);
        assert_eq!(fixture.items[0].text, "a post");

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_fixture_omitting_the_optional_fields_still_parses() {
        // #9 以降に足したフィールドはすべて `TimelineItem` 上で
        // `#[serde(default)]` になっている｡これがフィクスチャを読みやすい
        // ままにしている: 扱う case だけを書き､それ以外は書かない｡
        let path = write("minimal", SAMPLE);
        let fixture = load(&path).unwrap();

        assert!(fixture.items[0].media.is_empty());
        assert!(fixture.items[0].quoted.is_none());
        assert!(fixture.items[0].author_avatar_url.is_none());
        // #21 のフィールドが `#[serde(default)]` なのも同じ理由だ: それが
        // できる前に書かれたフィクスチャはすべて読めつづけねばならないし､
        // 記載が無いことは「pending な post は無い」と読むのが正しい｡
        assert!(fixture.pending.is_empty());

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_missing_fixture_is_an_error_and_names_the_path() {
        let path = std::env::temp_dir().join("twigpui-fixture-nope.json");
        let error = load(&path).expect_err("a missing fixture must not be silently empty");

        assert!(
            format!("{error:#}").contains("twigpui-fixture-nope.json"),
            "the error has to say which file: {error:#}"
        );
    }

    #[test]
    fn the_bundled_fixture_parses_and_covers_what_it_claims_to() {
        // 読み込めなくなったフィクスチャは無いより悪い: 先に確かめずに
        // 手を伸ばせることが要点だからだ｡見せるために存在している case も
        // ここで固定しているので､後の編集が誰かの頼っていた行を黙って
        // 落とせない｡
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/timeline.json");
        let fixture = load(&path).expect("fixtures/timeline.json must load");

        assert!(
            fixture.items.iter().any(|item| item.text.len() > 200),
            "a row long enough to have to wrap (#140)"
        );
        assert!(
            fixture.items.iter().any(|item| item.media.len() == 4),
            "a row with a full media grid (#65)"
        );
        assert!(
            fixture
                .items
                .iter()
                .any(|item| item.quoted.as_ref().is_some_and(|q| !q.media.is_empty())),
            "a quote whose card carries an image (#123)"
        );
        assert!(
            fixture
                .items
                .iter()
                .any(|item| item.reposted_by.is_some() && !item.media.is_empty()),
            "a repost carrying the original's image (#104)"
        );
        assert!(
            fixture.items.iter().any(|item| item.replied_to.is_some()),
            "a reply, for the thread toggle (#12)"
        );
        assert!(
            fixture
                .items
                .iter()
                .any(|item| item.author_username == fixture.signed_in_as.username),
            "one of one's own posts, the only row offering Delete (#72)"
        );
        assert!(
            fixture.pending.len() > 1,
            "posts waiting behind the new-posts bar, more than one so the \
             plural wording is what gets drawn (#21)"
        );
        assert!(
            fixture.lists.len() > 1,
            "lists for the picker, more than one so the trough has \
             unselected segments beside Home (#164)"
        );
        // バーは表示中のものに対して新着を数えるので､すでに `items` に
        // ある pending な post は数えられたうえで何も現さない — 見せる
        // ために書かれたものを黙って見せなくなるフィクスチャだ｡
        for pending in &fixture.pending {
            assert!(
                !fixture.items.iter().any(|item| item.id == pending.id),
                "pending post {} is already in the timeline (#21)",
                pending.id
            );
        }
    }

    #[test]
    fn a_malformed_fixture_is_an_error() {
        // 手で編集したフィクスチャが実際に起こす失敗｡空の timeline へ
        // fallback すると､アプリが動いているように見えてしまう｡
        let path = write("broken", r#"{ "signed_in_as": {} }"#);
        assert!(load(&path).is_err());

        std::fs::remove_file(&path).unwrap();
    }
}
