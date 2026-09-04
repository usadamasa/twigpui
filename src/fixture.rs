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
//!
//! ## timeline 以外を書くフィールド
//!
//! [`Fixture::lists`] (#164) と [`Fixture::sync`] (#205) は post ではない｡
//! それでも規則は同じで､どちらもアカウントとその同期の *状態* を書き､
//! widget を書かない｡本物の fetch や tick が通るのと同じ判断が､その状態から
//! 画面を決める｡

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
    /// "N new posts" の toast (#206) が差し出しているものだ｡
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
    /// list sync が置かれている状態 (#205)｡無ければ sync の行は出ない｡
    #[serde(default)]
    pub sync: Option<FixtureSync>,
    /// 起動時に on にする source の集合 (#43)｡空なら Home 1 件 —
    /// `saved_selection_for` は fixture の選択を無視するので (毎回同じ
    /// 画面という約束)、その代わりにここが初期値になる｡
    #[serde(default)]
    pub sources: Vec<crate::ui::source_picker::Selection>,
    /// list id → その list の post (#43)｡`items` は Home の post のまま —
    /// 合成レーンの出自表示 (list 由来の post だけに list 名) を撮るには
    /// list ごとの post が要る｡
    #[serde(default)]
    pub list_items: std::collections::BTreeMap<String, Vec<TimelineItem>>,
    /// 起動直後に source picker のドロップダウンを開いた状態にするか (#43,
    /// #192)｡widget の開閉状態を fixture に持つのはこのモジュールの規則の
    /// 例外だが、`--fixture` の窓はクリックを合成する手段が無く、開いた
    /// 状態を撮るにはここで宣言する以外に道が無い｡
    #[serde(default)]
    pub picker_open: bool,
    /// いいね済みとして描く post id (#156)｡`toggle::load_all` の永続ファイルを
    /// 読む代わりに､fixture が直接そう言う｡撮る画面は毎回同じでなければ
    /// ならないので､手元の状態ファイルに依存させられない｡
    #[serde(default)]
    pub liked: Vec<String>,
    /// repost 済みとして描く post id (#156)｡[`Fixture::liked`] と同じ｡
    #[serde(default)]
    pub reposted: Vec<String>,
}

/// フィクスチャが言う list sync の状態 (#205)｡
///
/// 書けるのは sync が何を負っていて何に拒まれているかまで｡行を出すかも
/// 文言も､本物の tick が通るのと同じ判断が決める｡「行を出せ」と書けると､
/// 本物の sync にはありえない画面をフィクスチャが描ける｡
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct FixtureSync {
    /// ディスク上の計画がまだ負っているメンバーシップ変更の件数｡
    pub pending: usize,
    /// 拒否が明けるまでの秒数｡0 なら拒否されていない｡
    ///
    /// 絶対時刻ではなく起動からの相対秒｡ファイルに書いた unix 時刻は翌日には
    /// 過去になり､毎回違う画面が出てしまう｡
    #[serde(default)]
    pub blocked_for_seconds: i64,
    /// 上限が続けて何回 no と言ったか (#197)｡2 回以上で文言と色が変わる｡
    #[serde(default)]
    pub refusals: u32,
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
    let mut fixture: Fixture = serde_json::from_str(&contents)
        .with_context(|| format!("could not parse the fixture {}", path.display()))?;

    // #234: 画像は fixture の隣のファイルから読む｡相対パスの基準は cwd
    // ではなく fixture 自身の場所 — `cargo run` の cwd がどこであっても
    // 同じ画像を指すように｡`load` の時点で絶対化しておけば､描く側は本物
    // の URL とローカルのパスを見分けずに済む (`image_cache::ensure_cached`)｡
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    for item in fixture.items.iter_mut().chain(fixture.pending.iter_mut()) {
        if let Some(avatar) = item.author_avatar_url.as_mut() {
            resolve_image(base, avatar);
        }
        for media in &mut item.media {
            resolve_image(base, &mut media.url);
        }
        if let Some(quoted) = item.quoted.as_mut() {
            for media in &mut quoted.media {
                resolve_image(base, &mut media.url);
            }
        }
    }
    Ok(fixture)
}

