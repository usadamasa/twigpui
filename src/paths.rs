//! twigpui が永続化するファイルの XDG Base Directory パス｡
//!
//! [`Paths`] は 3 つのベースディレクトリを起動時に一度だけ解決する｡今後の
//! 慣習として､twigpui が永続化するファイルはそれぞれここに専用のアクセサを
//! 持つ ([`Paths::settings_file`] のように) — 呼び出し側がパスを自分で
//! 結合することは無い｡アクセサは必要とするファイルが入るたびに少しずつ
//! 足す: OAuth トークンストア ([`Paths::oauth_token_file`], #7)､レスポンス
//! キャッシュ (#9)､パネルレイアウト (#24) は､ここで先回りするのではなく
//! それぞれの issue で自分のアクセサを足す｡
//!
//! 各ベースディレクトリに何を付け足すかは [`crate::profile::Profile`]
//! (#169) が決めるので､開発ビルドと本物のインストールが互いのファイルを
//! 読んだり上書きしたりすることは決してない｡

use anyhow::{Context as _, Result};
use std::path::{Path, PathBuf};

use crate::profile::Profile;

/// twigpui が書き込む先の 3 つの XDG Base Directory｡それぞれに実行中の
/// プロファイルのディレクトリ名を付け足してある｡
// 共通の `_dir` という接尾辞は冗長ではなく狙いである — 各フィールドが何で
// あるか (ディレクトリ) を､どの XDG カテゴリを解決するかと並べて示す｡
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone)]
pub(crate) struct Paths {
    config_dir: PathBuf,
    cache_dir: PathBuf,
    state_dir: PathBuf,
    /// この 3 つがどのインストールのものか (#169)｡`Paths` を持つ呼び出し側
    /// — ウィンドウタイトルや OAuth のリダイレクト URI — が自分で導出し
    /// 直して､実際に使われているディレクトリと食い違う危険を負わずに済む
    /// よう､並べて持たせてある｡
    profile: Profile,
}

impl Paths {
    pub(crate) fn from_env() -> Result<Self> {
        Self::from_vars(|key| std::env::var(key).ok())
    }

    /// この `Paths` が解決したディレクトリのプロファイル (#169)｡
    pub(crate) fn profile(&self) -> Profile {
        self.profile
    }

    /// 任意の変数引きから 3 つのディレクトリを解決する｡
    ///
    /// [`Paths::from_env`] から切り出してあるのは､`set_var` 無しで解決規則
    /// をテストするため｡`set_var` は `unsafe` で､他のテストスレッドと競合
    /// する｡[`crate::config::Config`] が使う分け方に倣っている｡
    ///
    /// private ではなく `pub(crate)` なのは､`oauth::tokens` 自身のテストも
    /// スクラッチディレクトリを指す `Paths` を必要とし､これが `paths.rs`
    /// 自身のテストがすでに使っているのと同じ seam だからである｡
    pub(crate) fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Self> {
        Self::for_profile(var, Profile::current())
    }

    /// 任意のプロファイルについて 3 つのディレクトリを解決する (#169)｡
    ///
    /// プロファイルを明示的に名指すのは下のテストだけである — アプリ内の
    /// 呼び出し側はすべて [`Paths::from_env`] を通り､このバイナリが
    /// コンパイルされたときのプロファイルを使う｡この seam が無ければ､
    /// テストはたまたま自分が動いているプロファイルしか観測できず､それは
    /// もっとも意味の薄い assertion である｡
    pub(crate) fn for_profile(
        var: impl Fn(&str) -> Option<String>,
        profile: Profile,
    ) -> Result<Self> {
        let component = profile.dir_component();
        let config_dir = resolve_dir(&var, "XDG_CONFIG_HOME", ".config", component)?;
        let cache_dir = resolve_dir(&var, "XDG_CACHE_HOME", ".cache", component)?;
        let state_dir = resolve_dir(&var, "XDG_STATE_HOME", ".local/state", component)?;
        Ok(Self {
            config_dir,
            cache_dir,
            state_dir,
            profile,
        })
    }

