//! resource 単位での使用量の追跡 (#162､#18 の後継): 追跡対象の各
//! endpoint がいくら費やしたかを､累計と今日の分について `state_dir` の下に
//! 再起動をまたいで永続化する｡
//!
//! ## #18 からの転換: リクエスト数ではなく resource 数
//!
//! X の読み取りは**リクエスト単位ではなく resource 単位** (返ってきた
//! オブジェクトの数) で課金される (`x-api-budget` skill の `pricing.md` —
//! `max_results=100` の取得 1 本が 99 resource として課金されることを実測
//! 済み)｡#18 はリクエスト本数を数えていたため､実際の消費より 1〜2 桁
//! 小さい数字を見せていた｡ここではレスポンス body の `data` から返って
//! きた id を数える ([`kind::extract_resource_ids`])｡書き込みは今も
//! リクエスト単位のまま — X 自身がそう課金する｡
//!
//! 単価は種別ごとに 10 倍違う ([`kind::ResourceKind`]) ので､合算せず
//! 種別ごとに持つ｡`Endpoint::kind` ([`kind`] モジュール) が対応表だ｡
//!
//! ## 同日 dedup
//!
//! X は resource を UTC 24 時間の窓で重複除去する: 同じ id を同じ日に
//! 何度取得しても 2 回目以降は課金されない (`pricing.md` の原文引用)｡
//! [`dedup`] がこれを再現する — キーは `(種別, id)` で､[`Endpoint`] では
//! ない｡同じ post が `Timeline` と `ListTimeline` の両方から返ってきても
//! 1 回しか課金されないのと同じに扱うためだ｡dedup を Users/Owned にも
//! 同じ関数で適用しているのは､X が文書化しているのは Posts の dedup
//! だけだが､`(kind, id)` を素直に一般化した先が今のコードだからで､
//! Posts 以外の実測はまだ無い｡
//!
//! ## `rate_limit::Endpoint` を再利用する
//!
//! X は同じ 17 個の endpoint を別々に制限し (このモジュールも別々に数え)､
//! ここに並行した enum を置いても `Endpoint` の刻印を削り落としただけの
//! ものになる｡[`Endpoint::ALL`] があるので､このモジュール (と `main.rs`
//! の `--usage`) は一覧を重複させずに追跡対象の endpoint をすべて回れる｡
//!
//! ## 純粋な継ぎ目
//!
//! [`record`] (保存済みの数 + 注入した `now` + 課金する resource 数 ->
//! 次の数｡"today" バケツのロールオーバーを含む)､[`today_count`] (保存
//! 済みの数 + `now` -> 何も変更せずに "today" が*今この瞬間*どう読めるか
//! — 深夜を過ぎてから次のリクエストが書き込む前にファイルを読む場合を
//! 扱う)､[`dedup`] (保存済みの id 集合 + 注入した `now` + 新しく届いた
//! id -> 今日まだ課金していない id の数 + 更新後の集合)､
//! [`estimated_amount`] (Posts の resource 数 + 単価 (USD) -> 推定金額
//! (USD))､そして [`budget_status`] (今日の Posts resource 数 + 日次予算
//! -> ヘッダが描くべき 3 段階の深刻度のどれか)｡
//! [`report::build_report`] はこれらを合成して､ヘッダと `--usage` の
//! 両方が見せるのと同じ形にする｡メモリの外に触れるのは [`load`]/[`save`]/
//! [`load_file`]/[`write_file`] (ディスク) と `x_api::client::XClient::get`
//! (ネットワーク｡[`record_response`] 経由) だけだ｡
//!
//! ## 日付の境界: ローカル時刻ではなく UTC
//!
//! "Today" はマシンのローカルの深夜ではなく UTC の深夜にリセットされる｡
//! 理由は 2 つ:
//!
//! 1. X API 自身が `created_at` を UTC で報告し､dedup の窓自体も UTC の
//!    24 時間なので､同じ境界に対して支出を追えば "today" はこのアプリの
//!    どこでも一貫した 1 つの意味を保つ｡
//! 2. UTC なら日の境界がちょうど `unix_seconds.div_euclid(86_400)` で､
//!    `oauth::unix_now()` がすでにここの他のモジュールすべてに渡している
//!    のと同じ `i64` の Unix タイムスタンプの上で計算できる｡
//!
//! ここで受け入れているトレードオフ: UTC より西にいる人には､"today" が
//! 自分の深夜ではなくローカルの午後の途中でロールオーバーするように見える｡
//! 暗黙に任せず､ここと README に書いてある｡

