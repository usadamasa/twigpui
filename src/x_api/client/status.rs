//! レスポンスのステータスをエラーへ変える側 (#241): [`Denied`] (401/403)､
//! 2 種類の 429､retry してよいかの判定､429 の記録｡ネットワークには
//! 触れないので､`client` のテストの半分はここにある｡

use anyhow::{Result, bail};

use crate::rate_limit::{self, Endpoint, RateLimitState};
use crate::x_api::model::ApiProblem;

/// そのステータスが retry に値するか: サーバー側 (5xx) の失敗だけだ｡
/// ネットワークエラーも retry するが､それがステータスコードとしてここへ届く
/// ことはない — `XClient::get` の `Err` の腕へ短絡する｡429 では決して true に
/// ならない (`500..600` に入らない): どちらの種類の 429 も retry の候補では
/// ないというのが #10 の眼目だ｡
pub(super) fn is_retryable_status(status: u16) -> bool {
    (500..600).contains(&status)
}

/// レスポンスボディに API 自身のエラーテキストがあれば取り出す｡
pub(super) fn describe_problem(body: &str) -> Option<String> {
    serde_json::from_str::<ApiProblem>(body)
        .ok()
        .and_then(|problem| problem.message())
}

/// 429 の記録を残す (#199): どのエンドポイントか､ウィンドウのヘッダが何と
/// 言っていたか､そしてボディの先頭だ｡これ以前は､不透明な拒否 (300 のうち
/// `remaining` 299 なのに拒まれる) が動かなくなった mtime しか残さず､それが
/// どの上限から来たのかは今も特定できていない — 後からログを読むときに頼れる
/// のがこの行だ｡エラーボディはリクエストパラメータをそのまま返すので
/// (`x-api-endpoints`) ボディは上限を切ってあり､他の行と同じく `log::redact`
/// を通る｡
pub(super) fn log_429(
    endpoint: Endpoint,
    state: RateLimitState,
    refusal: rate_limit::Refusal,
    body: &str,
) {
    let snippet: String = body.chars().take(400).collect();
    let kind = match refusal {
        rate_limit::Refusal::Window { .. } => "window exhausted",
        rate_limit::Refusal::Opaque => "opaque: headroom left in the window",
    };
    crate::log::warn(&format!(
        "{} refused with 429 ({kind}); x-rate-limit limit={} remaining={} reset={}; body: {snippet}",
        endpoint.key(),
        state.limit.map_or("-".to_string(), |n| n.to_string()),
        state.remaining.map_or("-".to_string(), |n| n.to_string()),
        state.reset_at.map_or("-".to_string(), |n| n.to_string()),
    ));
}

/// X がリクエストそのものを拒んだ (#239)｡retry では直らない 2 つの場合を
/// 運ぶ｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Denial {
    /// 401 — token が失効しているか無効｡`x-api-endpoints` の実測どおり､
    /// このレスポンスには `x-rate-limit-*` すら付かない｡
    Rejected,
    /// 403 — この app にそのエンドポイントの権限が無いか､アカウントの
    /// monthly spend cap に当たっている｡後者は残高切れ (429 +
    /// [`rate_limit::UsageCapExceeded`]) とは別物で､平文の detail でしか
    /// 名乗らない｡
    Forbidden,
}

/// 401/403 を型で運ぶ (#239)｡呼び出し側がメッセージを grep せずに
/// 「これは待っても直らない」と判断できるようにするためで､429 の 2 種類を
/// 型に分けた #10 と同じ理屈だ｡
///
/// `endpoint` を持つのは #239 のログがまさにそれを欠いていたからだ:
/// 拒否が 180 秒ごとに 100 行積み上がっても､どの呼び出しが拒まれたのかは
/// どこにも書かれていなかった｡
#[derive(Debug)]
pub(crate) struct Denied {
    pub endpoint: Endpoint,
    pub denial: Denial,
    pub detail: String,
}

impl std::fmt::Display for Denied {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self.denial {
            Denial::Rejected => "401 Unauthorized — the bearer token was rejected",
            Denial::Forbidden => "403 Forbidden — this app cannot access the endpoint",
        };
        write!(
            formatter,
            "{}: {reason}: {}",
            self.endpoint.key(),
            self.detail
        )
    }
}

impl std::error::Error for Denied {}