    /// `config_dir` 配下の `config.toml` 設定ファイルへのパス｡
    pub(crate) fn settings_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// `state_dir` 配下の OAuth トークンストアへのパス｡
    /// [`crate::oauth::tokens::save`] が `0600` で書く — config ではなく
    /// state なのは､手で編集する設定ではなく認証情報を持つからである｡
    pub(crate) fn oauth_token_file(&self) -> PathBuf {
        self.state_dir.join("oauth_tokens.json")
    }

    /// `cache_dir` 配下の screen name → user id キャッシュへのパス (#9)｡
    /// user id は事実上恒久なので､これだけキャッシュする ([`crate::cache`]
    /// が TTL を付ける) だけでリロードの 2 リクエストが 1 つになる｡
    pub(crate) fn user_ids_file(&self) -> PathBuf {
        self.cache_dir.join("user_ids.json")
    }

    /// `cache_dir` 配下の､1 ユーザー分のキャッシュ済み timeline へのパス
    /// (#9)｡1 つの共有ファイルではなくユーザーごとに分けてあるので､#24 で
    /// 増えるパネルが競合せずそれぞれのキャッシュファイルを育てられる｡
    pub(crate) fn timeline_file(&self, user_id: &str) -> PathBuf {
        self.cache_dir.join(format!("timeline-{user_id}.json"))
    }

    /// `cache_dir` 配下の､1 ユーザー分のキャッシュ済み *home* timeline への
    /// パス (#11)｡同じ `user_id` に対して [`Self::timeline_file`] とは意図的
    /// に別のファイル名にしてある: home timeline と単一ユーザーの timeline は
    /// 中身が違うので､両方のモードを動かしたユーザー (例えばサインアウトして
    /// bearer token で入り直した場合) の一方が他方を黙って上書きしては
    /// ならない｡
    pub(crate) fn home_timeline_file(&self, user_id: &str) -> PathBuf {
        self.cache_dir.join(format!("home-timeline-{user_id}.json"))
    }

    /// `cache_dir` 配下の､1 つの list のキャッシュ済み timeline へのパス
    /// (#161)｡サインイン中のユーザーではなく list id をキーにするのは､中身が
    /// そちらに属するからである: 2 つのアカウントが同じ list を読めば同じ
    /// post を読むし､1 つのアカウントが 2 つの list を読むとき後者が前者を
    /// 上書きしてはならない — #164 の switcher は 1 セッションに何度もそれを
    /// やる｡
    ///
    /// [`Self::timeline_file`] や [`Self::home_timeline_file`] と別の
    /// ファイル名にしてあるのは､その 2 つが互いに違うのと同じ理由による:
    /// list id も user id も裸の数字なので､接頭辞が無いと list は､たまたま
    /// id が一致したユーザーと区別がつかなくなる｡
    pub(crate) fn list_timeline_file(&self, list_id: &str) -> PathBuf {
        self.cache_dir.join(format!("list-timeline-{list_id}.json"))
    }

    /// `state_dir` 配下の､#163 の sync plan へのパス｡
    ///
    /// cache ではなく state である: 失って高くつくのは安い再取得ではなく､
    /// フォロー一覧と list のメンバーの両方を再びページングすることであり､
    /// これはこのアプリが行うもっとも高価な読み取りの組である｡どの項目を
    /// すでに送ったかも記録しているので､再構築できるとして捨てると適用済み
    /// の書き込みがすべて再発火する｡
    pub(crate) fn sync_plan_file(&self) -> PathBuf {
        self.state_dir.join("sync_plan.json")
    }

    /// `state_dir` 配下の､バックグラウンド sync の時計へのパス: 最後に差分
    /// を試みた時刻｡
    ///
    /// plan のフィールドではなく [`Self::sync_plan_file`] と別ファイルなの
    /// は､plan が完全に適用された瞬間に消されるからである — まさにそのとき
    /// 時計にはまだ 6 時間残っている｡plan と同じ理由で state である: 失うと
    /// 次の起動が間隔を待たずに､両方の full read をただちに払うことになる｡
    pub(crate) fn sync_state_file(&self) -> PathBuf {
        self.state_dir.join("sync_state.json")
    }

