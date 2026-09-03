//! X API レスポンスのローカル JSON キャッシュ (#9)｡
//!
//! API の出費を削る: 起動時は request をまったく送らずキャッシュから直接
//! 描画し ([`startup`] を見よ)､明示的な reload は user id がキャッシュ済み
//! なら 2 回ではなく 1 回の request で済む ([`reload`] を見よ)｡
//! `oauth::tokens` の `now` 注入の継ぎ目を踏襲している — TTL とマージの
//! ロジックは実時計を読まずファイルシステムにも自分で触れないので単体で
//! テストできる｡ディスクに触るのは下の薄い `cached_*` / `save_*` ラッパだけだ｡
//! ネットワークにも触れる唯一の関数は [`reload`] で (`XClient` 経由)､この
//! モジュールの他と違って unit test されていない — その関数自身の doc comment
//! を見よ｡
//!
//! ## キャッシュされた行は､それを書いたコードより長生きしうる
//!
//! `since_id`/`pagination_token` の歩みはキャッシュ範囲の *外* の post しか
//! API へ要求しないので､既にファイルにある行が取り直されることは無い｡
//! フィールドの中身を変えても — `TimelineItem` に一つ足す､#104 が repost の
//! media でやったように `expansions` を広げる — 既にキャッシュ済みの行は
//! すべて古く中身の薄い値を持ち続ける｡
//!
//! これに対処するものが 2 つあり､どちらも自動ではない:
//!
//! - [`splice`] は同じ id が再び現れたときに､キャッシュ済みの行の欠けた
//!   フィールドを流入したコピーから埋める｡ページ境界や `since_id` の重なりは
//!   これで賄えるが､その間の行は賄えない｡
//! - **残りは手でキャッシュファイルを消すことで賄う｡** ファイルは
//!   `Paths::cache_dir` の下にある｡消しても､どのみち起きるはずだった reload
//!   1 回以外に代償は無い｡空のキャッシュでは `since_id` が `None` を返すからだ｡
//!
//! #97 は後半を自動化した｡書き込み時に schema version を刻み､読み込み時に
//! 照合するものだった｡これは再び取り除かれた: 単一ユーザーの開発ツールに
//! とって､この定数は bump を忘れないよう覚えておくものが一つ増えるだけで､
//! ファイルを消し忘れるのと同じ失敗の仕方をし､しかも発火のたびに 500 行分の
//! スクロールバックを捨てていた｡

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

mod timeline;

// 兄弟ではなく子モジュールにした (#117､#126 の `ui` の分割にならって):
// `timeline` から見えるようにするためにここの可視性を広げる必要が無く､
// 下の re-export によって呼び出し側のパスはどれも変わらない --
// `cache::splice` は `cache::splice` のままだ｡
pub(crate) use timeline::{Side, since_id, splice, without_post};

use crate::paths::Paths;
use crate::thread::{self, ThreadChain};
use crate::x_api::{ListSummary, TimelineItem, XClient};

/// キャッシュした screen name → user id の対応が､reload が API で再解決する
/// までどれだけ使えるか｡user id は事実上恒久的なので長めに取ってある —
/// 狙いは reload の 2 request をできるだけ長く 1 回に減らすことだ｡
const USER_ID_TTL_SECONDS: i64 = 30 * 24 * 60 * 60;

/// ユーザーごとに保持するキャッシュ済み post の上限｡古いものから捨てる｡
/// `~/.cache` は `~/Library/Caches` と違って macOS が自動で掃除しないので､
/// これが無いと活発に reload するユーザーのキャッシュは無限に膨らむ｡
///
/// `ui.rs` もこれを読む: 上限に達すると [`splice`] は "Load older" の request
/// が買ったものをすべて捨ててしまうので､ボタンは無駄に credit を使わせるまま
/// にせず出さないでおく必要がある｡
pub(crate) const MAX_CACHED_POSTS: usize = 500;

/// キャッシュされた screen name → user id の対応 1 件｡
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserIdEntry {
    id: String,
    cached_at: i64,
}

/// `GET /2/users/me` の結果のキャッシュ (#11): サインイン中のユーザー自身の
/// id と screen name｡[`user_id_is_fresh`] を通じて [`UserIdEntry`] と同じ TTL
/// 方針を使い回す (id は事実上恒久的だ) が､[`UserIdCacheFile`] の
/// `username → id` マップには入れない — あのマップは一方向にしか引けず､
/// 「サインイン中のアカウントは誰か」に要るのはその逆だ: この値は呼び出し側が
/// 既に知っている screen name から引くのではなく､`/me` 自身から判明する｡
/// 同じ形の 1 件だけのファイル ([`Paths::me_file`]) が一番素直に嵌まる｡
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MeEntry {
    pub id: String,
    pub username: String,
    cached_at: i64,
}

/// `GET /2/users/:id/owned_lists` の結果のキャッシュ (#164): サインイン中の
/// アカウントが所有するすべての list を､API が返した順で持つ｡picker が描く
/// のもその順だ｡
///
/// [`MeEntry`] と違い TTL は無い｡user id は決して変わらないのでキャッシュを
/// 時計で失効させられるが､list の名前はいつ変わってもおかしくなく､どの日か
/// を知っている時計は無い｡これを動かすのは picker 自身の refresh だけだ —
/// [`load_timeline`] が従うのと同じ規則で､陳腐化は経過時間ではなく明示的な
/// reload で区切られる｡`cached_at` はそれでも記録する｡最後にいつ信じたかを
/// ファイルが語れるようにだ｡
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct OwnedListsEntry {
    lists: Vec<ListSummary>,
    cached_at: i64,
}

/// [`Paths::user_ids_file`] の中身すべて: これまでに解決した screen name を
/// 設定されたとおりのキーで持つ (`Config::target_username` の値で､trim 済み
/// かつ `@` を落としてある)｡
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct UserIdCacheFile {
    #[serde(default)]
    users: HashMap<String, UserIdEntry>,
}

/// [`Paths::timeline_file`] 1 つ分の中身すべて｡
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TimelineCacheFile {
    fetched_at: i64,
    items: Vec<TimelineItem>,
}

/// `cached_at` にキャッシュした user id が､`now` 時点でまだ TTL の窓の中か｡
fn user_id_is_fresh(cached_at: i64, now: i64) -> bool {
    now.saturating_sub(cached_at) < USER_ID_TTL_SECONDS
}

/// `path` を JSON として読み込みパースする｡3 つの結末を区別する: ファイルが
/// 存在しない (`Ok(None)`｡`oauth::tokens::load` のファイル欠落時と同じ)､
/// 存在するがパースに失敗する — 破損､あるいは将来や過去のバージョンの形 —
/// これも error ではなく *やはり* `Ok(None)` になる､そして本物の I/O error
/// (権限など) はそのまま伝播する｡キャッシュの目的はまるごと金を節約すること
/// なので､壊れたキャッシュファイルがアプリの起動を止めてはならない｡次の
/// 書き込みで黙って作り直されるだけだ｡
fn load_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    Ok(serde_json::from_str(&contents).ok())
}

fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let json = serde_json::to_vec_pretty(value)
        .with_context(|| format!("could not serialize {}", path.display()))?;
    std::fs::write(path, json).with_context(|| format!("could not write {}", path.display()))
}