use std::collections::{BTreeSet, HashMap};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::paths::Paths;
use crate::rate_limit::Endpoint;

mod kind;
mod report;

pub(crate) use kind::{ResourceKind, extract_resource_ids};
pub(crate) use report::build_report;

/// 1 日の秒数 — [`epoch_day`] が Unix タイムスタンプを振り分ける単位だ｡
const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// 設定された予算のうち､[`budget_status`] が予算を使い切るのを待たずに
/// 警告を始める割合 — 予算設定の要点は近づいているのが見えることであって､
/// 越えた瞬間に初めて知ることではない｡
const NEAR_BUDGET_RATIO: f64 = 0.8;

/// `usage.json` の現在のスキーマ版 (#162)｡#18 まではリクエスト本数､ここ
/// からは resource 数で､単位が違うので同じ数として引き継がない —
/// [`load_file`] を見よ｡
const CURRENT_VERSION: u32 = 1;

/// 1 つの endpoint について追跡している課金対象の数: 全期間分と､現在の
/// UTC 日の分 — なぜ UTC かはモジュール doc を見よ｡読み取り endpoint は
/// resource 数､書き込み endpoint はリクエスト数 (`Endpoint::kind` が
/// [`ResourceKind::Write`] を返すもの) — [`record`] の呼び出し側
/// ([`record_response`]) がどちらを渡すかを決める｡全フィールドの既定値が
/// ゼロなので､まだファイルに項目の無い endpoint (や､フィールドの欠けた
/// 古いバージョンのファイル) はパース失敗ではなく「一度も呼ばれていない」
/// と読める｡
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EndpointUsage {
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub today: u64,
    /// `today` を最後にリセットした対象の UTC epoch day ([`epoch_day`] を
    /// 見よ)｡日付リテラルとして保存せず現在の epoch day と比較するので､
    /// 日付の整形もパースも一切要らない｡
    #[serde(default)]
    pub today_epoch_day: i64,
}

/// `now` (Unix 秒) が属する UTC epoch day｡2 つのタイムスタンプが同じ日に
/// 写るのは､UTC の深夜がその間に挟まらないときちょうどそのときだ｡
pub(crate) fn epoch_day(now: i64) -> i64 {
    now.div_euclid(SECONDS_PER_DAY)
}

/// 何も変更せずに `entry.today` が*今この瞬間*どう読めるか: `now` が
/// 最後のリセット対象の UTC 日の中にまだあれば保存済みの数､そうでなければ
/// ゼロ — 深夜を過ぎてから､次の [`record`] 呼び出しが実際にディスク上で
/// リセットする前にファイルを読む場合を扱う｡
pub(crate) fn today_count(entry: EndpointUsage, now: i64) -> u64 {
    if epoch_day(now) == entry.today_epoch_day {
        entry.today
    } else {
        0
    }
}

/// `now` の時点で `entry` に `amount` 件の課金対象を記録する (#162: #18 の
/// `record` は常に 1 件だったが､読み取りは 1 回の応答が何十件もの
/// resource を運ぶので可変にした)｡`total` は常に増える｡`today` は､
/// `entry.today_epoch_day` から見て `now` が新しい UTC 日に入っていれば
/// ゼロから､そうでなければ現在の値から増える｡素の `+` ではなく
/// `saturating_add` なのは､カウンタで `u64` が overflow するのは
/// 天文学的にありえないとはいえ､黙ってゼロに巻き戻るほうが saturate
/// より悪い失敗モードだからだ｡
pub(crate) fn record(entry: EndpointUsage, now: i64, amount: u64) -> EndpointUsage {
    EndpointUsage {
        total: entry.total.saturating_add(amount),
        today: today_count(entry, now).saturating_add(amount),
        today_epoch_day: epoch_day(now),
    }
}