    /// `GET /2/users/me` (#11) のキャッシュ済み結果へのパス: サインイン中の
    /// ユーザー自身の id と screen name｡`cache_dir` 配下｡アカウントが決まれ
    /// ば不変なので､キャッシュすれば (#9 が screen name → id をキャッシュ
    /// するのと同じく) 起動のたびにリクエストを払い直さずに済む｡
    pub(crate) fn me_file(&self) -> PathBuf {
        self.cache_dir.join("me.json")
    }

    /// `GET /2/users/:id/owned_lists` (#164) のキャッシュ済み結果へのパス:
    /// picker が並べる list｡`cache_dir` 配下｡state ではなく cache なのは
    /// いつもの理由による — 失って高くつくのはリクエスト 1 回で､それは
    /// picker 自身の再取得がどのみち払う分である｡
    pub(crate) fn owned_lists_file(&self) -> PathBuf {
        self.cache_dir.join("owned-lists.json")
    }

    /// picker の永続化された選択へのパス (#164): ウィンドウがどの timeline
    /// で開くか｡`state_dir` 配下｡
    ///
    /// 失っても安いが cache ではなく state である: ネットワーク上の何も
    /// これを返してくれないし､`cache_dir` はキャッシュの形が古くなったとき
    /// 消すよう案内されるディレクトリである — 設定がそれに巻き込まれては
    /// ならない｡
    pub(crate) fn selection_file(&self) -> PathBuf {
        self.state_dir.join("selection.json")
    }

    /// `cache_dir` 配下の､1 つの返信 post についてキャッシュした親チェーン
    /// へのパス (#12)｡*返信自身の* id — "Show thread" をクリックした元の
    /// post — をキーにするので､同じ返信を開き直すと最大
    /// [`crate::thread::MAX_THREAD_DEPTH`] 回のリクエストを払い直さず､
    /// 辿り済みのチェーンを描画する｡
    pub(crate) fn thread_file(&self, reply_post_id: &str) -> PathBuf {
        self.cache_dir.join(format!("thread-{reply_post_id}.json"))
    }

    /// `state_dir` 配下の､追跡しているレートリミット状態へのパス (#10)｡
    /// cache ではなく state である: プロセスの再起動で X のレートリミット窓
    /// はリセットされないので､このファイルを失うと､キャッシュ項目を失った
    /// ときのようにコールドスタートが遅くなるだけでは済まず､すでに枯れた窓
    /// にリクエストを撃ち込んで有料のリクエストを無駄にする危険がある｡
    pub(crate) fn rate_limit_file(&self) -> PathBuf {
        self.state_dir.join("rate_limit.json")
    }

    /// `state_dir` 配下の､エンドポイントごとに追跡しているリクエスト数の
    /// 利用状況へのパス (#18)｡cache ではなく state である: レスポンス
    /// キャッシュと違い､このファイルを失うとコールドスタートが遅くなるだけ
    /// では済まない — 累積の支出履歴そのものを失う｡そもそもそれを追跡する
    /// のが目的だったのに｡
    pub(crate) fn usage_file(&self) -> PathBuf {
        self.state_dir.join("usage.json")
    }

    /// `state_dir` 配下の､サインイン中のユーザーがこのアプリから repost した
    /// post id のローカルな記録へのパス (#15)｡cache ではなく state である:
    /// X API v2 の timeline レスポンスには「自分がこれを repost したか」を
    /// 表すフィールドが無い (v1.1 の `retweeted` に当たるものが無い) ので､
    /// repost ボタンの初期状態について twigpui が持つ *唯一の* 真実の源が
    /// このファイルである — コールドスタートが遅くなるだけのキャッシュ項目
    /// の喪失と違い､これを失うと喪失前に repost した post がすべて「未
    /// repost」に見え､次のクリックで二重に repost する危険がある (#15 自身の
    /// エラー突き合わせで回復できるが､黙って無害というわけではない)｡
    pub(crate) fn reposted_posts_file(&self) -> PathBuf {
        self.state_dir.join("reposted_posts.json")
    }

    /// `cache_dir` 配下の､ダウンロードしたアバター画像を置くディレクトリ
    /// (#64)｡トップレベルにキーごとのファイルを置くのではなくディレクトリに
    /// するのは､活発な timeline ではこれが数百たまるからで､まとめておけば
    /// ユーザーにとって「アバターを消す」が `rm -r` 一発になる｡state では
    /// なく cache である: 失って高くつくのは著者ごとの再ダウンロード 1 回だけ｡
    pub(crate) fn avatar_dir(&self) -> PathBuf {
        self.cache_dir.join("avatars")
    }

