use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use std::path::Path;

use crate::log;
use crate::paths::Paths;
use crate::profile::Profile;
use crate::theme::ThemeMode;

/// ランタイム設定｡環境変数 > `config.toml` > 組み込みの既定値の優先順位で
/// 解決する｡
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// PKCE のサインインフローに使う OAuth 2.0 の client id (#7)｡秘密では
    /// ないため — public な OAuth client に client secret は無い —
    /// `config.toml` に置いてよい｡
    ///
    /// #33 が app-only の bearer token を落として以来､必須になった: 今や認証
    /// 手段はこれしか無いので､欠けていることは 2 択のうちの片方ではなく起動の
    /// 失敗だ｡
    pub oauth_client_id: String,
    /// post を表示する対象の screen name｡先頭の `@` は付けない｡
    pub target_username: String,
    /// 1 回の fetch で要求する post 数｡X API は 5..=100 を受け付ける｡
    pub max_results: u32,
    /// fetch を走らせてよい頻度の下限｡秒 (#10)｡
    ///
    /// これについて一致している必要のある 2 つのものが読む｡どちらが存在する
    /// より前にここを通してあったのはそのためだ: `ui::reload_policy::reload_gate`
    /// はこの窓の内側の `Polling` reload を拒み､#21 の
    /// `auto_refresh_interval_seconds` はこれを下回らないことを検証される
    /// — この下限より短い周期は､どの tick も何かを送る前に拒まれるタイマーに
    /// なってしまう｡
    pub min_fetch_interval_seconds: u32,
    /// カラーテーマ (#19): `light`､`dark`､または `system` (OS の外観に従う)｡
    /// 既定は `light`｡認識できない値は起動を失敗させるのではなく既定へ落ちる
    /// — [`Config::resolve`] を見よ｡
    pub theme: ThemeMode,
    /// ログファイルへどれだけの詳しさが届くか (#49)｡既定は
    /// [`log::Level::Info`]｡認識できない値は `theme` とまったく同じく､起動を
    /// 失敗させるのではなくそこへ落ちる｡
    pub log_level: log::Level,
    /// Posts の resource 1 件あたりの価格 (#162､#18 の後継)｡単位は USD
    /// 固定 — X 自身が USD 建てで単価を公開している
    /// (`https://docs.x.com/x-api/getting-started/pricing`) ので､#18 が
    /// 課していた「通貨を仮定しない」規則はここでは意図して外してある｡
    /// 既定は [`DEFAULT_POST_RESOURCE_PRICE`]｡`usage` のモジュール doc の
    /// [`crate::usage::estimated_amount`] を見よ｡
    pub post_resource_price: f64,
    /// timeline がウィンドウを埋める list (#161)｡`None` なら､それ以前のどの
    /// 起動もそうだったように home timeline を表示する｡
    ///
    /// `GET /2/users/:id/timelines/reverse_chronological` はこのアカウントに
    /// 対してフォロー中の著者の post を返さなくなり (#157)､ここを変えても
    /// それは直せない｡だから following の形をしたフィードをアプリが読む手段は
    /// そもそも list しか無い｡[`Config::resolve`] がすべて ASCII の数字である
    /// ことを検証する: この値は URL のパスセグメントへ埋め込まれる｡
    pub list_id: Option<String>,
    /// 1 日の Posts resource 数の予算 (#162､#18 の後継): 今日の Posts の
    /// resource 数の合計がこれに近づくか達すると､ヘッダの使用量行が
    /// warning/danger の色へ切り替わる — `usage::budget_status` を見よ｡
    /// 金額ではなく件数なのは意図的だ: `post_resource_price` と違い､
    /// こちらは比較する値が常にある (resource 数は常に判っている)｡
    /// 既定は [`DEFAULT_DAILY_POST_BUDGET`]｡
    pub daily_post_budget: u32,
    /// ウィンドウが動いている間､`list_id` のメンバーシップをこのアプリが
    /// フォローしているアカウントに合わせ続けるかどうか｡
    ///
    /// 既定は on｡効くのは `list_id` が設定され､かつセッションが
    /// `sync::missing_scope` の求める scope を持っているときだけだ — どちらも
    /// 既に意図的な行為である｡off にするのは､設定済みの list を代わりに自分の
    /// 手元に置いておくためのやり方だ｡
    ///
    /// **これはタイマーで金を使う**｡下の interval が長いのも､README が声に
    /// 出してそう言っているのもそのためだ｡
    pub auto_sync_list: bool,
    /// background sync が diff と diff の間に待つ長さ｡秒｡
    ///
    /// `min_fetch_interval_seconds` ではない: あちらは post 1 ページ分の費用が
    /// かかる reload を絞るもので､こちらはフォロー中のアカウント 1 件につき
    /// 1 課金リソースかかる 2 本の全読み取りの歩調を決める｡
    pub sync_interval_seconds: u32,
    /// background sync が 1 つの plan で取り除いてよい list メンバーシップの
    /// 上限｡パーセント (#176)｡
    ///
    /// 200 で短く返ってきたフォローの読み取りは大量アンフォローに見え､
    /// background sync は尋ねずに刈り込む｡この割合を超えると､削除は
    /// `--sync-list --apply --prune` が確認するために plan ファイルへ
    /// 留め置かれる — `sync::schedule::prune_allowed` を見よ｡0..=100:
    /// `100` は上限を off にし､`0` は background sync を追加専用にする｡
    /// CLI に上限がかかることは決して無い｡
    pub sync_prune_limit_percent: u8,
    /// background sync の 1 batch が送ってよい書き込みの数 (#197)｡
    ///
    /// batch と batch の間は 90〜300 秒に揺らぐので､持続的な速度はおよそ
    /// 「この値 ÷ 195 秒」になる｡なぜ間隔が固定値ではなく範囲なのかは
    /// `sync::state` の module doc を見よ｡
    ///
    /// 既定が遅いのは意図的だ: #197 を 24 時間締め出した上限は､およそ 1 分に
    /// 7 回の書き込みの後に働いた｡その大きさは今も実測されていない｡この
    /// つまみは実測のもう一方の向きのためにある — しばらく既定で走らせ､拒否が
    /// 出ないのを見て､上げてログを見る｡どちらに転んでも拒否は事故ではない:
    /// `sync::state` の backoff の階段がそれを吸収し､上限が何と言ったかは
    /// ログが記録する｡1..=[`MAX_SYNC_WRITES_PER_BATCH`]｡
    pub sync_writes_per_batch: u8,
    /// ウィンドウが動いている間､新しい post を求めて timeline を poll するか
    /// どうか (#21)｡
    ///
    /// 既定は on｡off なら､クリックで送らせたもの以外をアプリが送ることは無い
    /// — #21 の完了条件がその結末を名指ししているので､これは傾向ではなく硬い
    /// 保証だ: `TimelineView::start_auto_refresh` を見よ｡これが false のとき､
    /// あの関数は何一つ spawn する前に return する｡
    pub auto_refresh: bool,
    /// auto-refresh が poll と poll の間に待つ長さ｡秒 (#21)｡
    ///
    /// `sync_interval_seconds` がそうであるのと同じように
    /// `min_fetch_interval_seconds` とは別物だ: あちらはあらゆる fetch の下に
    /// 敷かれた *下限* で､こちらは poll が実際に走る周期であり､その下限を
    /// 下回らないことを検証される — [`resolve_auto_refresh_interval`] を見よ｡
    pub auto_refresh_interval_seconds: u32,
    /// 読み手が既に一番上に居るとき､poll が持ち帰った新しい post が自ずと
    /// 画面へ流れ込むかどうか (#22)｡
    ///
    /// 既定は on — #177 の体験の眼目は､頼まれなくても動き続ける timeline だ｡
    /// off なら､スクロール位置がどこであれどの poll も代わりに pill を通る｡
    /// 純粋に表示上のものだ: このスイッチが何をいつ取得するかを変えることは
    /// 決して無く — それは `auto_refresh` の仕事だ — 既に起きた fetch を
    /// ウィンドウがどう扱うかだけを変える｡`TimelineView::follow_new_posts` の
    /// 種であり､View メニューはここへ書き戻さずに実行時それを切り替える｡
    pub follow_new_posts: bool,
}

const DEFAULT_USERNAME: &str = "XDevelopers";
const DEFAULT_MAX_RESULTS: u32 = 20;
const MAX_RESULTS_RANGE: std::ops::RangeInclusive<u32> = 5..=100;