/// `count` (Posts の resource 数) を USD の推定金額に変える｡単位は USD
/// 固定だ (#162): X 自身が USD 建てで単価を公開している
/// (`https://docs.x.com/x-api/getting-started/pricing`) ので､Posts の
/// resource 数という単位が定まった今､これはもう当てずっぽうの価格では
/// なく出典のある数になる｡#18 が課していた「組み込みの既定価格は置かない」
/// 規則 (単位の定まらない数に価格を掛けないための規則だった) は､その理由が
/// 消えたのでここだけ意図して上書きする — `config::DEFAULT_POST_RESOURCE_PRICE`
/// を見よ｡
pub(crate) fn estimated_amount(count: u64, post_resource_price: f64) -> f64 {
    // f64 として精度を失うほど (2^53) 大きい resource 数は､このアプリでは
    // 現実的でない｡
    #[allow(clippy::cast_precision_loss)]
    let count = count as f64;
    count * post_resource_price
}

/// 日次予算に対して､今日の Posts resource 数が 3 段階の深刻度のどれに
/// 当たるか (#162: `daily_post_budget` は既定値 `1000` を持つので､予算が
/// 未設定という状態はもう無い — `config::DEFAULT_DAILY_POST_BUDGET` を見よ)｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BudgetStatus {
    /// 今日の数が余裕をもって予算を下回っている｡
    Ok,
    /// 今日の数が予算の [`NEAR_BUDGET_RATIO`] に達したが､予算そのものには
    /// まだ達していない｡
    Near,
    /// 今日の数が予算に達したか､それを越えた｡
    Exceeded,
}

/// `today_total` (Posts の resource 数) を `daily_budget` に対して分類する
/// — [`BudgetStatus`] を見よ｡`daily_budget == 0` はゼロ除算にせず (そうすると
/// `today_total == 0` で `NaN` が出て以下の比較がすべて偽になる)､非負の
/// どんな数でもすでに超過しているものとして扱う｡
pub(crate) fn budget_status(today_total: u64, daily_budget: u32) -> BudgetStatus {
    if daily_budget == 0 {
        return BudgetStatus::Exceeded;
    }

    #[allow(clippy::cast_precision_loss)]
    let ratio = today_total as f64 / f64::from(daily_budget);
    if ratio >= 1.0 {
        BudgetStatus::Exceeded
    } else if ratio >= NEAR_BUDGET_RATIO {
        BudgetStatus::Near
    } else {
        BudgetStatus::Ok
    }
}

/// 1 つの [`ResourceKind`] について､その日すでに課金した id の集合
/// (#162)｡X の重複排除 (`pricing.md`) をこちらでも再現する — [`dedup`]
/// を見よ｡
///
/// ponytail: `usage.json` はリクエストのたびに丸ごと読み書きするので､
/// 一度に数千件の id が届く操作 (`--sync-list` のフォロー全読み) は
/// この集合をそのぶん太らせ､ファイルサイズがリクエスト 1 回あたり
/// 増える｡天井は「フォロー数千件のアカウントで 1 日あたり数百 KB」｡
/// 上げるなら､日をまたいだ古い集合を [`load_file`] 側で刈り込むか､
/// dedup 状態を `usage.json` から別ファイルへ切り出す｡
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct DedupBucket {
    #[serde(default)]
    epoch_day: i64,
    #[serde(default)]
    ids: BTreeSet<String>,
}

/// `bucket` に対して `ids` のうち今日まだ課金していないものを判定する
/// (#162)｡`now` が `bucket` の最後の epoch day と違う UTC 日なら､
/// 集合を空から始める (前日の id は今日の課金には関係ない — dedup の窓は
/// UTC 24 時間だ)｡戻り値は更新後の `bucket` と､新しく課金する件数｡
///
/// `BTreeSet` なのは `HashSet` と違って serialize の順序が安定するからで､
/// `usage.json` の diff を読みやすくする以上の意味は無い｡
pub(crate) fn dedup(bucket: DedupBucket, ids: &[String], now: i64) -> (DedupBucket, u64) {
    // ponytail-red(#162): まだ実装していない｡何も新規と認めない誤った
    // stub で､RED を確認するために置いてある｡
    let _ = ids;
    (
        DedupBucket {
            epoch_day: epoch_day(now),
            ids: bucket.ids,
        },
        0,
    )
}

/// [`Paths::usage_file`] の全内容: バージョンと､全 endpoint の追跡した数
/// ([`Endpoint::key`] をキーにする)､種別ごとの dedup 状態
/// ([`ResourceKind::key`] をキーにする)｡
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct UsageFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    endpoints: HashMap<String, EndpointUsage>,
    #[serde(default)]
    dedup: HashMap<String, DedupBucket>,
}

