//! X の OAuth リダイレクトを受けるループバックリスナと､その背後にある生の
//! リクエストのパース｡
//!
//! パース (`parse_request_line`, そのクエリ補助関数, `interpret_query`) は
//! `&str`/`&HashMap` に対して純粋で､既製の HTTP リクエストのバイト列で
//! テストする — ソケットは使わない｡リスナ自体 (`await_authorization_code`)
//! はここで唯一実際の `TcpListener` に触れる部分で､`x_api::client::XClient`
//! のネットワーク呼び出しと同じく直接のテストはしないままだ｡

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use gpui::BackgroundExecutor;

use crate::profile::Profile;

/// ループバックリスナが､諦めるまでにブラウザの同意フロー完了を待つ時間｡
const CALLBACK_TIMEOUT: Duration = Duration::from_mins(2);

/// accept ループがノンブロッキングのリスナをポーリングする間隔｡ブロックする
/// `std::thread::sleep` ではなくこのタイマーを await するからこそ､外側の
/// `Task` を drop すれば待ちの途中でもループが実際に止まりソケットが閉じる｡
/// #7 の設計メモに従う｡
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// 1 つのリクエストに許すヘッダー行の数｡これを超えたら不正か停滞として
/// 諦める｡1 本の悪い接続が､自身の読み取りタイムアウトを超えてループを
/// 止め続けられないようにするためだ｡
const MAX_HEADER_LINES: usize = 100;

/// accept した 1 本の接続に､リクエストを送りレスポンスを受け取るまでに
/// 与える時間｡これを超えたら偽の接続 (favicon の探り､プリフェッチ) と
/// みなして先へ進む｡
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// `profile` 向けに X の Developer Portal へ登録した redirect URI —
/// profile ごとに 1 つ登録し (#169)､どれもこの文字列をそのまま持つ｡
/// [`Profile::loopback_port`] から導くので､X へ送る URI と
/// [`await_authorization_code`] が bind する port がコード上でずれようがない｡
pub(crate) fn redirect_uri(profile: Profile) -> String {
    format!("http://127.0.0.1:{}/callback", profile.loopback_port())
}

/// パース済みの HTTP リクエスト行 1 つ: method､path､デコード済みのクエリ
/// パラメータ｡
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestLine {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
}

/// HTTP のリクエスト行 — リクエストの最初の行､たとえば
/// `GET /callback?code=abc&state=xyz HTTP/1.1` — とそのクエリ文字列を
/// パースする｡`&str` に対して純粋なので既製のバイト列でテストできる｡
pub(crate) fn parse_request_line(raw: &str) -> Option<RequestLine> {
    let first_line = raw.lines().next()?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?;
    parts.next()?; // HTTP のバージョン — 整形式のリクエスト行には必須｡

    let (path, query_str) = target.split_once('?').unwrap_or((target, ""));
    Some(RequestLine {
        method,
        path: path.to_string(),
        query: parse_query(query_str),
    })
}

fn parse_query(raw: &str) -> HashMap<String, String> {
    raw.split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((percent_decode(key), percent_decode(value)))
        })
        .collect()
}

fn hex_digit(byte: u8) -> Option<u8> {
    // 各腕の減算はそれを選んだパターンで押さえられているので､`wrapping_sub`
    // と `wrapping_add` はここで実際には wrap しない — が､範囲が編集された
    // ときに気づく手立てを debug 限定のオーバーフロー検査に頼るより､そう
    // 明記するほうがよい (#47)｡
    match byte {
        b'0'..=b'9' => Some(byte.wrapping_sub(b'0')),
        b'a'..=b'f' => Some(byte.wrapping_sub(b'a').wrapping_add(10)),
        b'A'..=b'F' => Some(byte.wrapping_sub(b'A').wrapping_add(10)),
        _ => None,
    }
}