/// Posts の resource 1 件あたりの既定価格: USD $0.005 (#162)｡出典は
/// `https://docs.x.com/x-api/getting-started/pricing` (`x-api-budget`
/// skill の `pricing.md` が同じ表を最終確認 2026-08-23 で引いている)｡
/// #18 は「組み込みの既定価格は置かない」規則を持っていたが､それは単位の
/// 定まらない数 (当時は request 数) に価格を掛けないためのものだった｡
/// #162 で単位が Posts の resource 数に定まり､X 自身が USD 建てで単価を
/// 公開している以上､この値は推測ではなく出典のある数になったので､ここで
/// 意図してその規則を上書きする｡X が改定したら､この定数と上の出典の
/// 日付を合わせて更新する｡
const DEFAULT_POST_RESOURCE_PRICE: f64 = 0.005;

/// 1 日の Posts resource 数の既定予算: 1000 (#162)｡
///
/// 同日 dedup が効くので､定常で積むのは「その日に実際に増えた post 数」
/// だけになる — list 1 本のタイムラインなら概ね 300〜700 / 日に収まる
/// (`x-api-budget` skill を見よ)｡1000 なら 80% の警告 (800) が重い日に
/// だけ立ち､鳴りっぱなしにはならない｡[`DEFAULT_POST_RESOURCE_PRICE`]
/// と掛けると $5.00 / 日 が上限の目安になる｡
const DEFAULT_DAILY_POST_BUDGET: u32 = 1000;
/// 60 秒: X の endpoint ごとの厳しめな rate limit の窓に対してさえ､reload
/// 1 回 (request 1 ないし 2 回) の窓あたり費用を余裕をもって上回りつつ､
/// reload ボタンを押す人間に対しては反応良くいられる長さだ｡
const DEFAULT_MIN_FETCH_INTERVAL_SECONDS: u32 = 60;

/// diff と diff の間は 6 時間｡
///
/// diff の両側とも返ってきたリソース単位で課金されるので､数千フォローの
/// diff 1 回はドル単位になる｡X はリソースが UTC の 24 時間以内で重複除去
/// されると文書化しており､それならその日の最初以降の diff はほぼ無料になる
/// — が､`x-api-budget` がそれを実測しているのは Posts だけで､Users や
/// Owned Reads については実測していない｡この interval は､検証できていない側
/// がどちらに転んでも大した費用にならないよう選んである: 1 日 4 回の diff は､
/// 重複除去が効けば千フォローあたりおよそ $2､効かなければおよそ $8 だ｡
const DEFAULT_SYNC_INTERVAL_SECONDS: u32 = 21_600;

/// sync が受け付ける最短の interval｡
///
/// 警告ではなく下限にしてあるのは､これが防ぐ失敗が後から気づいて取り返せる
/// 類のものではないからだ: `6000` のつもりで打った
/// `X_SYNC_INTERVAL_SECONDS=60` は､重複除去が Users には適用されないと判明
/// した場合､プリペイド残高に対して 2 本の全読み取りを 1 時間に 60 回買う｡
/// 15 分はこの機能が使い道を持つどの周期よりはるかに下で､それでいてなお
/// そこから 2 桁離れている｡
const MIN_SYNC_INTERVAL_SECONDS: u32 = 900;

/// 10%: background sync は 1 plan につき list の 10 分の 1 まで削除できる (#176)｡
///
/// 意図して保守的にしてある｡本物の大量アンフォローは稀で､CLI という逃げ道が
/// ある｡短く返ってくるフォローの読み取りの方は､list が空になるまで誰にも
/// 見えない失敗だ｡小さい list ほどこれを強く受ける — 15 件中 1 件でもう線を
/// 越える — が､絶対値の下限を当てて塞ぐのではなくそれを受け入れている:
/// 誤って留め置く代償は CLI コマンド 1 回､誤って通す代償は list そのものだ｡
const DEFAULT_SYNC_PRUNE_LIMIT_PERCENT: u8 = 10;

/// 1 batch に 2 件の書き込み: background sync の既定の追いつき速度 (#197)｡
///
/// たった一つある実測から選んだ — `POST /2/lists/:id/members` の隠れた上限が
/// およそ 1 分に 7 回の書き込みの後に働き､24 時間下りたままだった — それを
/// 踏まない速度であって､それが許す最速の速度ではない｡上げるためにあるのが
/// `sync_writes_per_batch` で､この既定での走行が拒否を出さないと示してから
/// 使う｡
///
/// 揺らぎを入れた後もこの値を下げていないのは､下げても拒否が止まらなかった
/// ため｡1 に落としても毎分ちょうど 1 件という規則正しさは残る｡
const DEFAULT_SYNC_WRITES_PER_BATCH: u8 = 2;

/// `sync_writes_per_batch` が受け付ける最大値: 20 は X の *文書化された*
/// 書き込みの窓 (15 分で 300 回) を 1 分へならした数｡batch が最短の 90 秒
/// 間隔で並んでも毎分 8 件ほどにしか届かないので上限としては余裕があるが､
/// 超えても速くなるのは refusal だけなので残してある — `2` のつもりで
/// 打った `25` は､バーストではなくキー名を挙げた error になるべき｡
const MAX_SYNC_WRITES_PER_BATCH: u8 = 20;

/// auto-refresh の poll と poll の間は 3 分 (#21)｡
///
/// timeline が理論上どれだけ新鮮でありうるかではなく､poll が実際に何を課金
/// されるかから選んだ｡poll は先頭ページを読み直す —
/// `GET /2/lists/:id/tweets` は `since_id` を取らないので､これより安い
/// request は送りようが無い — そして読み取りは返ってきたリソース単位で課金
/// され､UTC の 1 日以内で重複除去される｡だから定常状態では 1 日の poll の
/// 費用はその日に本当に新しかった post の分になり､それはどう届こうとそれらを
/// 読む費用そのものだ｡繰り返し課金されるのは UTC の深夜 0 時ごとに 1 回の
/// 先頭ページだけで､それは `max_results` で頭打ちになる｡
///
/// そのため､この interval は出費のつまみではなく反応の良さのつまみになる｡
/// 最初は 5 分だった｡#22 の一番上へ貼り付く follow が timeline を､ちらと
/// 見るものではなく眺めるものに変え､3 分は､誰かが目をやるたびにウィンドウが
/// request を送ることなく流れが生きて感じられる点だ｡詰めることで実際に使う
/// のは request であって — この周期なら 1 日 480 回で､288 回から増える —
/// リソースではない｡
const DEFAULT_AUTO_REFRESH_INTERVAL_SECONDS: u32 = 180;

/// `config.toml` から読み込むファイルレベルの設定｡
///
/// どのフィールドも `Option` で `#[serde(default)]` が効いており､この struct
/// は意図的に `deny_unknown_fields` を使わない: 将来の issue (#19 の theme､
/// #24 の layout) がキーを少しずつ足していくので､古いバイナリが新しい
/// ファイルを読んだとき､まだ知らないキーで詰まってはならない｡
#[derive(Debug, Default, Deserialize)]
struct FileSettings {
    #[serde(default)]
    target_username: Option<String>,
    #[serde(default)]
    max_results: Option<u32>,
    #[serde(default)]
    min_fetch_interval_seconds: Option<u32>,
    /// 秘密ではない ([`Config::oauth_client_id`] を見よ)｡だから — と違って
    /// このキーは `config.toml` に置いてよい｡
    #[serde(default)]
    oauth_client_id: Option<String>,
    /// 生の `theme` 値 (#19)｡ここではなく [`Config::resolve`] がパースする｡
    /// 認識できない値がファイル読み込み全体を失敗させるのではなく､既定へ
    /// 落ちられるようにするためだ｡
    #[serde(default)]
    theme: Option<String>,
    /// 生の `log_level` 値 (#49)｡`theme` と同じやり方で､同じ理由でパースする｡
    /// Finder から起動した `.app` にとって効いてくるのはこの設定だ｡そこでは
    /// shell で設定した環境変数は一切見えない (#40)｡
    #[serde(default)]
    log_level: Option<String>,
    /// 秘密ではない ([`Config::post_resource_price`] の doc を見よ)｡だから上の
    /// `oauth_client_id` と同じくこのキーは `config.toml` に置いてよい｡
    #[serde(default)]
    post_resource_price: Option<f64>,
    /// 秘密ではない｡`post_resource_price` と同じ理由だ｡
    #[serde(default)]
    daily_post_budget: Option<u32>,
    /// 秘密ではない｡`post_resource_price` と同じ理由だ｡Finder から起動した `.app`
    /// にとって効いてくるのはこのキーで､そこでは shell の変数は見えない
    /// (#40) — `log_level` がここにあるのと同じ理由だ｡
    #[serde(default)]
    auto_sync_list: Option<bool>,
    /// 秘密ではない｡`post_resource_price` と同じ理由だ｡
    #[serde(default)]
    sync_interval_seconds: Option<u32>,
    /// 秘密ではない｡`post_resource_price` と同じ理由だ｡`u8` ではなく `u32` なのは､
    /// ファイル中の `300` を serde が型 error で弾くのではなく､`resolve` が
    /// キー名を挙げて拒めるようにするためだ｡
    #[serde(default)]
    sync_prune_limit_percent: Option<u32>,
    /// 秘密ではない｡`post_resource_price` と同じ理由だ｡`u32` なのは
    /// `sync_prune_limit_percent` と同じ理由による｡
    #[serde(default)]
    sync_writes_per_batch: Option<u32>,
    /// 秘密ではない｡`post_resource_price` と同じ理由だ｡上の `auto_sync_list` と
    /// 同じく､Finder から起動した `.app` にとって効いてくるのはこのキーで､
    /// そこでは shell の変数は見えない (#40) — そしてこれは､ウィンドウが自ら
    /// 何かを送るのをやめさせる唯一のスイッチだ｡
    #[serde(default)]
    auto_refresh: Option<bool>,
    /// 秘密ではない｡`post_resource_price` と同じ理由だ｡
    #[serde(default)]
    auto_refresh_interval_seconds: Option<u32>,
    /// 秘密ではない｡`post_resource_price` と同じ理由だ｡
    #[serde(default)]
    follow_new_posts: Option<bool>,
    /// 生の `list_id` 値 (#161)｡秘密ではない — list id は x.com 上でその list
    /// 自身の URL に見えている — ので､上のどのキーとも同じく `config.toml` に
    /// 置くべきものだ｡ここではなく [`Config::resolve`] が検証する｡2 つのうち
    /// どちらのソースから来た値かを error が名指しできるようにするためだ｡
    #[serde(default)]
    list_id: Option<String>,
    /// [`Config::resolve`] が､これをまだ抱えているファイルを拒めるようにする
    /// ためだけに置いてある｡かつては dotfiles リポジトリのファイルに決して
    /// 置いてはならない credential だった｡#33 以降そもそも credential では
    /// なくなり､黙って無視すれば､設定できていないのに設定できていると信じた
    /// ままの人が残る｡型の無い `toml::Value` のままにしてあるのは､このキーの
    /// 下がどんな形でも deserialize error で落ちず検査が走るようにするためだ｡
    #[serde(default)]
    bearer_token: Option<toml::Value>,
}