/// [`UsageFile`] をディスクから読む｡ファイルが無いのは「まだ何も追跡して
/// いない」という綺麗なケースだ｡壊れたファイルや形の違うファイルも*同様に*
/// エラーではなく綺麗なミスとして扱う — `rate_limit::load_file` と
/// `cache::load_json` が共有する規則に倣っている｡
///
/// **バージョンが [`CURRENT_VERSION`] と違うファイルも同じに扱う** (#162):
/// #18 まではリクエスト本数を数えていて､resource 数とは単位が違う｡古い
/// ファイル (`version` フィールドが無ければ `#[serde(default)]` で `0` に
/// なる) をそのまま読み継ぐと､リクエスト本数と resource 数が同じカウンタに
/// 混ざる｡だから version が違えば「まだ何も追跡していない」として初期化
/// する — 全期間の累計はこの切り替えでリセットされる｡[`write_file`] が
/// 保存のたびに `version` を [`CURRENT_VERSION`] へ揃えるので､一度でも
/// 書けばこの初期化は二度と起きない｡
fn load_file(paths: &Paths) -> Result<UsageFile> {
    let path = paths.usage_file();
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(UsageFile::default());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    let file: UsageFile = serde_json::from_str(&contents).unwrap_or_default();
    // ponytail-red(#162): まだバージョンを見ていない｡RED を確認するための
    // stub — 古いファイルもそのまま通してしまう誤った振る舞い｡
    Ok(file)
}

/// `file` を [`CURRENT_VERSION`] を刻んでディスクへ書く｡[`load_file`] の
/// バージョンチェックが自分の書いたファイルに二度と当たらないための唯一の
/// 書き込み口 — `load`/`save`/`record_response` はすべてここを通る｡
fn write_file(paths: &Paths, mut file: UsageFile) -> Result<()> {
    file.version = CURRENT_VERSION;
    let path = paths.usage_file();
    let json = serde_json::to_vec_pretty(&file)
        .with_context(|| format!("could not serialize {}", path.display()))?;
    std::fs::write(&path, json).with_context(|| format!("could not write {}", path.display()))
}

/// `endpoint` について追跡している使用量｡まだファイルに何も無ければ
/// [`EndpointUsage::default`] (すべてゼロ) を返す｡テスト専用 (#162):
/// 本体のコードは [`load_all`] か [`record_response`] を使う｡
#[cfg(test)]
pub(crate) fn load(paths: &Paths, endpoint: Endpoint) -> Result<EndpointUsage> {
    let file = load_file(paths)?;
    Ok(file
        .endpoints
        .get(endpoint.key())
        .copied()
        .unwrap_or_default())
}

/// [`Endpoint::ALL`] の全 endpoint について追跡している使用量｡endpoint
/// ごとにではなくファイルを一度だけ読む — ヘッダの更新と `--usage` が使い､
/// どちらも常に全 endpoint を一度に欲しがる｡
pub(crate) fn load_all(paths: &Paths) -> Result<HashMap<Endpoint, EndpointUsage>> {
    let file = load_file(paths)?;
    Ok(Endpoint::ALL
        .into_iter()
        .map(|endpoint| {
            let usage = file
                .endpoints
                .get(endpoint.key())
                .copied()
                .unwrap_or_default();
            (endpoint, usage)
        })
        .collect())
}

/// `endpoint` の `usage` を､すでにファイルにあった他の endpoint や dedup
/// 状態と並べて永続化する — 既存ファイルを読む際の本物の I/O エラー (単に
/// 無いとか壊れているのとは違う) は依然として伝播する｡テスト専用 (#162):
/// dedup を経由せず endpoint の数だけを直に置きたいテストのための薄い
/// 入口で､本体の記録経路は [`record_response`]｡
#[cfg(test)]
pub(crate) fn save(paths: &Paths, endpoint: Endpoint, usage: EndpointUsage) -> Result<()> {
    let mut file = load_file(paths)?;
    file.endpoints.insert(endpoint.key().to_string(), usage);
    write_file(paths, file)
}

