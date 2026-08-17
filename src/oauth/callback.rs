//! The loopback listener that catches X's OAuth redirect, and the raw-request
//! parsing behind it.
//!
//! The parsing (`parse_request_line`, its query helpers, `interpret_query`)
//! is pure over `&str`/`&HashMap`, tested with canned HTTP request bytes —
//! no socket. The listener itself (`await_authorization_code`) is the one
//! piece here that touches a real `TcpListener`; it stays untested directly,
//! same as `x_api::client::XClient`'s network calls.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use gpui::BackgroundExecutor;

/// The fixed loopback port. X requires an exact redirect-URI match, so this
/// can't be an ephemeral port — see [`redirect_uri`]. Kept as the single
/// named constant the README and the Developer Portal registration both
/// have to agree with.
pub(crate) const LOOPBACK_PORT: u16 = 8733;

/// How long the loopback listener waits for the browser to complete the
/// consent flow before giving up.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(120);

/// How often the accept loop polls the non-blocking listener. Awaiting this
/// timer (rather than a blocking `std::thread::sleep`) is what lets dropping
/// the enclosing `Task` actually stop the loop mid-wait and close the
/// socket, per #7's design notes.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// How many header lines a single request is allowed before this gives up
/// on it as malformed or stalled, so one bad connection can't hang the loop
/// past its own read timeout.
const MAX_HEADER_LINES: usize = 100;

/// How long a single accepted connection is given to send its request and
/// receive the response before this treats it as a spurious connection
/// (favicon probe, prefetch) and moves on.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// The redirect URI registered with X's Developer Portal, derived from
/// [`LOOPBACK_PORT`] so the two can never drift apart in code.
pub(crate) fn redirect_uri() -> String {
    let _ = LOOPBACK_PORT;
    String::new()
}

/// One parsed HTTP request line: method, path, and decoded query
/// parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestLine {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
}

/// Parse an HTTP request line — the first line of a request, e.g.
/// `GET /callback?code=abc&state=xyz HTTP/1.1` — plus its query string.
/// Pure over `&str` so it's testable with canned request bytes.
pub(crate) fn parse_request_line(raw: &str) -> Option<RequestLine> {
    let _ = raw;
    None
}

/// The outcome of interpreting a `/callback` request's query parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Authorization {
    pub code: String,
    pub state: String,
}

/// Why a `/callback` request didn't yield an authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CallbackError {
    /// The user pressed "Cancel" on X's consent screen — RFC 6749 §4.1.2.1's
    /// `access_denied`, called out as its own outcome per #7's design notes.
    AccessDenied,
    /// X reported some other OAuth error (`invalid_scope`, `server_error`, ...).
    Provider(String),
    /// A `/callback` request with no `code` — malformed or spoofed.
    MissingCode,
    /// A `/callback` request with no `state` — malformed or spoofed.
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

/// Turn a `/callback` request's query parameters into an authorization, or
/// the specific reason it isn't one.
pub(crate) fn interpret_query(query: &HashMap<String, String>) -> Result<Authorization, CallbackError> {
    let _ = query;
    Err(CallbackError::MissingCode)
}

const SUCCESS_BODY: &str = "Signed in with X. You can close this tab.";

fn error_body(message: &str) -> String {
    format!("Sign-in failed: {message}. You can close this tab.")
}

/// Build a minimal HTTP response so the browser sees something readable
/// rather than a connection reset — a status line, a couple of headers, and
/// a plain-text body.
pub(crate) fn http_response(status_line: &str, body: &str) -> String {
    let _ = (status_line, body);
    String::new()
}

/// Block (asynchronously) until the browser redirects back with an
/// authorization code, or the deadline passes.
///
/// Filters every accepted connection on `/callback`: browsers routinely open
/// extra connections against a loopback listener (favicon, prefetch,
/// connection probing) and none of those are the redirect, so the first
/// connection accepted is never treated as authoritative.
pub(crate) async fn await_authorization_code(
    executor: &BackgroundExecutor,
    expected_state: &str,
) -> Result<String> {
    let listener = TcpListener::bind(("127.0.0.1", LOOPBACK_PORT)).with_context(|| {
        format!("could not bind the loopback listener on 127.0.0.1:{LOOPBACK_PORT}")
    })?;
    listener
        .set_nonblocking(true)
        .context("could not set the loopback listener to non-blocking")?;

    let deadline = executor.now() + CALLBACK_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                if let Some(authorization) = handle_connection(stream)? {
                    super::pkce::verify_state(expected_state, &authorization.state)?;
                    return Ok(authorization.code);
                }
                // Not `/callback` (or unparseable) — a spurious connection.
                // Keep listening rather than treating it as authoritative.
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

/// Read one HTTP request off `stream`, answer it, and return the parsed
/// authorization if the request was for `/callback`. Returns `Ok(None)` for
/// any other path, or for a connection that doesn't look like a real
/// browser request, so the caller keeps accepting.
fn handle_connection(mut stream: TcpStream) -> Result<Option<Authorization>> {
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

    // Drain the remaining headers so the browser's request is fully read
    // before this end writes and closes. A stalled or malicious peer is
    // bounded by `MAX_HEADER_LINES` and the read timeout above, not by
    // trusting the request to be well-formed.
    let mut line = String::new();
    for _ in 0..MAX_HEADER_LINES {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line == "\r\n" || line == "\n" => break,
            Ok(_) => continue,
            Err(_) => break,
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
        let _ = write_response(&mut stream, &http_response("HTTP/1.1 404 Not Found", "not found"));
        return Ok(None);
    }

    match interpret_query(&parsed.query) {
        Ok(authorization) => {
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
        assert_eq!(redirect_uri(), "http://127.0.0.1:8733/callback");
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
        assert_eq!(parsed.query.get("error"), Some(&"access_denied".to_string()));
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
            ("error_description".to_string(), "unsupported scope".to_string()),
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
        assert_eq!(interpret_query(&query).unwrap_err(), CallbackError::MissingCode);
    }

    #[test]
    fn interpret_query_rejects_a_missing_state() {
        let query = HashMap::from([("code".to_string(), "abc123".to_string())]);
        assert_eq!(interpret_query(&query).unwrap_err(), CallbackError::MissingState);
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
