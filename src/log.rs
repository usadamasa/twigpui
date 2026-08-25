//! ファイルへの診断ログ (#49)｡
//!
//! ## なぜこれがあるか
//!
//! このアプリが言えることはすべて stderr へ行っていたが､Finder から
//! 起動した `.app` には誰にも読める stderr が無い (#40, #45)｡起動時の
//! アラートダイアログが覆うのはちょうど 1 つの場合 — 「起動しなかった」
//! — だけで､問題なく起動してからおかしくなるセッションには何も残らない｡
//!
//! ## なぜ `tracing` や `log` でないか
//!
//! #46 はビルド時間についての open な issue で､ここで要るのはレベルと
//! タイムスタンプの付いた 1 行と､サイズの上限だ｡そのために `tracing` と
//! subscriber と appender をビルドのたびにコンパイルするのは大きな木すぎる｡
//! この crate 自身の JSON 永続化とレートリミット追跡をフレームワークでは
//! なく手書きにしたのも､すでに同じ理屈による｡
//!
//! ## 最も重要な規則
//!
//! **トークンがファイルに届いてはならない｡** このアプリは OAuth の access
//! token と refresh token､場合によっては app-only の bearer token を持つ｡
//! トークンファイル自体は `0600` だが (#7)､同じ値が誰でも読めるログに
//! 落ちるならそれは何の役にも立たない｡すべてのメッセージは [`redact`] を
//! 通り､ログファイルも `0600` で作られる｡そして実際の保証は､次の
//! 呼び出し箇所を書く人の注意深さではなく､以下のテストだ｡

use std::fmt::Write as _;
use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::paths::Paths;

/// ファイルがこれを超えたら rotate する｡`~/.local/state` は macOS に
/// 掃除されないので､上限の無いログはゆっくりした漏れになる — #9 の
/// キャッシュ上限の裏にあるのと同じ理屈だ｡
const MAX_BYTES: u64 = 1024 * 1024;

/// どれだけの詳細さがログに届くか｡
///
/// 順序付きなので､設定したレベルはそれ以上のものをすべて通す — [`write`] の
/// `level > sink.level` の判定を見よ｡この順序はそのためにある｡`Off` は
/// 意図的に無い: 最も静かで有用な設定は `error` であり､何も記録しないログは
/// #49 が終わらせるために存在する状態と見分けが付かないからだ｡
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) enum Level {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
}

impl Level {
    /// 設定されたレベルを大文字小文字を区別せずパースする｡認識できない
    /// ものは `None` — 呼び出し元は起動を失敗させず既定値へ fallback する｡
    /// `config.rs` が未知のテーマを扱うのと同じだ｡
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "error" => Some(Self::Error),
            "warn" | "warning" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
        }
    }
}

/// writer が必要とするものすべて｡起動時に [`init`] が一度だけ設定する｡
struct Sink {
    path: PathBuf,
    level: Level,
    /// stderr が端末かどうか｡端末ならメッセージは両方へ行く — issue の
    /// 「`cargo run` の体験を壊すな」という要求だ — 端末でなければ
    /// (Finder からの `.app`)､ファイルが唯一の記録になる｡
    echo_to_stderr: bool,
    /// 書き込みを直列化する｡プロセスは 1 つ､gpui の background task は複数｡
    file: Mutex<()>,
}

static SINK: OnceLock<Sink> = OnceLock::new();

/// ログを `paths` のログファイルへ `level` で向ける｡起動時に一度呼ぶ｡
///
/// ディレクトリの作成に失敗しても致命的ではないし報告もしない: *ログ* を
/// 開けなかったせいで起動を拒むアプリは優先順位が逆だ｡ログは単に切れた
/// ままになり､stderr が — あるなら — 引き続きすべてを見せる｡
pub(crate) fn init(paths: &Paths, level: Level) {
    let dir = paths.log_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = SINK.set(Sink {
        path: paths.log_file(),
        level,
        echo_to_stderr: std::io::stderr().is_terminal(),
        file: Mutex::new(()),
    });
}

/// `level` で 1 行書く｡
///
/// メッセージも､その前にファイルも防御的に扱う: 書けないログがアプリを
/// 道連れにしてはならないので､ここでの失敗はすべて握り潰す｡任意でない
/// 唯一のものが [`redact`] だ｡
pub(crate) fn write(level: Level, message: &str) {
    let safe = redact(message);

    let Some(sink) = SINK.get() else {
        // `init` の前か､それが失敗した後: stderr しか無い｡
        eprintln!("{} {safe}", level.label());
        return;
    };

    if sink.echo_to_stderr {
        eprintln!("{} {safe}", level.label());
    }
    if level > sink.level {
        return;
    }

    let line = format!("{} {} {safe}\n", format_utc(now()), level.label());
    let Ok(_guard) = sink.file.lock() else {
        return;
    };
    rotate_if_needed(&sink.path);
    let _ = append(&sink.path, &line);
}