/// `username` に対するキャッシュ済みの user id｡ファイルにあってまだ新鮮な
/// ときだけ返す｡`None` は「API で解決してキャッシュせよ」の意味 — 何も
/// キャッシュされていないか､ファイルが読めない/壊れているか､TTL が切れたかだ｡
pub(crate) fn cached_user_id(paths: &Paths, username: &str, now: i64) -> Result<Option<String>> {
    let file: UserIdCacheFile = load_json(&paths.user_ids_file())?.unwrap_or_default();
    Ok(file
        .users
        .get(username)
        .filter(|entry| user_id_is_fresh(entry.cached_at, now))
        .map(|entry| entry.id.clone()))
}

/// `username` の解決済み id を､既にキャッシュされている他の screen name と
/// 並べて永続化する｡
pub(crate) fn save_user_id(paths: &Paths, username: &str, user_id: &str, now: i64) -> Result<()> {
    let path = paths.user_ids_file();
    let mut file: UserIdCacheFile = load_json(&path)?.unwrap_or_default();
    file.users.insert(
        username.to_string(),
        UserIdEntry {
            id: user_id.to_string(),
            cached_at: now,
        },
    );
    save_json(&path, &file)
}

/// キャッシュ済みの `/me` の結果｡ファイルにあってまだ新鮮なときだけ返す｡
/// `None` は「API で解決してキャッシュせよ」の意味 — [`cached_user_id`] と
/// 同じ契約だ｡
pub(crate) fn cached_me(paths: &Paths, now: i64) -> Result<Option<MeEntry>> {
    let entry: Option<MeEntry> = load_json(&paths.me_file())?;
    Ok(entry.filter(|entry| user_id_is_fresh(entry.cached_at, now)))
}

/// `/me` から得たサインイン中ユーザーの id と screen name を永続化する｡
pub(crate) fn save_me(paths: &Paths, id: &str, username: &str, now: i64) -> Result<()> {
    let entry = MeEntry {
        id: id.to_string(),
        username: username.to_string(),
        cached_at: now,
    };
    save_json(&paths.me_file(), &entry)
}

/// picker が最後に取得した list 群 (#164)｡一度も取得していなければ `None` —
/// それは picker にとって､取得を実行せず提示せよという合図だ｡
pub(crate) fn cached_owned_lists(paths: &Paths) -> Result<Option<Vec<ListSummary>>> {
    let entry: Option<OwnedListsEntry> = load_json(&paths.owned_lists_file())?;
    Ok(entry.map(|entry| entry.lists))
}

/// `GET /2/users/:id/owned_lists` が返した list 群を永続化する (#164)｡
pub(crate) fn save_owned_lists(paths: &Paths, lists: &[ListSummary], now: i64) -> Result<()> {
    let entry = OwnedListsEntry {
        lists: lists.to_vec(),
        cached_at: now,
    };
    save_json(&paths.owned_lists_file(), &entry)
}

/// `user_id` のキャッシュ済み timeline を newest-first で返す｡使えるものが
/// キャッシュされていなければ (ファイルが無いか壊れている) `None`｡user id の
/// キャッシュと違い､ここに TTL は無い — 陳腐化は明示的な reload で区切られ､
/// 経過時間だけで区切られることは無い｡issue の「キャッシュから描画し､明示的な
/// reload だけが credit を使う」という決定に沿っている｡
pub(crate) fn load_timeline(paths: &Paths, user_id: &str) -> Result<Option<Vec<TimelineItem>>> {
    load_timeline_file(&paths.timeline_file(user_id))
}

/// timeline のキャッシュファイルを 1 つ読む｡2 つのうちどちらでもよい (#92)｡
fn load_timeline_file(path: &Path) -> Result<Option<Vec<TimelineItem>>> {
    let file: Option<TimelineCacheFile> = load_json(path)?;
    Ok(file.map(|file| file.items))
}

/// `items` (呼び出し側で既にマージと上限処理を済ませたもの) を `user_id` の
/// timeline キャッシュとして永続化する｡
pub(crate) fn save_timeline(
    paths: &Paths,
    user_id: &str,
    items: &[TimelineItem],
    now: i64,
) -> Result<()> {
    save_timeline_file(&paths.timeline_file(user_id), items, now)
}

/// timeline のキャッシュファイルを 1 つ書く (#92) —
/// [`load_timeline_file`] の対になるものだ｡
fn save_timeline_file(path: &Path, items: &[TimelineItem], now: i64) -> Result<()> {
    let file = TimelineCacheFile {
        fetched_at: now,
        items: items.to_vec(),
    };
    save_json(path, &file)
}

/// どの timeline がウィンドウを埋めるか (#161)｡
///
/// この名前は復活だ: `TimelineSource` は #33 まで存在していて､app-only の
/// bearer token が消えて分岐する対象が無くなった時点で失われた｡#157 が分岐を
/// 戻した — `GET /2/users/:id/timelines/reverse_chronological` がこの
/// アカウントのフォロー中著者の post を返さなくなり､following の形をした
/// フィードを読む手段は今や List しか無い｡
///
/// variant が 2 つなのは意図的だ｡複数の list から選ぶのは #164､ソースを 1 つの
/// レーンへ混ぜるのは #43｡どちらもこれ以上を求めており､どちらもここで形を先に
/// 当て推量して得をしない｡単一ユーザーの timeline (`--fetch-only`) は variant
/// ではない: [`reload`] が取得しウィンドウに出ることは決して無く､variant を
/// 与えれば下のすべての match の腕が､ウィンドウから到達できない case を抱える
/// ことになる｡
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TimelineSource {
    /// `GET /2/users/:id/timelines/reverse_chronological` (#11)｡サインイン中の
    /// user id ごとにキャッシュする｡list 未設定ならどの起動もこれを出す｡
    Home,
    /// `GET /2/lists/:id/tweets` (#161)｡list id ごとにキャッシュする｡
    List(String),
}

impl TimelineSource {
    /// このソースの post がどのキャッシュファイルに属するか｡
    ///
    /// `user_id` はサインイン中アカウント自身の id で､使うのは [`Self::Home`]
    /// だけだ: list の中身は誰が読んでも同じなので､読み手でキャッシュを
    /// キーづけすると､別のアカウントが同じ list を開いた瞬間に同じ post が
    /// 2 つ目のファイルへ書かれる｡
    fn cache_file(&self, paths: &Paths, user_id: &str) -> PathBuf {
        match self {
            Self::Home => paths.home_timeline_file(user_id),
            Self::List(list_id) => paths.list_timeline_file(list_id),
        }
    }
}

/// ウィンドウが表示しているソースのキャッシュ済み timeline を newest-first で
/// 返す (#161)｡使えるものがキャッシュされていなければ `None`｡[`load_timeline`]
/// をそのまま写しているが､読むのは [`TimelineSource::cache_file`] が選ぶ
/// ファイルだ — ソースごとに別のファイルなので､同じ id でキャッシュされた
/// 単一ユーザーの timeline が home の中身として読み戻されることは無く (#11)､
/// list の post が home timeline の上に載ることも無い (#161)｡
pub(crate) fn load_primary_timeline(
    paths: &Paths,
    source: &TimelineSource,
    user_id: &str,
) -> Result<Option<Vec<TimelineItem>>> {
    load_timeline_file(&source.cache_file(paths, user_id))
}

/// `items` を `source` のキャッシュとして永続化する｡[`save_timeline`] を写した
/// もので､書き込み先は [`TimelineSource::cache_file`] が選ぶファイルになる｡
pub(crate) fn save_primary_timeline(
    paths: &Paths,
    source: &TimelineSource,
    user_id: &str,
    items: &[TimelineItem],
    now: i64,
) -> Result<()> {
    save_timeline_file(&source.cache_file(paths, user_id), items, now)
}

