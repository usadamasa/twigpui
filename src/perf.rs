//! 走っているアプリ自身のメモリと CPU を測る (`--perf <seconds>`)｡
//!
//! ## なぜアプリの中にあるのか
//!
//! 外から `ps` で眺めるほうが普通だ｡しかし Claude Code の sandbox は
//! `ps` も `top` も `footprint` も許さず (プロセス一覧の取得そのものが
//! 拒まれる)､sandbox の外で走るのは `cargo *` だけだ｡`cargo run` で
//! 立ったプロセスは自分の数字を読めるので､測定はアプリ自身にやらせる｡
//! 測る側と測られる側が同じプロセスになるが､`ps` を 1 秒に 1 回起こす
//! コストは子プロセスの側に付き､この過程の CPU 時間にも RSS にも乗らない｡
//!
//! ## なぜ `ps` なのか
//!
//! `getrusage(2)` と `proc_pidinfo` は `unsafe` で､この crate は
//! `unsafe_code = "forbid"` を掲げている｡`rustix` には rusage の包みが
//! 無く､`sysinfo` はこの target のビルドに入っていない (#46 はビルド時間
//! についての open な issue だ)｡[`super::activity`] が `ioreg` を､
//! [`super::browser`] が `open(1)` を起こすのと同じ線で､`ps(1)` を起こす｡
//!
//! 読むのは 3 列: `rss` (KiB)､`time` (user + system の累積 CPU 時間､
//! `[h:]mm:ss.cc`)､`%cpu` (カーネルが持つ減衰平均)｡区間ごとの CPU 使用率は
//! `time` の差分を壁時計の差分で割って出す — `%cpu` は起動直後ほど
//! 当てにならないので､参考値として横に置くだけだ｡
//!
//! ## 何を出すか
//!
//! stdout へ 1 秒に 1 行の TSV (機械が読む)､終わりに stderr とログへ
//! 要約 (人が読む)｡要約には測定条件 — debug/release､画面ロックの有無､
//! occluded でも描くか — を添える｡fixture の window は画面ロック中も
//! 描き続けるので (fork した gpui の patch)､ロック中の数字は本番の idle
//! と比べられない｡条件を並べて出すのは､その取り違えを後から見つける
//! ためだ｡
//!
//! `--fixture` としか組まない｡live の window は起動だけで課金される｡

use std::fmt;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context as _, Result};

/// `ps(1)` の絶対パス｡`PATH` を引かない｡
const PS: &str = "/bin/ps";

/// `ps` が返した 1 回分の数字｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Reading {
    /// resident set size (KiB)｡
    pub(crate) rss_kb: u64,
    /// user + system の累積 CPU 時間｡
    pub(crate) cpu_time: Duration,
    /// `%cpu` を 10 倍した整数 (`1.5` なら 15)｡浮動小数を持ち回らない｡
    pub(crate) pcpu_tenths: u32,
}

/// 起動からの経過時間を添えた [`Reading`]｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sample {
    /// 測定を始めてからの経過｡
    pub(crate) elapsed: Duration,
    /// そのときの数字｡
    pub(crate) reading: Reading,
}

/// `[[h:]mm:]ss.cc` を読む｡`ps -o time` の書式｡
pub(crate) fn parse_cpu_time(_text: &str) -> Option<Duration> {
    None
}

/// `ps -o rss=,time=,%cpu=` の 1 行を読む｡
pub(crate) fn parse_reading(_line: &str) -> Option<Reading> {
    None
}

