//! リクエスト数による使用量の追跡 (#18): 追跡対象の各エンドポイントが
//! いくら費やしたかを､累計と今日の分について､`state_dir` の下に再起動を
//! またいで永続化する｡
//!
//! `rate_limit::Endpoint` を再利用する — X は同じ 5 つのエンドポイントを
//! 別々に制限し (このモジュールも別々に数え)､ここに並行した enum を置いても
//! `Endpoint` の刻印を削り落としただけのものになる｡[`Endpoint::ALL`] が
//! あるので､このモジュール (と `main.rs` の `--usage`) は一覧を重複させずに
//! 追跡対象のエンドポイントをすべて回れる｡
//!
//! `rate_limit.rs` 自身の慣習に倣った 4 つの純粋な継ぎ目: [`record`]
//! (保存済みの数 + 注入した `now` -> 次の数｡"today" バケツのロールオーバーを
//! 含む)､[`today_count`] (保存済みの数 + `now` -> 何も変更せずに "today" が
//! *今この瞬間* どう読めるか — 深夜を過ぎてから次のリクエストが書き込む前に
//! ファイルを読む場合を扱う)､[`estimated_amount`] (リクエスト数 + 設定
//! された価格 (任意) -> 金額 (任意)｡価格が設定されていなければ常に `None`
//! で､その代わりに推測した数を置くことは決してない)､そして
//! [`budget_status`] (今日の合計 + 設定された予算 (任意) -> ヘッダが描く
//! べき 3 段階の深刻度のどれか)｡[`build_report`] はこの 4 つを合成して､
//! ヘッダと `--usage` の両方が見せるのと同じ形にする｡だから「使用量の
//! 数字」が何を意味するかを決める場所はちょうど 1 つだ｡メモリの外に触れる
//! のは [`load`]/[`save`] (ディスク) と `x_api::client::XClient::get`
//! (ネットワーク｡[`record_request`] 経由) だけだ｡
//!
//! ## 日付の境界: ローカル時刻ではなく UTC
//!
//! "Today" はマシンのローカルの深夜ではなく UTC の深夜にリセットされる｡
//! 理由は 2 つ:
//!
//! 1. X API 自身が `created_at` を UTC で報告する (`ui::format_timestamp`
//!    の doc comment を見よ) — このアカウントのデータについて X が持つ
//!    「1 日」の概念はすでに UTC なので､同じ境界に対して支出を追えば
//!    "today" はこのアプリのどこでも一貫した 1 つの意味を保つ｡2 種類の
//!    "today" のために 2 つの異なる時計を持たずに済む｡
//! 2. Rust の標準ライブラリには､日付/時刻の crate (`chrono`, `time`, ...)
//!    無しにローカルの UTC オフセットを確実に読む手段が無いし､この crate
//!    は今のところそれに依存していない｡UTC なら何も要らない: 日の境界は
//!    ちょうど `unix_seconds.div_euclid(86_400)` で､`oauth::unix_now()` が
//!    すでにここの他のモジュールすべてに渡しているのと同じ `i64` の Unix
//!    タイムスタンプの上で計算する｡この issue のために新しい依存は要らな
//!    かった｡
//!
//! ここで受け入れているトレードオフ: UTC より西にいる人には､"today" が
//! 自分の深夜ではなくローカルの午後の途中でロールオーバーするように見える｡
//! 暗黙に任せず､ここと README に書いてある｡

use std::collections::HashMap;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::paths::Paths;
use crate::rate_limit::Endpoint;

/// 1 日の秒数 — [`epoch_day`] が Unix タイムスタンプを振り分ける単位だ｡
const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// 設定された予算のうち､[`budget_status`] が予算を使い切るのを待たずに
/// 警告を始める割合 — 予算設定の要点は近づいているのが見えることであって､
/// 越えた瞬間に初めて知ることではない｡
const NEAR_BUDGET_RATIO: f64 = 0.8;

/// 1 つのエンドポイントについて追跡しているリクエスト数: 全期間分と､
/// 現在の UTC 日の分 — なぜ UTC かはモジュール doc を見よ｡全フィールドの
/// 既定値がゼロなので､まだファイルに項目の無いエンドポイント (や､
/// フィールドの欠けた古いバージョンのファイル) はパース失敗ではなく
/// 「一度も呼ばれていない」と読める｡
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

/// 何も変更せずに `entry.today` が *今この瞬間* どう読めるか: `now` が
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