/// ディスク上のキャッシュ済み timeline から `post_id` を落とし､残ったものを
/// 返す (#72)｡
///
/// X から post を消したのにキャッシュへ残す､というのが issue の警告する失敗の
/// 仕方だ: 行は次の起動まで消え､そのあと戻ってくる — 動いていないのに動いた
/// ように見えるアプリになる｡だからこれはファイルを書き換えたうえで
/// **読み戻し**､今書いたものではなく実際にディスクにあるものを返す｡黙って
/// 何もしなかった書き込みは､post がまだ在ることとして表に出る｡
///
/// `source` はどのキャッシュファイルに触るかを選ぶ — 同じ post が home
/// timeline と list に同時に居ることはあり得て､ユーザーが今操作したのは
/// 表示されている方だけだ｡
///
/// かつてこれは `home: bool` で､`false` の腕が単一ユーザーのキャッシュへ届いて
/// いた｡`false` を渡すものは無かった: あのキャッシュを書くのは `--fetch-only`
/// を担う [`reload`] だけで､headless な取得にはここへ至る削除の操作面が無い｡
/// #161 は､ウィンドウが取れない経路のために 3 つ目の状態を生やすのではなく､
/// このフラグを [`TimelineSource`] へ置き換えた｡
///
/// キャッシュファイルが無いのは error ではない: 消すものが無く､どのみち post
/// は X から消えている｡
pub(crate) fn forget_post(
    paths: &Paths,
    source: &TimelineSource,
    user_id: &str,
    post_id: &str,
    now: i64,
) -> Result<Vec<TimelineItem>> {
    // 一度だけ解決する (#92)｡かつてこの selector は二度分岐され､それぞれの腕が
    // load と save の両方を名指ししていたので､片方のファイルへ書いてもう片方を
    // 読み戻す､という書き方ができてしまった — それでは上の読み戻しが台無しだ｡
    // 読み戻しは *この* 書き込みが着地したことを示すためにある｡
    let path = source.cache_file(paths, user_id);

    let Some(cached) = load_timeline_file(&path)? else {
        return Ok(Vec::new());
    };
    let remaining = without_post(cached, post_id);
    save_timeline_file(&path, &remaining, now)?;
    Ok(load_timeline_file(&path)?.unwrap_or_default())
}

/// reload が使ったもの: 描画するマージ済み・上限処理済みの timeline と､
/// user id の引き当てがキャッシュ済みゆえに省かれたかどうか (省かれた場合､
/// reload は 2 回ではなく 1 回の request で済んでいる)｡
#[derive(Debug)]
pub(crate) struct Reloaded {
    pub items: Vec<TimelineItem>,
    pub user_id_cache_hit: bool,
}

/// 明示的な reload に許された credit を使う: user id を解決し (新鮮なら
/// キャッシュから､でなければ API request 1 回｡そのあと次回のためにキャッシュ
/// する)､キャッシュ済みで最も新しいものより新しい post を取得し､キャッシュ済み
/// のものの前へマージし､結果を永続化して返す｡
///
/// 直接の unit test は無い — `client` を通じて本物の HTTP request を送るからだ｡
/// これが組み立てている部品 ([`cached_user_id`]､[`save_user_id`]､
/// [`load_timeline`]､[`since_id`]､[`splice`]､[`save_timeline`]) は
/// 単体でテストされている｡`oauth::resolve_credential` のネットワークを呼ぶ
/// refresh 分岐が直接テストされていないのと同じだ｡
pub(crate) fn reload(
    paths: &Paths,
    client: &XClient,
    username: &str,
    max_results: u32,
    now: i64,
) -> Result<Reloaded> {
    let (user_id, user_id_cache_hit) = if let Some(id) = cached_user_id(paths, username, now)? {
        (id, true)
    } else {
        let id = client.user_id_by_username(paths, username, now)?;
        save_user_id(paths, username, &id, now)?;
        (id, false)
    };

    let cached = load_timeline(paths, &user_id)?.unwrap_or_default();
    let since = since_id(&cached);
    let fresh = client.timeline(paths, &user_id, max_results, since, now)?;
    let items = splice(cached, fresh, Side::Ahead);
    save_timeline(paths, &user_id, &items, now)?;
    Ok(Reloaded {
        items,
        user_id_cache_hit,
    })
}

/// ウィンドウの主 timeline の reload が使ったもの (#11､#161): 描画する
/// マージ済み・上限処理済みの timeline､解決した [`MeEntry`] そのもの
/// (`ui.rs` がヘッダを埋め､後の "Load older" のために id を覚えられるように)､
/// そしてレスポンスの `meta.next_token` があればそれ｡
///
/// [`Reloaded`] と違い `me_cache_hit` フラグは持たない: `main.rs` の
/// `--fetch-only` が `user_id_cache_hit` を通じて [`Reloaded`] に対してやって
/// いるような reload ごとの request 費用の報告を､ウィンドウの経路については
/// この crate の誰もしていないので､ここで追跡しても死荷重になる｡呼び出し側が
/// 要るようになったら戻せばよい｡
#[derive(Debug)]
pub(crate) struct ReloadedPrimary {
    pub items: Vec<TimelineItem>,
    pub me: MeEntry,
    pub next_token: Option<String>,
}

/// ウィンドウの timeline の reload に許された credit を使う: `/me` を解決し
/// (新鮮ならキャッシュから､でなければ API request 1 回｡そのあと次回のために
/// キャッシュする)､`source` から 1 ページ取得し､キャッシュ済みのものの前へ
/// マージし (後ろへ足すことは決してしない — それは [`load_older_primary`] の
/// 仕事だ)､結果を永続化して `meta.next_token` と一緒に返す｡
///
/// **増分で取得するのは [`TimelineSource::Home`] だけだ｡** これは `since_id`
/// を渡すので API は既にファイルにあるものを返さない｡
/// `GET /2/lists/:id/tweets` はそのパラメータを受け付けないので､list の reload
/// は常に先頭ページを読み直す｡どちらにせよ [`splice`] が id でマージするので､
/// 違いは何が課金されるかであって何が描画されるかではない —
/// [`XClient::list_timeline`] を見よ｡
///
/// 単一ユーザー版の [`reload`] を写したもの｡直接の unit test が無いのは
/// `reload` と同じ理由だ — `client` を通じて本物の HTTP request を送る｡
/// 組み立てている部品はすべて単体でテストされている｡
pub(crate) fn reload_primary(
    paths: &Paths,
    client: &XClient,
    source: &TimelineSource,
    max_results: u32,
    now: i64,
) -> Result<ReloadedPrimary> {
    let me = if let Some(entry) = cached_me(paths, now)? {
        entry
    } else {
        let user = client.me(paths, now)?;
        save_me(paths, &user.id, &user.username, now)?;
        MeEntry {
            id: user.id,
            username: user.username,
            cached_at: now,
        }
    };

    let cached = load_primary_timeline(paths, source, &me.id)?.unwrap_or_default();
    let (fresh, next_token) = match source {
        TimelineSource::Home => {
            client.home_timeline(paths, &me.id, max_results, since_id(&cached), None, now)?
        }
        TimelineSource::List(list_id) => {
            client.list_timeline(paths, list_id, max_results, None, now)?
        }
    };
    let items = splice(cached, fresh, Side::Ahead);
    save_primary_timeline(paths, source, &me.id, &items, now)?;
    Ok(ReloadedPrimary {
        items,
        me,
        next_token,
    })
}

