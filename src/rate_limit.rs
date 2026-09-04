//! レートリミットの追跡とリトライのバックオフ (#10)｡
//!
//! 純粋な seam が 4 つ｡`config.rs` の `now` 注入と `pkce.rs` の乱数注入の
//! 慣習に倣っている: [`parse_headers`] (ヘッダのテキスト -> 型の付いた
//! スナップショット)､[`decision`] (スナップショット + `now` -> 送るか
//! 拒むか)､[`classify_429`] (レスポンスボディ -> どの種類の 429 か)､
//! [`backoff_delay`] (試行回数 + 注入された jitter の割合 -> `Duration`)｡
//! どれも時計を読まず､実際に乱数を振らず､ディスクにもネットワークにも
//! 触らない — 触るのは [`load`]/[`save`] (ディスク) と
//! [`random_jitter_fraction`] (OS の CSPRNG) だけで､ネットワークに触るのは
//! このクレートでは `x_api::client` だけである｡
//!
//! このモジュールが存在する理由となる中心の規則: GUI アプリは､最大 15 分に
//! なりうるリセット窓を待つためにバックグラウンドスレッドを sleep させては
//! ならない｡[`decision`] は `x_api::client` が *送信前に* リクエストを
//! きっぱり拒んで､代わりにリセット時刻を持つ型付きのエラーを返すかどうかを
//! 決めるための仕組みである｡

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::paths::Paths;
use crate::x_api::model::ApiProblem;

/// 1 つのエンドポイントについて追跡するレートリミット窓｡`x-rate-limit-*`
/// レスポンスヘッダが報告する内容そのもの｡各フィールドは独立に optional で､
/// ヘッダが欠けていても､あってもゴミでも､それが「リクエストを失敗させる」
/// ことを意味しはしない — [`parse_headers`] を見よ｡
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RateLimitState {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub remaining: Option<u32>,
    /// `remaining` が `limit` に戻る時刻 (unix 秒)｡
    #[serde(default)]
    pub reset_at: Option<i64>,
}

/// 1 つのレスポンスの `x-rate-limit-limit` / `-remaining` / `-reset`
/// ヘッダをパースする｡各引数は独立に optional (ヘッダが無い) であり､パース
/// も独立に失敗しうる (ヘッダはあるがゴミ)｡どちらの場合も､パース全体を
/// 失敗させるのではなく対応するフィールドが `None` で返る｡パースできない
/// ヘッダは「情報が無い」を意味するのであって､「リクエストを失敗させる」
/// ではない｡
pub(crate) fn parse_headers(
    limit: Option<&str>,
    remaining: Option<&str>,
    reset: Option<&str>,
) -> RateLimitState {
    RateLimitState {
        limit: limit.and_then(|value| value.trim().parse().ok()),
        remaining: remaining.and_then(|value| value.trim().parse().ok()),
        reset_at: reset.and_then(|value| value.trim().parse().ok()),
    }
}

/// 追跡している窓が送るなと言うときに [`decision`] が返す型｡`ui.rs` は
/// `anyhow::Error` をこれに downcast して､素のエラー文字列ではなく
/// カウントダウンを描画する｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RateLimited {
    /// 窓がリセットされると見込まれる時刻 (unix 秒)｡分かる場合のみ｡`None`
    /// になるのは､実際の 429 レスポンスが使える `x-rate-limit-reset` を
    /// 持たなかったときだけ — [`decision`] 自身はリセット時刻が分かって
    /// いるときしか拒まない｡それが発火条件の一部だからである｡
    pub reset_at: Option<i64>,
    /// その拒否が `x-rate-limit-*` ヘッダの説明しない上限から来たかどうか
    /// (#197) — [`Refusal::Opaque`] を見よ｡追跡している窓で説明のつく拒否
    /// なら `false`｡[`decision`] 自身による送信前の拒否も含む｡この区別に
    /// 従って動くのは呼び出し側の責任である: 窓はヘッダの言う時刻に開き
    /// 直すが､opaque な上限は拒まれるたびにさらに長く間を置いて退く
    /// しかない｡
    pub opaque: bool,
}

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.reset_at {
            Some(reset_at) => write!(f, "rate limited until unix time {reset_at}"),
            None => write!(f, "rate limited (reset time unknown)"),
        }
    }
}

impl std::error::Error for RateLimited {}

/// 中心の規則 (#10): 追跡している窓が remaining ゼロを報告し､そのリセット
/// 時刻がまだ `now` より先なら､送信を拒んで待つかどうかは呼び出し側に
/// 決めさせる — 呼び出し元のスレッドを sleep させて待たせてはならない｡
/// それ以外はすべて送って安全である: remaining がゼロより大きい､remaining
/// が不明 (まだ情報が無い)､リセット時刻がすでに過ぎている｡
pub(crate) fn decision(state: RateLimitState, now: i64) -> Result<(), RateLimited> {
    match (state.remaining, state.reset_at) {
        (Some(0), Some(reset_at)) if reset_at > now => Err(RateLimited {
            reset_at: Some(reset_at),
            opaque: false,
        }),
        _ => Ok(()),
    }
}