/// `now` の時点で `endpoint` の応答 `body` を 1 件記録する (#162)｡
/// `x_api::client::XClient::send_with_retry` から実際の HTTP 応答を受け
/// 取るたび呼ばれる — このモジュールの計数の継ぎ目で唯一純粋でない関数だ｡
///
/// `endpoint.kind()` が [`ResourceKind::Write`] なら `body` は見ずに 1 件
/// (request 単位) を記録する｡読み取りなら [`extract_resource_ids`] で
/// `body` から id を取り出し､[`dedup`] で今日まだ課金していない分だけを
/// 数える｡読み書き問わずファイルは 1 回読んで 1 回書く
/// (`load_file`→`write_file`) — dedup 状態と endpoint の数を別々に
/// 読み書きすると､途中の状態を読む競合が増えるだけで得るものが無い｡
pub(crate) fn record_response(
    paths: &Paths,
    endpoint: Endpoint,
    body: &str,
    now: i64,
) -> Result<()> {
    let mut file = load_file(paths)?;
    let kind = endpoint.kind();

    let amount = if kind == ResourceKind::Write {
        1
    } else {
        let ids = extract_resource_ids(body);
        let bucket = file.dedup.remove(kind.key()).unwrap_or_default();
        let (updated_bucket, billed) = dedup(bucket, &ids, now);
        file.dedup.insert(kind.key().to_string(), updated_bucket);
        billed
    };

    let entry = file
        .endpoints
        .get(endpoint.key())
        .copied()
        .unwrap_or_default();
    file.endpoints
        .insert(endpoint.key().to_string(), record(entry, now, amount));

    write_file(paths, file)
}

/// 追跡対象の endpoint を合計した数 — [`kind_totals`]/[`posts_totals`] が
/// 種別で絞り込んだ後に使う形｡`today` は合計する前に項目ごとに
/// [`today_count`] のロールオーバーを適用するので､UTC の深夜直後に
/// (次のリクエストが何か書き込む前に) 読んだ要約は､昨日の古い数ではなく
/// ゼロを見せる｡
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Totals {
    pub total: u64,
    pub today: u64,
}

/// `entries` のうち `kind` の endpoint だけを合計する｡
fn kind_totals(entries: &HashMap<Endpoint, EndpointUsage>, kind: ResourceKind, now: i64) -> Totals {
    entries
        .iter()
        .filter(|(endpoint, _)| endpoint.kind() == kind)
        .fold(Totals::default(), |acc, (_, entry)| Totals {
            total: acc.total.saturating_add(entry.total),
            today: acc.today.saturating_add(today_count(*entry, now)),
        })
}