impl FileSettings {
    /// `path` から設定を読み込む｡ファイルが無いのは error ではない — まだ
    /// ファイルレベルの設定が無いというだけだ｡壊れたファイルは error で､
    /// そのメッセージは `path` を名指しする｡
    fn load(path: &Path) -> Result<Self> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("could not read {}", path.display()));
            }
        };
        toml::from_str(&contents).with_context(|| format!("could not parse {}", path.display()))
    }
}

impl Config {
    pub(crate) fn from_env() -> Result<Self> {
        // .env が無くてもよい — 変数は本物の環境から来るかもしれない｡
        let _ = dotenvy::dotenv();
        let paths = Paths::from_env()?;
        // ここが本物の起動経路で､一度きりの Time Machine 除外がおよそ 1 秒の
        // サブプロセスに見合う唯一の場所だ｡
        if paths.ensure_dirs()? {
            paths.exclude_cache_from_backups();
        }
        let file = FileSettings::load(&paths.settings_file())?;
        Self::resolve(|key| std::env::var(key).ok(), file)
    }

    /// 任意の変数引き当てと､既に読み込んだファイル設定から設定をパースして
    /// 検証する｡
    ///
    /// [`Config::from_env`] から切り出したのは､下の規則を `set_var` 無しで
    /// テストできるようにするためだ｡`set_var` は `unsafe` で､他のテスト
    /// スレッドと競合する｡
    ///
    /// このバイナリがコンパイルされたときの profile に対して解決する｡テスト
    /// 以外のどの呼び出し側もそれを望んでいる｡特定の profile の既定値を気に
    /// するテスト (#169) は代わりに [`Config::resolve_for_profile`] を使う｡
    fn resolve(var: impl Fn(&str) -> Option<String>, file: FileSettings) -> Result<Self> {
        Self::resolve_for_profile(var, file, Profile::current())
    }

    /// 任意の profile に対する [`Config::resolve`] (#169)｡[`Paths::for_profile`]
    /// が同じ理由で使っている継ぎ目を写している: profile ごとに異なる既定値は､
    /// profile を名指しすることでしか固定できない｡
    fn resolve_for_profile(
        var: impl Fn(&str) -> Option<String>,
        file: FileSettings,
        profile: Profile,
    ) -> Result<Self> {
        // #33 が app-only の bearer token を取り除いた｡更新した人のファイルに
        // はまだキーが残っていて､無視すれば､誰もそれを読まないのに設定できて
        // いると信じたままになる — だから何が起きたのか､代わりに何をすべきか
        // を言う｡
        if file.bearer_token.is_some() {
            bail!(
                "bearer_token is no longer supported (#33): app-only access could not read \
                 the home timeline or write anything, so twigpui now signs in with X. \
                 Remove the key from config.toml and set oauth_client_id (or \
                 X_OAUTH_CLIENT_ID) instead."
            );
        }

        let oauth_client_id = var("X_OAUTH_CLIENT_ID")
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .or_else(|| {
                file.oauth_client_id
                    .map(|c| c.trim().to_string())
                    .filter(|c| !c.is_empty())
            });

        // #33 以降これが唯一の credential なので､欠けていることは 2 択のうち
        // の片方ではなく起動の失敗だ｡
        let Some(oauth_client_id) = oauth_client_id else {
            bail!(
                "no oauth_client_id is configured. Set X_OAUTH_CLIENT_ID, or add \
                 oauth_client_id = \"…\" to config.toml, then click \"Sign in with X\"."
            );
        };

        let target_username = var("X_TARGET_USERNAME")
            .filter(|u| !u.trim().is_empty())
            .or_else(|| file.target_username.filter(|u| !u.trim().is_empty()))
            .unwrap_or_else(|| DEFAULT_USERNAME.to_string());
        let target_username = target_username.trim().trim_start_matches('@').to_string();

        let (max_results, max_results_source) = match var("X_MAX_RESULTS") {
            Some(raw) => {
                let value = raw
                    .trim()
                    .parse::<u32>()
                    .with_context(|| format!("X_MAX_RESULTS is not a number: {raw:?}"))?;
                (value, "X_MAX_RESULTS")
            }
            None => match file.max_results {
                Some(value) => (value, "max_results in config.toml"),
                None => (DEFAULT_MAX_RESULTS, "the default"),
            },
        };
        if !MAX_RESULTS_RANGE.contains(&max_results) {
            bail!(
                "{max_results_source} must be between {} and {}, got {max_results}",
                MAX_RESULTS_RANGE.start(),
                MAX_RESULTS_RANGE.end()
            );
        }

        let min_fetch_interval_seconds = match var("X_MIN_FETCH_INTERVAL_SECONDS") {
            Some(raw) => raw.trim().parse::<u32>().with_context(|| {
                format!("X_MIN_FETCH_INTERVAL_SECONDS is not a number: {raw:?}")
            })?,
            None => file
                .min_fetch_interval_seconds
                .unwrap_or(DEFAULT_MIN_FETCH_INTERVAL_SECONDS),
        };
        if min_fetch_interval_seconds == 0 {
            bail!(
                "X_MIN_FETCH_INTERVAL_SECONDS (or min_fetch_interval_seconds in config.toml) \
                 must be greater than 0"
            );
        }

        let theme = resolve_theme(&var, file.theme);

        let log_level = resolve_log_level(&var, file.log_level);

        let list_id = resolve_list_id(&var, file.list_id, profile)?;

        let post_resource_price = resolve_post_resource_price(&var, file.post_resource_price)?;
        let daily_post_budget = resolve_daily_post_budget(&var, file.daily_post_budget)?;

        let auto_sync_list = resolve_switch("X_AUTO_SYNC_LIST", &var, file.auto_sync_list)?;
        let sync_interval_seconds = resolve_sync_interval(&var, file.sync_interval_seconds)?;
        let sync_prune_limit_percent =
            resolve_sync_prune_limit(&var, file.sync_prune_limit_percent)?;
        let sync_writes_per_batch =
            resolve_sync_writes_per_batch(&var, file.sync_writes_per_batch)?;

        let auto_refresh = resolve_switch("X_AUTO_REFRESH", &var, file.auto_refresh)?;
        let follow_new_posts = resolve_switch("X_FOLLOW_NEW_POSTS", &var, file.follow_new_posts)?;
        // `min_fetch_interval_seconds` を取るのは､これが強制する下限がそれ
        // だからだ — [`resolve_auto_refresh_interval`] を見よ｡
        let auto_refresh_interval_seconds = resolve_auto_refresh_interval(
            &var,
            file.auto_refresh_interval_seconds,
            min_fetch_interval_seconds,
        )?;

        Ok(Self {
            oauth_client_id,
            target_username,
            max_results,
            min_fetch_interval_seconds,
            theme,
            log_level,
            post_resource_price,
            list_id,
            daily_post_budget,
            auto_sync_list,
            sync_interval_seconds,
            sync_prune_limit_percent,
            sync_writes_per_batch,
            auto_refresh,
            auto_refresh_interval_seconds,
            follow_new_posts,
        })
    }
}