/// レスポンスのステータスを検証し､2xx でないものをエラーへ変換する｡
///
/// `refusal` は普通の 429 がどの limit から来たかで､生の
/// `x-rate-limit-reset` ヘッダを読むのではなく
/// [`rate_limit::Refusal::classify`] がすでにウィンドウの状態と突き合わせて
/// いる｡だからこれらのヘッダが説明しない limit からの 429 は､誤ったウィンドウ
/// の時計ではなく保守的な backoff を持ち — しかもそう明言するので､`sync` は
/// 毎回さらに退がれる｡普通の 429 以外のステータスでは無視する｡
///
/// どのステータスも `endpoint` を名乗る (#239)｡#10 の時点でそう名乗って
/// いたのは 429 のログ ([`log_429`]) だけで､401 と 403 は名前の無いまま
/// ログへ流れていた｡[`log_429`] と同じ `{key}: ` の前置に揃えてある｡
///
/// 401/403 は [`Denied`] へ (#239)､2 種類の 429 は
/// [`rate_limit::classify_429`] を通じて [`rate_limit::UsageCapExceeded`] と
/// [`rate_limit::RateLimited`] へ分かれる｡404 とその他のステータスだけが
/// 平文の `anyhow` エラーのままだ — 呼び出し側がこれらを型で見分ける必要は
/// まだ無い｡
pub(super) fn check_status(
    endpoint: Endpoint,
    status: u16,
    body: &str,
    refusal: rate_limit::Refusal,
    now: i64,
) -> Result<()> {
    if (200..300).contains(&status) {
        return Ok(());
    }

    let detail = describe_problem(body).unwrap_or_else(|| {
        let snippet: String = body.chars().take(400).collect();
        if snippet.is_empty() {
            "(empty response body)".to_string()
        } else {
            snippet
        }
    });

    let key = endpoint.key();
    match status {
        401 => Err(Denied {
            endpoint,
            denial: Denial::Rejected,
            detail,
        }
        .into()),
        403 => Err(Denied {
            endpoint,
            denial: Denial::Forbidden,
            detail,
        }
        .into()),
        404 => bail!("{key}: 404 Not Found — {detail}"),
        429 => match rate_limit::classify_429(body) {
            rate_limit::RateLimitKind::UsageCapExceeded => {
                Err(rate_limit::UsageCapExceeded { detail }.into())
            }
            rate_limit::RateLimitKind::RateLimited => Err(refusal.into_error(now).into()),
        },
        _ => bail!("{key}: HTTP {status} — {detail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_success_statuses() {
        assert!(check_status(Endpoint::Me, 200, "{}", rate_limit::Refusal::Opaque, 0).is_ok());
        assert!(check_status(Endpoint::Me, 299, "", rate_limit::Refusal::Opaque, 0).is_ok());
    }

    #[test]
    fn explains_an_exhausted_credit_cap() {
        let body =
            r#"{"title":"UsageCapExceeded","detail":"Usage cap exceeded: Monthly product cap"}"#;
        let error = check_status(
            Endpoint::ListTimeline,
            429,
            body,
            rate_limit::Refusal::Opaque,
            0,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("429"), "{error}");
        assert!(error.contains("Usage cap exceeded"), "{error}");
    }

    #[test]
    fn a_usage_cap_429_downcasts_to_the_typed_error() {
        // #10: この区別は呼び出し側での文字列比較ではなく型でなければならない
        // — だからこそ `ui.rs` (や他のどの呼び出し側) もメッセージを grep せず
        // match できる｡
        let body =
            r#"{"title":"UsageCapExceeded","detail":"Usage cap exceeded: Monthly product cap"}"#;
        let error = check_status(
            Endpoint::ListTimeline,
            429,
            body,
            rate_limit::Refusal::Opaque,
            1_700_000_000,
        )
        .unwrap_err();
        let typed = error
            .downcast_ref::<rate_limit::UsageCapExceeded>()
            .unwrap();
        assert!(typed.detail.contains("Usage cap exceeded"), "{typed:?}");
    }

    #[test]
    fn an_ordinary_rate_limit_429_downcasts_to_the_typed_error_carrying_the_retry_time() {
        // `check_status` は渡された refusal をそのまま運ぶ — ウィンドウの状態
        // との突き合わせは `rate_limit::Refusal::classify` にあり､テストも
        // そこにある｡
        let body = r#"{"title":"TooManyRequests","detail":"Rate limit exceeded"}"#;
        let refusal = rate_limit::Refusal::Window {
            reset_at: 1_700_000_000,
        };
        let error =
            check_status(Endpoint::ListTimeline, 429, body, refusal, 1_699_999_000).unwrap_err();
        let typed = error.downcast_ref::<rate_limit::RateLimited>().unwrap();
        assert_eq!(typed.reset_at, Some(1_700_000_000));
        assert!(!typed.opaque);
    }

    #[test]
    fn an_opaque_429_downcasts_to_a_typed_error_that_says_so() {
        // #197: sync はこのフラグを見て backoff を強めるので､client は 2 種類
        // を retry 時刻へ潰さず､そのまま通さなければならない｡
        let body = r#"{"title":"Too Many Requests","detail":"Too Many Requests"}"#;
        let error = check_status(
            Endpoint::ListTimeline,
            429,
            body,
            rate_limit::Refusal::Opaque,
            1_000,
        )
        .unwrap_err();
        let typed = error.downcast_ref::<rate_limit::RateLimited>().unwrap();
        assert!(typed.opaque);
        assert_eq!(
            typed.reset_at,
            Some(1_000 + rate_limit::OPAQUE_LIMIT_BACKOFF_SECONDS)
        );
    }

    #[test]
    fn explains_a_rejected_token() {
        let error = check_status(
            Endpoint::ListTimeline,
            401,
            r#"{"title":"Unauthorized"}"#,
            rate_limit::Refusal::Opaque,
            0,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("bearer token was rejected"), "{error}");
    }

    // --- #239: 拒否はどの呼び出しが拒まれたのかを言う ---
    //
    // issue のログは 180 秒ごとの 403 を 30 行と 401 を 100 行以上積み上げた
    // が､どのエンドポイントのものかはどこにも書かれておらず､読み手には
    // 特定できなかった｡下の 3 本はその穴を塞ぐ｡

    #[test]
    fn a_rejected_token_names_the_endpoint_it_was_rejected_for() {
        let error = check_status(
            Endpoint::ListTimeline,
            401,
            r#"{"title":"Unauthorized","detail":"Unauthorized"}"#,
            rate_limit::Refusal::Opaque,
            0,
        )
        .unwrap_err()
        .to_string();
        assert!(error.starts_with("list_timeline: "), "{error}");
    }

    #[test]
    fn a_spend_cap_403_downcasts_to_the_typed_error_carrying_the_endpoint() {
        // #239 のログの前半｡残高切れ (429 + `UsageCapExceeded`) ではなく
        // アカウントの monthly spend cap で､平文の detail でしか名乗らない｡
        let body = r#"{"title":"Forbidden","detail":"Forbidden: Your monthly spend cap has been reached."}"#;
        let error = check_status(
            Endpoint::ListTimeline,
            403,
            body,
            rate_limit::Refusal::Opaque,
            0,
        )
        .unwrap_err();
        let typed = error.downcast_ref::<Denied>().unwrap();
        assert_eq!(typed.endpoint, Endpoint::ListTimeline);
        assert_eq!(typed.denial, Denial::Forbidden);
        assert!(typed.detail.contains("monthly spend cap"), "{typed:?}");
        assert!(
            typed.to_string().contains("list_timeline: 403 Forbidden"),
            "{typed}"
        );
    }

    #[test]
    fn other_statuses_name_the_endpoint_too() {
        let error = check_status(Endpoint::Me, 404, "", rate_limit::Refusal::Opaque, 0)
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("me: 404 Not Found"), "{error}");
    }

    #[test]
    fn a_400_downcasts_to_the_typed_error_naming_the_endpoint() {
        // sync はこれを「この entry は駄目」と読んで次へ進む (#254)｡
        // 文字列を grep せずに済むよう 401/403 と同じく型で運ぶ｡
        let error = check_status(
            Endpoint::AddListMember,
            400,
            r#"{"title":"Invalid Request","detail":"One or more parameters to your request was invalid."}"#,
            rate_limit::Refusal::Opaque,
            0,
        )
        .unwrap_err();
        let rejected = error
            .downcast_ref::<InvalidRequest>()
            .expect("a 400 must carry the typed error");
        assert_eq!(rejected.endpoint, Endpoint::AddListMember);
        assert!(
            rejected.detail.contains("One or more parameters"),
            "{rejected:?}"
        );
        let text = error.to_string();
        assert!(text.starts_with("add_list_member: 400 Bad Request"), "{text}");
    }

    #[test]
    fn falls_back_to_the_raw_body_when_it_is_not_json() {
        let error = check_status(
            Endpoint::Me,
            503,
            "upstream unavailable",
            rate_limit::Refusal::Opaque,
            0,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("upstream unavailable"), "{error}");
    }

    #[test]
    fn reports_an_empty_body_rather_than_nothing() {
        let error = check_status(Endpoint::Me, 500, "", rate_limit::Refusal::Opaque, 0)
            .unwrap_err()
            .to_string();
        assert!(error.contains("empty response body"), "{error}");
    }

    #[test]
    fn treats_5xx_as_retryable_and_4xx_as_not() {
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(599));
        assert!(!is_retryable_status(429));
        assert!(!is_retryable_status(404));
        assert!(!is_retryable_status(200));
    }
}