/// Posts の resource 数だけを合計する (#162): ヘッダの footer が見せる数字
/// はこれで､X の `project_usage` と同じ単位になり Developer Console の
/// 数字と照合できる｡Users/Owned/Write は `--usage` の JSON にだけ残る
/// ([`report::build_report`] の `by_kind`)｡
pub(crate) fn posts_totals(entries: &HashMap<Endpoint, EndpointUsage>, now: i64) -> Totals {
    kind_totals(entries, ResourceKind::Posts, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(root: &std::path::Path) -> Paths {
        let home = root.display().to_string();
        Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap()
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("twigpui-test-usage-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    // --- epoch_day ---

    #[test]
    fn epoch_day_is_zero_at_the_unix_epoch() {
        assert_eq!(epoch_day(0), 0);
    }

    #[test]
    fn epoch_day_stays_the_same_within_one_utc_day() {
        assert_eq!(epoch_day(0), epoch_day(86_399));
    }

    #[test]
    fn epoch_day_advances_exactly_at_utc_midnight() {
        assert_eq!(epoch_day(86_400), epoch_day(0) + 1);
    }

    // --- today_count (ロールオーバー､読み取り専用) ---

    #[test]
    fn today_count_reads_the_stored_value_within_the_same_utc_day() {
        let entry = EndpointUsage {
            total: 10,
            today: 3,
            today_epoch_day: 0,
        };
        assert_eq!(today_count(entry, 86_399), 3);
    }

    #[test]
    fn today_count_reads_zero_once_a_new_utc_day_has_started() {
        let entry = EndpointUsage {
            total: 10,
            today: 3,
            today_epoch_day: 0,
        };
        assert_eq!(today_count(entry, 86_400), 0);
    }

    // --- record (#162: amount は可変) ---

    #[test]
    fn record_increments_both_total_and_today_within_the_same_day() {
        let entry = EndpointUsage {
            total: 5,
            today: 2,
            today_epoch_day: 0,
        };
        let updated = record(entry, 100, 1);
        assert_eq!(updated.total, 6);
        assert_eq!(updated.today, 3);
        assert_eq!(updated.today_epoch_day, 0);
    }

    #[test]
    fn record_resets_today_to_the_amount_when_crossing_the_utc_day_boundary() {
        let entry = EndpointUsage {
            total: 5,
            today: 2,
            today_epoch_day: 0,
        };
        let updated = record(entry, 86_400, 1);
        assert_eq!(updated.total, 6, "total keeps accumulating regardless");
        assert_eq!(updated.today, 1, "today starts over at exactly the amount");
        assert_eq!(updated.today_epoch_day, 1);
    }

    #[test]
    fn record_pins_the_exact_boundary_second() {
        let entry = EndpointUsage {
            total: 0,
            today: 0,
            today_epoch_day: 0,
        };
        assert_eq!(record(entry, 86_399, 1).today, 1);
        assert_eq!(record(entry, 86_399, 1).today_epoch_day, 0);

        let rolled = record(entry, 86_400, 1);
        assert_eq!(rolled.today, 1);
        assert_eq!(rolled.today_epoch_day, 1);
    }

    #[test]
    fn record_starts_a_fresh_entry_at_the_given_amount() {
        let updated = record(EndpointUsage::default(), 0, 1);
        assert_eq!(updated.total, 1);
        assert_eq!(updated.today, 1);
    }

    #[test]
    fn record_adds_a_resource_count_greater_than_one_in_a_single_call() {
        // #162 の核心: 1 回の応答が何十件もの resource を運びうる —
        // `max_results=100` の取得 1 本が 99 resource として課金された実測
        // (`pricing.md`) がこの挙動を要求する｡
        let updated = record(EndpointUsage::default(), 0, 42);
        assert_eq!(updated.total, 42);
        assert_eq!(updated.today, 42);
    }

    // --- estimated_amount (#162: 価格は常に設定されている) ---

    #[test]
    fn estimated_amount_multiplies_count_by_the_configured_price() {
        let amount = estimated_amount(4, 2.5);
        assert!(approx_eq(amount, 10.0), "{amount}");
    }

    #[test]
    fn estimated_amount_is_zero_for_zero_resources() {
        let amount = estimated_amount(0, 2.5);
        assert!(approx_eq(amount, 0.0), "{amount}");
    }

    // --- budget_status (#162: 予算は常に設定されている) ---

    #[test]
    fn budget_status_is_ok_comfortably_under_the_budget() {
        assert_eq!(budget_status(5, 10), BudgetStatus::Ok);
    }

    #[test]
    fn budget_status_is_near_at_the_warning_threshold() {
        assert_eq!(budget_status(8, 10), BudgetStatus::Near);
    }

    #[test]
    fn budget_status_is_ok_just_below_the_warning_threshold() {
        assert_eq!(budget_status(7, 10), BudgetStatus::Ok);
    }

    #[test]
    fn budget_status_is_exceeded_at_the_budget() {
        assert_eq!(budget_status(10, 10), BudgetStatus::Exceeded);
    }

    #[test]
    fn budget_status_is_exceeded_past_the_budget() {
        assert_eq!(budget_status(11, 10), BudgetStatus::Exceeded);
    }

    #[test]
    fn budget_status_treats_a_zero_budget_as_already_exceeded() {
        assert_eq!(budget_status(0, 0), BudgetStatus::Exceeded);
    }

    // --- dedup (#162: 同日は 1 回だけ課金) ---

    #[test]
    fn dedup_bills_a_new_id() {
        let (_, billed) = dedup(DedupBucket::default(), &["1".to_string()], 0);
        assert_eq!(billed, 1);
    }

    #[test]
    fn dedup_does_not_bill_the_same_id_twice_within_the_same_utc_day() {
        let (bucket, first) = dedup(DedupBucket::default(), &["1".to_string()], 0);
        assert_eq!(first, 1);
        let (_, second) = dedup(bucket, &["1".to_string()], 100);
        assert_eq!(second, 0, "same id, same UTC day, must not bill again");
    }

    #[test]
    fn dedup_bills_again_after_the_utc_day_rolls_over() {
        let (bucket, _) = dedup(DedupBucket::default(), &["1".to_string()], 0);
        let (_, billed) = dedup(bucket, &["1".to_string()], 86_400);
        assert_eq!(billed, 1, "a new UTC day reopens the same id for billing");
    }

    #[test]
    fn dedup_bills_only_the_new_ids_in_a_mixed_batch() {
        let (bucket, _) = dedup(DedupBucket::default(), &["1".to_string()], 0);
        let (_, billed) = dedup(
            bucket,
            &["1".to_string(), "2".to_string(), "3".to_string()],
            100,
        );
        assert_eq!(
            billed, 2,
            "only 2 and 3 are new; 1 was already billed today"
        );
    }

    // --- load / save / load_all ---

    #[test]
    fn load_is_the_default_usage_when_nothing_is_on_file() {
        let root = temp_root("load-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert_eq!(
            load(&paths, Endpoint::Timeline).unwrap(),
            EndpointUsage::default()
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_then_load_roundtrips_for_the_same_endpoint() {
        let root = temp_root("roundtrip");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let usage = EndpointUsage {
            total: 7,
            today: 2,
            today_epoch_day: 3,
        };
        save(&paths, Endpoint::Timeline, usage).unwrap();
        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), usage);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_keeps_other_endpoints_usage_untouched() {
        let root = temp_root("multi-endpoint");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let user_lookup_usage = EndpointUsage {
            total: 3,
            today: 1,
            today_epoch_day: 0,
        };
        let timeline_usage = EndpointUsage {
            total: 9,
            today: 4,
            today_epoch_day: 0,
        };
        save(&paths, Endpoint::UserLookup, user_lookup_usage).unwrap();
        save(&paths, Endpoint::Timeline, timeline_usage).unwrap();

        assert_eq!(
            load(&paths, Endpoint::UserLookup).unwrap(),
            user_lookup_usage
        );
        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), timeline_usage);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn load_all_returns_every_tracked_endpoint_including_untouched_ones() {
        let root = temp_root("load-all");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let timeline_usage = EndpointUsage {
            total: 2,
            today: 2,
            today_epoch_day: 0,
        };
        save(&paths, Endpoint::Timeline, timeline_usage).unwrap();

        let all = load_all(&paths).unwrap();
        assert_eq!(all.len(), Endpoint::ALL.len());
        assert_eq!(all[&Endpoint::Timeline], timeline_usage);
        assert_eq!(all[&Endpoint::Me], EndpointUsage::default());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_corrupted_usage_file_is_a_clean_miss_not_an_error() {
        let root = temp_root("corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.usage_file(), b"not json at all").unwrap();

        assert_eq!(
            load(&paths, Endpoint::Timeline).unwrap(),
            EndpointUsage::default()
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_recovers_cleanly_from_a_corrupted_existing_file() {
        let root = temp_root("save-over-corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.usage_file(), b"{ not valid json").unwrap();

        let usage = EndpointUsage {
            total: 1,
            today: 1,
            today_epoch_day: 0,
        };
        save(&paths, Endpoint::Timeline, usage).unwrap();
        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), usage);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_genuine_io_error_reading_the_usage_file_still_propagates() {
        let root = temp_root("io-error");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::create_dir(paths.usage_file()).unwrap();

        assert!(load(&paths, Endpoint::Timeline).is_err());

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- version の切り替え (#162) ---

    #[test]
    fn a_pre_162_usage_file_without_a_version_field_is_treated_as_untracked() {
        let root = temp_root("pre-162-version");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        // #18 が書いていた形: version フィールドが無い｡リクエスト本数
        // だったこの `total: 999` は resource 数として引き継いではならない｡
        std::fs::write(
            paths.usage_file(),
            br#"{"endpoints":{"timeline":{"total":999,"today":999,"today_epoch_day":0}}}"#,
        )
        .unwrap();

        assert_eq!(
            load(&paths, Endpoint::Timeline).unwrap(),
            EndpointUsage::default(),
            "a version-less file must reset, not carry over request counts as resource counts"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn after_one_save_the_file_is_versioned_and_keeps_accumulating() {
        // version スタンプの罠: write_file が version を刻まないと､次の
        // load がまた 0 を見て「まだ何も追跡していない」に戻り､累計が
        // 絶対に貯まらなくなる｡
        let root = temp_root("version-sticks");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save(
            &paths,
            Endpoint::Timeline,
            EndpointUsage {
                total: 1,
                today: 1,
                today_epoch_day: 0,
            },
        )
        .unwrap();
        save(
            &paths,
            Endpoint::Timeline,
            EndpointUsage {
                total: 2,
                today: 2,
                today_epoch_day: 0,
            },
        )
        .unwrap();

        assert_eq!(load(&paths, Endpoint::Timeline).unwrap().total, 2);

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- record_response (#162: 実際の記録経路) ---

    #[test]
    fn record_response_bills_the_resource_count_from_the_body() {
        let root = temp_root("record-response-posts");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let body = r#"{"data":[{"id":"1"},{"id":"2"},{"id":"3"}]}"#;
        record_response(&paths, Endpoint::Timeline, body, 0).unwrap();

        let usage = load(&paths, Endpoint::Timeline).unwrap();
        assert_eq!(usage.total, 3);
        assert_eq!(usage.today, 3);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn record_response_does_not_bill_the_same_post_twice_in_one_day_across_endpoints() {
        // 同じ post が Timeline と ListTimeline の両方から返ってきても､
        // 同じ日には 1 回しか課金されない — dedup のキーは (kind, id) で
        // endpoint ではないことの直接の証拠｡
        let root = temp_root("record-response-cross-endpoint-dedup");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let body = r#"{"data":[{"id":"1"}]}"#;
        record_response(&paths, Endpoint::Timeline, body, 0).unwrap();
        record_response(&paths, Endpoint::ListTimeline, body, 100).unwrap();

        let all = load_all(&paths).unwrap();
        assert_eq!(all[&Endpoint::Timeline].total, 1);
        assert_eq!(
            all[&Endpoint::ListTimeline].total,
            0,
            "the same post id was already billed today via Timeline"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn record_response_keeps_dedup_state_separate_per_resource_kind() {
        // id が "1" で衝突しても Posts と Users は別勘定 — dedup は
        // (kind, id) のキーで､id だけでは判定しない｡
        let root = temp_root("record-response-kind-isolation");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let body = r#"{"data":[{"id":"1"}]}"#;
        record_response(&paths, Endpoint::Timeline, body, 0).unwrap(); // Posts
        record_response(&paths, Endpoint::UserLookup, body, 0).unwrap(); // Users

        let all = load_all(&paths).unwrap();
        assert_eq!(all[&Endpoint::Timeline].total, 1);
        assert_eq!(all[&Endpoint::UserLookup].total, 1);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn record_response_bills_a_write_endpoint_once_regardless_of_body() {
        let root = temp_root("record-response-write");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        // 書き込みのレスポンスに `data` があっても id 抽出はしない —
        // per request のまま｡
        let body = r#"{"data":{"id":"1","text":"posted"}}"#;
        record_response(&paths, Endpoint::CreatePost, body, 0).unwrap();
        record_response(&paths, Endpoint::CreatePost, body, 100).unwrap();

        let usage = load(&paths, Endpoint::CreatePost).unwrap();
        assert_eq!(usage.total, 2);

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- kind_totals / posts_totals ---

    #[test]
    fn posts_totals_sums_only_posts_kind_endpoints() {
        let mut entries = HashMap::new();
        entries.insert(
            Endpoint::Timeline,
            EndpointUsage {
                total: 5,
                today: 2,
                today_epoch_day: 0,
            },
        );
        entries.insert(
            Endpoint::HomeTimeline,
            EndpointUsage {
                total: 3,
                today: 1,
                today_epoch_day: 0,
            },
        );
        entries.insert(
            Endpoint::UserLookup,
            EndpointUsage {
                total: 100,
                today: 100,
                today_epoch_day: 0,
            },
        );

        let totals = posts_totals(&entries, 0);
        assert_eq!(totals.total, 8, "Users endpoint must not be included");
        assert_eq!(totals.today, 3);
    }

    #[test]
    fn posts_totals_applies_the_rollover_per_entry_before_summing() {
        let mut entries = HashMap::new();
        entries.insert(
            Endpoint::Timeline,
            EndpointUsage {
                total: 3,
                today: 1,
                today_epoch_day: 0,
            },
        );

        let totals = posts_totals(&entries, 86_400);
        assert_eq!(totals.total, 3);
        assert_eq!(totals.today, 0);
    }
}
