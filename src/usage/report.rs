//! 機械可読な使用量レポート (#162､#18 の後継): `--usage` が JSON として
//! 印字するものであり､ヘッダが描くのと同じ数字だ — [`build_report`] 1 つが
//! 両方を作るので､2 つが離れていく余地は無い｡
//!
//! footer が見せるのは Posts の resource 数だけ ([`TotalsReport`]) だが､
//! Users/Owned/Write の内訳を捨てはしない — `by_kind` に残す
//! ([`UsageReport::by_kind`])｡

use std::collections::HashMap;

use serde::Serialize;

use crate::rate_limit::Endpoint;

use super::{
    BudgetStatus, EndpointUsage, ResourceKind, budget_status, estimated_amount, kind_totals,
};

/// [`UsageReport`] に見せる 1 つの endpoint の数 — `today` には
/// [`super::today_count`] のロールオーバーが適用済みだ｡読み取り endpoint
/// では resource 数､書き込み endpoint ではリクエスト数 (`kind` が
/// `"write"`) — [`ResourceKind`] を見よ｡
#[derive(Debug, Serialize)]
pub(crate) struct EndpointReport {
    pub kind: &'static str,
    pub total: u64,
    pub today: u64,
}

/// 1 つの [`ResourceKind`] を通した合計 — [`UsageReport::by_kind`] の値｡
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct KindTotals {
    pub total: u64,
    pub today: u64,
}

/// [`UsageReport`] の集計値: Posts の resource 数を通した合計と､そこから
/// [`estimated_amount`]/[`budget_status`] が導くもの (#162: footer が見せる
/// のはこの構造体だけ — Users/Owned/Write は `by_kind` にある)｡
#[derive(Debug, Serialize)]
pub(crate) struct TotalsReport {
    pub posts_total: u64,
    pub posts_today: u64,
    pub post_resource_price: f64,
    pub estimated_amount_total: f64,
    pub estimated_amount_today: f64,
    pub daily_post_budget: u32,
    pub budget_status: &'static str,
}

/// 機械可読な使用量レポート一式 (#162)｡
#[derive(Debug, Serialize)]
pub(crate) struct UsageReport {
    pub endpoints: HashMap<String, EndpointReport>,
    pub by_kind: HashMap<String, KindTotals>,
    pub total: TotalsReport,
}

/// 追跡対象の各 endpoint の保存済みの数､注入した `now`､呼び出し元の設定
/// した Posts 単価/日次予算から [`UsageReport`] を組み立てる｡純粋だ:
/// `paths` を自分で読まず読み込み済みの `entries` を受け取るので､ディスクに
/// 触れずにテストできる｡
pub(crate) fn build_report(
    entries: &HashMap<Endpoint, EndpointUsage>,
    now: i64,
    post_resource_price: f64,
    daily_post_budget: u32,
) -> UsageReport {
    let endpoints = entries
        .iter()
        .map(|(endpoint, usage)| {
            (
                endpoint.key().to_string(),
                EndpointReport {
                    kind: endpoint.kind().key(),
                    total: usage.total,
                    today: super::today_count(*usage, now),
                },
            )
        })
        .collect();

    let by_kind: HashMap<String, KindTotals> = ResourceKind::ALL
        .into_iter()
        .map(|kind| {
            let totals = kind_totals(entries, kind, now);
            (
                kind.key().to_string(),
                KindTotals {
                    total: totals.total,
                    today: totals.today,
                },
            )
        })
        .collect();

    let posts = by_kind
        .get(ResourceKind::Posts.key())
        .copied()
        .unwrap_or(KindTotals { total: 0, today: 0 });

    let status = match budget_status(posts.today, daily_post_budget) {
        BudgetStatus::Ok => "ok",
        BudgetStatus::Near => "near",
        BudgetStatus::Exceeded => "exceeded",
    };

    UsageReport {
        endpoints,
        by_kind,
        total: TotalsReport {
            posts_total: posts.total,
            posts_today: posts.today,
            post_resource_price,
            estimated_amount_total: estimated_amount(posts.total, post_resource_price),
            estimated_amount_today: estimated_amount(posts.today, post_resource_price),
            daily_post_budget,
            budget_status: status,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn build_report_shows_the_posts_totals_in_the_top_level_report() {
        let mut entries = HashMap::new();
        entries.insert(
            Endpoint::Timeline,
            EndpointUsage {
                total: 4,
                today: 4,
                today_epoch_day: 0,
            },
        );

        let report = build_report(&entries, 0, 0.005, 1000);
        assert_eq!(report.total.posts_total, 4);
        assert_eq!(report.total.posts_today, 4);
        assert!(approx_eq(report.total.post_resource_price, 0.005));
        assert!(approx_eq(report.total.estimated_amount_total, 0.02));
        assert_eq!(report.total.daily_post_budget, 1000);
        assert_eq!(report.total.budget_status, "ok");
        assert_eq!(report.endpoints["timeline"].total, 4);
        assert_eq!(report.endpoints["timeline"].kind, "posts");
    }

    #[test]
    fn build_report_computes_estimated_amounts_from_the_configured_price() {
        let mut entries = HashMap::new();
        entries.insert(
            Endpoint::Timeline,
            EndpointUsage {
                total: 4,
                today: 2,
                today_epoch_day: 0,
            },
        );

        let report = build_report(&entries, 0, 2.5, 1000);
        assert!(approx_eq(report.total.estimated_amount_total, 10.0));
        assert!(approx_eq(report.total.estimated_amount_today, 5.0));
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

        let report = build_report(&entries, 0, 0.005, 10);
        assert_eq!(report.total.budget_status, "exceeded");
        assert_eq!(report.total.daily_post_budget, 10);
    }

    #[test]
    fn build_report_splits_non_posts_endpoints_into_by_kind_only() {
        let mut entries = HashMap::new();
        entries.insert(
            Endpoint::Timeline,
            EndpointUsage {
                total: 4,
                today: 4,
                today_epoch_day: 0,
            },
        );
        entries.insert(
            Endpoint::UserLookup,
            EndpointUsage {
                total: 30,
                today: 10,
                today_epoch_day: 0,
            },
        );
        entries.insert(
            Endpoint::Me,
            EndpointUsage {
                total: 1,
                today: 0,
                today_epoch_day: 0,
            },
        );
        entries.insert(
            Endpoint::CreatePost,
            EndpointUsage {
                total: 2,
                today: 1,
                today_epoch_day: 0,
            },
        );

        let report = build_report(&entries, 0, 0.005, 1000);

        // footer/total は Posts だけ — Users/Owned/Write の巨大な数字に
        // 引きずられない｡
        assert_eq!(report.total.posts_total, 4);

        assert_eq!(report.by_kind["posts"].total, 4);
        assert_eq!(report.by_kind["users"].total, 30);
        assert_eq!(report.by_kind["owned"].total, 1);
        assert_eq!(report.by_kind["write"].total, 2);
    }
}