/// 追跡している窓では説明のつかない 429 のあと､その `x-rate-limit-reset`
/// ヘッダが信用できないときに待つ長さ — [`Refusal::Opaque`] を見よ｡15 分:
/// X のエンドポイントごとの窓はこの周期で動くので､隠れた上限を数秒おきに
/// 突き直さない程度には長く､かつ経過するころには本物の窓が開き直している
/// 程度には短い｡これは *最初の* 待ちであり､再度拒まれた呼び出し側はもっと
/// 長く待つことが期待されている (`sync::state::opaque_backoff_seconds`)｡
pub(crate) const OPAQUE_LIMIT_BACKOFF_SECONDS: i64 = 900;

/// その 429 がどの上限から来たか､ヘッダから言える範囲で｡
///
/// `x-rate-limit-reset` ヘッダが正直な答えになるのは､それが記述する窓が
/// リクエストを拒んだ当の窓であるとき — つまり `remaining` がゼロのとき
/// だけである｡余裕を残したまま届く 429 (`remaining > 0`) は､X がこれらの
/// ヘッダで公開していない上限に拒まれている (実測: `POST /2/lists/:id/members`
/// は 300 のうち `remaining` 299 で 429 を返し､より厳しい書き込み上限が
/// 拒んでいた — しかもそれが 20 時間以上続いた､#197)｡そのリセットは手つかず
/// の窓のもので､数秒先かもしれないしすでに過去かもしれない｡だから信用すると
/// 呼び出し側はほとんど即座に隠れた上限を突き直すことになる｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// 追跡している窓が枯れていて､いつ開き直るかを言っている｡
    Window { reset_at: i64 },
    /// ヘッダが記述していない上限｡いつ解けるかは不明で､正直な答えは今から
    /// 保守的にバックオフすることだけである｡再度拒まれた呼び出し側は､さらに
    /// 長く退くべきである (`sync::state`)｡
    Opaque,
}

impl Refusal {
    /// 429 の窓の状態を読んで､どの上限が拒んだかに落とす｡本当に枯れていて
    /// リセットが未来にある窓以外はすべて opaque である — すでに過去の
    /// リセット (古くなったヘッダ) も､ヘッダがまったく無い場合も含めて
    /// そう扱う｡
    pub(crate) fn classify(state: RateLimitState, now: i64) -> Self {
        match (state.remaining, state.reset_at) {
            (Some(0), Some(reset_at)) if reset_at > now => Self::Window { reset_at },
            _ => Self::Opaque,
        }
    }

    /// 呼び出し側がリトライしてよい時刻 (unix 秒): 窓自身のリセットか､
    /// `now` から [`OPAQUE_LIMIT_BACKOFF_SECONDS`] 後 — 誤った時計への
    /// カウントダウンではなく､正直な「あとで試せ」である｡
    pub(crate) fn retry_at(self, now: i64) -> i64 {
        match self {
            Self::Window { reset_at } => reset_at,
            Self::Opaque => now.saturating_add(OPAQUE_LIMIT_BACKOFF_SECONDS),
        }
    }

    /// この上限に拒まれた 429 がなる型付きのエラー｡
    pub(crate) fn into_error(self, now: i64) -> RateLimited {
        RateLimited {
            reset_at: Some(self.retry_at(now)),
            opaque: matches!(self, Self::Opaque),
        }
    }
}

/// X が返しうる HTTP 429 の､種類の異なる 2 つ (#10)｡呼び出し側での文字列
/// 比較ではなく型にしてある — `x_api::client::check_status` はレスポンス
/// ボディ自体を grep する代わりにこれで match する｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RateLimitKind {
    /// プリペイド残高が尽きた (`title: "UsageCapExceeded"`)｡リトライしても
    /// 決して助けにならない — アカウントへの入金が要る｡
    UsageCapExceeded,
    /// ふつうのエンドポイントごとのレートリミット｡窓のリセット時刻に回復
    /// する｡
    RateLimited,
}

/// 429 のレスポンスボディを分類する｡`UsageCapExceeded` と見て取れない
/// ボディは — まったくパースできないものも含めて — ふつうのレートリミット
/// として扱う｡こちらのほうが安全な既定である｡決して回復しない側ではなく､
/// 自力で回復する側だからである｡
pub(crate) fn classify_429(body: &str) -> RateLimitKind {
    let title = serde_json::from_str::<ApiProblem>(body)
        .ok()
        .and_then(|problem| problem.title);
    match title.as_deref() {
        Some("UsageCapExceeded") => RateLimitKind::UsageCapExceeded,
        _ => RateLimitKind::RateLimited,
    }
}

/// 使用上限が尽きたことについて､API 自身の説明を運ぶ (#10)｡[`RateLimited`]
/// と違ってこれは自力では決して回復しないので､リセット時刻を持たない —
/// これに対してカウントダウンを出してよいものは何も無い｡
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UsageCapExceeded {
    pub detail: String,
}

impl std::fmt::Display for UsageCapExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "429 Too Many Requests — usage cap exceeded: {}",
            self.detail
        )
    }
}

impl std::error::Error for UsageCapExceeded {}