    /// `cache_dir` 配下の､ダウンロードした post のメディアを置くディレクトリ
    /// (#65) — [`Self::avatar_dir`] と分けてあるので「キャッシュした写真を
    /// 消す」と「キャッシュしたアバターを消す」が独立に保たれ､片方が大きく
    /// なってももう片方が見えなくならない｡
    pub(crate) fn media_dir(&self) -> PathBuf {
        self.cache_dir.join("media")
    }

    /// `state_dir` 配下の､診断ログを置くディレクトリ (#49)｡XDG の spec は
    /// ログを state ディレクトリの用途として明示的に挙げているし､cache と
    /// 違ってこれらは残ることが前提である — 次の起動で消えるログは､昨日
    /// 何が起きたかという問いに何も答えない｡
    pub(crate) fn log_dir(&self) -> PathBuf {
        self.state_dir.join("logs")
    }

    /// 現在のログファイル (#49)｡ローテートされた 1 つ前は､このパスに `.1`
    /// を付けたもの — `log::rotate_if_needed` を見よ｡
    pub(crate) fn log_file(&self) -> PathBuf {
        self.log_dir().join("twigpui.log")
    }

    /// `state_dir` 配下の､サインイン中のユーザーがこのアプリからいいねした
    /// post id のローカルな記録へのパス (#68)｡1 つにまとめず
    /// [`Self::reposted_posts_file`] と分けてあるのは､2 つの記録が独立した
    /// トグルから書かれ､壊れたり失われたりしたファイルはすでに「何も記録
    /// されていない」に劣化するからである — 共有すると､その劣化が 2 つの
    /// 機能を一度に襲う｡これが cache ではなく state である *理由* について
    /// `reposted_posts_file` の doc が言うことは､そのままここにも当てはまる:
    /// X API v2 の timeline レスポンスには「自分がこれをいいねしたか」を表す
    /// フィールドも無い｡
    pub(crate) fn liked_posts_file(&self) -> PathBuf {
        self.state_dir.join("liked_posts.json")
    }

    /// 3 つのディレクトリがまだ無ければ (再帰的に) 作る｡
    ///
    /// この呼び出しで `cache_dir` が作られたかどうかを返すので､呼び出し側は
    /// [`Paths::exclude_cache_from_backups`] の一度きりの設定を実行できる｡
    /// その副作用をここから外してあるのは意図的である: `ensure_dirs` は
    /// このクレートのファイルシステムのテストのほとんどから呼ばれ､その
    /// たびに `tmutil` を起動すると 1 回あたり 1 秒かかる｡
    pub(crate) fn ensure_dirs(&self) -> Result<bool> {
        // 作成前に採る｡あとではどちらにせよディレクトリが存在するため｡
        let cache_dir_is_new = !self.cache_dir.exists();

        for dir in [&self.config_dir, &self.cache_dir, &self.state_dir] {
            create_private_dir(dir)?;
        }

        Ok(cache_dir_is_new)
    }

    /// `tmutil addexclusion` 経由で `cache_dir` を Time Machine から除外する
    /// best-effort な試み (#9)｡`~/Library/Caches` は macOS が自動でバック
    /// アップ対象から外すが､このアプリが実際に使う XDG のキャッシュ位置
    /// (`~/.cache`) は外れないので､これが無いとレスポンスキャッシュが
    /// Time Machine のたびにふつうのデータと同じくバックアップされる｡
    ///
    /// 失敗 — `tmutil` が無い､権限が無い､何であれ — は黙って無視する:
    /// これはあれば嬉しい程度のもので､起動を止めては決してならない｡呼び
    /// 出しに 1 秒ほどかかるので､`ensure_dirs` がディレクトリを今作ったと
    /// 報告したときだけ実行する｡
    pub(crate) fn exclude_cache_from_backups(&self) {
        let _ = std::process::Command::new("tmutil")
            .arg("addexclusion")
            .arg(&self.cache_dir)
            .output();
    }
}

