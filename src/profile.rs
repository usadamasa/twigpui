//! このバイナリが twigpui のどちらのインストールで *ある* か — 開発用の
//! ものか本物か (#169)｡
//!
//! 衝突しうる箇所ではすべて両者を分けてある: XDG のディレクトリ要素
//! (OAuth セッション､レスポンスキャッシュ､使用量の台帳が別ファイルになる)､
//! OAuth の loopback ポート (それぞれが自分の redirect URI を持ち､両方が
//! 同時に redirect を待てる)､そしてウィンドウタイトル (スクリーンショット
//! や Dock が､今見ているのがどちらかを示す)｡
//!
//! 選択はフラグでも環境変数でもなく `debug_assertions` によりコンパイル時
//! に決まる｡設計上守りたい失敗モードが *忘れること* だからだ｡`cargo run`
//! にフラグを付け忘れれば､開発ビルドが本物のアカウントにサインインし､
//! そのキャッシュを本物の上へ書いてしまう｡ここには同等の取りこぼしが無い｡
//! debug バイナリは release プロファイルのファイルを物理的に指せないからだ｡
//! その選択の代償は､リポジトリからの `cargo run --release` が本物の
//! プロファイルを使うことだ — `scripts/build-app-bundle.sh --dev` を見よ｡
//! インストール済みのように振る舞う開発ビルドが欲しいときに､これが debug
//! の `.app` を組む｡

/// 開発ビルドが読み､同期する List (#169)｡同じアカウント上の使い捨ての
/// リストなので､#161 の timeline や #163 の sync の作業が本当に読んでいる
/// List に触れることは無い｡
const DEV_LIST_ID: &str = "2091351590695588200";

/// サインイン中のユーザーがフォローしている全員の代わりに､開発時の
/// `--sync-list` がミラーするアカウント (#169)｡
///
/// フォローグラフ全体の読み取りは返ってきたアカウント単位で課金されるので､
/// 数千フォローに対する dry run はドル単位でかかる — sync 自体の作業中に
/// 払うには高すぎる｡この 4 つがその代役だ: X 自身のアカウントで､
/// ハードコードした screen name が知らないうちに別人へ解決され始めない
/// 程度には安定していて､開発時の sync の読み取り側が paginate した
/// クロールではなく月 4 回のキャッシュ済み lookup で済む程度には少ない｡
const DEV_SYNC_SEED: &[&str] = &["X", "XDevelopers", "Support", "Safety"];

/// このバイナリがどちらのインストールであるか｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Profile {
    /// 本物のインストール — release ビルドで､通常は
    /// `scripts/build-app-bundle.sh` が組んだ `.app` バンドル｡
    Release,
    /// 開発用のインストール — 素の `cargo run` を含む､あらゆる debug
    /// ビルド｡
    Dev,
}

impl Profile {
    /// このバイナリがコンパイルされたプロファイル｡
    pub(crate) fn current() -> Self {
        if cfg!(debug_assertions) {
            Self::Dev
        } else {
            Self::Release
        }
    }