/// リトライはこの回数の再送で打ち切る (最初の試行にこの回数を足したもの)｡
/// ネットワークが不安定なときや上流が苦しいときに､1 回のリロードが
/// バックグラウンドスレッドを塞ぐ長さを抑える｡
pub(crate) const MAX_RETRIES: u32 = 4;

/// 最初のリトライまでの待ち｡
const BACKOFF_BASE_MILLIS: u64 = 500;
/// jitter を掛ける前の上限の頭打ち｡長い障害が積み上がって､試行の間隔が
/// 途方もない長さになるのを防ぐ｡
const BACKOFF_MAX_MILLIS: u64 = 30_000;

/// full jitter 付きの指数バックオフ (AWS の "full jitter" 式)｡ネットワーク
/// エラーと 5xx にだけ使う (#10) — どちらの種類の 429 にも決して使わない｡
/// 一方は自分のスケジュールで回復し､もう一方はまったく回復せず､リトライ
/// ループはそのどちらも直せないからである｡
///
/// `attempt` は 1 始まり (最初のリトライ)｡`jitter_fraction` は — テストで
/// スケジュールを決定的にするために注入し､`0.0..=1.0` に clamp する —
/// 頭打ちした指数の上限を実際の待ち時間まで縮める｡本番では
/// [`random_jitter_fraction`] 経由で､呼び出しのたびに OS の RNG から新しい
/// 割合を引く｡
pub(crate) fn backoff_delay(attempt: u32, jitter_fraction: f64) -> Duration {
    // 6 で頭打ちにする (2^6 = base の 64 倍)｡あとで `MAX_RETRIES` が増えても
    // 下のシフトが決してあふれないようにするため｡実際にはそこに届くずっと
    // 手前で `BACKOFF_MAX_MILLIS` が本当の上限になる｡
    let exponent = attempt.saturating_sub(1).min(6);
    let ceiling_millis = BACKOFF_BASE_MILLIS
        .saturating_mul(1u64 << exponent)
        .min(BACKOFF_MAX_MILLIS);
    let jitter_fraction = jitter_fraction.clamp(0.0, 1.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let delay_millis = (ceiling_millis as f64 * jitter_fraction) as u64;
    Duration::from_millis(delay_millis)
}

/// `0.0..=1.0` の新しい jitter の割合｡`getrandom` 経由で OS の CSPRNG から
/// 引く (`oauth::pkce` のためにすでに依存に入っている)｡このモジュールの
/// バックオフの seam で唯一純粋でない関数である — 本番はリトライごとに
/// 1 回呼び､テストは代わりに固定の割合で [`backoff_delay`] を直接呼ぶ｡
pub(crate) fn random_jitter_fraction() -> f64 {
    let mut bytes = [0u8; 8];
    // ここでの失敗 (OS の RNG が使えない) はきわめて稀である｡jitter を掛け
    // ない満額の待ちに落とすほうが､バックオフを飛ばすより安全である｡
    if getrandom::fill(&mut bytes).is_err() {
        return 1.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let fraction = u64::from_le_bytes(bytes) as f64 / u64::MAX as f64;
    fraction
}

/// [`RateLimitState`] がどの追跡対象エンドポイントのものか｡X のレート
/// リミットはエンドポイントごとなので､`x_api::client::XClient` が行う 2 つの
/// 呼び出しは 1 つのバケットを共有せず別々に追跡する｡
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Endpoint {
    UserLookup,
    Timeline,
    /// `GET /2/users/me` (#11) — X はこれを上の screen name 引きとは別に
    /// 制限する｡
    Me,
    /// `GET /2/users/:id/timelines/reverse_chronological` (#11) — X は home
    /// timeline を単一ユーザーの `Timeline` 取得とは別に制限する｡
    HomeTimeline,
    /// `GET /2/lists/:id/tweets` (#161) — List のタイムライン｡list id が
    /// 設定されると､ウィンドウの主たる取得元として home timeline に取って
    /// 代わる｡自分のバケットを持つのはいつもの理由による: X はこれを独自の
    /// スケジュールで制限し､しかも 2 つは対ではなく択一なので､バケットを
    /// 共有すると取得すらしていない取得元の状態を持ち回ることになる｡
    ListTimeline,
    /// `GET /2/users/:id/following` (#163) — このアプリがフォローしている
    /// アカウントの 1 ページ｡list のメンバーと差分を取るために読む｡他の
    /// エンドポイントと同じく自分のバケットを持つ: full sync は cursor が
    /// 尽きるまでこれをページングするので､上限に当たる可能性がもっとも
    /// 高く､他のエンドポイントの追跡窓を借りるとそれが見えなくなる｡
    Following,
    /// `GET /2/lists/:id/members` (#163) — その差分のもう一方の側｡
    ListMembers,
    /// `POST /2/lists/:id/members` (#163) — list にアカウントを 1 つ足す｡
    /// `RemoveListMember` と分けて追跡する理由は
    /// `CreateRepost`/`DeleteRepost` と同じ: X は作成と削除を別々に制限
    /// する｡
    AddListMember,
    /// `DELETE /2/lists/:id/members/:user_id` (#163) — アカウントを 1 つ
    /// 外す｡`AddListMember` を見よ｡
    RemoveListMember,
    /// `GET /2/tweets?ids=` (#12) — "Show thread" の裏にある親チェーンの
    /// 辿り｡独立に追跡する: 例えば `Timeline` のバケットを使い回すと両方の
    /// 追跡状態が壊れる｡X は各エンドポイントを独自のスケジュールで制限する
    /// からである｡
    TweetById,
    /// `POST /2/tweets` (#14) — composer の送信操作｡X は投稿を上のどの読み
    /// 取りエンドポイントとも別に制限するので､どれかとバケットを共有すると
    /// 両方の追跡状態が壊れる｡
    CreatePost,
    /// `POST /2/users/:id/retweets` (#15) — repost の作成｡`DeleteRepost`
    /// とは独立に追跡する: X は作成と削除を別々に制限するので､どちらかの
    /// バケットをもう一方に使い回すと両方の追跡状態が壊れる｡
    CreateRepost,
    /// `DELETE /2/users/:id/retweets/:source_tweet_id` (#15) — repost の
    /// 取り消し｡自分のバケットが要る理由は `CreateRepost` の doc を見よ｡
    DeleteRepost,
    /// `POST /2/users/:id/likes` (#68) — post へのいいね｡`DeleteLike` とは
    /// 独立に追跡する｡理由は `CreateRepost` と `DeleteRepost` を分けて追跡
    /// するのとまったく同じである｡
    CreateLike,
    /// `DELETE /2/users/:id/likes/:tweet_id` (#68) — いいねの取り消し｡
    /// `CreateLike` の doc を見よ｡
    DeleteLike,
    /// `DELETE /2/tweets/:id` (#72) — 自分の post の削除｡どの書き込み
    /// エンドポイントもそうであるように自分のバケットを持つ: X はそれぞれを
    /// 別に制限し､#18 はこれも他と同じく支出として数える必要がある｡
    DeletePost,
    /// `GET /2/users/:id/owned_lists` (#164) — サインイン中のアカウントが
    /// 所有する list｡picker のセグメント名を得るために一度読み､次に picker
    /// が呼ばれるまでキャッシュする｡他の list 読み取りと同じく自分の
    /// バケットを持つ: X は独自のスケジュールで制限するし､明示的なクリック
    /// ごとに 1 回しか起きない読み取りが､timeline のポーリングが焼き尽くす
    /// 窓を借りる筋合いは無い｡
    OwnedLists,
}

impl Endpoint {
    /// 追跡しているすべてのエンドポイント｡1 つずつではなく全体を横断して
    /// 集計する必要がある呼び出し側のためにある — 現在の利用者は `usage` の
    /// `--usage`/ヘッダの合計 (#18) なので､必要な場所ごとに複製せず一覧を
    /// ここに置いてある｡
    /// 書き込みエンドポイントもすべて数える: 単価も課金の単位 (per request)
    /// も読み取り (per resource､#162) とは違うが､どちらも支出を発生させる
    /// ことに変わりは無いので､1 つでも落とすと支出を過少に報告する —
    /// それこそ #18 が防ぐために存在する唯一の失敗である｡`CreatePost` は
    /// #50 までここから漏れていた｡新しい variant がまた漏れたら､下の
    /// テストは黙って通るのではなくコンパイルに失敗する｡
    pub(crate) const ALL: [Self; 17] = [
        Self::UserLookup,
        Self::Timeline,
        Self::Me,
        Self::HomeTimeline,
        Self::ListTimeline,
        Self::Following,
        Self::ListMembers,
        Self::AddListMember,
        Self::RemoveListMember,
        Self::TweetById,
        Self::CreatePost,
        Self::CreateRepost,
        Self::DeleteRepost,
        Self::CreateLike,
        Self::DeleteLike,
        Self::DeletePost,
        Self::OwnedLists,
    ];

    /// private ではなく `pub(crate)` である (#18 以前とは違う): `usage.rs`
    /// は自分のエンドポイント別ファイルをこれと同じ文字列で引くので､同じ
    /// エンドポイントに対する 2 つのモジュールのディスク上のキーが食い違う
    /// ことは決してない｡
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::UserLookup => "user_lookup",
            Self::Timeline => "timeline",
            Self::Me => "me",
            Self::HomeTimeline => "home_timeline",
            Self::ListTimeline => "list_timeline",
            Self::Following => "following",
            Self::ListMembers => "list_members",
            Self::AddListMember => "add_list_member",
            Self::RemoveListMember => "remove_list_member",
            Self::TweetById => "tweet_by_id",
            Self::CreatePost => "create_post",
            Self::CreateRepost => "create_repost",
            Self::DeleteRepost => "delete_repost",
            Self::CreateLike => "create_like",
            Self::DeleteLike => "delete_like",
            Self::DeletePost => "delete_post",
            Self::OwnedLists => "owned_lists",
        }
    }
}