/// `now` の時点で `entry` にリクエストを 1 つ記録する: `total` は常に
/// 増える｡`today` は､`entry.today_epoch_day` から見て `now` が新しい UTC
/// 日に入っていればゼロから､そうでなければ現在の値から増える｡素の `+` では
/// なく `saturating_add` なのは､リクエストカウンタで `u64` が overflow する
/// のは天文学的にありえないとはいえ､黙ってゼロに巻き戻るほうが saturate
/// より悪い失敗モードだからだ｡
pub(crate) fn record(entry: EndpointUsage, now: i64) -> EndpointUsage {
    EndpointUsage {
        total: entry.total.saturating_add(1),
        today: today_count(entry, now).saturating_add(1),
        today_epoch_day: epoch_day(now),
    }
}

/// `count` を推定金額に変える｡単位は `price_per_request` が何で表されて
/// いようとそれに従う (このアプリは通貨を決めつけない) — 価格が設定されて
/// いなければ `None`｡これは #18 が強制するために存在する唯一の規則だ:
/// もっともらしいが誤った数は数が無いより悪いので､この crate のどこにも
/// 組み込みの既定価格は無い｡
pub(crate) fn estimated_amount(count: u64, price_per_request: Option<f64>) -> Option<f64> {
    // f64 として精度を失うほど (2^53) 大きいリクエスト数は､このアプリでは
    // 現実的でない｡
    #[allow(clippy::cast_precision_loss)]
    price_per_request.map(|price| price * count as f64)
}

/// 設定された日次予算 (任意) に対して､今日の使用量が 3 段階の深刻度の
/// どれに当たるか｡予算がまったく設定されていなければ常に `Ok` だ — 警告
/// すべき相手が無い｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BudgetStatus {
    /// 予算が未設定か､今日の数が余裕をもって予算を下回っている｡
    Ok,
    /// 今日の数が予算の [`NEAR_BUDGET_RATIO`] に達したが､予算そのものには
    /// まだ達していない｡
    Near,
    /// 今日の数が予算に達したか､それを越えた｡
    Exceeded,
}

/// `today_total` を `daily_budget` に対して分類する — [`BudgetStatus`] を
/// 見よ｡`budget == 0` はゼロ除算にせず (そうすると `today_total == 0` で
/// `NaN` が出て以下の比較がすべて偽になる)､非負のどんな数でもすでに超過
/// しているものとして扱う｡
pub(crate) fn budget_status(today_total: u64, daily_budget: Option<u32>) -> BudgetStatus {
    let Some(budget) = daily_budget else {
        return BudgetStatus::Ok;
    };
    if budget == 0 {
        return BudgetStatus::Exceeded;
    }

    #[allow(clippy::cast_precision_loss)]
    let ratio = today_total as f64 / f64::from(budget);
    if ratio >= 1.0 {
        BudgetStatus::Exceeded
    } else if ratio >= NEAR_BUDGET_RATIO {
        BudgetStatus::Near
    } else {
        BudgetStatus::Ok
    }
}

/// [`Paths::usage_file`] の全内容: 全エンドポイントの追跡した数を
/// [`Endpoint::key`] をキーにして持つ｡
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct UsageFile {
    #[serde(default)]
    endpoints: HashMap<String, EndpointUsage>,
}

/// [`UsageFile`] をディスクから読む｡ファイルが無いのは「まだ何も追跡して
/// いない」という綺麗なケースだ｡壊れたファイルや形の違うファイルも *同様に*
/// エラーではなく綺麗なミスとして扱う｡`rate_limit::load_file` と
/// `cache::load_json` が共有する規則に倣っている — このファイルを失っても
/// 代償はせいぜい累計カウンタであって､起動失敗には決してならない｡
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
    Ok(serde_json::from_str(&contents).unwrap_or_default())
}

/// `endpoint` について追跡している使用量｡まだファイルに何も無ければ
/// [`EndpointUsage::default`] (すべてゼロ) を返す｡
pub(crate) fn load(paths: &Paths, endpoint: Endpoint) -> Result<EndpointUsage> {
    let file = load_file(paths)?;
    Ok(file
        .endpoints
        .get(endpoint.key())
        .copied()
        .unwrap_or_default())
}

/// [`Endpoint::ALL`] の全エンドポイントについて追跡している使用量｡
/// エンドポイントごとにではなくファイルを一度だけ読む — ヘッダの更新と
/// `--usage` が使い､どちらも常に全エンドポイントを一度に欲しがる｡
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