/// request 1 回を使って `pagination_token` の *後ろ* のページを取得する
/// (#11 の "Load older"): キャッシュ済みのものの後ろへ足し — [`Side::Behind`]
/// であって決して [`Side::Ahead`] ではない — 合わせた結果を永続化し､次の
/// `meta.next_token` と一緒に返す (これ以上後ろが無ければ `None`)｡`user_id` を
/// 渡すのは呼び出し側の責任だ — `ui.rs` は `home_user_id` に持ち回っている
/// 解決済みの id をそのまま渡す｡既に表示している中身をさらに後ろへ辿る
/// ためだけに `/me` を解決し直す理由はこの関数に無いからだ｡
///
/// list を後ろへ辿るのも同じように動く: `pagination_token` は
/// `GET /2/lists/:id/tweets` が home timeline と共有する唯一のパラメータで､
/// だからここは 2 つのソースが非対称にならない唯一の向きだ｡
///
/// 直接の unit test は無い｡[`reload_primary`] に無いのと同じ理由だ｡
pub(crate) fn load_older_primary(
    paths: &Paths,
    client: &XClient,
    source: &TimelineSource,
    user_id: &str,
    max_results: u32,
    pagination_token: &str,
    now: i64,
) -> Result<(Vec<TimelineItem>, Option<String>)> {
    let cached = load_primary_timeline(paths, source, user_id)?.unwrap_or_default();
    let (older, next_token) = match source {
        TimelineSource::Home => client.home_timeline(
            paths,
            user_id,
            max_results,
            None,
            Some(pagination_token),
            now,
        )?,
        TimelineSource::List(list_id) => {
            client.list_timeline(paths, list_id, max_results, Some(pagination_token), now)?
        }
    };
    let items = splice(cached, older, Side::Behind);
    save_primary_timeline(paths, source, user_id, &items, now)?;
    Ok((items, next_token))
}

/// [`Paths::thread_file`] 1 つ分の中身すべて: 1 つの reply に対する親の連鎖の
/// キャッシュ (#12)｡
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ThreadCacheFile {
    fetched_at: i64,
    chain: ThreadChain,
}

/// `reply_post_id` に対する親の連鎖のキャッシュ｡ファイルにあれば返す (#12)｡
/// [`load_timeline`] と違い､ここには refresh の経路がそもそも無い — thread の
/// 親は投稿された時点で不変だ (削除は別だが､[`thread::assemble_chain`] が
/// 既に無難に描いている)｡だからキャッシュヒットは永久に信頼される｡
/// [`load_timeline`] 自身の「TTL 無し」の規則と､実質同じ理由で揃っている｡
pub(crate) fn load_thread(paths: &Paths, reply_post_id: &str) -> Result<Option<ThreadChain>> {
    let file: Option<ThreadCacheFile> = load_json(&paths.thread_file(reply_post_id))?;
    Ok(file.map(|file| file.chain))
}

/// `chain` を `reply_post_id` の親の連鎖のキャッシュとして永続化する (#12)｡
pub(crate) fn save_thread(
    paths: &Paths,
    reply_post_id: &str,
    chain: &ThreadChain,
    now: i64,
) -> Result<()> {
    let file = ThreadCacheFile {
        fetched_at: now,
        chain: chain.clone(),
    };
    save_json(&paths.thread_file(reply_post_id), &file)
}