/// [`Paths::rate_limit_file`] の中身すべて: 各エンドポイントで最後に観測した
/// 状態を､[`Endpoint::key`] をキーにして持つ｡
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RateLimitFile {
    #[serde(default)]
    endpoints: HashMap<String, RateLimitState>,
}

/// [`RateLimitFile`] をディスクから読む｡ファイルが無いのは「まだ何も追跡して
/// いない」というきれいな状態である｡壊れたファイルや形の違うファイルも､
/// エラーではなく *同じく* きれいな miss として扱う｡`cache::load_json` の
/// 規則に倣った — このファイルを失って高くつくのは､せいぜい避けられたはず
/// のリクエスト 1 回で､起動の失敗には決してならない｡
fn load_file(paths: &Paths) -> Result<RateLimitFile> {
    let path = paths.rate_limit_file();
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RateLimitFile::default());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    Ok(serde_json::from_str(&contents).unwrap_or_default())
}

/// `endpoint` について追跡している状態｡ファイルにまだ何も無ければ
/// [`RateLimitState::default`] (すべて `None`｡[`decision`] は常に送って安全と
/// 扱う) を返す｡
pub(crate) fn load(paths: &Paths, endpoint: Endpoint) -> Result<RateLimitState> {
    let file = load_file(paths)?;
    Ok(file
        .endpoints
        .get(endpoint.key())
        .copied()
        .unwrap_or_default())
}