/// 1 行追記する｡ファイルが無ければ `0600` で作る｡
///
/// `0600` なのはトークンファイルがそうだからだ (#7): 第一の防御線が
/// redaction で､第二がモードであり､認証情報の漏洩に対する安価な防御は
/// 1 つより 2 つのほうが値打ちがある｡
fn append(path: &Path, line: &str) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(line.as_bytes())
}

/// ログが [`MAX_BYTES`] を超えたら脇へ寄せ､前の世代をちょうど 1 つ残す｡
///
/// 複数ではなく 1 世代なのは､目的がディスクを抑えることであり､
/// 「読みたいものが今のファイルから流れ出てしまった」は 2 つめのファイルで
/// すでに賄えるからだ｡失敗はすべて無視する — rotate を失うほうがアプリを
/// 失うよりましだ｡
fn rotate_if_needed(path: &Path) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if !should_rotate(metadata.len(), MAX_BYTES) {
        return;
    }
    let mut previous = path.as_os_str().to_os_string();
    previous.push(".1");
    let _ = std::fs::rename(path, previous);
}

/// `len` バイトのログが `cap` を超えたかどうか｡ディスクに 1 メガバイト
/// 書かずに閾値をテストできるよう切り出してある｡
fn should_rotate(len: u64, cap: u64) -> bool {
    len >= cap
}

/// 認証情報らしきものをすべて取り除く｡
///
/// 意図的に大雑把だ｡次の順で書き換える:
///
/// - `Bearer <token>` — `Authorization` ヘッダが取るそのままの形｡
/// - `access_token` / `refresh_token` / `client_secret` / `code` /
///   `token` のいずれかのキーに `=` か `":"` が続くもの｡token
///   エンドポイントの JSON レスポンスや redirect URL のクエリ文字列に
///   現れる形だ｡
///
/// ここでは大雑把なほうを取るのが正しい: 取りこぼす redactor は､
/// メッセージを使いものにならないほど消しすぎる redactor より悪い｡失敗が
/// 静かで永続的だからだ — 誰かが気づく頃には認証情報はもうディスクの上に
/// ある｡
pub(crate) fn redact(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;

    while !rest.is_empty() {
        let Some((keyword, at)) = next_secret(rest) else {
            out.push_str(rest);
            break;
        };
        let (before, from) = rest.split_at(at);
        out.push_str(before);
        out.push_str(keyword);
        // ここから次の区切りまでがすべて値だ｡
        let after_keyword = from.get(keyword.len()..).unwrap_or_default();
        let value_end = after_keyword
            .find(|c: char| c.is_whitespace() || c == '&' || c == '"' || c == ',' || c == '}')
            .unwrap_or(after_keyword.len());
        out.push_str("[redacted]");
        rest = after_keyword.get(value_end..).unwrap_or_default();
    }

    out
}

/// `haystack` の中で次に認証情報を導くトークン: 何を残すかと､どこから
/// 始まるか｡その後ろから区切りまでがすべて秘密の値だ｡
fn next_secret(haystack: &str) -> Option<(&'static str, usize)> {
    const KEYWORDS: [&str; 8] = [
        "Bearer ",
        "bearer ",
        "access_token=",
        "refresh_token=",
        "client_secret=",
        "token=",
        "code=",
        "state=",
    ];
    let lowered = haystack.to_ascii_lowercase();
    KEYWORDS
        .iter()
        .filter_map(|keyword| {
            let at = if keyword.starts_with("Bearer") {
                haystack.find(*keyword)?
            } else {
                lowered.find(*keyword)?
            };
            Some((*keyword, at))
        })
        .min_by_key(|(_, at)| *at)
}

/// Unix epoch からの秒数｡時計がそれより前なら 0｡
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// `unix_seconds` を `2026-08-19T00:31:04Z` として描く｡
///
/// 1 行の書式のために日付 crate を引き込まず手書きにした — モジュール doc と
/// 同じ理屈だ｡Howard Hinnant の `civil_from_days` を使う｡先発グレゴリオ暦に
/// 対して正確で､表も要らない｡タイムスタンプが生の epoch 秒であるログは
/// 誰も読まないログなので､この 20 行は値打ちがある｡
fn format_utc(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let seconds_of_day = unix_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day.div_euclid(3_600);
    let minute = seconds_of_day.div_euclid(60).rem_euclid(60);
    let second = seconds_of_day.rem_euclid(60);

    let mut out = String::with_capacity(20);
    // 失敗しない: String への書き込みは決して失敗しない｡
    let _ = write!(
        out,
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
    );
    out
}