/// `list_id` を解決する (#161): env > file > profile 自身の既定値 (#169)
/// という､他のすべてと同じ層構造だ｡どちら側でも空の値は未設定として扱う —
/// shell に置き去りにされた `X_LIST_ID=` は「素通し」を意味すべきで､
/// `/2/lists//tweets` へのリクエストではない｡
///
/// profile の既定値は､何も設定しなくても development ビルドが使い捨ての
/// list を拾う場所だ; release の profile には無いので､そちらでは今も
/// 「list 無し､home timeline を読む」に解決される｡
/// [`Profile::default_list_id`] を見よ｡
///
/// 空でなく ASCII 数字だけでもない値は､(`theme` や `log_level` と違い)
/// 警告して無視ではなく起動の失敗にする: あの二つは見た目の話だが､これは
/// どの timeline を取るかを決めるもので､黙って home timeline へ落ちると､
/// #157 が空だと見つけたフィードを読んでいるのに自分の list を読んでいると
/// 思わせてしまう｡エラーは値がどの出所から来たかを名指すので､直すべきもの
/// を指す｡
fn resolve_list_id(
    var: impl Fn(&str) -> Option<String>,
    file_value: Option<String>,
    profile: Profile,
) -> Result<Option<String>> {
    let (raw, source) = match var("X_LIST_ID")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        Some(value) => (value, "X_LIST_ID"),
        None => match file_value
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            Some(value) => (value, "list_id in config.toml"),
            // 下の数字チェックには通さない: これはこの crate 内の
            // リテラルで､`profile.rs` 自身のテストが押さえており､
            // ユーザーが打ったものではない｡
            None => return Ok(profile.default_list_id().map(str::to_string)),
        },
    };

    if !raw.chars().all(|c| c.is_ascii_digit()) {
        bail!("{source} must be a numeric list id, got {raw:?}");
    }
    Ok(Some(raw))
}

/// 既定で on の boolean スイッチを解決する: env > file > on｡
///
/// `theme` のようにフォールバックせず､認識できない値は拒否する｡theme の
/// 打ち間違いは見た目の話だが､`X_AUTO_SYNC_LIST=flase` の打ち間違いを
/// 既定として読めば､切ろうとしていた人のために課金される background の
/// loop が回り続けることになる｡`X_FOLLOW_NEW_POSTS` はどちらでも費用は
/// かからないが､`flase` を「on」と読めばやはりその人が書いたものを黙って
/// 無視することになる｡
fn resolve_switch(
    key: &str,
    var: impl Fn(&str) -> Option<String>,
    file_value: Option<bool>,
) -> Result<bool> {
    let Some(raw) = var(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(file_value.unwrap_or(true));
    };
    match raw.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => bail!("{key} must be true or false, got {raw:?}"),
    }
}

/// `sync_interval_seconds` を解決する: env > file >
/// [`DEFAULT_SYNC_INTERVAL_SECONDS`]｡[`MIN_SYNC_INTERVAL_SECONDS`] 未満は
/// 拒否する｡
///
/// 下限のエラーは境界だけでなくその数字が何を買うのかを述べる｡捕まえる
/// 間違いが小数点だからであり､「must be at least 900」では 60 がなぜ
/// まずいのかが伝わらないからだ｡
fn resolve_sync_interval(
    var: impl Fn(&str) -> Option<String>,
    file_value: Option<u32>,
) -> Result<u32> {
    let (seconds, source) = match var("X_SYNC_INTERVAL_SECONDS")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        Some(raw) => (
            raw.parse::<u32>()
                .with_context(|| format!("X_SYNC_INTERVAL_SECONDS is not a number: {raw:?}"))?,
            "X_SYNC_INTERVAL_SECONDS",
        ),
        None => match file_value {
            Some(seconds) => (seconds, "sync_interval_seconds in config.toml"),
            None => return Ok(DEFAULT_SYNC_INTERVAL_SECONDS),
        },
    };

    if seconds < MIN_SYNC_INTERVAL_SECONDS {
        bail!(
            "{source} must be at least {MIN_SYNC_INTERVAL_SECONDS} seconds, got {seconds}. \
             Each sync reads the whole follow list and the whole list membership, and both \
             bill per account returned — this floor is what stops a mistyped interval from \
             buying them over and over."
        );
    }
    Ok(seconds)
}

/// `sync_prune_limit_percent` を解決する (#176): env > file >
/// [`DEFAULT_SYNC_PRUNE_LIMIT_PERCENT`]｡100 を超えるものは拒否する｡
///
/// 丸め込みではなく上限だ: `150` は「off」ではなく､別の何かのつもりで
/// あった数字であり､それを 100 と読めば､まさに上限を設定しようとしていた
/// 当の人のために上限を切ってしまう｡
fn resolve_sync_prune_limit(
    var: impl Fn(&str) -> Option<String>,
    file_value: Option<u32>,
) -> Result<u8> {
    let (percent, source) = match var("X_SYNC_PRUNE_LIMIT_PERCENT")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        Some(raw) => (
            raw.parse::<u32>()
                .with_context(|| format!("X_SYNC_PRUNE_LIMIT_PERCENT is not a number: {raw:?}"))?,
            "X_SYNC_PRUNE_LIMIT_PERCENT",
        ),
        None => match file_value {
            Some(percent) => (percent, "sync_prune_limit_percent in config.toml"),
            None => return Ok(DEFAULT_SYNC_PRUNE_LIMIT_PERCENT),
        },
    };

    u8::try_from(percent)
        .ok()
        .filter(|percent| *percent <= 100)
        .with_context(|| {
            format!("{source} must be at most 100 (a share of the list, in percent), got {percent}")
        })
}

/// `sync_writes_per_batch` を解決する (#197): env > file >
/// [`DEFAULT_SYNC_WRITES_PER_BATCH`]｡0 と
/// [`MAX_SYNC_WRITES_PER_BATCH`] を超えるものは拒否する｡
///
/// 0 は「off」と読まずに拒否する — そのためのスイッチは `auto_sync_list`
/// であり､ペース 0 は走っていると称しながら plan を決して流し切らない
/// sync になるからだ｡上限が上限であるのは
/// [`MAX_SYNC_WRITES_PER_BATCH`] の理由による: それを越えても速くなるのは
/// refusal だけだ｡
fn resolve_sync_writes_per_batch(
    var: impl Fn(&str) -> Option<String>,
    file_value: Option<u32>,
) -> Result<u8> {
    let (writes, source) = match var("X_SYNC_WRITES_PER_BATCH")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        Some(raw) => (
            raw.parse::<u32>()
                .with_context(|| format!("X_SYNC_WRITES_PER_BATCH is not a number: {raw:?}"))?,
            "X_SYNC_WRITES_PER_BATCH",
        ),
        None => match file_value {
            Some(writes) => (writes, "sync_writes_per_batch in config.toml"),
            None => return Ok(DEFAULT_SYNC_WRITES_PER_BATCH),
        },
    };

    u8::try_from(writes)
        .ok()
        .filter(|writes| (1..=MAX_SYNC_WRITES_PER_BATCH).contains(writes))
        .with_context(|| {
            format!(
                "{source} must be between 1 and {MAX_SYNC_WRITES_PER_BATCH} (X's documented \
                 write window is 300 per 15 minutes), got {writes}"
            )
        })
}