/// `pid` の今の数字を `ps` に尋ねる｡
///
/// テストは [`the_process_can_read_itself`] だけで､中身の parse は
/// [`parse_reading`] のテストが担う｡
pub(crate) fn read(pid: u32) -> Result<Reading> {
    let output = Command::new(PS)
        .args(["-o", "rss=,time=,%cpu=", "-p", &pid.to_string()])
        .output()
        .with_context(|| format!("could not run {PS}"))?;
    if !output.status.success() {
        anyhow::bail!("{PS} exited with {}", output.status);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_reading(&text).with_context(|| format!("{PS} printed something unexpected: {text:?}"))
}

/// `--perf <seconds>` を読む｡無ければ `Ok(None)`､値が無いか数でなければ
/// 人に見せるメッセージを `Err` で返す｡
pub(crate) fn seconds_arg(_args: &[String]) -> Result<Option<u64>, String> {
    Ok(None)
}

/// 直前の sample からの区間で､CPU を何 % 使ったか (10 倍した整数)｡
/// 壁時計が進んでいなければ `None`｡
pub(crate) fn interval_cpu_tenths(_previous: &Sample, _current: &Sample) -> Option<u64> {
    None
}

/// 10 倍した整数を `1.5` の形に戻す｡
pub(crate) fn tenths(_value: u64) -> String {
    String::new()
}

/// stdout に出す TSV のヘッダ｡
pub(crate) const TSV_HEADER: &str = "elapsed_ms\trss_kb\tcpu_ms\tcpu_pct\tps_pcpu";

/// stdout に出す TSV の 1 行｡`cpu_pct` は直前の sample からの区間､
/// 最初の行は空欄｡
pub(crate) fn tsv_row(_sample: &Sample, _previous: Option<&Sample>) -> String {
    String::new()
}

/// 測定の要約｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Summary {
    /// sample の数｡
    pub(crate) samples: usize,
    /// 最初の sample から最後の sample までの壁時計｡
    pub(crate) wall: Duration,
    /// その間に使った CPU 時間｡
    pub(crate) cpu: Duration,
    /// `cpu / wall` を 10 倍した整数｡
    pub(crate) cpu_average_tenths: u64,
    /// 区間ごとの CPU 使用率の最大 (10 倍した整数)｡
    pub(crate) cpu_peak_tenths: u64,
    /// 最初の RSS (KiB)｡
    pub(crate) rss_first_kb: u64,
    /// 最後の RSS (KiB)｡
    pub(crate) rss_last_kb: u64,
    /// 最大の RSS (KiB)｡
    pub(crate) rss_peak_kb: u64,
}