/// 1970-01-01 からの日数を暦上の `(year, month, day)` へ変える｡
#[expect(
    clippy::arithmetic_side_effects,
    reason = "civil_from_days is exact arithmetic over a bounded range; \
              saturating any step would silently produce a wrong date \
              instead of a clamped one"
)]
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (
        year,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

/// `Level::Info` でログする｡
pub(crate) fn info(message: &str) {
    write(Level::Info, message);
}

/// `Level::Warn` でログする｡
pub(crate) fn warn(message: &str) {
    write(Level::Warn, message);
}

/// `Level::Error` でログする｡
pub(crate) fn error(message: &str) {
    write(Level::Error, message);
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- redact: 重要なテスト ---

    #[test]
    fn a_bearer_header_never_survives() {
        assert_eq!(
            redact("GET /2/users/me Authorization: Bearer AAAAAAAAsecret123"),
            "GET /2/users/me Authorization: Bearer [redacted]"
        );
    }

    #[test]
    fn a_token_response_body_never_survives() {
        let body = r#"{"access_token":"abc123","refresh_token":"def456","expires_in":7200}"#;
        let safe = redact(&body.replace("\":\"", "="));
        assert!(!safe.contains("abc123"), "{safe}");
        assert!(!safe.contains("def456"), "{safe}");
    }

    #[test]
    fn a_redirect_query_string_never_survives() {
        let safe = redact("callback: /?code=SplxlOBeZQQYbYS6WxSbIA&state=xyz");
        assert!(!safe.contains("SplxlOBeZQQYbYS6WxSbIA"), "{safe}");
        assert!(!safe.contains("xyz"), "{safe}");
    }

    #[test]
    fn redaction_is_case_insensitive_for_query_keys() {
        let safe = redact("ACCESS_TOKEN=hunter2");
        assert!(!safe.contains("hunter2"), "{safe}");
    }

    #[test]
    fn every_secret_in_one_line_is_redacted_not_just_the_first() {
        let safe = redact("access_token=one refresh_token=two");
        assert!(!safe.contains("one"), "{safe}");
        assert!(!safe.contains("two"), "{safe}");
    }

    #[test]
    fn an_ordinary_message_is_left_alone() {
        // 消しすぎるほうが安全な方向だが､あらゆるメッセージを食う
        // redactor は誰にも使えないログになる｡
        let message = "reload: 20 posts, cache hit, 1 request";
        assert_eq!(redact(message), message);
    }

    #[test]
    fn redaction_keeps_the_keyword_so_the_line_still_reads() {
        assert!(redact("Bearer secret").starts_with("Bearer "));
    }

    // --- Level ---

    #[test]
    fn levels_parse_case_insensitively() {
        assert_eq!(Level::parse("INFO"), Some(Level::Info));
        assert_eq!(Level::parse(" debug "), Some(Level::Debug));
        assert_eq!(Level::parse("warning"), Some(Level::Warn));
    }

    #[test]
    fn an_unknown_level_is_none_so_the_caller_can_fall_back() {
        assert_eq!(Level::parse("loud"), None);
        assert_eq!(Level::parse(""), None);
    }

    #[test]
    fn levels_order_from_least_to_most_verbose() {
        // `write` は `>` で比較するので､メッセージを通すかどうかを決めて
        // いるのはこの順序だ｡
        assert!(Level::Error < Level::Warn);
        assert!(Level::Warn < Level::Info);
        assert!(Level::Info < Level::Debug);
    }

    // --- ローテーション ---

    #[test]
    fn rotation_triggers_at_the_cap_not_past_it() {
        assert!(!should_rotate(999, 1000));
        assert!(should_rotate(1000, 1000));
        assert!(should_rotate(1001, 1000));
    }

    // --- ファイルそのもの ---

    #[test]
    fn the_log_file_is_created_owner_only() {
        // トークンファイルは 0600 だ (#7)｡同じ値を持つログが 0644 なら
        // それを台無しにする｡redaction が第一の防御で､これが第二だ｡
        use std::os::unix::fs::PermissionsExt as _;

        let dir = std::env::temp_dir().join(format!("twigpui-test-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("twigpui.log");

        append(&path, "hello\n").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "log file mode was {:o}", mode & 0o777);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rotation_moves_the_file_aside_and_keeps_it_readable() {
        let dir = std::env::temp_dir().join(format!("twigpui-test-rot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("twigpui.log");
        std::fs::write(&path, vec![b'x'; usize::try_from(MAX_BYTES).unwrap()]).unwrap();

        rotate_if_needed(&path);

        assert!(!path.exists(), "the current log should have been moved");
        assert!(
            dir.join("twigpui.log.1").exists(),
            "the previous generation should be readable"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_small_log_is_left_where_it_is() {
        let dir = std::env::temp_dir().join(format!("twigpui-test-norot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("twigpui.log");
        std::fs::write(&path, b"short").unwrap();

        rotate_if_needed(&path);

        assert!(path.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // --- タイムスタンプ ---

    #[test]
    fn the_epoch_renders_as_itself() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn a_known_instant_renders_correctly() {
        // 2026-08-19T00:31:04Z
        assert_eq!(format_utc(1_787_099_464), "2026-08-19T00:31:04Z");
    }

    #[test]
    fn a_leap_day_renders_correctly() {
        // 2024-02-29T12:00:00Z — 手書きの暦が間違えるケースだ｡
        assert_eq!(format_utc(1_709_208_000), "2024-02-29T12:00:00Z");
    }

    #[test]
    fn a_time_before_the_epoch_does_not_panic() {
        // `now()` は clamp するが､それが止んだとき panic するのが
        // `format_utc` であってはならない｡
        assert_eq!(format_utc(-1), "1969-12-31T23:59:59Z");
    }
}