/// `%XX` のエスケープをデコードする｡X から来るクエリ値は base64url か
/// 不透明な code なので､このパーサが戻す必要のあるエスケープはこれだけだ —
/// `+` を空白として扱う処理は無い｡それは `application/x-www-form-urlencoded`
/// の慣習であってクエリ文字列のものではない (RFC 3986)｡`&str` をスライス
/// せずバイト単位で動くので､多バイト文字の近くに紛れた `%` が文字境界外の
/// スライスで panic することはない｡
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    // `bytes[i]` ではなく `get` で添字を引く (#47,
    // `clippy::indexing_slicing`): ここの境界はすでに正しいが､`Option` と
    // して表しておけば､将来ループを編集しても､不正なリダイレクト — これは
    // リモート入力だ — で panic に変わることはない｡
    while let Some(&byte) = bytes.get(i) {
        if byte == b'%'
            && let (Some(Some(hi)), Some(Some(lo))) = (
                bytes.get(i.saturating_add(1)).copied().map(hex_digit),
                bytes.get(i.saturating_add(2)).copied().map(hex_digit),
            )
        {
            // `hex_digit` の作りから両ニブルとも 0..=15 なので `u8` に収まる
            // — `wrapping_*` はそれを明示する｡変更を捕まえるのが debug 限定
            // の検査だけ､という状態にしない｡
            out.push(hi.wrapping_mul(16).wrapping_add(lo));
            i = i.saturating_add(3);
            continue;
        }
        out.push(byte);
        i = i.saturating_add(1);
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `/callback` リクエストのクエリパラメータを解釈した結果｡
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Authorization {
    pub code: String,
    pub state: String,
}

/// `/callback` リクエストが authorization にならなかった理由｡
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CallbackError {
    /// ユーザーが X の同意画面で "Cancel" を押した — RFC 6749 §4.1.2.1 の
    /// `access_denied`｡#7 の設計メモに従い独立した結果として扱う｡
    AccessDenied,
    /// X が他の OAuth エラーを返した (`invalid_scope`, `server_error`, ...)｡
    Provider(String),
    /// `code` の無い `/callback` リクエスト — 不正か偽装だ｡
    MissingCode,
    /// `state` の無い `/callback` リクエスト — 不正か偽装だ｡
    MissingState,
}

impl std::fmt::Display for CallbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AccessDenied => write!(f, "sign-in was cancelled"),
            Self::Provider(message) => write!(f, "X rejected the sign-in request: {message}"),
            Self::MissingCode => write!(f, "the callback did not include an authorization code"),
            Self::MissingState => write!(f, "the callback did not include a state parameter"),
        }
    }
}

impl std::error::Error for CallbackError {}

/// `/callback` リクエストのクエリパラメータを authorization に変える｡
/// あるいはそうならない具体的な理由に｡
pub(crate) fn interpret_query(
    query: &HashMap<String, String>,
) -> Result<Authorization, CallbackError> {
    if let Some(error) = query.get("error") {
        return Err(if error == "access_denied" {
            CallbackError::AccessDenied
        } else {
            CallbackError::Provider(match query.get("error_description") {
                Some(description) => format!("{error}: {description}"),
                None => error.clone(),
            })
        });
    }

    let code = query
        .get("code")
        .cloned()
        .ok_or(CallbackError::MissingCode)?;
    let state = query
        .get("state")
        .cloned()
        .ok_or(CallbackError::MissingState)?;
    Ok(Authorization { code, state })
}

const SUCCESS_BODY: &str = "Signed in with X. You can close this tab.";

fn error_body(message: &str) -> String {
    format!("Sign-in failed: {message}. You can close this tab.")
}

/// 最小の HTTP レスポンスを組み立てる｡ブラウザが接続リセットではなく
/// 読めるものを見るようにするためだ — ステータス行､いくつかのヘッダー､
/// そしてプレーンテキストの body｡
pub(crate) fn http_response(status_line: &str, body: &str) -> String {
    format!(
        "{status_line}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        len = body.len(),
    )
}