/// XDG のベースディレクトリを 1 つ解決する: `xdg_var` が空白でない絶対パスを
/// 持つなら `$<xdg_var>/<component>`､でなければ `$HOME/<default_relative>/<component>`｡
///
/// `component` は実行中のプロファイルのディレクトリ名 (#169) — 本物の
/// インストールなら `twigpui`､開発ビルドなら `twigpui-dev`｡
///
/// XDG Base Directory の spec によれば､`XDG_*` 変数の相対パスは未設定と同じ
/// に扱わなければならない｡ここではさらに空白だけの値 (空文字または空白のみ)
/// も未設定として扱う｡
fn resolve_dir(
    var: &impl Fn(&str) -> Option<String>,
    xdg_var: &str,
    default_relative: &str,
    component: &str,
) -> Result<PathBuf> {
    let base = var(xdg_var)
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && Path::new(value).is_absolute())
        .map(PathBuf::from);

    let base = if let Some(base) = base {
        base
    } else {
        let home = var("HOME").context("HOME is unset")?;
        PathBuf::from(home).join(default_relative)
    };
    Ok(base.join(component))
}

/// `dir` と足りない親を `0o700` (所有者のみ) の権限で作る｡すでにある
/// ディレクトリを作るのはエラーではない｡
///
/// #7 は `state_dir` 配下に OAuth トークンファイルを書く｡最初からすべての
/// ディレクトリを `0700` で作っておけば､それが入るころにはファイルが入って
/// いるかもしれないツリーに､あとから権限を付け直さずに済む｡
fn create_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("could not create directory: {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::Paths;
    use crate::profile::Profile;
    use anyhow::Result;
    use std::path::PathBuf;

    /// 固定の `(key, value)` の表に対する引きを作る｡`config::tests::vars`
    /// に倣っている｡
    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    /// テストバイナリ自身がどのプロファイルで作られていようと､*release*
    /// プロファイルのパスを解決する｡
    ///
    /// 下で assert するパスのリテラルはどれも README が記載し､既存の
    /// インストールがすでにディスクに持っているものなので､名指しした
    /// プロファイルに対して固定する必要がある｡[`Paths::from_vars`] を通すと
    /// 代わりにテストビルドがたまたま何であるかを assert することになる —
    /// `cargo test` ではそれは dev プロファイルで､唯一､誤った答えが何も
    /// 損なわない場合である｡
    fn release_paths(var: impl Fn(&str) -> Option<String>) -> Result<Paths> {
        Paths::for_profile(var, Profile::Release)
    }

    #[test]
    fn falls_back_to_the_xdg_defaults_under_home() {
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.config_dir,
            PathBuf::from("/home/alice/.config/twigpui")
        );
        assert_eq!(paths.cache_dir, PathBuf::from("/home/alice/.cache/twigpui"));
        assert_eq!(
            paths.state_dir,
            PathBuf::from("/home/alice/.local/state/twigpui")
        );
    }

    #[test]
    fn honors_xdg_overrides_when_absolute() {
        let paths = release_paths(vars(&[
            ("HOME", "/home/alice"),
            ("XDG_CONFIG_HOME", "/etc/xdg-config"),
            ("XDG_CACHE_HOME", "/var/xdg-cache"),
            ("XDG_STATE_HOME", "/var/xdg-state"),
        ]))
        .unwrap();
        assert_eq!(paths.config_dir, PathBuf::from("/etc/xdg-config/twigpui"));
        assert_eq!(paths.cache_dir, PathBuf::from("/var/xdg-cache/twigpui"));
        assert_eq!(paths.state_dir, PathBuf::from("/var/xdg-state/twigpui"));
    }

    #[test]
    fn ignores_a_relative_xdg_override_and_falls_back_to_the_default() {
        let paths = release_paths(vars(&[
            ("HOME", "/home/alice"),
            ("XDG_CONFIG_HOME", "relative/path"),
        ]))
        .unwrap();
        assert_eq!(
            paths.config_dir,
            PathBuf::from("/home/alice/.config/twigpui")
        );
    }

    #[test]
    fn ignores_a_blank_xdg_override_and_falls_back_to_the_default() {
        let paths =
            release_paths(vars(&[("HOME", "/home/alice"), ("XDG_CACHE_HOME", "   ")])).unwrap();
        assert_eq!(paths.cache_dir, PathBuf::from("/home/alice/.cache/twigpui"));
    }

    #[test]
    fn does_not_need_home_when_all_three_overrides_are_absolute() {
        let paths = release_paths(vars(&[
            ("XDG_CONFIG_HOME", "/etc/xdg-config"),
            ("XDG_CACHE_HOME", "/var/xdg-cache"),
            ("XDG_STATE_HOME", "/var/xdg-state"),
        ]))
        .unwrap();
        assert_eq!(paths.config_dir, PathBuf::from("/etc/xdg-config/twigpui"));
    }

    #[test]
    fn errors_naming_home_when_a_default_is_needed_and_home_is_unset() {
        let error = release_paths(vars(&[])).unwrap_err().to_string();
        assert!(error.contains("HOME"), "{error}");
    }

    // --- #169: dev プロファイルは本物のインストールとファイルを共有しない ---

    #[test]
    fn the_dev_profile_resolves_a_separate_directory_in_each_xdg_category() {
        let dev = Paths::for_profile(vars(&[("HOME", "/home/alice")]), Profile::Dev).unwrap();
        assert_eq!(
            dev.config_dir,
            PathBuf::from("/home/alice/.config/twigpui-dev")
        );
        assert_eq!(
            dev.cache_dir,
            PathBuf::from("/home/alice/.cache/twigpui-dev")
        );
        assert_eq!(
            dev.state_dir,
            PathBuf::from("/home/alice/.local/state/twigpui-dev")
        );
    }

    #[test]
    fn no_file_the_dev_profile_writes_lands_where_the_release_profile_reads() {
        // この issue を 1 つの assertion に: 開発時の実行が本物の
        // インストールの OAuth セッション､キャッシュ､台帳を上書きできては
        // ならない｡抜き取りではなく列挙してあるので､#169 を考えずに足された
        // *新しい* アクセサ — `config_dir`/`cache_dir`/`state_dir` を通さず
        // ディレクトリをハードコードしたもの — は､誰かが最初にサインイン
        // したときではなくここで落ちる｡
        let release = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        let dev = Paths::for_profile(vars(&[("HOME", "/home/alice")]), Profile::Dev).unwrap();

        let user = "2244994945";
        let reply = "1800000000000000003";
        let list = "2091351590695588200";
        let pairs: [(PathBuf, PathBuf); 19] = [
            (release.owned_lists_file(), dev.owned_lists_file()),
            (release.selection_file(), dev.selection_file()),
            (release.settings_file(), dev.settings_file()),
            (release.oauth_token_file(), dev.oauth_token_file()),
            (release.user_ids_file(), dev.user_ids_file()),
            (release.timeline_file(user), dev.timeline_file(user)),
            (
                release.home_timeline_file(user),
                dev.home_timeline_file(user),
            ),
            (
                release.list_timeline_file(list),
                dev.list_timeline_file(list),
            ),
            (release.sync_plan_file(), dev.sync_plan_file()),
            (release.me_file(), dev.me_file()),
            (release.thread_file(reply), dev.thread_file(reply)),
            (release.rate_limit_file(), dev.rate_limit_file()),
            (release.usage_file(), dev.usage_file()),
            (release.reposted_posts_file(), dev.reposted_posts_file()),
            (release.liked_posts_file(), dev.liked_posts_file()),
            (release.avatar_dir(), dev.avatar_dir()),
            (release.media_dir(), dev.media_dir()),
            (release.log_dir(), dev.log_dir()),
            (release.log_file(), dev.log_file()),
        ];
        for (release_path, dev_path) in pairs {
            assert_ne!(release_path, dev_path, "shared between the two profiles");
        }
    }

    #[test]
    fn an_xdg_override_still_separates_the_two_profiles() {
        // `XDG_STATE_HOME` を自分の好きな場所に向けても分離が潰れては
        // ならない — プロファイルの名前は override の代わりではなく､
        // override のあとに付け足される｡
        let table = [
            ("HOME", "/home/alice"),
            ("XDG_STATE_HOME", "/var/xdg-state"),
        ];
        let release = Paths::for_profile(vars(&table), Profile::Release).unwrap();
        let dev = Paths::for_profile(vars(&table), Profile::Dev).unwrap();
        assert_eq!(release.state_dir, PathBuf::from("/var/xdg-state/twigpui"));
        assert_eq!(dev.state_dir, PathBuf::from("/var/xdg-state/twigpui-dev"));
    }

    #[test]
    fn paths_remembers_which_profile_it_resolved() {
        let dev = Paths::for_profile(vars(&[("HOME", "/home/alice")]), Profile::Dev).unwrap();
        assert_eq!(dev.profile(), Profile::Dev);
        assert_eq!(
            release_paths(vars(&[("HOME", "/home/alice")]))
                .unwrap()
                .profile(),
            Profile::Release
        );
    }

    #[test]
    fn from_env_resolves_the_profile_this_binary_was_compiled_as() {
        // `from_vars` は他のテストがすべて使う seam である｡これは､それが
        // ハードコードされたものではなくコンパイル時のプロファイルを既定に
        // し続けることを固定する｡
        let paths = Paths::from_vars(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(paths.profile(), Profile::current());
    }

    #[test]
    fn settings_file_is_config_dot_toml_under_the_config_dir() {
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.settings_file(),
            PathBuf::from("/home/alice/.config/twigpui/config.toml")
        );
    }

    #[test]
    fn oauth_token_file_is_under_the_state_dir() {
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.oauth_token_file(),
            PathBuf::from("/home/alice/.local/state/twigpui/oauth_tokens.json")
        );
    }

    #[test]
    fn user_ids_file_is_under_the_cache_dir() {
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.user_ids_file(),
            PathBuf::from("/home/alice/.cache/twigpui/user_ids.json")
        );
    }

    #[test]
    fn timeline_file_is_under_the_cache_dir_named_by_user_id() {
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.timeline_file("2244994945"),
            PathBuf::from("/home/alice/.cache/twigpui/timeline-2244994945.json")
        );
    }

    #[test]
    fn home_timeline_file_is_under_the_cache_dir_named_by_user_id() {
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.home_timeline_file("2244994945"),
            PathBuf::from("/home/alice/.cache/twigpui/home-timeline-2244994945.json")
        );
    }

    #[test]
    fn home_timeline_file_does_not_collide_with_the_single_user_timeline_file() {
        // #11: 同じ user id で中身は違う — 一方を他方で上書きすると､表示
        // していないほうのモードが黙って壊れる｡
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_ne!(
            paths.timeline_file("2244994945"),
            paths.home_timeline_file("2244994945")
        );
    }

    #[test]
    fn list_timeline_file_is_under_the_cache_dir_named_by_list_id() {
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.list_timeline_file("2091351590695588200"),
            PathBuf::from("/home/alice/.cache/twigpui/list-timeline-2091351590695588200.json")
        );
    }

    #[test]
    fn list_timeline_file_collides_with_neither_timeline_file() {
        // #161: list id も user id も裸の数字なので､同じ数字が list と
        // ユーザーを同時に指しうる｡3 つのファイルはどれも違う中身を持つ｡
        let paths = Paths::from_vars(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_ne!(
            paths.list_timeline_file("2244994945"),
            paths.timeline_file("2244994945")
        );
        assert_ne!(
            paths.list_timeline_file("2244994945"),
            paths.home_timeline_file("2244994945")
        );
    }

    #[test]
    fn list_timeline_files_for_different_lists_are_different_files() {
        // #164 は list を切り替える｡1 つのファイルを共有すると､切り替える
        // たびに前の list がキャッシュした timeline を上書きすることになる｡
        let paths = Paths::from_vars(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_ne!(
            paths.list_timeline_file("111"),
            paths.list_timeline_file("222")
        );
    }

    #[test]
    fn sync_plan_file_is_under_the_state_dir() {
        // #163: cache dir ではない｡失うと両方の full read をまた払う｡
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.sync_plan_file(),
            PathBuf::from("/home/alice/.local/state/twigpui/sync_plan.json")
        );
    }

    #[test]
    fn sync_state_file_is_under_the_state_dir() {
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.sync_state_file(),
            PathBuf::from("/home/alice/.local/state/twigpui/sync_state.json")
        );
    }

    #[test]
    fn the_sync_clock_is_not_the_same_file_as_the_plan() {
        // plan は完全に適用された瞬間に消される｡ファイルを共有すると間隔の
        // 時計もそれに巻き込まれ､次の起動が両方の full read を払うことに
        // なる｡
        let paths = Paths::from_vars(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_ne!(paths.sync_state_file(), paths.sync_plan_file());
    }

    #[test]
    fn owned_lists_file_is_under_the_cache_dir() {
        // #164: 再取得は安いリクエスト 1 回なので､state ではなく cache｡
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.owned_lists_file(),
            PathBuf::from("/home/alice/.cache/twigpui/owned-lists.json")
        );
    }

    #[test]
    fn selection_file_is_under_the_state_dir() {
        // #164: ウィンドウがどの list を見せるかはネットワークが返して
        // くれるものではないので､モジュールの doc が手で消すよう案内する
        // ディレクトリではなく､他の state と並べて置く｡
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.selection_file(),
            PathBuf::from("/home/alice/.local/state/twigpui/selection.json")
        );
    }

    #[test]
    fn me_file_is_under_the_cache_dir() {
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.me_file(),
            PathBuf::from("/home/alice/.cache/twigpui/me.json")
        );
    }

    #[test]
    fn thread_file_is_under_the_cache_dir_named_by_reply_post_id() {
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.thread_file("1800000000000000003"),
            PathBuf::from("/home/alice/.cache/twigpui/thread-1800000000000000003.json")
        );
    }

    #[test]
    fn rate_limit_file_is_under_the_state_dir() {
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.rate_limit_file(),
            PathBuf::from("/home/alice/.local/state/twigpui/rate_limit.json")
        );
    }

    #[test]
    fn usage_file_is_under_the_state_dir() {
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.usage_file(),
            PathBuf::from("/home/alice/.local/state/twigpui/usage.json")
        );
    }

    #[test]
    fn media_and_avatar_caches_are_separate_directories() {
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.media_dir(),
            PathBuf::from("/home/alice/.cache/twigpui/media")
        );
        assert_ne!(paths.media_dir(), paths.avatar_dir());
    }

    #[test]
    fn the_log_file_is_under_the_state_dir() {
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.log_file(),
            PathBuf::from("/home/alice/.local/state/twigpui/logs/twigpui.log")
        );
        assert_eq!(paths.log_file().parent().unwrap(), paths.log_dir());
    }

    #[test]
    fn liked_posts_file_is_under_the_state_dir() {
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.liked_posts_file(),
            PathBuf::from("/home/alice/.local/state/twigpui/liked_posts.json")
        );
    }

    #[test]
    fn liked_and_reposted_records_are_separate_files() {
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_ne!(paths.liked_posts_file(), paths.reposted_posts_file());
    }

    #[test]
    fn reposted_posts_file_is_under_the_state_dir() {
        let paths = release_paths(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.reposted_posts_file(),
            PathBuf::from("/home/alice/.local/state/twigpui/reposted_posts.json")
        );
    }

    #[test]
    fn ensure_dirs_creates_all_three_directories_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let root =
            std::env::temp_dir().join(format!("twigpui-test-ensure-dirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let paths = release_paths(vars(&[("HOME", &root.display().to_string())])).unwrap();
        paths.ensure_dirs().unwrap();

        for dir in [
            root.join(".config/twigpui"),
            root.join(".cache/twigpui"),
            root.join(".local/state/twigpui"),
        ] {
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", dir.display());
        }

        // すでに中身のあるツリーに対して再度呼んでもエラーになってはならない｡
        paths.ensure_dirs().unwrap();

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn ensure_dirs_reports_the_cache_dir_as_new_only_on_the_call_that_creates_it() {
        // このフラグは約 1 秒かかる `tmutil` のサブプロセスを制御するので､
        // 起動のたびに「新規」と報告するのは見た目の粗ではなく実際の損である｡
        let root = std::env::temp_dir().join(format!(
            "twigpui-test-ensure-dirs-new-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);

        let paths = release_paths(vars(&[("HOME", &root.display().to_string())])).unwrap();
        assert!(paths.ensure_dirs().unwrap(), "first call creates cache_dir");
        assert!(
            !paths.ensure_dirs().unwrap(),
            "second call finds it already there"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }
}