/// fixture からの相対パスを `base` 基準の絶対パスに書き換える｡URL と
/// 絶対パスはそのまま｡
fn resolve_image(base: &Path, image: &mut String) {
    if crate::image_cache::is_remote(image) || Path::new(image.as_str()).is_absolute() {
        return;
    }
    *image = base.join(image.as_str()).to_string_lossy().into_owned();
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
        let lone = |item: &TimelineItem| item.media.len() == 1;
        assert!(
            fixture.items.iter().any(|item| {
                lone(item)
                    && item.media.iter().all(|media| {
                        media.width.is_some_and(|width| width <= 128)
                            && media.height.is_some_and(|height| height <= 72)
                    })
            }),
            "a lone photo small enough to have to grow (#256)"
        );
        assert!(
            fixture.items.iter().any(|item| {
                lone(item)
                    && item
                        .media
                        .iter()
                        .all(|media| media.width.zip(media.height).is_some_and(|(w, h)| h > w))
            }),
            "a lone portrait photo, for the max height (#256)"
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
        // #205: 行に出せるいちばん込み入った状態｡件数も連続回数も JST の
        // 解除予定も 1 行に載るので､22px に収まるかを目で確かめられる｡
        let sync = fixture
            .sync
            .as_ref()
            .expect("a sync state, or there is no row to look at (#205)");
        assert!(sync.pending > 0, "a row with nothing owed does not appear");
        assert!(
            sync.blocked_for_seconds > 0 && sync.refusals >= 2,
            "the busiest label: a stuck catch-up counting down to a JST hour"
        );
        // #205: ダイアログの list 名は所有 list のキャッシュ (#164) からしか
        // 来ない｡フィクスチャはそのキャッシュを丸ごと置き換えるので､ミラー先が
        // 無いとダイアログが id へ落ちる｡`cargo run --fixture` は debug ビルド
        // なので､突き合わせる相手は dev プロファイルの既定の list｡
        let mirrored = crate::profile::Profile::Dev
            .default_list_id()
            .expect("the dev profile mirrors into a list");
        assert!(
            fixture
                .lists
                .iter()
                .any(|list| list.id == mirrored && !list.name.is_empty()),
            "the list the dev profile syncs into has to be among the cached \
             lists with a name, or the sync dialog shows a bare id (#205)"
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

    /// #156: `metrics` の JSON キーは `PostMetrics` の `rename` (API の
    /// `reply_count`/`retweet_count`/`like_count`) であって､この構造体
    /// 自身のフィールド名 (`replies`/`reposts`/`likes`) ではない｡
    /// `deny_unknown_fields` が無いので､書き間違えたキーはエラーになら
    /// ず静かに 0 になる — この assert が無いと action row の件数が
    /// 1 つも出ない状態のまま気づけない (実際に一度そうなった)｡
    #[test]
    fn the_bundled_fixture_has_a_non_zero_engagement_count() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/timeline.json");
        let fixture = load(&path).expect("fixtures/timeline.json must load");

        assert!(
            fixture
                .items
                .iter()
                .any(|item| item.metrics.as_ref().is_some_and(|metrics| {
                    metrics.replies > 0 || metrics.reposts > 0 || metrics.likes > 0
                })),
            "at least one row must carry a non-zero engagement count, or the \
             metrics JSON keys have drifted from PostMetrics's `rename`s (#156)"
        );
    }

    #[test]
    fn relative_image_paths_resolve_against_the_fixture_directory() {
        // #234: fixture が持ち込む画像はファイルの隣に置く｡書くのは
        // fixture からの相対パスで､読んだ側がそれを絶対パスにする —
        // `cargo run` の cwd がどこであっても同じファイルを指すように｡
        let path = write(
            "relative-images",
            r#"{
              "signed_in_as": { "id": "1", "username": "me" },
              "items": [
                {
                  "id": "1", "text": "with images",
                  "author_name": "A", "author_username": "a",
                  "author_avatar_url": "media/avatar.png",
                  "media": [{ "url": "media/one.png" }],
                  "quoted": {
                    "author_name": "B", "author_username": "b", "text": "q",
                    "media": [{ "url": "media/quoted.png" }]
                  }
                }
              ],
              "pending": [
                {
                  "id": "2", "text": "pending",
                  "author_name": "C", "author_username": "c",
                  "author_avatar_url": "https://pbs.twimg.com/profile_images/1/c_normal.jpg",
                  "media": [{ "url": "/absolute/stays.png" }]
                }
              ]
            }"#,
        );
        let fixture = load(&path).unwrap();
        let base = path.parent().unwrap();

        let item = &fixture.items[0];
        assert_eq!(
            item.author_avatar_url.as_deref(),
            base.join("media/avatar.png").to_str()
        );
        assert_eq!(
            item.media[0].url,
            base.join("media/one.png").to_str().unwrap()
        );
        assert_eq!(
            item.quoted.as_ref().unwrap().media[0].url,
            base.join("media/quoted.png").to_str().unwrap()
        );
        // URL と絶対パスはそのまま｡
        let pending = &fixture.pending[0];
        assert_eq!(
            pending.author_avatar_url.as_deref(),
            Some("https://pbs.twimg.com/profile_images/1/c_normal.jpg")
        );
        assert_eq!(pending.media[0].url, "/absolute/stays.png");

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn the_bundled_fixture_never_reaches_for_the_network() {
        // #234: fixture 起動は課金なしで毎回同じ画面を出すためにある｡
        // 画像 1 枚でもネットワークへ出ると､オフラインでは別の画面になり､
        // 本番のログに fixture 由来の WARN が混ざる｡
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/timeline.json");
        let fixture = load(&path).expect("fixtures/timeline.json must load");

        let images = fixture
            .items
            .iter()
            .chain(fixture.pending.iter())
            .flat_map(|item| {
                item.author_avatar_url
                    .iter()
                    .map(String::as_str)
                    .chain(item.media.iter().map(|media| media.url.as_str()))
                    .chain(
                        item.quoted
                            .iter()
                            .flat_map(|quoted| quoted.media.iter().map(|media| media.url.as_str())),
                    )
            });
        let mut seen = 0;
        for image in images {
            seen += 1;
            assert!(
                !crate::image_cache::is_remote(image),
                "{image} would be fetched over the network"
            );
            assert!(
                Path::new(image).is_file(),
                "{image} is not a file next to the fixture"
            );
        }
        assert!(seen >= 7, "the fixture is meant to show images, saw {seen}");
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