/// sample 列を要約する｡2 つ未満なら `None` — 区間が無い｡
pub(crate) fn summarize(_samples: &[Sample]) -> Option<Summary> {
    None
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(elapsed_ms: u64, rss_kb: u64, cpu_ms: u64) -> Sample {
        Sample {
            elapsed: Duration::from_millis(elapsed_ms),
            reading: Reading {
                rss_kb,
                cpu_time: Duration::from_millis(cpu_ms),
                pcpu_tenths: 0,
            },
        }
    }

    // --- parse_cpu_time ---

    #[test]
    fn cpu_time_reads_minutes_seconds_and_centiseconds() {
        assert_eq!(parse_cpu_time("0:00.12"), Some(Duration::from_millis(120)));
        assert_eq!(parse_cpu_time("12:34.56"), Some(Duration::from_millis(754_560)));
    }

    #[test]
    fn cpu_time_reads_hours_when_ps_prints_them() {
        assert_eq!(
            parse_cpu_time("1:02:03.45"),
            Some(Duration::from_millis(3_723_450))
        );
    }

    #[test]
    fn cpu_time_tolerates_surrounding_whitespace() {
        assert_eq!(parse_cpu_time("  0:01.00 \n"), Some(Duration::from_secs(1)));
    }

    #[test]
    fn cpu_time_rejects_garbage() {
        assert_eq!(parse_cpu_time(""), None);
        assert_eq!(parse_cpu_time("abc"), None);
        assert_eq!(parse_cpu_time("1:2:3:4.5"), None);
        assert_eq!(parse_cpu_time("0:00"), None);
    }

    // --- parse_reading ---

    #[test]
    fn a_ps_line_reads_as_a_reading() {
        assert_eq!(
            parse_reading("  84512   0:01.23   1.5\n"),
            Some(Reading {
                rss_kb: 84512,
                cpu_time: Duration::from_millis(1230),
                pcpu_tenths: 15,
            })
        );
    }

    #[test]
    fn a_whole_percent_reads_as_tenths() {
        assert_eq!(parse_reading("1 0:00.00 12.0").map(|r| r.pcpu_tenths), Some(120));
        assert_eq!(parse_reading("1 0:00.00 0.0").map(|r| r.pcpu_tenths), Some(0));
    }

    #[test]
    fn a_short_or_empty_line_is_not_a_reading() {
        assert_eq!(parse_reading(""), None);
        assert_eq!(parse_reading("84512 0:01.23"), None);
        assert_eq!(parse_reading("x 0:01.23 1.5"), None);
    }

    #[test]
    fn the_process_can_read_itself() {
        let reading = read(std::process::id()).unwrap();
        assert!(reading.rss_kb > 0, "rss should be positive: {reading:?}");
    }

    // --- seconds_arg ---

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn no_perf_flag_means_no_measurement() {
        assert_eq!(seconds_arg(&args(&["twigpui", "--fixture", "f.json"])), Ok(None));
    }

    #[test]
    fn perf_flag_takes_a_number_of_seconds() {
        assert_eq!(seconds_arg(&args(&["twigpui", "--perf", "60"])), Ok(Some(60)));
    }

    #[test]
    fn perf_flag_without_a_value_or_with_a_bad_one_is_an_error() {
        assert!(seconds_arg(&args(&["twigpui", "--perf"])).is_err());
        assert!(seconds_arg(&args(&["twigpui", "--perf", "soon"])).is_err());
        assert!(seconds_arg(&args(&["twigpui", "--perf", "0"])).is_err());
    }

    // --- interval and rows ---

    #[test]
    fn interval_cpu_is_cpu_delta_over_wall_delta() {
        let a = sample(0, 100, 0);
        let b = sample(1000, 100, 15);
        assert_eq!(interval_cpu_tenths(&a, &b), Some(15));
        let c = sample(3000, 100, 1015);
        assert_eq!(interval_cpu_tenths(&b, &c), Some(500));
    }

    #[test]
    fn interval_cpu_needs_the_clock_to_move() {
        let a = sample(1000, 100, 0);
        assert_eq!(interval_cpu_tenths(&a, &a), None);
    }

    #[test]
    fn tenths_render_with_one_decimal() {
        assert_eq!(tenths(0), "0.0");
        assert_eq!(tenths(15), "1.5");
        assert_eq!(tenths(1234), "123.4");
    }

    #[test]
    fn the_first_row_has_no_interval_and_later_rows_do() {
        let a = sample(0, 84512, 120);
        let mut b = sample(1000, 84600, 130);
        b.reading.pcpu_tenths = 7;
        assert_eq!(tsv_row(&a, None), "0\t84512\t120\t\t0.0");
        assert_eq!(tsv_row(&b, Some(&a)), "1000\t84600\t130\t1.0\t0.7");
    }

    // --- summarize ---

    #[test]
    fn a_summary_needs_two_samples() {
        assert_eq!(summarize(&[]), None);
        assert_eq!(summarize(&[sample(0, 1, 0)]), None);
    }

    #[test]
    fn a_summary_reports_cpu_and_rss_over_the_run() {
        let samples = [
            sample(0, 84000, 0),
            sample(1000, 84500, 20),
            sample(2000, 90000, 30),
            sample(4000, 85000, 50),
        ];
        assert_eq!(
            summarize(&samples),
            Some(Summary {
                samples: 4,
                wall: Duration::from_secs(4),
                cpu: Duration::from_millis(50),
                cpu_average_tenths: 12,
                cpu_peak_tenths: 20,
                rss_first_kb: 84000,
                rss_last_kb: 85000,
                rss_peak_kb: 90000,
            })
        );
    }

    #[test]
    fn a_summary_reads_as_one_line_per_axis() {
        let summary = summarize(&[sample(0, 84000, 0), sample(60_000, 85024, 600)]).unwrap();
        let text = summary.to_string();
        assert!(text.contains("cpu: avg 1.0%, peak 1.0%, 0.6s over 60.0s"), "{text}");
        assert!(text.contains("rss: first 84000 kB, last 85024 kB, peak 85024 kB, growth +1024 kB"), "{text}");
        assert!(text.contains("samples: 2"), "{text}");
    }
}