/// ブラウザが authorization code を持って戻ってくるか期限が過ぎるまで
/// (非同期に) ブロックする｡
///
/// accept した接続はすべて `/callback` で絞る: ブラウザはループバックの
/// リスナに対して日常的に余分な接続を開く (favicon､プリフェッチ､接続の
/// 探り) し､そのどれもリダイレクトではないので､最初に accept した接続を
/// 権威あるものとして扱うことは決してない｡
///
/// `profile` 自身の port を bind する (#169)｡これにより development の
/// サインインと本番のサインインが同時に飛んでいても､どちらのリスナも
/// 相手のリダイレクトを捕まえない｡
pub(crate) async fn await_authorization_code(
    executor: &BackgroundExecutor,
    expected_state: &str,
    profile: Profile,
) -> Result<String> {
    let port = profile.loopback_port();
    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("could not bind the loopback listener on 127.0.0.1:{port}"))?;
    listener
        .set_nonblocking(true)
        .context("could not set the loopback listener to non-blocking")?;

    // `checked_add` で､失敗したら `now` へ落とす (#47): `Instant` を
    // オーバーフローさせるほど先へ進んだ時計では､サインインの途中で panic
    // するのではなく待ちが即座に期限切れになるべきだ｡
    let deadline = executor
        .now()
        .checked_add(CALLBACK_TIMEOUT)
        .unwrap_or_else(|| executor.now());
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                if let Some(authorization) = handle_connection(stream, expected_state)? {
                    return Ok(authorization.code);
                }
                // `/callback` でない (かパースできない) — 偽の接続だ｡
                // 権威あるものとして扱わず listen を続ける｡
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if executor.now() >= deadline {
                    bail!(
                        "timed out after {}s waiting for the browser to complete sign-in",
                        CALLBACK_TIMEOUT.as_secs()
                    );
                }
                executor.timer(POLL_INTERVAL).await;
            }
            Err(error) => return Err(error).context("loopback listener accept failed"),
        }
    }
}

/// `stream` から HTTP リクエストを 1 つ読んで応答し､それが `/callback` 宛
/// だったならパースした authorization を返す｡他のパス､あるいは本物の
/// ブラウザのリクエストに見えない接続には `Ok(None)` を返し､呼び出し側は
/// accept を続ける｡
///
/// CSRF の `state` 検査を呼び出し側ではなくここで行うのは､ブラウザが描く
/// ページを結果に合わせるためだ: `state` が食い違ったとき､フローが中断する
/// 一方でユーザーが "Signed in with X" を読んでいる状態にしてはならない｡
fn handle_connection(mut stream: TcpStream, expected_state: &str) -> Result<Option<Authorization>> {
    if stream.set_nonblocking(false).is_err() {
        return Ok(None);
    }
    let _ = stream.set_read_timeout(Some(CONNECTION_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CONNECTION_TIMEOUT));

    let Ok(cloned) = stream.try_clone() else {
        return Ok(None);
    };
    let mut reader = BufReader::new(cloned);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() || request_line.is_empty() {
        return Ok(None);
    }

    // 残りのヘッダーを読み捨てて､こちら側が書いて閉じる前にブラウザの
    // リクエストを最後まで読み切る｡停滞した相手や悪意ある相手は
    // `MAX_HEADER_LINES` と上の読み取りタイムアウトで押さえる｡リクエストが
    // 整形式だと信じることでは押さえない｡
    let mut line = String::new();
    for _ in 0..MAX_HEADER_LINES {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) if line == "\r\n" || line == "\n" => break,
            Ok(_) => {}
        }
    }

    let Some(parsed) = parse_request_line(&request_line) else {
        let _ = write_response(
            &mut stream,
            &http_response("HTTP/1.1 400 Bad Request", "malformed request"),
        );
        return Ok(None);
    };

    if parsed.path != "/callback" {
        let _ = write_response(
            &mut stream,
            &http_response("HTTP/1.1 404 Not Found", "not found"),
        );
        return Ok(None);
    }

    match interpret_query(&parsed.query) {
        Ok(authorization) => {
            if let Err(error) = super::pkce::verify_state(expected_state, &authorization.state) {
                let body = error_body(&error.to_string());
                let _ = write_response(
                    &mut stream,
                    &http_response("HTTP/1.1 400 Bad Request", &body),
                );
                return Err(error);
            }
            let _ = write_response(&mut stream, &http_response("HTTP/1.1 200 OK", SUCCESS_BODY));
            Ok(Some(authorization))
        }
        Err(error) => {
            let body = error_body(&error.to_string());
            let _ = write_response(&mut stream, &http_response("HTTP/1.1 200 OK", &body));
            Err(error.into())
        }
    }
}