    /// 各 XDG base directory の末尾に付くディレクトリ名 — `config.toml`､
    /// トークンストア､レスポンスキャッシュ､使用量の台帳が 2 つの
    /// プロファイル間で共有されるのを防いでいる唯一のものだ｡
    pub(crate) fn dir_component(self) -> &'static str {
        match self {
            Self::Release => "twigpui",
            Self::Dev => "twigpui-dev",
        }
    }

    /// OAuth の redirect が返ってくる loopback ポート｡X は redirect URI の
    /// 完全一致を要求するので､これは ephemeral にできない: 各プロファイルの
    /// ポートは Developer Portal でそれぞれの X app に対しそのまま登録して
    /// ある｡ポートが別であることは､開発時のサインインと本物のサインインが
    /// 同時に進行しても､一方の listener がもう一方の redirect を奪わないと
    /// いうことでもある｡
    pub(crate) fn loopback_port(self) -> u16 {
        match self {
            Self::Release => 8733,
            Self::Dev => 8734,
        }
    }

    /// ウィンドウのタイトルバーの文字列｡プロファイルごとに違えてあるので､
    /// スクリーンショットツールがタイトルでウィンドウを 1 つに絞れるし､
    /// 起動中の 2 つを一目で見分けられる｡
    pub(crate) fn window_title(self) -> &'static str {
        match self {
            Self::Release => "twigpui",
            Self::Dev => "twigpui (dev)",
        }
    }

    /// 画像 viewer ウィンドウ (#188) のタイトルバー文字列｡[`Self::window_title`]
    /// と同じ理由でプロファイルごとに違える — dev と release を並べて開いても
    /// どちらの viewer かが分かる｡
    pub(crate) fn photo_window_title(self) -> String {
        format!("{} — Photo", self.window_title())
    }

    /// `X_LIST_ID` も `config.toml` の `list_id` も List を指定しないときに
    /// このプロファイルが fallback する先の List (#161, #169)｡
    ///
    /// 既定値があるのは開発ビルドだけだ｡release ビルドが誰かの意図した
    /// List を推測する筋合いは無いし､ハードコードしたものへ fallback すれば
    /// 未設定のインストールで他人のリストを読むことになる｡開発ビルドが
    /// 使い捨てリストを既定にしていることが､`--sync-list` を「export を
    /// 1 つ忘れたら本物を書き換える」状態から遠ざけている｡
    pub(crate) fn default_list_id(self) -> Option<&'static str> {
        match self {
            Self::Release => None,
            Self::Dev => Some(DEV_LIST_ID),
        }
    }

    /// `--sync-list` が List へミラーするアカウント｡`None` ならサインイン
    /// 中のユーザーがフォローしている全員をミラーする (#163, #169)｡
    ///
    /// `Some` になるのは開発ビルドだけだ — sync の作業中に本物の
    /// フォローグラフを読むのが誤りである理由は [`DEV_SYNC_SEED`] を見よ｡
    pub(crate) fn sync_seed_usernames(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Release => None,
            Self::Dev => Some(DEV_SYNC_SEED),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DEV_LIST_ID, DEV_SYNC_SEED, Profile};

    #[test]
    fn the_two_profiles_never_share_a_directory() {
        // #169 の要点そのもの: 開発時の実行が本物のインストールの
        // トークン､キャッシュ､状態を読んだり上書きしたりできてはならない｡
        assert_ne!(
            Profile::Dev.dir_component(),
            Profile::Release.dir_component()
        );
    }

    #[test]
    fn the_two_profiles_never_share_a_loopback_port() {
        // 共有すれば redirect URI が同一になり､2 つの X app の登録を
        // 区別できなくなる — そして一方のプロファイルで始めたサインインに
        // もう一方の listener が応えてしまいうる｡
        assert_ne!(
            Profile::Dev.loopback_port(),
            Profile::Release.loopback_port()
        );
    }

    #[test]
    fn the_two_profiles_never_share_a_window_title() {
        assert_ne!(Profile::Dev.window_title(), Profile::Release.window_title());
    }

    #[test]
    fn the_two_profiles_never_share_a_photo_window_title() {
        // #188 の viewer が `window_title` と同じ invariant を持つことの確認｡
        assert_ne!(
            Profile::Dev.photo_window_title(),
            Profile::Release.photo_window_title()
        );
    }

    #[test]
    fn the_photo_window_title_names_no_url_or_path() {
        // タイトルバーは肩越しに一番読まれる場所なので (#188)､URL もパスも
        // クエリも出してはならない｡
        for profile in [Profile::Dev, Profile::Release] {
            let title = profile.photo_window_title();
            assert!(
                !title.contains('/') && !title.contains("http") && !title.contains('?'),
                "{title:?}"
            );
        }
    }

    #[test]
    fn the_release_profile_keeps_the_names_that_predate_this_split() {
        // どちらを変えても既存のインストールのファイルが迷子になり､
        // Developer Portal に登録済みの redirect URI が無効になる｡これらは
        // 既定値ではなく､構造を支えているリテラルだ｡
        assert_eq!(Profile::Release.dir_component(), "twigpui");
        assert_eq!(Profile::Release.loopback_port(), 8733);
        assert_eq!(Profile::Release.window_title(), "twigpui");
    }

    #[test]
    fn the_dev_profile_matches_what_the_developer_portal_is_registered_with() {
        assert_eq!(Profile::Dev.dir_component(), "twigpui-dev");
        assert_eq!(Profile::Dev.loopback_port(), 8734);
    }

    #[test]
    fn only_the_dev_profile_defaults_to_a_list() {
        // 何も設定されていない release ビルドは､他人のリストではなく
        // home timeline を読まなければならない｡
        assert_eq!(Profile::Release.default_list_id(), None);
        assert_eq!(Profile::Dev.default_list_id(), Some(DEV_LIST_ID));
    }

    #[test]
    fn only_the_dev_profile_syncs_from_a_fixed_seed() {
        // release の sync に本物のフォローグラフを読ませているのは `None`
        // だ｡ここを逆にすると､開発時の dry run に数千アカウント分を課金
        // するか､X の 4 アカウントを本物のリストへミラーするかの
        // どちらかになる｡
        assert_eq!(Profile::Release.sync_seed_usernames(), None);
        assert_eq!(Profile::Dev.sync_seed_usernames(), Some(DEV_SYNC_SEED));
    }

    #[test]
    fn the_dev_seed_is_small_enough_to_read_without_paging() {
        // 1 ページは 100 アカウント｡これを超えた seed は､避けるために
        // 存在している paginate した読み取りを黙って呼び戻してしまう｡
        assert!(
            !DEV_SYNC_SEED.is_empty() && DEV_SYNC_SEED.len() <= 100,
            "{DEV_SYNC_SEED:?}"
        );
    }

    #[test]
    fn the_dev_seed_holds_bare_screen_names() {
        // `user_id_by_username` 経由で解決する｡これは `@` の付かない､
        // URL に包まれていない名前を受け取る｡
        for username in DEV_SYNC_SEED {
            assert!(
                !username.is_empty()
                    && username
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "{username:?}"
            );
        }
    }

    /// 2 つのプロファイルが異なることだけでなく､対応そのものを固定する:
    /// `current` が逆になっていたら､普通の `cargo run` が本物のアカウントに
    /// サインインし､本物のインストールのキャッシュを上書きしてしまう｡
    /// 記述対象のビルドでのみコンパイルされるので､`cargo test --release`
    /// が意図どおりの挙動を失敗として報告することは無い｡
    #[test]
    #[cfg(debug_assertions)]
    fn a_debug_build_is_the_dev_profile() {
        assert_eq!(Profile::current(), Profile::Dev);
    }

    /// [`a_debug_build_is_the_dev_profile`] のもう半分 —
    /// `scripts/build-app-bundle.sh` が配布するのはこのビルドだ｡
    #[test]
    #[cfg(not(debug_assertions))]
    fn a_release_build_is_the_release_profile() {
        assert_eq!(Profile::current(), Profile::Release);
    }
}