/// `endpoint` の `state` を､すでにファイルにあった他のエンドポイントと並べて
/// 永続化する — 既存ファイルの読み取りで本物の I/O エラーが起きた場合は
/// (単に無いだけ､壊れているだけの場合と違って) そのまま伝播する｡`cache.rs`
/// が引くのと同じ区別である｡
pub(crate) fn save(paths: &Paths, endpoint: Endpoint, state: RateLimitState) -> Result<()> {
    let path = paths.rate_limit_file();
    let mut file = load_file(paths)?;
    file.endpoints.insert(endpoint.key().to_string(), state);

    let json = serde_json::to_vec_pretty(&file)
        .with_context(|| format!("could not serialize {}", path.display()))?;
    std::fs::write(&path, json).with_context(|| format!("could not write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(root: &std::path::Path) -> Paths {
        let home = root.display().to_string();
        Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap()
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "twigpui-test-rate-limit-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    // --- parse_headers ---

    #[test]
    fn parses_every_header_when_all_are_present_and_valid() {
        let state = parse_headers(Some("15"), Some("3"), Some("1700000000"));
        assert_eq!(
            state,
            RateLimitState {
                limit: Some(15),
                remaining: Some(3),
                reset_at: Some(1_700_000_000),
            }
        );
    }

    #[test]
    fn missing_headers_parse_to_none_rather_than_erroring() {
        let state = parse_headers(None, None, None);
        assert_eq!(state, RateLimitState::default());
    }

    #[test]
    fn non_numeric_header_values_parse_to_none_for_that_field_only() {
        let state = parse_headers(Some("fifteen"), Some("3"), Some("soon"));
        assert_eq!(state.limit, None);
        assert_eq!(state.remaining, Some(3));
        assert_eq!(state.reset_at, None);
    }

    #[test]
    fn tolerates_surrounding_whitespace() {
        let state = parse_headers(Some(" 15 "), Some(" 0 "), Some(" 1700000000 "));
        assert_eq!(state.limit, Some(15));
        assert_eq!(state.remaining, Some(0));
        assert_eq!(state.reset_at, Some(1_700_000_000));
    }

    #[test]
    fn a_reset_header_in_the_past_still_parses_cleanly() {
        // パースは妥当性を判断しない — それは decision() の仕事である｡
        let state = parse_headers(None, None, Some("0"));
        assert_eq!(state.reset_at, Some(0));
    }

    // --- decision ---

    #[test]
    fn refuses_to_send_when_remaining_is_zero_and_the_reset_has_not_arrived() {
        let state = RateLimitState {
            limit: Some(15),
            remaining: Some(0),
            reset_at: Some(1_000),
        };
        let error = decision(state, 500).unwrap_err();
        assert_eq!(error.reset_at, Some(1_000));
    }

    #[test]
    fn sends_when_remaining_is_zero_but_the_reset_has_already_passed() {
        let state = RateLimitState {
            limit: Some(15),
            remaining: Some(0),
            reset_at: Some(1_000),
        };
        assert!(decision(state, 1_000).is_ok());
        assert!(decision(state, 1_001).is_ok());
    }

    #[test]
    fn sends_when_remaining_is_above_zero_regardless_of_reset() {
        let state = RateLimitState {
            limit: Some(15),
            remaining: Some(1),
            reset_at: Some(1_000),
        };
        assert!(decision(state, 0).is_ok());
    }

    #[test]
    fn sends_when_there_is_no_tracked_information_at_all() {
        assert!(decision(RateLimitState::default(), 0).is_ok());
    }

    #[test]
    fn sends_when_remaining_is_zero_but_the_reset_time_is_unknown() {
        // リセット時刻が無いといつ安全に戻るか知りようがないので､永遠に
        // 塞ぐほうがたまの 429 より悪い｡
        let state = RateLimitState {
            limit: Some(15),
            remaining: Some(0),
            reset_at: None,
        };
        assert!(decision(state, 0).is_ok());
    }

    // --- Refusal ---

    #[test]
    fn an_exhausted_window_is_trusted_to_reset_when_its_header_says() {
        // remaining 0 は､この窓が本当にリクエストを拒んだ当の窓だという
        // ことなので､その窓自身のリセットが正直な答えになる｡
        let state = RateLimitState {
            limit: Some(300),
            remaining: Some(0),
            reset_at: Some(5_000),
        };
        let refusal = Refusal::classify(state, 1_000);
        assert_eq!(refusal, Refusal::Window { reset_at: 5_000 });
        assert_eq!(refusal.retry_at(1_000), 5_000);
    }

    #[test]
    fn a_429_with_headroom_left_ignores_the_window_reset() {
        // 実測した失敗 (POST /2/lists/:id/members, 2026-08-24): X は 300 の
        // うち remaining 299 で 429 を返し､リセットは手つかずのその窓の
        // もので約 14 分先だった｡信用すると呼び出し側は誤った時計で待つ
        // ことになるので､代わりに保守的な下限を使う｡
        let state = RateLimitState {
            limit: Some(300),
            remaining: Some(299),
            reset_at: Some(1_014),
        };
        let refusal = Refusal::classify(state, 1_000);
        assert_eq!(refusal, Refusal::Opaque);
        assert_eq!(
            refusal.retry_at(1_000),
            1_000 + OPAQUE_LIMIT_BACKOFF_SECONDS
        );
    }

    #[test]
    fn a_429_whose_window_reset_is_already_past_is_opaque() {
        // もう 1 つの実測サンプル: リセットヘッダはほぼ現在時刻だった｡
        // 尊重すると数秒のうちに隠れた上限を突き直すことになる｡
        let state = RateLimitState {
            limit: Some(300),
            remaining: Some(299),
            reset_at: Some(999),
        };
        assert_eq!(Refusal::classify(state, 1_000), Refusal::Opaque);
    }

    #[test]
    fn an_exhausted_window_whose_reset_already_passed_is_opaque() {
        // remaining 0 でリセットは過ぎているのに X はなお 429 を返した —
        // ヘッダが古いので､すぐリトライせずフォールバックする｡
        let state = RateLimitState {
            limit: Some(300),
            remaining: Some(0),
            reset_at: Some(999),
        };
        assert_eq!(Refusal::classify(state, 1_000), Refusal::Opaque);
    }

    #[test]
    fn a_429_with_no_headers_at_all_is_opaque() {
        let refusal = Refusal::classify(RateLimitState::default(), 1_000);
        assert_eq!(refusal, Refusal::Opaque);
        assert_eq!(
            refusal.retry_at(1_000),
            1_000 + OPAQUE_LIMIT_BACKOFF_SECONDS
        );
    }

    #[test]
    fn the_error_a_refusal_becomes_says_which_kind_it_was() {
        // そのフィールドが存在する理由そのもの (#197): sync は opaque な
        // 上限からは毎回さらに長く退くが､予定どおり開き直す窓に対して
        // それをやってはならない｡
        assert_eq!(
            Refusal::Opaque.into_error(1_000),
            RateLimited {
                reset_at: Some(1_000 + OPAQUE_LIMIT_BACKOFF_SECONDS),
                opaque: true,
            }
        );
        assert_eq!(
            Refusal::Window { reset_at: 5_000 }.into_error(1_000),
            RateLimited {
                reset_at: Some(5_000),
                opaque: false,
            }
        );
    }

    #[test]
    fn a_pre_send_refusal_by_the_tracked_window_is_not_opaque() {
        // `decision` は見えている窓についてしか拒まないので､その拒否に
        // 隠れたところは無い｡
        let state = RateLimitState {
            limit: Some(300),
            remaining: Some(0),
            reset_at: Some(5_000),
        };
        assert!(!decision(state, 1_000).unwrap_err().opaque);
    }

    // --- classify_429 ---

    #[test]
    fn classifies_a_usage_cap_body_as_usage_cap_exceeded() {
        let body =
            r#"{"title":"UsageCapExceeded","detail":"Usage cap exceeded: Monthly product cap"}"#;
        assert_eq!(classify_429(body), RateLimitKind::UsageCapExceeded);
    }

    #[test]
    fn classifies_a_different_title_as_an_ordinary_rate_limit() {
        let body = r#"{"title":"TooManyRequests","detail":"Rate limit exceeded"}"#;
        assert_eq!(classify_429(body), RateLimitKind::RateLimited);
    }

    #[test]
    fn classifies_an_unparseable_body_as_the_recoverable_kind() {
        assert_eq!(classify_429("not json"), RateLimitKind::RateLimited);
    }

    #[test]
    fn classifies_an_empty_body_as_the_recoverable_kind() {
        assert_eq!(classify_429(""), RateLimitKind::RateLimited);
    }

    // --- backoff_delay ---

    #[test]
    fn backoff_delay_is_zero_with_zero_jitter() {
        assert_eq!(backoff_delay(1, 0.0), Duration::ZERO);
        assert_eq!(backoff_delay(4, 0.0), Duration::ZERO);
    }

    #[test]
    fn backoff_delay_doubles_the_ceiling_each_attempt_with_full_jitter() {
        assert_eq!(backoff_delay(1, 1.0), Duration::from_millis(500));
        assert_eq!(backoff_delay(2, 1.0), Duration::from_secs(1));
        assert_eq!(backoff_delay(3, 1.0), Duration::from_secs(2));
    }

    #[test]
    fn backoff_delay_is_capped_for_a_large_attempt_count() {
        assert_eq!(backoff_delay(20, 1.0), Duration::from_secs(30));
    }

    #[test]
    fn backoff_delay_scales_linearly_with_the_jitter_fraction() {
        assert_eq!(backoff_delay(1, 0.5), Duration::from_millis(250));
    }

    #[test]
    fn backoff_delay_clamps_an_out_of_range_jitter_fraction() {
        assert_eq!(backoff_delay(1, -1.0), Duration::ZERO);
        assert_eq!(backoff_delay(1, 2.0), Duration::from_millis(500));
    }

    #[test]
    fn backoff_delay_is_deterministic_given_the_same_inputs() {
        assert_eq!(backoff_delay(3, 0.37), backoff_delay(3, 0.37));
    }

    // --- load / save ---

    #[test]
    fn load_is_the_default_state_when_nothing_is_on_file() {
        let root = temp_root("load-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert_eq!(
            load(&paths, Endpoint::Timeline).unwrap(),
            RateLimitState::default()
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_then_load_roundtrips_for_the_same_endpoint() {
        let root = temp_root("roundtrip");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let state = RateLimitState {
            limit: Some(15),
            remaining: Some(3),
            reset_at: Some(1_700_000_000),
        };
        save(&paths, Endpoint::Timeline, state).unwrap();
        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), state);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_keeps_other_endpoints_state_untouched() {
        let root = temp_root("multi-endpoint");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let user_lookup_state = RateLimitState {
            limit: Some(300),
            remaining: Some(299),
            reset_at: Some(1_000),
        };
        let timeline_state = RateLimitState {
            limit: Some(15),
            remaining: Some(0),
            reset_at: Some(2_000),
        };
        save(&paths, Endpoint::UserLookup, user_lookup_state).unwrap();
        save(&paths, Endpoint::Timeline, timeline_state).unwrap();

        assert_eq!(
            load(&paths, Endpoint::UserLookup).unwrap(),
            user_lookup_state
        );
        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), timeline_state);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn me_and_home_timeline_endpoints_are_tracked_independently_of_the_originals() {
        // #11: X は `/users/me` と home timeline を､既存の user-lookup や
        // 単一ユーザーの timeline エンドポイントとは別に制限するので､
        // どちらかとキーを共有すると両方の追跡状態が壊れる｡
        let root = temp_root("four-endpoints");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let user_lookup_state = RateLimitState {
            limit: Some(300),
            remaining: Some(299),
            reset_at: Some(1_000),
        };
        let timeline_state = RateLimitState {
            limit: Some(15),
            remaining: Some(10),
            reset_at: Some(2_000),
        };
        let me_state = RateLimitState {
            limit: Some(25),
            remaining: Some(24),
            reset_at: Some(3_000),
        };
        let home_timeline_state = RateLimitState {
            limit: Some(15),
            remaining: Some(0),
            reset_at: Some(4_000),
        };
        save(&paths, Endpoint::UserLookup, user_lookup_state).unwrap();
        save(&paths, Endpoint::Timeline, timeline_state).unwrap();
        save(&paths, Endpoint::Me, me_state).unwrap();
        save(&paths, Endpoint::HomeTimeline, home_timeline_state).unwrap();

        assert_eq!(
            load(&paths, Endpoint::UserLookup).unwrap(),
            user_lookup_state
        );
        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), timeline_state);
        assert_eq!(load(&paths, Endpoint::Me).unwrap(), me_state);
        assert_eq!(
            load(&paths, Endpoint::HomeTimeline).unwrap(),
            home_timeline_state
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn tweet_by_id_endpoint_is_tracked_independently_of_the_others() {
        // #12: `GET /2/tweets?ids=` は自分のバケットを持つ — 例えば
        // `Timeline` のを使い回すと両方の追跡状態が壊れる｡
        let root = temp_root("tweet-by-id-endpoint");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let timeline_state = RateLimitState {
            limit: Some(15),
            remaining: Some(10),
            reset_at: Some(2_000),
        };
        let tweet_by_id_state = RateLimitState {
            limit: Some(300),
            remaining: Some(0),
            reset_at: Some(5_000),
        };
        save(&paths, Endpoint::Timeline, timeline_state).unwrap();
        save(&paths, Endpoint::TweetById, tweet_by_id_state).unwrap();

        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), timeline_state);
        assert_eq!(
            load(&paths, Endpoint::TweetById).unwrap(),
            tweet_by_id_state
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn create_post_endpoint_is_tracked_independently_of_the_others() {
        // #14: `POST /2/tweets` は自分のバケットを持つ — 例えば
        // `Timeline` のを使い回すと両方の追跡状態が壊れる｡
        let root = temp_root("create-post-endpoint");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let timeline_state = RateLimitState {
            limit: Some(15),
            remaining: Some(10),
            reset_at: Some(2_000),
        };
        let create_post_state = RateLimitState {
            limit: Some(200),
            remaining: Some(0),
            reset_at: Some(6_000),
        };
        save(&paths, Endpoint::Timeline, timeline_state).unwrap();
        save(&paths, Endpoint::CreatePost, create_post_state).unwrap();

        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), timeline_state);
        assert_eq!(
            load(&paths, Endpoint::CreatePost).unwrap(),
            create_post_state
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn create_repost_and_delete_repost_endpoints_are_tracked_independently() {
        // #15: 作成と削除はそれぞれ自分のバケットを持つ — どちらかのを
        // もう一方に､あるいは既存のエンドポイントに使い回すと､両方の
        // 追跡状態が壊れる｡
        let root = temp_root("repost-endpoints");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let timeline_state = RateLimitState {
            limit: Some(15),
            remaining: Some(10),
            reset_at: Some(2_000),
        };
        let create_repost_state = RateLimitState {
            limit: Some(50),
            remaining: Some(49),
            reset_at: Some(7_000),
        };
        let delete_repost_state = RateLimitState {
            limit: Some(50),
            remaining: Some(0),
            reset_at: Some(8_000),
        };
        save(&paths, Endpoint::Timeline, timeline_state).unwrap();
        save(&paths, Endpoint::CreateRepost, create_repost_state).unwrap();
        save(&paths, Endpoint::DeleteRepost, delete_repost_state).unwrap();

        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), timeline_state);
        assert_eq!(
            load(&paths, Endpoint::CreateRepost).unwrap(),
            create_repost_state
        );
        assert_eq!(
            load(&paths, Endpoint::DeleteRepost).unwrap(),
            delete_repost_state
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_corrupted_rate_limit_file_is_a_clean_miss_not_an_error() {
        let root = temp_root("corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.rate_limit_file(), b"not json at all").unwrap();

        assert_eq!(
            load(&paths, Endpoint::Timeline).unwrap(),
            RateLimitState::default()
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_recovers_cleanly_from_a_corrupted_existing_file() {
        let root = temp_root("save-over-corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.rate_limit_file(), b"{ not valid json").unwrap();

        let state = RateLimitState {
            limit: Some(15),
            remaining: Some(15),
            reset_at: None,
        };
        save(&paths, Endpoint::Timeline, state).unwrap();
        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), state);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn endpoint_all_lists_every_variant_with_a_unique_key() {
        // #18 の usage tracker は全エンドポイントを横断して集計するために
        // `Endpoint::ALL` を回すので､そこから漏れた variant は黙って支出を
        // 過少に数えることになる｡このテストの以前の版はキーが一意なことしか
        // 検査していなかった｡だから `CreatePost` がリリース 1 本分まるごと
        // 漏れていても何も落ちなかった (#50)｡
        //
        // 実際のガードは下の match である: 網羅的なので､variant を足すと
        // 新しい腕を書くまでこのファイルはコンパイルできなくなる — そして
        // その腕は､一緒に伸びなければならない一覧のすぐ隣にある｡一意性
        // だけ､あるいは素の長さチェックだけでは､漏れがあっても通って
        // しまう｡
        let every = [
            Endpoint::UserLookup,
            Endpoint::Timeline,
            Endpoint::Me,
            Endpoint::HomeTimeline,
            Endpoint::ListTimeline,
            Endpoint::Following,
            Endpoint::ListMembers,
            Endpoint::AddListMember,
            Endpoint::RemoveListMember,
            Endpoint::TweetById,
            Endpoint::CreatePost,
            Endpoint::CreateRepost,
            Endpoint::DeleteRepost,
            Endpoint::CreateLike,
            Endpoint::DeleteLike,
            Endpoint::DeletePost,
            Endpoint::OwnedLists,
        ];
        for endpoint in every {
            match endpoint {
                Endpoint::UserLookup
                | Endpoint::Timeline
                | Endpoint::Me
                | Endpoint::HomeTimeline
                | Endpoint::ListTimeline
                | Endpoint::Following
                | Endpoint::ListMembers
                | Endpoint::AddListMember
                | Endpoint::RemoveListMember
                | Endpoint::TweetById
                | Endpoint::CreatePost
                | Endpoint::CreateRepost
                | Endpoint::DeleteRepost
                | Endpoint::CreateLike
                | Endpoint::DeleteLike
                | Endpoint::DeletePost
                | Endpoint::OwnedLists => {}
            }
            assert!(
                Endpoint::ALL.contains(&endpoint),
                "{endpoint:?} is missing from Endpoint::ALL, so its requests \
                 would not be counted as spend"
            );
        }
        assert_eq!(Endpoint::ALL.len(), every.len());

        let keys: std::collections::HashSet<&str> = Endpoint::ALL
            .iter()
            .map(|endpoint| endpoint.key())
            .collect();
        assert_eq!(keys.len(), Endpoint::ALL.len());
    }

    #[test]
    fn a_genuine_io_error_reading_the_rate_limit_file_still_propagates() {
        let root = temp_root("io-error");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        // ファイルがあるはずの場所にディレクトリがあるのは､破損ではなく
        // 本物の I/O エラーである — 握り潰さず表に出す必要がある｡
        std::fs::create_dir(paths.rate_limit_file()).unwrap();

        assert!(load(&paths, Endpoint::Timeline).is_err());

        std::fs::remove_dir_all(&root).unwrap();
    }
}