/// "Show thread" (#12) に許された credit を使う: `reply_post_id` の連鎖が既に
/// キャッシュされていれば無料で描画する｡そうでなければ 1 段につき
/// `GET /2/tweets?ids=` request 1 回で上へ辿る — 起点は `first_parent_id`
/// (reply 自身の `TimelineItem::replied_to.post_id` で､request 費用ゼロで
/// 既に判っている) — [`thread::MAX_THREAD_DEPTH`] 段か､最初に欠けた/不在の
/// 親で止まり､組み上げた結果をキャッシュして返す｡
///
/// 空の結果は意図的にキャッシュ *しない* — 本体末尾のコメントを見よ｡
///
/// 下のループは深さの上限を各 fetch の *前* に確かめる｡後ではない｡だから
/// 最悪でもちょうど [`thread::MAX_THREAD_DEPTH`] request で済む: 上限に達した
/// ことは既に手元にあるデータ (最後に取得した post 自身の `replied_to`) から
/// 判り､それを知るために request をもう 1 回使うことは決して無い｡
///
/// 直接の unit test は無い — `client` を通じて本物の HTTP request を送るからで､
/// [`reload`] に無いのと同じだ｡この関数の順序づけ・重複除去・上限のロジックを
/// 担う純粋な継ぎ目は [`thread::assemble_chain`] だ｡
/// [`load_thread`]/[`save_thread`] はこのモジュールの他のキャッシュ
/// アクセサと同じく単体でテストされている｡
pub(crate) fn fetch_thread(
    paths: &Paths,
    client: &XClient,
    reply_post_id: &str,
    first_parent_id: &str,
    now: i64,
) -> Result<ThreadChain> {
    if let Some(cached) = load_thread(paths, reply_post_id)? {
        return Ok(cached);
    }

    let mut hops: Vec<thread::ThreadItem> = Vec::new();
    let mut next_id = Some(first_parent_id.to_string());
    let mut reached_cap = false;

    while let Some(id) = next_id.take() {
        if hops.len() >= thread::MAX_THREAD_DEPTH {
            // さらに上の親は判っている (`id`) が､上限は前の反復で既に
            // 達している — 判りきったことを確かめるために request を使わず
            // ここで止める｡
            reached_cap = true;
            break;
        }

        let items = client.tweets_by_id(paths, &id, now)?;
        let Some(fetched) = items.into_iter().next() else {
            // 削除された､保護されている､あるいは他の理由でレスポンスに
            // 不在 — 歩みは error にせずここで綺麗に止まる (#12)｡
            break;
        };

        next_id = fetched.replied_to.as_ref().map(|r| r.post_id.clone());
        hops.push(thread::ThreadItem {
            id: fetched.id,
            text: fetched.text,
            author_name: fetched.author_name,
            author_username: fetched.author_username,
        });
    }

    let chain = thread::assemble_chain(hops, reached_cap);
    // 空の連鎖は､一番最初の親が不在で返ってきたことを意味する｡たいていそれは
    // 恒久的だが (削除､保護)､一時的なしゃっくりもそう見える — そしてこの
    // キャッシュには TTL が無いので､永続化すればこの reply の "Show thread" は
    // 永久に詰まり､手でファイルを消す以外に逃げ道が無くなる｡導出し直す代償は
    // 明示的なクリック時のちょうど request 1 回だ｡そちらが安い方の間違いだ｡
    if !chain.items.is_empty() {
        save_thread(paths, reply_post_id, &chain, now)?;
    }
    Ok(chain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> TimelineItem {
        TimelineItem {
            id: id.to_string(),
            text: String::new(),
            created_at: None,
            author_name: String::new(),
            author_username: String::new(),
            reposted_by: None,
            quoted: None,
            replied_to: None,
            metrics: None,
            links: Vec::new(),
            author_avatar_url: None,
            original_post_id: None,
            media: Vec::new(),
        }
    }

    fn ids(items: &[TimelineItem]) -> Vec<&str> {
        items.iter().map(|item| item.id.as_str()).collect()
    }

    /// 下の #102 の順序テストのために､`created_at` に実物と同じ形の固定幅な
    /// タイムスタンプ文字列を入れた [`item`]｡
    fn item_at(id: &str, created_at: &str) -> TimelineItem {
        let mut built = item(id);
        built.created_at = Some(created_at.to_string());
        built
    }

    /// `n` とともに増える固定幅の `created_at` 文字列｡API が実際に送るのと
    /// 同じ `YYYY-MM-DDTHH:MM:SS.mmmZ` の形をしている｡テストごとに別々の
    /// タイムスタンプ literal を手で書く代わりにこれを使うことで､テスト対象の
    /// 順序 (`n` が大きい => 文字列比較でも大きい) が構成から明らかに正しく
    /// なる｡
    fn ts(n: u32) -> String {
        format!(
            "2026-01-01T{:02}:{:02}:{:02}.000Z",
            n / 3600,
            (n / 60) % 60,
            n % 60
        )
    }

    fn test_paths(root: &Path) -> Paths {
        let home = root.display().to_string();
        Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap()
    }

    fn temp_root(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("twigpui-test-cache-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    // --- user_id_is_fresh ---

    #[test]
    fn user_id_is_fresh_just_inside_the_ttl_window() {
        assert!(user_id_is_fresh(0, USER_ID_TTL_SECONDS - 1));
    }

    #[test]
    fn user_id_is_stale_once_the_ttl_has_fully_elapsed() {
        assert!(!user_id_is_fresh(0, USER_ID_TTL_SECONDS));
    }

    // --- since_id ---

    // --- #72: post の削除 ---

    #[test]
    fn without_post_drops_only_the_named_post() {
        let items = vec![item("1"), item("2"), item("3")];
        assert_eq!(ids(&without_post(items, "2")), ["1", "3"]);
    }

    #[test]
    fn without_post_leaves_an_unknown_id_alone() {
        let items = vec![item("1"), item("2")];
        assert_eq!(ids(&without_post(items, "nonexistent")), ["1", "2"]);
    }

    #[test]
    fn forget_post_rewrites_the_displayed_cache_and_reads_it_back() {
        // issue の本当の完了条件: キャッシュからも消えていて､次の起動で
        // 戻ってこられないこと｡書き込みを信じるのではなくファイルを読み直して
        // assert する｡
        let root = temp_root("forget-home");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        save_primary_timeline(
            &paths,
            &TimelineSource::Home,
            "me",
            &[item("1"), item("2")],
            0,
        )
        .unwrap();

        let remaining = forget_post(&paths, &TimelineSource::Home, "me", "1", 1).unwrap();
        assert_eq!(ids(&remaining), ["2"]);
        assert_eq!(
            ids(&load_primary_timeline(&paths, &TimelineSource::Home, "me")
                .unwrap()
                .unwrap()),
            ["2"]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn forget_post_touches_only_the_displayed_timelines_file() {
        // 同じ post は両方のキャッシュに居られる｡ユーザーが操作したのは
        // 見ていた方だけだ｡
        let root = temp_root("forget-one-file");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        save_primary_timeline(&paths, &TimelineSource::Home, "me", &[item("1")], 0).unwrap();
        save_timeline(&paths, "me", &[item("1")], 0).unwrap();

        forget_post(&paths, &TimelineSource::Home, "me", "1", 1).unwrap();

        assert!(
            load_primary_timeline(&paths, &TimelineSource::Home, "me")
                .unwrap()
                .unwrap()
                .is_empty()
        );
        assert_eq!(ids(&load_timeline(&paths, "me").unwrap().unwrap()), ["1"]);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn forget_post_is_not_an_error_when_no_cache_file_exists() {
        let root = temp_root("forget-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert!(
            forget_post(&paths, &TimelineSource::Home, "me", "1", 1)
                .unwrap()
                .is_empty()
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn since_id_is_none_for_an_empty_cache() {
        assert_eq!(since_id(&[]), None);
    }

    #[test]
    fn since_id_is_the_first_and_therefore_newest_cached_post() {
        let cached = vec![item("300"), item("200"), item("100")];
        assert_eq!(since_id(&cached), Some("300"));
    }

    // --- splice ahead (#92｡かつての merge_timeline) ---

    #[test]
    fn splice_ahead_places_fresh_posts_before_cached_posts() {
        let fresh = vec![item("3"), item("2")];
        let cached = vec![item("1")];
        let merged = splice(cached, fresh, Side::Ahead);
        assert_eq!(ids(&merged), vec!["3", "2", "1"]);
    }

    #[test]
    fn splice_ahead_drops_a_fresh_post_whose_id_is_already_cached() {
        // API は既にファイルにある post を返してくることがある｡キャッシュ済み
        // のコピーは重複せずそのまま居座る｡
        let fresh = vec![item("3"), item("2")];
        let cached = vec![item("2"), item("1")];
        let merged = splice(cached, fresh, Side::Ahead);
        assert_eq!(ids(&merged), vec!["3", "2", "1"]);
    }

    #[test]
    fn splice_ahead_keeps_the_result_ordered_newest_first() {
        let fresh = vec![item("6"), item("5"), item("4")];
        let cached = vec![item("3"), item("2"), item("1")];
        let merged = splice(cached, fresh, Side::Ahead);
        assert_eq!(ids(&merged), vec!["6", "5", "4", "3", "2", "1"]);
    }

    #[test]
    fn splice_ahead_truncates_to_the_500_post_cap() {
        let fresh = vec![item("502"), item("501")];
        let cached: Vec<_> = (1..=500).rev().map(|n| item(&n.to_string())).collect();
        let merged = splice(cached, fresh, Side::Ahead);
        assert_eq!(merged.len(), 500);
        assert_eq!(merged.first().unwrap().id, "502");
        // キャッシュ済みで最も古い 2 つの post ("2" と "1") は上限で押し出された｡
        assert!(!ids(&merged).contains(&"1"));
        assert!(!ids(&merged).contains(&"2"));
        assert_eq!(merged.last().unwrap().id, "3");
    }

    #[test]
    fn splice_keeps_the_cached_copy_of_a_duplicate_in_both_directions() {
        // #92: これが置き換えた 2 つの関数はどちらも既にファイルにあるものを
        // 残していて､それは偶然ではなく効いている — post の metrics (#67) は
        // 取得した時点のスナップショットなので､キャッシュ済みのコピーを残す
        // ことが､ユーザーが今見ている行のカウントを reload にかき混ぜさせない
        // ようにしている｡
        let mut cached_copy = item("1");
        cached_copy.text = "on file".to_string();
        let mut incoming_copy = item("1");
        incoming_copy.text = "just fetched".to_string();

        let ahead = splice(
            vec![cached_copy.clone()],
            vec![incoming_copy.clone()],
            Side::Ahead,
        );
        assert_eq!(ahead.len(), 1);
        assert_eq!(ahead[0].text, "on file");

        let behind = splice(vec![cached_copy], vec![incoming_copy], Side::Behind);
        assert_eq!(behind.len(), 1);
        assert_eq!(behind[0].text, "on file");
    }

    // --- splice は再登場した id の欠けたフィールドをマージする (#97) ---

    #[test]
    fn splice_fills_a_missing_optional_field_from_the_incoming_copy() {
        let mut cached_copy = item("1");
        cached_copy.author_avatar_url = None;
        let mut incoming_copy = item("1");
        incoming_copy.author_avatar_url = Some("https://example.com/avatar.png".to_string());

        let merged = splice(vec![cached_copy], vec![incoming_copy], Side::Ahead);
        assert_eq!(
            merged[0].author_avatar_url.as_deref(),
            Some("https://example.com/avatar.png")
        );
    }

    #[test]
    fn splice_keeps_the_cached_metrics_snapshot_instead_of_the_incoming_one() {
        // #67: metrics は post を最初に取得した時点のスナップショットだ｡
        // マージの規則 ("cached Some wins") はこれを特別扱いしてはならない —
        // 欠けた author_avatar_url を埋めるのと同じ規則から自然に出てくるもの
        // であり､このテストがそうなっていることを示している｡
        let mut cached_copy = item("1");
        cached_copy.metrics = Some(crate::x_api::PostMetrics {
            likes: 1,
            reposts: 2,
            replies: 3,
        });
        let mut incoming_copy = item("1");
        incoming_copy.metrics = Some(crate::x_api::PostMetrics {
            likes: 100,
            reposts: 200,
            replies: 300,
        });

        let merged = splice(vec![cached_copy], vec![incoming_copy], Side::Ahead);
        assert_eq!(merged[0].metrics.as_ref().unwrap().likes, 1);
    }

    #[test]
    fn splice_fills_metrics_when_the_cached_copy_has_none() {
        let cached_copy = item("1");
        assert_eq!(cached_copy.metrics, None);
        let mut incoming_copy = item("1");
        incoming_copy.metrics = Some(crate::x_api::PostMetrics {
            likes: 5,
            reposts: 6,
            replies: 7,
        });

        let merged = splice(vec![cached_copy], vec![incoming_copy], Side::Ahead);
        assert_eq!(merged[0].metrics.as_ref().unwrap().likes, 5);
    }

    #[test]
    fn splice_fills_an_empty_links_vec_from_the_incoming_copy() {
        let cached_copy = item("1");
        assert!(cached_copy.links.is_empty());
        let mut incoming_copy = item("1");
        incoming_copy.links = vec![crate::x_api::PostLink {
            url: "https://example.com".to_string(),
            label: "example.com".to_string(),
        }];

        let merged = splice(vec![cached_copy], vec![incoming_copy], Side::Ahead);
        assert_eq!(merged[0].links.len(), 1);
        assert_eq!(merged[0].links[0].label, "example.com");
    }

    // --- splice behind (#92｡かつての append_older) ---

    #[test]
    fn splice_behind_places_older_posts_after_cached_posts() {
        let cached = vec![item("3"), item("2")];
        let older = vec![item("1")];
        let merged = splice(cached, older, Side::Behind);
        assert_eq!(ids(&merged), vec!["3", "2", "1"]);
    }

    #[test]
    fn splice_behind_drops_an_older_post_whose_id_is_already_cached() {
        // ページ境界は重なりうる: API は既にファイルにある post を返して
        // くることがあり､それを重複させてはならない｡
        let cached = vec![item("3"), item("2")];
        let older = vec![item("2"), item("1")];
        let merged = splice(cached, older, Side::Behind);
        assert_eq!(ids(&merged), vec!["3", "2", "1"]);
    }

    #[test]
    fn splice_behind_keeps_the_result_ordered_newest_first() {
        let cached = vec![item("6"), item("5"), item("4")];
        let older = vec![item("3"), item("2"), item("1")];
        let merged = splice(cached, older, Side::Behind);
        assert_eq!(ids(&merged), vec!["6", "5", "4", "3", "2", "1"]);
    }

    #[test]
    fn splice_behind_truncates_to_the_500_post_cap() {
        let cached: Vec<_> = (3..=502).rev().map(|n| item(&n.to_string())).collect();
        let older = vec![item("2"), item("1")];
        let merged = splice(cached, older, Side::Behind);
        assert_eq!(merged.len(), 500);
        assert_eq!(merged.first().unwrap().id, "502");
        // 取得したうち最も古い 2 つの post ("2" と "1") は上限で押し出される｡
        assert!(!ids(&merged).contains(&"1"));
        assert!(!ids(&merged).contains(&"2"));
        assert_eq!(merged.last().unwrap().id, "3");
    }

    // --- splice は取得順ではなく created_at で結果を並べる (#102) ---

    #[test]
    fn splice_ahead_orders_by_created_at_even_when_the_fresh_batch_is_older() {
        // `since_id` reload が持ち帰る post はキャッシュ済みのものより新しい
        // はずだが､それを結果へ焼き込んだ前提にしてはならない — そうでない
        // とき (時計のずれ､後から埋められた post) でも､splice は fresh の
        // バッチが常に先､ではなく created_at 順に着地しなければならない｡
        let cached = vec![item_at("2", &ts(20))];
        let fresh = vec![item_at("3", &ts(10))];
        let merged = splice(cached, fresh, Side::Ahead);
        assert_eq!(ids(&merged), vec!["2", "3"]);
    }

    #[test]
    fn splice_behind_orders_by_created_at_even_when_the_older_batch_is_newer() {
        // 上の Ahead の場合を "Load older" のページについて写したものだ｡
        let cached = vec![item_at("5", &ts(10))];
        let older = vec![item_at("4", &ts(20))];
        let merged = splice(cached, older, Side::Behind);
        assert_eq!(ids(&merged), vec!["4", "5"]);
    }

    #[test]
    fn splice_sort_is_stable_for_equal_created_at() {
        // 文字列レベルまで同じ created_at: API が返した相対順序 (ここでは
        // ソート前の連結順) が生き残らなければならない｡それはまさに安定
        // ソートが保証するもので､`sort_unstable_by` では保証されない｡
        let cached = vec![item_at("1", &ts(10))];
        let fresh = vec![item_at("3", &ts(10)), item_at("2", &ts(10))];
        let merged = splice(cached, fresh, Side::Ahead);
        assert_eq!(ids(&merged), vec!["3", "2", "1"]);
    }

    #[test]
    fn splice_sinks_a_missing_created_at_row_to_the_end() {
        // created_at を持たない行 (#97 の古いキャッシュ行や､壊れた
        // レスポンス) は､取得順なら先頭に来る場合でも､持っている行より
        // 前に並んではならない｡
        let cached = vec![item_at("2", &ts(10))];
        let fresh = vec![item("3")]; // created_at: None
        let merged = splice(cached, fresh, Side::Ahead);
        assert_eq!(ids(&merged), vec!["2", "3"]);
    }

    #[test]
    fn splice_preserves_relative_order_among_multiple_missing_created_at_rows() {
        // None の行が 2 つあるとき､ソートが両方を created_at を持つ行の
        // 後ろへ動かしただけで､互いの順序が入れ替わってはならない｡
        let fresh = vec![item("9")]; // created_at: None
        let cached = vec![item_at("5", &ts(10)), item("8")]; // 2 つ目: None
        // ソート前の連結 (Side::Ahead) は ["9", "5", "8"] だ: None の行が
        // Some の行の前にあり､その後ろにもう一つ None の行がある｡
        let merged = splice(cached, fresh, Side::Ahead);
        assert_eq!(ids(&merged), vec!["5", "9", "8"]);
    }

    #[test]
    fn splice_sorts_before_capping_so_the_500_cap_drops_the_oldest_rows() {
        // 切り詰めをソートの後に置くことの回帰ガード: 素朴な「連結して
        // 500 で切る」順序だと間違った行が残るようなバッチを組む｡`fresh` は
        // Side::Ahead の連結の先頭 (位置 0-1) に居るが､created_at では
        // キャッシュ済みのどの行より古い｡ソートの前に切り詰めが走ったら
        // (あるいはソートがまったく走らなかったら)､上限はこの 2 つを残し､
        // 代わりに最も古い *キャッシュ済み* の 2 行を落とす — それらの
        // キャッシュ済み行の方が `fresh` より時系列で新しいのにだ｡
        let cached: Vec<_> = (1000..1500)
            .rev()
            .map(|n| item_at(&n.to_string(), &ts(n)))
            .collect();
        let fresh = vec![item_at("502", &ts(2)), item_at("501", &ts(1))];
        let merged = splice(cached, fresh, Side::Ahead);
        assert_eq!(merged.len(), 500);
        assert_eq!(merged.first().unwrap().id, "1499");
        assert_eq!(merged.last().unwrap().id, "1000");
        // 上限が落とすのは時系列で最も古い行 (fresh の方) であって､
        // キャッシュ済みで最も古い 2 行ではない｡
        assert!(!ids(&merged).contains(&"502"));
        assert!(!ids(&merged).contains(&"501"));
    }

    // --- cached_me / save_me ---

    #[test]
    fn cached_me_is_none_when_the_file_is_missing() {
        let root = temp_root("me-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert_eq!(cached_me(&paths, 0).unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_me_then_cached_me_roundtrips_while_fresh() {
        let root = temp_root("me-roundtrip");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_me(&paths, "2244994945", "alice", 1_000).unwrap();
        let me = cached_me(&paths, 1_000 + USER_ID_TTL_SECONDS - 1)
            .unwrap()
            .unwrap();
        assert_eq!(me.id, "2244994945");
        assert_eq!(me.username, "alice");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cached_me_is_none_once_the_ttl_has_elapsed() {
        // #11 は #9 の TTL 方針を使い回す: id は事実上恒久的だが､それでも
        // これは､その後削除されたり改名されたりしたアカウントのキャッシュ
        // ファイルが永久に信頼され続けるのを防いでいる｡
        let root = temp_root("me-stale");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_me(&paths, "2244994945", "alice", 0).unwrap();
        let me = cached_me(&paths, USER_ID_TTL_SECONDS).unwrap();
        assert_eq!(me, None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_corrupted_me_cache_file_is_a_clean_miss_not_an_error() {
        let root = temp_root("me-corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.me_file(), b"not json at all").unwrap();

        assert_eq!(cached_me(&paths, 0).unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- cached_owned_lists / save_owned_lists (#164) ---

    fn list(id: &str, name: &str) -> ListSummary {
        ListSummary {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn owned_lists_round_trip_in_the_order_the_api_returned_them() {
        let root = temp_root("owned-lists-roundtrip");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let lists = vec![list("2", "second"), list("1", "first")];
        save_owned_lists(&paths, &lists, 100).unwrap();
        assert_eq!(cached_owned_lists(&paths).unwrap(), Some(lists));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn owned_lists_never_expire_by_age() {
        // #164: 陳腐化は picker の明示的な refresh で区切られる｡
        // `load_timeline` が明示的な reload で区切られるのと同じだ —
        // 先月改名された list も､切り替え先として依然正しい list だ｡
        let root = temp_root("owned-lists-old");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_owned_lists(&paths, &[list("1", "old name")], 0).unwrap();
        assert_eq!(
            cached_owned_lists(&paths).unwrap(),
            Some(vec![list("1", "old name")])
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn owned_lists_are_none_when_the_file_is_missing() {
        let root = temp_root("owned-lists-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert_eq!(cached_owned_lists(&paths).unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_corrupted_owned_lists_file_is_a_clean_miss_not_an_error() {
        let root = temp_root("owned-lists-corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.owned_lists_file(), b"not json at all").unwrap();

        assert_eq!(cached_owned_lists(&paths).unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- cached_user_id / save_user_id ---

    #[test]
    fn cached_user_id_is_none_when_the_file_is_missing() {
        let root = temp_root("user-id-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert_eq!(cached_user_id(&paths, "XDevelopers", 0).unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_user_id_then_cached_user_id_roundtrips_while_fresh() {
        let root = temp_root("user-id-roundtrip");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_user_id(&paths, "XDevelopers", "2244994945", 1_000).unwrap();
        let id = cached_user_id(&paths, "XDevelopers", 1_000 + USER_ID_TTL_SECONDS - 1).unwrap();
        assert_eq!(id.as_deref(), Some("2244994945"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cached_user_id_is_none_once_the_ttl_has_elapsed() {
        let root = temp_root("user-id-stale");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_user_id(&paths, "XDevelopers", "2244994945", 0).unwrap();
        let id = cached_user_id(&paths, "XDevelopers", USER_ID_TTL_SECONDS).unwrap();
        assert_eq!(id, None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_user_id_preserves_other_already_cached_screen_names() {
        let root = temp_root("user-id-multi");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_user_id(&paths, "alice", "1", 0).unwrap();
        save_user_id(&paths, "bob", "2", 0).unwrap();

        assert_eq!(
            cached_user_id(&paths, "alice", 0).unwrap().as_deref(),
            Some("1")
        );
        assert_eq!(
            cached_user_id(&paths, "bob", 0).unwrap().as_deref(),
            Some("2")
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_corrupted_user_id_cache_file_is_a_clean_miss_not_an_error() {
        let root = temp_root("user-id-corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.user_ids_file(), b"not json at all").unwrap();

        let id = cached_user_id(&paths, "XDevelopers", 0).unwrap();
        assert_eq!(id, None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_genuine_io_error_reading_the_user_id_cache_still_propagates() {
        let root = temp_root("user-id-io-error");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        // ファイルがあるはずの場所にディレクトリがあるのは本物の I/O error で
        // (NotFound ではない)､破損とは別物だ — キャッシュミスとして飲み込まず
        // 表に出さなければならない｡
        std::fs::create_dir(paths.user_ids_file()).unwrap();

        assert!(cached_user_id(&paths, "XDevelopers", 0).is_err());

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- load_timeline / save_timeline ---

    #[test]
    fn load_timeline_is_none_when_the_file_is_missing() {
        let root = temp_root("timeline-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert_eq!(load_timeline(&paths, "2244994945").unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_timeline_then_load_timeline_roundtrips() {
        let root = temp_root("timeline-roundtrip");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let items = vec![item("2"), item("1")];
        save_timeline(&paths, "2244994945", &items, 1_000).unwrap();
        let loaded = load_timeline(&paths, "2244994945").unwrap();
        assert_eq!(loaded, Some(items));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_corrupted_timeline_cache_file_is_a_clean_miss_not_an_error() {
        let root = temp_root("timeline-corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.timeline_file("2244994945"), b"{ not valid json").unwrap();

        assert_eq!(load_timeline(&paths, "2244994945").unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_timeline_cache_file_from_a_future_shape_is_a_clean_miss_not_an_error() {
        let root = temp_root("timeline-future-shape");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        // JSON としては妥当だが､このバージョンが期待する形ではない —
        // 将来のバージョンの twigpui が書いたキャッシュファイルを模している｡
        std::fs::write(
            paths.timeline_file("2244994945"),
            br#"{"schema_version": 99, "wildly_different_shape": true}"#,
        )
        .unwrap();

        assert_eq!(load_timeline(&paths, "2244994945").unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_cache_file_carrying_a_schema_version_key_still_reads_back() {
        // 今ディスクにあるキャッシュファイルはどれも `schema_version` が
        // 存在していた頃に書かれた (#97｡その後また取り除かれた)｡serde は
        // 未知のフィールドを無視するのでそれらのファイルは今も読める — が､
        // それは誰かが書き留めた意図ではなく derive の性質でしかなく､
        // 成り立たなくなったときキャッシュ全体は騒がしくではなく静かに
        // 死ぬ: `load_json` はパース失敗を `Ok(None)` に変え､それは空の
        // キャッシュとまったく同じに読めるからだ｡
        let root = temp_root("timeline-legacy-schema-version-key");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(
            paths.timeline_file("2244994945"),
            br#"{"schema_version": 2, "fetched_at": 0, "items": []}"#,
        )
        .unwrap();

        assert_eq!(
            load_timeline(&paths, "2244994945").unwrap(),
            Some(Vec::new()),
            "a file with the old key must load, not read as an empty cache"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_timeline_cache_file_written_by_this_version_reads_back() {
        let root = temp_root("timeline-current-schema-version");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let items = vec![item("1")];
        save_timeline(&paths, "2244994945", &items, 0).unwrap();

        assert_eq!(load_timeline(&paths, "2244994945").unwrap(), Some(items));

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- load_primary_timeline / save_primary_timeline ---

    #[test]
    fn load_primary_timeline_is_none_when_the_file_is_missing() {
        let root = temp_root("home-timeline-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert_eq!(
            load_primary_timeline(&paths, &TimelineSource::Home, "2244994945").unwrap(),
            None
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_primary_timeline_then_load_primary_timeline_roundtrips() {
        let root = temp_root("home-timeline-roundtrip");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let items = vec![item("2"), item("1")];
        save_primary_timeline(&paths, &TimelineSource::Home, "2244994945", &items, 1_000).unwrap();
        let loaded = load_primary_timeline(&paths, &TimelineSource::Home, "2244994945").unwrap();
        assert_eq!(loaded, Some(items));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn single_user_and_home_timeline_caches_for_the_same_user_id_do_not_collide() {
        // #11 の眼目そのもの: 同じ user id であっても (たとえば単一ユーザー
        // モードで reload したあと､サインインして home timeline を reload
        // した場合)､片方のモードのキャッシュが他方を上書きしてはならない｡
        let root = temp_root("no-collision");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_timeline(&paths, "123", &[item("single-user-post")], 0).unwrap();
        save_primary_timeline(
            &paths,
            &TimelineSource::Home,
            "123",
            &[item("home-timeline-post")],
            0,
        )
        .unwrap();

        assert_eq!(
            load_timeline(&paths, "123").unwrap().unwrap()[0].id,
            "single-user-post"
        );
        assert_eq!(
            load_primary_timeline(&paths, &TimelineSource::Home, "123")
                .unwrap()
                .unwrap()[0]
                .id,
            "home-timeline-post"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- #161: list のソース ---

    #[test]
    fn a_lists_cache_and_the_home_cache_do_not_collide() {
        // #161: ウィンドウはどちらか一方を表示する｡両者を切り替えたときに､
        // 新しく来た方がもう一方の持っていたものを上書きしてはならない｡
        let root = temp_root("list-vs-home");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let list = TimelineSource::List("2091351590695588200".to_string());
        save_primary_timeline(&paths, &TimelineSource::Home, "me", &[item("home-post")], 0)
            .unwrap();
        save_primary_timeline(&paths, &list, "me", &[item("list-post")], 0).unwrap();

        assert_eq!(
            ids(&load_primary_timeline(&paths, &TimelineSource::Home, "me")
                .unwrap()
                .unwrap()),
            ["home-post"]
        );
        assert_eq!(
            ids(&load_primary_timeline(&paths, &list, "me").unwrap().unwrap()),
            ["list-post"]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_lists_cache_is_keyed_by_the_list_not_the_reader() {
        // 同じ list を 2 つのアカウントが読んでも post は同じなので､
        // サインイン中の id がファイル名に現れてはならない — さもないと
        // 2 つ目のアカウントが､1 つ目が既に払ったものを取り直す｡
        let root = temp_root("list-not-per-reader");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let list = TimelineSource::List("2091351590695588200".to_string());
        save_primary_timeline(&paths, &list, "alice", &[item("1")], 0).unwrap();

        assert_eq!(
            ids(&load_primary_timeline(&paths, &list, "bob")
                .unwrap()
                .unwrap()),
            ["1"]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn two_lists_keep_separate_caches() {
        let root = temp_root("two-lists");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let one = TimelineSource::List("111".to_string());
        let two = TimelineSource::List("222".to_string());
        save_primary_timeline(&paths, &one, "me", &[item("from-one")], 0).unwrap();
        save_primary_timeline(&paths, &two, "me", &[item("from-two")], 0).unwrap();

        assert_eq!(
            ids(&load_primary_timeline(&paths, &one, "me").unwrap().unwrap()),
            ["from-one"]
        );
        assert_eq!(
            ids(&load_primary_timeline(&paths, &two, "me").unwrap().unwrap()),
            ["from-two"]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn re_reading_the_whole_head_page_adds_no_rows() {
        // `since_id` が無いことの #161 における代償: list の reload は毎回
        // 同じ先頭ページを返す｡`splice` はキャッシュと完全に重なるバッチを
        // 吸収しなければならない｡さもないと timeline は reload のたびに
        // 自分自身の複製を育てる｡
        let cached = vec![item("3"), item("2"), item("1")];
        let head_page_again = vec![item("3"), item("2"), item("1")];

        let spliced = splice(cached, head_page_again, Side::Ahead);

        assert_eq!(ids(&spliced), ["3", "2", "1"]);
    }

    #[test]
    fn forget_post_removes_a_post_from_a_lists_cache() {
        let root = temp_root("forget-list");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let list = TimelineSource::List("2091351590695588200".to_string());
        save_primary_timeline(&paths, &list, "me", &[item("1"), item("2")], 0).unwrap();

        let remaining = forget_post(&paths, &list, "me", "1", 1).unwrap();
        assert_eq!(ids(&remaining), ["2"]);
        assert_eq!(
            ids(&load_primary_timeline(&paths, &list, "me").unwrap().unwrap()),
            ["2"]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- load_thread / save_thread ---

    #[test]
    fn load_thread_is_none_when_the_file_is_missing() {
        let root = temp_root("thread-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert_eq!(load_thread(&paths, "1800000000000000003").unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_thread_then_load_thread_roundtrips() {
        let root = temp_root("thread-roundtrip");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let chain = ThreadChain {
            items: vec![thread::ThreadItem {
                id: "1700000000000000001".to_string(),
                text: "hello from the timeline".to_string(),
                author_name: "Developers".to_string(),
                author_username: "XDevelopers".to_string(),
            }],
            capped: false,
        };
        save_thread(&paths, "1800000000000000003", &chain, 1_000).unwrap();
        let loaded = load_thread(&paths, "1800000000000000003").unwrap();
        assert_eq!(loaded, Some(chain));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_corrupted_thread_cache_file_is_a_clean_miss_not_an_error() {
        let root = temp_root("thread-corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.thread_file("1800000000000000003"), b"not json at all").unwrap();

        assert_eq!(load_thread(&paths, "1800000000000000003").unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }
}