/// `endpoint` の `usage` を､すでにファイルにあった他のエンドポイントと
/// 並べて永続化する — 既存ファイルを読む際の本物の I/O エラー (単に
/// 無いとか壊れているのとは違う) は依然として伝播する｡`rate_limit::save`
/// と `cache.rs` が引いているのと同じ区別だ｡
pub(crate) fn save(paths: &Paths, endpoint: Endpoint, usage: EndpointUsage) -> Result<()> {
    let path = paths.usage_file();
    let mut file = load_file(paths)?;
    file.endpoints.insert(endpoint.key().to_string(), usage);

    let json = serde_json::to_vec_pretty(&file)
        .with_context(|| format!("could not serialize {}", path.display()))?;
    std::fs::write(&path, json).with_context(|| format!("could not write {}", path.display()))
}

/// `now` の時点で `endpoint` にリクエストを 1 つ記録する｡ディスクに触る —
/// このモジュールの計数の継ぎ目で唯一純粋でない関数で､
/// `x_api::client::XClient::get` から実際の HTTP 送信 1 回につき 1 度
/// 呼ばれる (リトライも含む｡それぞれ別に課金されるからだ — その関数自身の
/// doc comment を見よ)｡
pub(crate) fn record_request(paths: &Paths, endpoint: Endpoint, now: i64) -> Result<()> {
    let current = load(paths, endpoint)?;
    let updated = record(current, now);
    save(paths, endpoint, updated)
}

/// 追跡対象の全エンドポイントを合計した数 — ヘッダと `--usage` の両方が
/// 「その」使用量の数字として見せるもので､一目では誰も求めていない
/// エンドポイント別の 5 つの数字ではない｡`today` は合計する前に項目ごとに
/// [`today_count`] のロールオーバーを適用するので､UTC の深夜直後に
/// (次のリクエストが何か書き込む前に) 読んだ要約は､昨日の古い数ではなく
/// ゼロを見せる｡
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Totals {
    pub total: u64,
    pub today: u64,
}

pub(crate) fn totals(entries: &HashMap<Endpoint, EndpointUsage>, now: i64) -> Totals {
    entries
        .values()
        .fold(Totals::default(), |acc, entry| Totals {
            total: acc.total.saturating_add(entry.total),
            today: acc.today.saturating_add(today_count(*entry, now)),
        })
}

/// [`UsageReport`] に見せる 1 つのエンドポイントの数 — `today` には
/// [`today_count`] のロールオーバーが適用済みだ｡
#[derive(Debug, Serialize)]
pub(crate) struct EndpointReport {
    pub total: u64,
    pub today: u64,
}

/// [`UsageReport`] の集計値: 全エンドポイントを通した合計と､そこから
/// `estimated_amount`/`budget_status` が呼び出し元の設定した価格/予算と
/// 合わせて導くものだ｡
#[derive(Debug, Serialize)]
pub(crate) struct TotalsReport {
    pub total_requests: u64,
    pub today_requests: u64,
    pub price_per_request: Option<f64>,
    pub estimated_amount_total: Option<f64>,
    pub estimated_amount_today: Option<f64>,
    pub daily_budget: Option<u32>,
    pub budget_status: &'static str,
}

/// 機械可読な使用量レポート一式 (#18): `--usage` が JSON として印字する
/// ものであり､ヘッダが描くのと同じ数字だ — 1 つの関数が両方を作るので､
/// 2 つが離れていく余地は無い｡
#[derive(Debug, Serialize)]
pub(crate) struct UsageReport {
    pub endpoints: HashMap<String, EndpointReport>,
    pub total: TotalsReport,
}