/// `auto_refresh_interval_seconds` を解決する (#21): env > file >
/// [`DEFAULT_AUTO_REFRESH_INTERVAL_SECONDS`]｡
/// `min_fetch_interval_seconds` を下回るものは拒否する｡
///
/// 下限は金の話ではなく — poll がほぼ無料である理由は
/// [`DEFAULT_AUTO_REFRESH_INTERVAL_SECONDS`] が説明する — loop がそもそも
/// 動くかどうかの話だ｡どの poll も `ReloadTrigger::Polling` として
/// `ui::reload_policy::reload_gate` を通り､それは前回から
/// `min_fetch_interval_seconds` 以内の fetch を拒否する｡その下限より短い
/// 間隔にすれば､どの tick も何かを送る前に残らず拒否される: ウィンドウは
/// 決して仕事のできないタイマーを回すことになり､画面にはそう告げるものが
/// 何も無い｡起動時に拒否することが､その食い違いの見える唯一の場所だ｡
///
/// 下限ちょうどは受け入れる — gate が開き直すのとまさに同時に組まれた
/// poll が､なお動く最も詰めた間隔だ｡
fn resolve_auto_refresh_interval(
    var: &impl Fn(&str) -> Option<String>,
    file_value: Option<u32>,
    min_fetch_interval_seconds: u32,
) -> Result<u32> {
    let (seconds, source) = match var("X_AUTO_REFRESH_INTERVAL_SECONDS")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        Some(raw) => (
            raw.parse::<u32>().with_context(|| {
                format!("X_AUTO_REFRESH_INTERVAL_SECONDS is not a number: {raw:?}")
            })?,
            "X_AUTO_REFRESH_INTERVAL_SECONDS",
        ),
        None => match file_value {
            Some(seconds) => (seconds, "auto_refresh_interval_seconds in config.toml"),
            None => return Ok(DEFAULT_AUTO_REFRESH_INTERVAL_SECONDS),
        },
    };

    if seconds < min_fetch_interval_seconds {
        bail!(
            "{source} must be at least {min_fetch_interval_seconds} seconds — the value of \
             min_fetch_interval_seconds — got {seconds}. A poll scheduled inside that floor is \
             refused before it sends anything, so auto-refresh would never actually run."
        );
    }
    Ok(seconds)
}

/// `theme` を解決する (#19): env > file > 既定値｡
///
/// 数値の設定とは違い､ここで認識できない値に `bail!` してはならない —
/// theme の打ち間違いは見た目の話であって､起動を止める理由ではない — ので
/// 既定値へフォールバックし､`eprintln!` で警告する｡致命的でない通知に
/// ついてこのプロジェクトが定着させたやり方だ (`main.rs` を見よ)｡下の
/// [`resolve_log_level`] も同じ理由で同じことをする｡
///
/// [`Config::resolve_for_profile`] のインラインではなく自由関数にして
/// あるのは､ここの他の `resolve_*` と同じく､あの関数を clippy の行数
/// lint の下に収めるためだけだ｡
fn resolve_theme(var: &impl Fn(&str) -> Option<String>, file_value: Option<String>) -> ThemeMode {
    var("X_THEME")
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .or_else(|| {
            file_value
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
        })
        .and_then(|raw| {
            ThemeMode::parse(&raw).or_else(|| {
                eprintln!(
                    "warning: unrecognized theme {raw:?} (expected light, dark, or \
                     system); using {} instead",
                    ThemeMode::default()
                );
                None
            })
        })
        .unwrap_or_default()
}

/// log level を解決する (#49): `TWIGPUI_LOG` が `config.toml` の
/// `log_level` に勝ち､認識できない値は起動を止めずに警告して既定値へ
/// フォールバックする — `theme` (#19) と同じ形､同じ理由だ: どちらも
/// 実行を拒むほどのものではない｡
///
/// 警告は log ではなく stderr へ出す｡これが `log::init` より *前* に
/// 走るからで — それが produce する level こそ `init` が待っているものだ｡
/// Finder から起動した `.app` では誰も見ないが､そこは悪い値が驚きに
/// なりにくい場合でもある: shell で設定した環境変数はそこからは見えない
/// ので､値の出どころはユーザーが今しがた編集した `config.toml` だ｡
fn resolve_log_level(
    var: &impl Fn(&str) -> Option<String>,
    file_value: Option<String>,
) -> log::Level {
    var("TWIGPUI_LOG")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            file_value
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .and_then(|raw| {
            log::Level::parse(&raw).or_else(|| {
                eprintln!(
                    "warning: unrecognized log_level {raw:?} (expected error, warn, info, or debug); using info instead"
                );
                None
            })
        })
        .unwrap_or_default()
}

/// `post_resource_price` を解決する (#162､#18 の後継): env > file >
/// [`DEFAULT_POST_RESOURCE_PRICE`]｡[`Config::resolve`] の他のどの設定とも
/// 同じ優先順位で — そこから切り出してあるのは､あの関数を clippy の行数
/// lint の下に収めるためだけであって､ロジック自体を他所で再利用している
/// からではない｡
///
/// #18 時点では価格が*無い*ことが通常で､既定値で片付けるものではなかった
/// ([`Config::post_resource_price`] の doc を見よ)｡#162 でその前提が変わる:
/// 数える対象が Posts の resource 数に定まり､X 自身が USD 建てで単価を
/// 公開している以上､既定値は当てずっぽうではなく出典のある数になる｡
/// どちらの source から来た値でも検証はする: 負や非有限の価格は､下流の
/// 見積り額をすべて黙って壊す｡
fn resolve_post_resource_price(
    var: &impl Fn(&str) -> Option<String>,
    file_value: Option<f64>,
) -> Result<f64> {
    let (value, source) = match var("X_POST_RESOURCE_PRICE") {
        Some(raw) => {
            let value = raw
                .trim()
                .parse::<f64>()
                .with_context(|| format!("X_POST_RESOURCE_PRICE is not a number: {raw:?}"))?;
            (value, "X_POST_RESOURCE_PRICE")
        }
        None => match file_value {
            Some(value) => (value, "post_resource_price in config.toml"),
            None => return Ok(DEFAULT_POST_RESOURCE_PRICE),
        },
    };
    if !value.is_finite() || value < 0.0 {
        bail!("{source} must be a non-negative number, got {value}");
    }
    Ok(value)
}