fn write_response(stream: &mut TcpStream, response: &str) -> Result<()> {
    stream
        .write_all(response.as_bytes())
        .context("could not write the callback response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_uri_uses_the_loopback_port_and_the_callback_path() {
        assert_eq!(
            redirect_uri(Profile::Release),
            "http://127.0.0.1:8733/callback"
        );
    }

    #[test]
    fn the_dev_profile_redirects_to_its_own_port() {
        // development 用の X アプリにこのまま登録してある (#169) — 1 文字
        // でも食い違えば X は認可リクエストをきっぱり拒否する｡
        assert_eq!(redirect_uri(Profile::Dev), "http://127.0.0.1:8734/callback");
    }

    #[test]
    fn parses_a_successful_callback_request_line() {
        let raw = "GET /callback?code=abc123&state=xyz789 HTTP/1.1\r\nHost: 127.0.0.1:8733\r\n\r\n";
        let parsed = parse_request_line(raw).unwrap();

        assert_eq!(parsed.method, "GET");
        assert_eq!(parsed.path, "/callback");
        assert_eq!(parsed.query.get("code"), Some(&"abc123".to_string()));
        assert_eq!(parsed.query.get("state"), Some(&"xyz789".to_string()));
    }

    #[test]
    fn parses_a_non_callback_path_so_it_can_be_answered_404() {
        let raw = "GET /favicon.ico HTTP/1.1\r\n\r\n";
        let parsed = parse_request_line(raw).unwrap();
        assert_eq!(parsed.path, "/favicon.ico");
        assert!(parsed.query.is_empty());
    }

    #[test]
    fn parses_an_access_denied_error_callback() {
        let raw = "GET /callback?error=access_denied&state=xyz789 HTTP/1.1\r\n\r\n";
        let parsed = parse_request_line(raw).unwrap();
        assert_eq!(
            parsed.query.get("error"),
            Some(&"access_denied".to_string())
        );
    }

    #[test]
    fn percent_decodes_query_values() {
        let raw = "GET /callback?state=a%3Db%2Fc HTTP/1.1\r\n\r\n";
        let parsed = parse_request_line(raw).unwrap();
        assert_eq!(parsed.query.get("state"), Some(&"a=b/c".to_string()));
    }

    #[test]
    fn rejects_a_blank_request_line() {
        assert!(parse_request_line("").is_none());
    }

    #[test]
    fn rejects_a_request_line_missing_a_target() {
        assert!(parse_request_line("GET\r\n").is_none());
    }

    #[test]
    fn interpret_query_returns_the_authorization_on_success() {
        let query = HashMap::from([
            ("code".to_string(), "abc123".to_string()),
            ("state".to_string(), "xyz789".to_string()),
        ]);
        let authorization = interpret_query(&query).unwrap();
        assert_eq!(authorization.code, "abc123");
        assert_eq!(authorization.state, "xyz789");
    }

    #[test]
    fn interpret_query_reports_access_denied_distinctly() {
        let query = HashMap::from([("error".to_string(), "access_denied".to_string())]);
        let error = interpret_query(&query).unwrap_err();
        assert_eq!(error, CallbackError::AccessDenied);
        assert_eq!(error.to_string(), "sign-in was cancelled");
    }

    #[test]
    fn interpret_query_reports_other_provider_errors_with_their_description() {
        let query = HashMap::from([
            ("error".to_string(), "invalid_scope".to_string()),
            (
                "error_description".to_string(),
                "unsupported scope".to_string(),
            ),
        ]);
        let error = interpret_query(&query).unwrap_err();
        match error {
            CallbackError::Provider(message) => {
                assert!(message.contains("invalid_scope"), "{message}");
                assert!(message.contains("unsupported scope"), "{message}");
            }
            other => panic!("expected Provider, got {other:?}"),
        }
    }

    #[test]
    fn interpret_query_rejects_a_missing_code() {
        let query = HashMap::from([("state".to_string(), "xyz789".to_string())]);
        assert_eq!(
            interpret_query(&query).unwrap_err(),
            CallbackError::MissingCode
        );
    }

    #[test]
    fn interpret_query_rejects_a_missing_state() {
        let query = HashMap::from([("code".to_string(), "abc123".to_string())]);
        assert_eq!(
            interpret_query(&query).unwrap_err(),
            CallbackError::MissingState
        );
    }

    #[test]
    fn http_response_includes_the_status_line_and_body() {
        let response = http_response("HTTP/1.1 200 OK", "hello");
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
        assert!(response.ends_with("hello"), "{response}");
        assert!(response.contains("Content-Length: 5"), "{response}");
    }

    #[test]
    fn http_response_404_names_the_status() {
        let response = http_response("HTTP/1.1 404 Not Found", "not found");
        assert!(response.starts_with("HTTP/1.1 404 Not Found"), "{response}");
    }
}