/// 追跡対象の各エンドポイントの保存済みの数､注入した `now`､呼び出し元の
/// 設定した価格/予算 (どちらも完全に任意 — モジュール doc を見よ) から
/// [`UsageReport`] を組み立てる｡純粋だ: `paths` を自分で読まず読み込み済みの
/// `entries` を受け取るので､ディスクに触れずにテストできる｡このモジュールの
/// 他の 3 つの継ぎ目に揃えてある｡
pub(crate) fn build_report(
    entries: &HashMap<Endpoint, EndpointUsage>,
    now: i64,
    price_per_request: Option<f64>,
    daily_budget: Option<u32>,
) -> UsageReport {
    let endpoints = entries
        .iter()
        .map(|(endpoint, usage)| {
            (
                endpoint.key().to_string(),
                EndpointReport {
                    total: usage.total,
                    today: today_count(*usage, now),
                },
            )
        })
        .collect();

    let totals = totals(entries, now);
    let status = match budget_status(totals.today, daily_budget) {
        BudgetStatus::Ok => "ok",
        BudgetStatus::Near => "near",
        BudgetStatus::Exceeded => "exceeded",
    };

    UsageReport {
        endpoints,
        total: TotalsReport {
            total_requests: totals.total,
            today_requests: totals.today,
            price_per_request,
            estimated_amount_total: estimated_amount(totals.total, price_per_request),
            estimated_amount_today: estimated_amount(totals.today, price_per_request),
            daily_budget,
            budget_status: status,
        },
    }
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

    /// 呼び出し側で `clippy::float_cmp` に引っかからない浮動小数の等値
    /// 比較 — ここで比較する値はすべて正確に表現できるものを選んである
    /// ので､これは本物のバグを覆い隠しうる許容誤差ではなく､念のための
    /// epsilon だ｡
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
        // 日が変わってからファイルは書かれていないが､読み手 (ヘッダや
        // `--usage`) が昨日の古い数を見せてはならない｡
        let entry = EndpointUsage {
            total: 10,
            today: 3,
            today_epoch_day: 0,
        };
        assert_eq!(today_count(entry, 86_400), 0);
    }

    // --- record ---

    #[test]
    fn record_increments_both_total_and_today_within_the_same_day() {
        let entry = EndpointUsage {
            total: 5,
            today: 2,
            today_epoch_day: 0,
        };
        let updated = record(entry, 100);
        assert_eq!(updated.total, 6);
        assert_eq!(updated.today, 3);
        assert_eq!(updated.today_epoch_day, 0);
    }

    #[test]
    fn record_resets_today_to_one_when_crossing_the_utc_day_boundary() {
        let entry = EndpointUsage {
            total: 5,
            today: 2,
            today_epoch_day: 0,
        };
        let updated = record(entry, 86_400);
        assert_eq!(updated.total, 6, "total keeps accumulating regardless");
        assert_eq!(updated.today, 1, "today starts over at exactly one");
        assert_eq!(updated.today_epoch_day, 1);
    }

    #[test]
    fn record_pins_the_exact_boundary_second() {
        // 86_399 はまだ "day 0" (23:59:59 UTC) で､86_400 が "day 1"
        // (00:00:00 UTC) だ — ロールオーバーはちょうどこの秒に当たらねば
        // ならず､その前後の秒ではいけない｡
        let entry = EndpointUsage {
            total: 0,
            today: 0,
            today_epoch_day: 0,
        };
        assert_eq!(record(entry, 86_399).today, 1);
        assert_eq!(record(entry, 86_399).today_epoch_day, 0);

        let rolled = record(entry, 86_400);
        assert_eq!(rolled.today, 1);
        assert_eq!(rolled.today_epoch_day, 1);
    }

    #[test]
    fn record_starts_a_fresh_entry_at_one() {
        let updated = record(EndpointUsage::default(), 0);
        assert_eq!(updated.total, 1);
        assert_eq!(updated.today, 1);
    }

    // --- estimated_amount ---

    #[test]
    fn estimated_amount_is_none_without_a_configured_price() {
        // #18 の中核の規則: 価格が未設定なら金額は決して表示しない —
        // 推測した数を出すことは決してない｡
        assert_eq!(estimated_amount(42, None), None);
    }

    #[test]
    fn estimated_amount_multiplies_count_by_the_configured_price() {
        let amount = estimated_amount(4, Some(2.5)).unwrap();
        assert!(approx_eq(amount, 10.0), "{amount}");
    }

    #[test]
    fn estimated_amount_is_zero_for_zero_requests_even_with_a_price() {
        let amount = estimated_amount(0, Some(2.5)).unwrap();
        assert!(approx_eq(amount, 0.0), "{amount}");
    }

    // --- budget_status ---

    #[test]
    fn budget_status_is_ok_without_a_configured_budget() {
        assert_eq!(budget_status(1_000_000, None), BudgetStatus::Ok);
    }

    #[test]
    fn budget_status_is_ok_comfortably_under_the_budget() {
        assert_eq!(budget_status(5, Some(10)), BudgetStatus::Ok);
    }

    #[test]
    fn budget_status_is_near_at_the_warning_threshold() {
        // 8/10 = 0.8 でちょうど NEAR_BUDGET_RATIO と一致する｡
        assert_eq!(budget_status(8, Some(10)), BudgetStatus::Near);
    }

    #[test]
    fn budget_status_is_ok_just_below_the_warning_threshold() {
        assert_eq!(budget_status(7, Some(10)), BudgetStatus::Ok);
    }

    #[test]
    fn budget_status_is_exceeded_at_the_budget() {
        assert_eq!(budget_status(10, Some(10)), BudgetStatus::Exceeded);
    }

    #[test]
    fn budget_status_is_exceeded_past_the_budget() {
        assert_eq!(budget_status(11, Some(10)), BudgetStatus::Exceeded);
    }

    #[test]
    fn budget_status_treats_a_zero_budget_as_already_exceeded() {
        assert_eq!(budget_status(0, Some(0)), BudgetStatus::Exceeded);
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
        // ファイルがあるべき場所のディレクトリは破損ではなく本物の I/O
        // エラーだ — 握り潰さず表に出さねばならない｡
        std::fs::create_dir(paths.usage_file()).unwrap();

        assert!(load(&paths, Endpoint::Timeline).is_err());

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- record_request (薄い I/O ラッパー) ---

    #[test]
    fn record_request_persists_an_incremented_count() {
        let root = temp_root("record-request");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        record_request(&paths, Endpoint::Timeline, 0).unwrap();
        record_request(&paths, Endpoint::Timeline, 100).unwrap();

        let usage = load(&paths, Endpoint::Timeline).unwrap();
        assert_eq!(usage.total, 2);
        assert_eq!(usage.today, 2);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn record_request_rolls_today_over_across_a_day_boundary() {
        let root = temp_root("record-request-rollover");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        record_request(&paths, Endpoint::Timeline, 86_399).unwrap();
        record_request(&paths, Endpoint::Timeline, 86_400).unwrap();

        let usage = load(&paths, Endpoint::Timeline).unwrap();
        assert_eq!(usage.total, 2, "total never resets");
        assert_eq!(usage.today, 1, "today resets across the UTC boundary");

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- totals ---

    #[test]
    fn totals_sums_every_endpoints_stored_counts() {
        let mut entries = HashMap::new();
        entries.insert(
            Endpoint::UserLookup,
            EndpointUsage {
                total: 3,
                today: 1,
                today_epoch_day: 0,
            },
        );
        entries.insert(
            Endpoint::Timeline,
            EndpointUsage {
                total: 5,
                today: 2,
                today_epoch_day: 0,
            },
        );

        let totals = totals(&entries, 0);
        assert_eq!(totals.total, 8);
        assert_eq!(totals.today, 3);
    }

    #[test]
    fn totals_applies_the_rollover_per_entry_before_summing() {
        let mut entries = HashMap::new();
        entries.insert(
            Endpoint::UserLookup,
            EndpointUsage {
                total: 3,
                today: 1,
                today_epoch_day: 0,
            },
        );

        // 次の UTC 日から読む: それ以降何も記録されていなくても `today` は
        // ゼロへロールオーバーせねばならない｡
        let totals = totals(&entries, 86_400);
        assert_eq!(totals.total, 3);
        assert_eq!(totals.today, 0);
    }

    // --- build_report ---

    #[test]
    fn build_report_shows_counts_without_amounts_when_no_price_is_configured() {
        let mut entries = HashMap::new();
        entries.insert(
            Endpoint::Timeline,
            EndpointUsage {
                total: 4,
                today: 4,
                today_epoch_day: 0,
            },
        );

        let report = build_report(&entries, 0, None, None);
        assert_eq!(report.total.total_requests, 4);
        assert_eq!(report.total.today_requests, 4);
        assert_eq!(report.total.price_per_request, None);
        assert_eq!(report.total.estimated_amount_total, None);
        assert_eq!(report.total.estimated_amount_today, None);
        assert_eq!(report.total.budget_status, "ok");
        assert_eq!(report.endpoints["timeline"].total, 4);
    }

    #[test]
    fn build_report_includes_estimated_amounts_once_a_price_is_configured() {
        let mut entries = HashMap::new();
        entries.insert(
            Endpoint::Timeline,
            EndpointUsage {
                total: 4,
                today: 2,
                today_epoch_day: 0,
            },
        );

        let report = build_report(&entries, 0, Some(2.5), None);
        assert!(approx_eq(
            report.total.estimated_amount_total.unwrap(),
            10.0
        ));
        assert!(approx_eq(report.total.estimated_amount_today.unwrap(), 5.0));
    }

    #[test]
    fn build_report_reflects_the_budget_status_string() {
        let mut entries = HashMap::new();
        entries.insert(
            Endpoint::Timeline,
            EndpointUsage {
                total: 10,
                today: 10,
                today_epoch_day: 0,
            },
        );

        let report = build_report(&entries, 0, None, Some(10));
        assert_eq!(report.total.budget_status, "exceeded");
        assert_eq!(report.total.daily_budget, Some(10));
    }
}