/// `daily_post_budget` を解決する (#162､#18 の後継): env > file >
/// [`DEFAULT_DAILY_POST_BUDGET`]｡切り出した理由は
/// [`resolve_post_resource_price`] と同じ｡`u32` として parse する以上の
/// 検証はしない: その範囲のどの値も (0 を含めて) `usage::budget_status` に
/// とって意味がある｡
fn resolve_daily_post_budget(
    var: &impl Fn(&str) -> Option<String>,
    file_value: Option<u32>,
) -> Result<u32> {
    match var("X_DAILY_POST_BUDGET") {
        Some(raw) => raw
            .trim()
            .parse::<u32>()
            .with_context(|| format!("X_DAILY_POST_BUDGET is not a number: {raw:?}")),
        None => Ok(file_value.unwrap_or(DEFAULT_DAILY_POST_BUDGET)),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Config, DEFAULT_DAILY_POST_BUDGET, DEFAULT_MAX_RESULTS, DEFAULT_POST_RESOURCE_PRICE,
        DEFAULT_USERNAME, FileSettings,
    };
    use crate::profile::Profile;
    use crate::theme::ThemeMode;

    /// 固定の `(key, value)` 表を引く lookup を作る｡
    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    /// 呼び出し側で `clippy::float_cmp` に引っかからない浮動小数の等値比較
    /// (#162)｡ここで比較する値はどちらも同じ 10 進リテラルから
    /// `str::parse::<f64>` を通っただけなので、丸め誤差が入る余地は無い —
    /// それでも exact 比較を lint がそう読めないので、念のための epsilon｡
    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn fills_in_the_defaults_when_only_the_client_id_is_set() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();

        assert_eq!(config.oauth_client_id, "client-123");
        assert_eq!(config.target_username, DEFAULT_USERNAME);
        assert_eq!(config.max_results, DEFAULT_MAX_RESULTS);
        assert_eq!(
            config.min_fetch_interval_seconds,
            super::DEFAULT_MIN_FETCH_INTERVAL_SECONDS
        );
        assert_eq!(config.theme, ThemeMode::default());
    }

    // #33 が client id を唯一の credential にしたので､これはまた強い失敗に
    // なった — #7 が 2 つ目を持ち込む前がそうだったように｡
    #[test]
    fn rejects_when_no_client_id_is_configured() {
        let error = Config::resolve(vars(&[]), FileSettings::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("X_OAUTH_CLIENT_ID"), "{error}");
        assert!(
            !error.contains("X_BEARER_TOKEN"),
            "the message must not point at a credential that no longer exists: {error}"
        );
    }

    // 空白だけの token は､そのまま使われるのではなく "not configured" として
    // 数えられなければならない — が oauth_client_id があるので､それはもう
    // --- #49: ログレベル ---

    #[test]
    fn the_log_level_defaults_to_info() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.log_level, crate::log::Level::Info);
    }

    #[test]
    fn the_log_level_comes_from_the_environment_when_set() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("TWIGPUI_LOG", "debug"),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.log_level, crate::log::Level::Debug);
    }

    #[test]
    fn the_environments_log_level_wins_over_the_files() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("TWIGPUI_LOG", "error"),
            ]),
            FileSettings {
                log_level: Some("debug".to_string()),
                ..FileSettings::default()
            },
        )
        .unwrap();
        assert_eq!(config.log_level, crate::log::Level::Error);
    }

    #[test]
    fn the_files_log_level_is_used_when_the_environment_is_silent() {
        // 実際に効いてくる場合: Finder から起動した `.app` には､シェルで
        // 設定された環境変数が見えない (#40)｡
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings {
                log_level: Some("warn".to_string()),
                ..FileSettings::default()
            },
        )
        .unwrap();
        assert_eq!(config.log_level, crate::log::Level::Warn);
    }

    #[test]
    fn an_unrecognized_log_level_falls_back_rather_than_failing_startup() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("TWIGPUI_LOG", "loud")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.log_level, crate::log::Level::Info);
    }

    #[test]
    fn treats_a_blank_client_id_as_unset_rather_than_a_literal_value() {
        let error = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "   ")]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_OAUTH_CLIENT_ID"), "{error}");
    }

    #[test]
    fn trims_the_client_id() {
        // .env に貼り付けた値は末尾に改行を抱えていることが多く､それが
        // authorize URL にそのまま入る｡
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "  client-123\n")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.oauth_client_id, "client-123");
    }

    #[test]
    fn trims_the_oauth_client_id() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "  client-123\n")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.oauth_client_id, "client-123");
    }

    #[test]
    fn resolve_reads_the_oauth_client_id_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            oauth_client_id: Some("file-client".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[]), file).unwrap();
        assert_eq!(config.oauth_client_id, "file-client");
    }

    #[test]
    fn resolve_prefers_the_env_oauth_client_id_over_the_file() {
        let file = FileSettings {
            oauth_client_id: Some("file-client".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "env-client")]), file).unwrap();
        assert_eq!(config.oauth_client_id, "env-client");
    }

    #[test]
    fn strips_a_leading_at_from_the_username() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_TARGET_USERNAME", " @XDevelopers "),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.target_username, "XDevelopers");
    }

    #[test]
    fn falls_back_to_the_default_username_when_blank() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_TARGET_USERNAME", "  "),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.target_username, DEFAULT_USERNAME);
    }

    #[test]
    fn parses_max_results() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_MAX_RESULTS", " 42 "),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.max_results, 42);
    }

    #[test]
    fn rejects_a_non_numeric_max_results() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_MAX_RESULTS", "lots"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("not a number"), "{error}");
    }

    #[test]
    fn accepts_both_ends_of_the_api_range() {
        for raw in ["5", "100"] {
            let config = Config::resolve(
                vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_MAX_RESULTS", raw)]),
                FileSettings::default(),
            )
            .unwrap();
            assert_eq!(config.max_results.to_string(), raw);
        }
    }

    #[test]
    fn rejects_max_results_outside_the_api_range() {
        for raw in ["4", "101"] {
            let error = Config::resolve(
                vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_MAX_RESULTS", raw)]),
                FileSettings::default(),
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains("between 5 and 100"), "{raw}: {error}");
        }
    }

    // --- config.toml の重ね順 (env > file > default) ---

    #[test]
    fn resolve_reads_target_username_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            target_username: Some("FileUser".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.target_username, "FileUser");
    }

    #[test]
    fn resolve_prefers_the_env_target_username_over_the_file() {
        let file = FileSettings {
            target_username: Some("FileUser".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_TARGET_USERNAME", "EnvUser"),
            ]),
            file,
        )
        .unwrap();
        assert_eq!(config.target_username, "EnvUser");
    }

    #[test]
    fn resolve_reads_max_results_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            max_results: Some(42),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.max_results, 42);
    }

    #[test]
    fn resolve_prefers_the_env_max_results_over_the_file() {
        let file = FileSettings {
            max_results: Some(42),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_MAX_RESULTS", "7")]),
            file,
        )
        .unwrap();
        assert_eq!(config.max_results, 7);
    }

    #[test]
    fn resolve_rejects_a_file_max_results_outside_the_api_range() {
        let file = FileSettings {
            max_results: Some(4),
            ..FileSettings::default()
        };
        let error = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file)
            .unwrap_err()
            .to_string();
        assert!(error.contains("between 5 and 100"), "{error}");
        assert!(error.contains("config.toml"), "{error}");
    }

    #[test]
    fn resolve_rejects_a_bearer_token_left_in_the_file() {
        // #33: アップグレードした人の手元にはまだこのキーが残っている｡黙って
        // 無視すると､誰も読まないのに設定できていると信じたままにさせるので､
        // 代わりのものを名指しする強い失敗にする — そして以前と同じく､値
        // そのものはメッセージに決して出さない｡
        let file = FileSettings {
            bearer_token: Some(toml::Value::String("leaked".to_string())),
            ..FileSettings::default()
        };
        let error = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no longer supported"), "{error}");
        assert!(error.contains("oauth_client_id"), "{error}");
        assert!(!error.contains("leaked"), "{error}");
    }

    // --- min_fetch_interval_seconds の重ね順 (env > file > default, #10) ---

    #[test]
    fn resolve_reads_min_fetch_interval_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            min_fetch_interval_seconds: Some(120),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.min_fetch_interval_seconds, 120);
    }

    #[test]
    fn resolve_prefers_the_env_min_fetch_interval_over_the_file() {
        let file = FileSettings {
            min_fetch_interval_seconds: Some(120),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_MIN_FETCH_INTERVAL_SECONDS", "30"),
            ]),
            file,
        )
        .unwrap();
        assert_eq!(config.min_fetch_interval_seconds, 30);
    }

    #[test]
    fn resolve_rejects_a_min_fetch_interval_of_zero() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_MIN_FETCH_INTERVAL_SECONDS", "0"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_MIN_FETCH_INTERVAL_SECONDS"), "{error}");
    }

    #[test]
    fn resolve_rejects_a_non_numeric_min_fetch_interval() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_MIN_FETCH_INTERVAL_SECONDS", "soon"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("not a number"), "{error}");
    }

    // --- theme の重ね順 (env > file > default, #19) ---

    #[test]
    fn resolve_parses_the_theme_from_env() {
        for (raw, expected) in [
            ("light", ThemeMode::Light),
            ("dark", ThemeMode::Dark),
            ("system", ThemeMode::System),
        ] {
            let config = Config::resolve(
                vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_THEME", raw)]),
                FileSettings::default(),
            )
            .unwrap();
            assert_eq!(config.theme, expected, "{raw}");
        }
    }

    #[test]
    fn resolve_theme_is_case_insensitive_and_trims_whitespace() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_THEME", "  DARK\n")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.theme, ThemeMode::Dark);
    }

    #[test]
    fn resolve_reads_the_theme_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            theme: Some("dark".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.theme, ThemeMode::Dark);
    }

    #[test]
    fn resolve_prefers_the_env_theme_over_the_file() {
        let file = FileSettings {
            theme: Some("dark".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_THEME", "light")]),
            file,
        )
        .unwrap();
        assert_eq!(config.theme, ThemeMode::Light);
    }

    #[test]
    fn resolve_falls_back_to_the_default_theme_when_unset() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.theme, ThemeMode::default());
    }

    // 認識できない theme で起動を失敗させてはならない (#19) — 既定へ落ちる｡
    // これは env と file のどちらの取得元でも成り立たなければならない｡

    #[test]
    fn resolve_falls_back_to_the_default_theme_on_an_unrecognized_env_value() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_THEME", "solarized"),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.theme, ThemeMode::default());
    }

    #[test]
    fn resolve_falls_back_to_the_default_theme_on_an_unrecognized_file_value() {
        let file = FileSettings {
            theme: Some("solarized".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.theme, ThemeMode::default());
    }

    #[test]
    fn resolve_falls_back_to_the_default_theme_when_blank() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_THEME", "   ")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.theme, ThemeMode::default());
    }

    // --- post_resource_price / daily_post_budget (#162, #18 の後継) ---

    #[test]
    fn post_resource_price_and_daily_budget_fall_back_to_their_defaults() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();
        assert!(approx_eq(
            config.post_resource_price,
            DEFAULT_POST_RESOURCE_PRICE
        ));
        assert_eq!(config.daily_post_budget, DEFAULT_DAILY_POST_BUDGET);
    }

    #[test]
    fn parses_the_post_resource_price_from_env() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_POST_RESOURCE_PRICE", "0.015"),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert!(approx_eq(config.post_resource_price, 0.015));
    }

    #[test]
    fn rejects_a_non_numeric_post_resource_price() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_POST_RESOURCE_PRICE", "free"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_POST_RESOURCE_PRICE"), "{error}");
    }

    #[test]
    fn rejects_a_negative_post_resource_price() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_POST_RESOURCE_PRICE", "-0.01"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_POST_RESOURCE_PRICE"), "{error}");
    }

    #[test]
    fn resolve_reads_the_post_resource_price_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            post_resource_price: Some(0.02),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert!(approx_eq(config.post_resource_price, 0.02));
    }

    #[test]
    fn resolve_prefers_the_env_post_resource_price_over_the_file() {
        let file = FileSettings {
            post_resource_price: Some(0.02),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_POST_RESOURCE_PRICE", "0.05"),
            ]),
            file,
        )
        .unwrap();
        assert!(approx_eq(config.post_resource_price, 0.05));
    }

    #[test]
    fn resolve_rejects_a_negative_post_resource_price_from_the_file() {
        let file = FileSettings {
            post_resource_price: Some(-1.0),
            ..FileSettings::default()
        };
        let error = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file)
            .unwrap_err()
            .to_string();
        assert!(error.contains("post_resource_price"), "{error}");
    }

    #[test]
    fn parses_the_daily_post_budget_from_env() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_DAILY_POST_BUDGET", "500"),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.daily_post_budget, 500);
    }

    #[test]
    fn rejects_a_non_numeric_daily_post_budget() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_DAILY_POST_BUDGET", "lots"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_DAILY_POST_BUDGET"), "{error}");
    }

    #[test]
    fn resolve_reads_the_daily_post_budget_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            daily_post_budget: Some(200),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.daily_post_budget, 200);
    }

    #[test]
    fn resolve_prefers_the_env_daily_post_budget_over_the_file() {
        let file = FileSettings {
            daily_post_budget: Some(200),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_DAILY_POST_BUDGET", "50"),
            ]),
            file,
        )
        .unwrap();
        assert_eq!(config.daily_post_budget, 50);
    }

    #[test]
    fn file_settings_load_returns_defaults_when_the_file_is_missing() {
        let path = std::env::temp_dir().join(format!(
            "twigpui-test-missing-config-{}.toml",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let settings = FileSettings::load(&path).unwrap();
        assert!(settings.target_username.is_none());
        assert!(settings.max_results.is_none());
    }

    #[test]
    fn file_settings_load_errors_naming_the_path_on_malformed_toml() {
        let path = std::env::temp_dir().join(format!(
            "twigpui-test-malformed-config-{}.toml",
            std::process::id()
        ));
        std::fs::write(&path, "??? not valid toml ???").unwrap();

        let error = FileSettings::load(&path).unwrap_err().to_string();
        assert!(error.contains(&path.display().to_string()), "{error}");

        std::fs::remove_file(&path).unwrap();
    }

    // --- #161: ウィンドウの主たる取得元を選ぶ list id ---

    #[test]
    fn no_list_id_is_configured_by_default() {
        // 無いことは "home timeline を出す" という意味で､#161 以前はどの
        // 起動もそうしていた｡意図して list id を設定するまで､その経路は
        // 何も変わらない｡release プロファイルを名指しするのは､#169 が
        // development 側に既定を与えたからで､これは人がインストールする
        // ビルドについての assert だ｡
        let config = Config::resolve_for_profile(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
            Profile::Release,
        )
        .unwrap();
        assert_eq!(config.list_id, None);
    }

    #[test]
    fn the_dev_profile_defaults_to_its_own_list() {
        // #169: development ビルドは何も設定しなくても使い捨ての list を
        // 読む｡export を忘れただけで `--sync-list` が本物の list を
        // 書き換えてしまうことがないようにするためだ｡
        let config = Config::resolve_for_profile(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
            Profile::Dev,
        )
        .unwrap();
        assert_eq!(
            config.list_id.as_deref(),
            Profile::Dev.default_list_id(),
            "the dev default must survive config resolution"
        );
    }

    #[test]
    fn a_configured_list_id_still_wins_over_the_dev_default() {
        let config = Config::resolve_for_profile(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_LIST_ID", "111")]),
            FileSettings::default(),
            Profile::Dev,
        )
        .unwrap();
        assert_eq!(config.list_id.as_deref(), Some("111"));
    }

    #[test]
    fn reads_the_list_id_from_the_environment() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_LIST_ID", " 2091351590695588200 "),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.list_id.as_deref(), Some("2091351590695588200"));
    }

    #[test]
    fn resolve_reads_the_list_id_from_the_file_when_env_is_unset() {
        // Finder から起動した `.app` にはシェルの環境が見えない (#40) ので､
        // そこでこれを設定する手立ては file しかない｡
        let file = FileSettings {
            list_id: Some("2091351590695588200".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.list_id.as_deref(), Some("2091351590695588200"));
    }

    #[test]
    fn resolve_prefers_the_env_list_id_over_the_file() {
        let file = FileSettings {
            list_id: Some("111".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_LIST_ID", "222")]),
            file,
        )
        .unwrap();
        assert_eq!(config.list_id.as_deref(), Some("222"));
    }

    #[test]
    fn a_blank_list_id_is_the_same_as_not_setting_one() {
        // さもないとシェルに残った空の `X_LIST_ID=` が `/2/lists//tweets` を
        // 組み立て､404 に 1 リクエストを費やす｡
        let config = Config::resolve_for_profile(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_LIST_ID", "   ")]),
            FileSettings::default(),
            Profile::Release,
        )
        .unwrap();
        assert_eq!(config.list_id, None);
    }

    #[test]
    fn rejects_a_list_id_that_is_not_all_digits() {
        // この値は URL のパスセグメントに埋め込まれるので､snowflake id で
        // ないものは送らずにここで弾く｡
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_LIST_ID", "../users/me"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_LIST_ID"), "{error}");
    }

    #[test]
    fn a_rejected_list_id_names_the_file_key_when_that_is_where_it_came_from() {
        let file = FileSettings {
            list_id: Some("not-an-id".to_string()),
            ..FileSettings::default()
        };
        let error = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file)
            .unwrap_err()
            .to_string();
        assert!(error.contains("list_id in config.toml"), "{error}");
    }

    // --- auto_sync_list (env > file > on) ---

    #[test]
    fn the_background_sync_is_on_unless_someone_turns_it_off() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();
        assert!(config.auto_sync_list);
    }

    #[test]
    fn resolve_reads_auto_sync_list_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            auto_sync_list: Some(false),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert!(!config.auto_sync_list);
    }

    #[test]
    fn resolve_prefers_the_env_auto_sync_list_over_the_file() {
        let file = FileSettings {
            auto_sync_list: Some(true),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_AUTO_SYNC_LIST", "false"),
            ]),
            file,
        )
        .unwrap();
        assert!(!config.auto_sync_list);
    }

    #[test]
    fn auto_sync_list_takes_the_usual_spellings_of_off() {
        for raw in ["false", "FALSE", "0", "no", "off", " off "] {
            let config = Config::resolve(
                vars(&[
                    ("X_OAUTH_CLIENT_ID", "client-123"),
                    ("X_AUTO_SYNC_LIST", raw),
                ]),
                FileSettings::default(),
            )
            .unwrap();
            assert!(!config.auto_sync_list, "{raw:?}");
        }
    }

    #[test]
    fn resolve_rejects_an_auto_sync_list_it_does_not_understand() {
        // `theme` のように既定へ落とすことはしない: theme の打ち間違いは
        // 見た目の話だが､ここでの打ち間違いは､切ろうとしていた人の手元で
        // 課金されるループを回したままにしてしまう｡
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_AUTO_SYNC_LIST", "flase"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_AUTO_SYNC_LIST"), "{error}");
        assert!(error.contains("flase"), "{error}");
    }

    // --- sync_interval_seconds (env > file > default, 下限つき) ---

    #[test]
    fn the_sync_interval_defaults_to_six_hours() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.sync_interval_seconds, 21_600);
    }

    #[test]
    fn resolve_reads_the_sync_interval_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            sync_interval_seconds: Some(3_600),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.sync_interval_seconds, 3_600);
    }

    #[test]
    fn resolve_prefers_the_env_sync_interval_over_the_file() {
        let file = FileSettings {
            sync_interval_seconds: Some(3_600),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_INTERVAL_SECONDS", "43200"),
            ]),
            file,
        )
        .unwrap();
        assert_eq!(config.sync_interval_seconds, 43_200);
    }

    #[test]
    fn resolve_rejects_a_sync_interval_below_the_floor() {
        // 小数点の取り違え: 6000 のつもりで 60 と書く類だ｡どちらの全件読みも
        // 返ってきたアカウント単位で課金されるので､これは後から気づけばよい
        // 打ち間違いではない｡
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_INTERVAL_SECONDS", "60"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("at least 900"), "{error}");
        assert!(error.contains("bill per account"), "{error}");
    }

    #[test]
    fn a_rejected_sync_interval_names_the_file_key_when_that_is_where_it_came_from() {
        let file = FileSettings {
            sync_interval_seconds: Some(0),
            ..FileSettings::default()
        };
        let error = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("sync_interval_seconds in config.toml"),
            "{error}"
        );
    }

    #[test]
    fn resolve_rejects_a_non_numeric_sync_interval() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_INTERVAL_SECONDS", "soon"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("is not a number"), "{error}");
    }

    // --- #197: sync_writes_per_batch (env > file > 2, 1..=20) ---

    #[test]
    fn the_write_pace_defaults_to_two_a_batch() {
        // 実測にもとづく根拠: 隠れた cap が毎分およそ 7 write で作動し､
        // 24 時間下がったままだった｡既定はそれを踏まない歩調であって､
        // 許される最速ではない｡
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.sync_writes_per_batch, 2);
    }

    #[test]
    fn resolve_reads_the_write_pace_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            sync_writes_per_batch: Some(5),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.sync_writes_per_batch, 5);
    }

    #[test]
    fn resolve_prefers_the_env_write_pace_over_the_file() {
        let file = FileSettings {
            sync_writes_per_batch: Some(5),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_WRITES_PER_BATCH", "10"),
            ]),
            file,
        )
        .unwrap();
        assert_eq!(config.sync_writes_per_batch, 10);
    }

    #[test]
    fn resolve_rejects_a_write_pace_of_zero() {
        // 0 は "off" ではない — off は `auto_sync_list` の役目だ｡歩調が
        // ゼロの sync は､走ると称しながら計画を永久に捌かないことになる｡
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_WRITES_PER_BATCH", "0"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("between 1 and 20"), "{error}");
    }

    #[test]
    fn resolve_rejects_a_write_pace_past_the_documented_window() {
        // 20 は 15 分あたり 300 を 1 分へならしたもの — X が文書化している
        // window だ｡batch が最短間隔で並んでもそこには届かないが､超えても
        // 速くなるのは refusal だけなので上限として残してある｡
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_WRITES_PER_BATCH", "21"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("between 1 and 20"), "{error}");
    }

    #[test]
    fn the_documented_window_pace_itself_is_accepted() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_WRITES_PER_BATCH", "20"),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.sync_writes_per_batch, 20);
    }

    #[test]
    fn a_rejected_write_pace_names_the_file_key_when_that_is_where_it_came_from() {
        let file = FileSettings {
            sync_writes_per_batch: Some(120),
            ..FileSettings::default()
        };
        let error = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("sync_writes_per_batch in config.toml"),
            "{error}"
        );
    }

    #[test]
    fn resolve_rejects_a_non_numeric_write_pace() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_WRITES_PER_BATCH", "fast"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("is not a number"), "{error}");
    }

    // --- #176: sync_prune_limit_percent (env > file > 10, 最大 100) ---

    #[test]
    fn the_prune_limit_defaults_to_ten_percent() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.sync_prune_limit_percent, 10);
    }

    #[test]
    fn resolve_reads_the_prune_limit_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            sync_prune_limit_percent: Some(25),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.sync_prune_limit_percent, 25);
    }

    #[test]
    fn resolve_prefers_the_env_prune_limit_over_the_file() {
        let file = FileSettings {
            sync_prune_limit_percent: Some(25),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_PRUNE_LIMIT_PERCENT", "100"),
            ]),
            file,
        )
        .unwrap();
        assert_eq!(config.sync_prune_limit_percent, 100);
    }

    #[test]
    fn resolve_rejects_a_prune_limit_over_one_hundred_percent() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_PRUNE_LIMIT_PERCENT", "150"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("at most 100"), "{error}");
    }

    #[test]
    fn a_rejected_prune_limit_names_the_file_key_when_that_is_where_it_came_from() {
        let file = FileSettings {
            sync_prune_limit_percent: Some(101),
            ..FileSettings::default()
        };
        let error = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("sync_prune_limit_percent in config.toml"),
            "{error}"
        );
    }

    #[test]
    fn resolve_rejects_a_non_numeric_prune_limit() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_PRUNE_LIMIT_PERCENT", "half"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("is not a number"), "{error}");
    }

    // --- #21: auto-refresh ---

    #[test]
    fn auto_refresh_is_on_by_default() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();

        assert!(config.auto_refresh);
        assert_eq!(
            config.auto_refresh_interval_seconds,
            super::DEFAULT_AUTO_REFRESH_INTERVAL_SECONDS
        );
    }

    #[test]
    fn auto_refresh_can_be_switched_off_from_the_environment() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_AUTO_REFRESH", "off"),
            ]),
            FileSettings::default(),
        )
        .unwrap();

        assert!(!config.auto_refresh);
    }

    #[test]
    fn auto_refresh_can_be_switched_off_from_the_file() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings {
                auto_refresh: Some(false),
                ..FileSettings::default()
            },
        )
        .unwrap();

        assert!(!config.auto_refresh);
    }

    #[test]
    fn the_environment_wins_over_the_file_for_auto_refresh() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_AUTO_REFRESH", "true"),
            ]),
            FileSettings {
                auto_refresh: Some(false),
                ..FileSettings::default()
            },
        )
        .unwrap();

        assert!(config.auto_refresh);
    }

    // `X_AUTO_SYNC_LIST` と同じ理屈だ: 打ち間違いを既定として読むと､切ろうと
    // していた人の手元で課金されるタイマーを回したままにしてしまう｡
    #[test]
    fn rejects_an_unrecognized_auto_refresh_value() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_AUTO_REFRESH", "flase"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_AUTO_REFRESH"), "{error}");
    }

    #[test]
    fn reads_the_auto_refresh_interval_from_the_environment() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_AUTO_REFRESH_INTERVAL_SECONDS", "900"),
            ]),
            FileSettings::default(),
        )
        .unwrap();

        assert_eq!(config.auto_refresh_interval_seconds, 900);
    }

    #[test]
    fn reads_the_auto_refresh_interval_from_the_file_when_env_is_unset() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings {
                auto_refresh_interval_seconds: Some(600),
                ..FileSettings::default()
            },
        )
        .unwrap();

        assert_eq!(config.auto_refresh_interval_seconds, 600);
    }

    // このループの歩調を決める間隔は､`reload_gate` がその内側で拒む間隔より
    // 短くできない — どの tick も何かを送る前に阻まれ､auto-refresh は黙って
    // 一度も起きないことになる｡
    #[test]
    fn rejects_an_auto_refresh_interval_below_the_fetch_interval() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_MIN_FETCH_INTERVAL_SECONDS", "120"),
                ("X_AUTO_REFRESH_INTERVAL_SECONDS", "60"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_AUTO_REFRESH_INTERVAL_SECONDS"), "{error}");
        assert!(error.contains("120"), "{error}");
    }

    #[test]
    fn accepts_an_auto_refresh_interval_equal_to_the_fetch_interval() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_MIN_FETCH_INTERVAL_SECONDS", "120"),
                ("X_AUTO_REFRESH_INTERVAL_SECONDS", "120"),
            ]),
            FileSettings::default(),
        )
        .unwrap();

        assert_eq!(config.auto_refresh_interval_seconds, 120);
    }

    // 値が file から来たときは､下限のメッセージが file のキーを名指しする｡
    // `resolve_sync_interval` の 2 取得元のメッセージに倣っている｡
    #[test]
    fn the_auto_refresh_interval_floor_names_the_file_key() {
        let error = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings {
                auto_refresh_interval_seconds: Some(10),
                ..FileSettings::default()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("auto_refresh_interval_seconds in config.toml"),
            "{error}"
        );
    }

    #[test]
    fn rejects_a_non_numeric_auto_refresh_interval() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_AUTO_REFRESH_INTERVAL_SECONDS", "often"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("is not a number"), "{error}");
    }

    // --- #22: 新着 post への追従 ---

    #[test]
    fn following_new_posts_is_on_by_default() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();

        assert!(config.follow_new_posts);
    }

    #[test]
    fn following_new_posts_can_be_switched_off_from_the_environment() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_FOLLOW_NEW_POSTS", "off"),
            ]),
            FileSettings::default(),
        )
        .unwrap();

        assert!(!config.follow_new_posts);
    }

    #[test]
    fn following_new_posts_can_be_switched_off_from_the_file() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings {
                follow_new_posts: Some(false),
                ..FileSettings::default()
            },
        )
        .unwrap();

        assert!(!config.follow_new_posts);
    }

    #[test]
    fn the_environment_wins_over_the_file_for_following_new_posts() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_FOLLOW_NEW_POSTS", "true"),
            ]),
            FileSettings {
                follow_new_posts: Some(false),
                ..FileSettings::default()
            },
        )
        .unwrap();

        assert!(config.follow_new_posts);
    }

    // `X_AUTO_REFRESH` と違い､ここでの打ち間違いは何の代償も生まない — この
    // スイッチは見せ方の話で､支出の話ではない｡それでも弾く｡`flase` を "on"
    // として読むのは､その人が書いたものを黙って無視することだ､という
    // より素朴な理由からだ｡
    #[test]
    fn rejects_an_unrecognized_follow_new_posts_value() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_FOLLOW_NEW_POSTS", "flase"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_FOLLOW_NEW_POSTS"), "{error}");
    }
}
