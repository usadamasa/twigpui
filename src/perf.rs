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
//! 測り方と読み方､それに過去の数字は `runtime-profiling` スキルにある｡

use std::fmt;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use gpui::App;

use crate::activity::{self, Activity};
use crate::ui::Startup;
use crate::{FetchPostArg, fetch_post_arg, log};

/// `ps(1)` の絶対パス｡`PATH` を引かない｡
const PS: &str = "/bin/ps";

/// sample の間隔｡`ps -o time` は 10 ms 刻みなので､これより短くすると
/// 区間の CPU 使用率が 1% 単位より粗くなる｡
const INTERVAL: Duration = Duration::from_secs(1);

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
pub(crate) fn parse_cpu_time(text: &str) -> Option<Duration> {
    let (clock, centi) = text.trim().rsplit_once('.')?;
    if centi.len() != 2 {
        return None;
    }
    let centi: u64 = centi.parse().ok()?;
    let parts: Vec<&str> = clock.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let mut seconds: u64 = 0;
    for part in parts {
        let value: u64 = part.parse().ok()?;
        seconds = seconds.checked_mul(60)?.checked_add(value)?;
    }
    let millis = seconds
        .checked_mul(1000)?
        .checked_add(centi.checked_mul(10)?)?;
    Some(Duration::from_millis(millis))
}

/// `12.3` を 10 倍した整数として読む｡`ps -o %cpu` は小数 1 桁で印字する｡
fn parse_tenths(text: &str) -> Option<u32> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, "0"));
    let whole: u32 = whole.parse().ok()?;
    let tenth = fraction.chars().next()?.to_digit(10)?;
    whole.checked_mul(10)?.checked_add(tenth)
}

/// `ps -o rss=,time=,%cpu=` の 1 行を読む｡
pub(crate) fn parse_reading(line: &str) -> Option<Reading> {
    let mut fields = line.split_whitespace();
    let rss_kb = fields.next()?.parse().ok()?;
    let cpu_time = parse_cpu_time(fields.next()?)?;
    let pcpu_tenths = parse_tenths(fields.next()?)?;
    if fields.next().is_some() {
        return None;
    }
    Some(Reading {
        rss_kb,
        cpu_time,
        pcpu_tenths,
    })
}

/// `pid` の今の数字を `ps` に尋ねる｡
///
/// テストは `the_process_can_read_itself` だけで､中身の parse は
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

/// 組まれた測定｡`--perf` が無ければ無い｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Plan {
    /// 何秒 sample を取るか｡
    pub(crate) seconds: u64,
    /// fixture の window だけ true｡[`Conditions`] に写す｡
    pub(crate) draws_while_occluded: bool,
}

/// `--perf <seconds>` を読む｡無ければ `Ok(None)`､値が無いか数でなければ
/// 人に見せるメッセージを `Err` で返す｡
///
/// `--fixture` としか組まない｡live の window は起動だけで課金されるので､
/// 測定のたびに払うことになる｡
pub(crate) fn arm(args: &[String], startup: &Startup) -> Result<Option<Plan>, String> {
    let seconds = match fetch_post_arg(args, "--perf") {
        FetchPostArg::Absent => return Ok(None),
        FetchPostArg::MissingValue => {
            return Err("--perf requires a number of seconds to sample for.".to_string());
        }
        FetchPostArg::Value(text) => match text.parse::<u64>() {
            Ok(seconds) if seconds > 0 => seconds,
            _ => {
                return Err(format!(
                    "--perf: {text:?} is not a positive number of seconds."
                ));
            }
        },
    };
    if matches!(startup, Startup::Live) {
        return Err(
            "--perf only runs with --fixture: a live window bills requests on startup.".to_string(),
        );
    }
    Ok(Some(Plan {
        seconds,
        draws_while_occluded: startup.draws_while_occluded(),
    }))
}

/// 直前の sample からの区間で､CPU を何 % 使ったか (10 倍した整数)｡
/// 壁時計が進んでいなければ `None`｡
pub(crate) fn interval_cpu_tenths(previous: &Sample, current: &Sample) -> Option<u64> {
    let wall = current.elapsed.checked_sub(previous.elapsed)?.as_millis();
    if wall == 0 {
        return None;
    }
    let cpu = current
        .reading
        .cpu_time
        .saturating_sub(previous.reading.cpu_time)
        .as_millis();
    u64::try_from(cpu.checked_mul(1000)?.checked_div(wall)?).ok()
}

/// 10 倍した整数を `1.5` の形に戻す｡
pub(crate) fn tenths(value: u64) -> String {
    format!("{}.{}", value / 10, value % 10)
}

/// stdout に出す TSV のヘッダ｡
pub(crate) const TSV_HEADER: &str = "elapsed_ms\trss_kb\tcpu_ms\tcpu_pct\tps_pcpu";

/// stdout に出す TSV の 1 行｡`cpu_pct` は直前の sample からの区間､
/// 最初の行は空欄｡
pub(crate) fn tsv_row(sample: &Sample, previous: Option<&Sample>) -> String {
    let interval = previous
        .and_then(|previous| interval_cpu_tenths(previous, sample))
        .map(tenths)
        .unwrap_or_default();
    format!(
        "{}\t{}\t{}\t{}\t{}",
        sample.elapsed.as_millis(),
        sample.reading.rss_kb,
        sample.reading.cpu_time.as_millis(),
        interval,
        tenths(u64::from(sample.reading.pcpu_tenths))
    )
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
pub(crate) fn summarize(samples: &[Sample]) -> Option<Summary> {
    let (first, rest) = samples.split_first()?;
    let last = rest.last()?;
    let wall = last.elapsed.checked_sub(first.elapsed)?;
    let cpu = last.reading.cpu_time.saturating_sub(first.reading.cpu_time);
    let cpu_average_tenths = interval_cpu_tenths(first, last)?;
    let cpu_peak_tenths = samples
        .windows(2)
        .filter_map(|pair| match pair {
            [previous, current] => interval_cpu_tenths(previous, current),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let rss_peak_kb = samples.iter().map(|sample| sample.reading.rss_kb).max()?;
    Some(Summary {
        samples: samples.len(),
        wall,
        cpu,
        cpu_average_tenths,
        cpu_peak_tenths,
        rss_first_kb: first.reading.rss_kb,
        rss_last_kb: last.reading.rss_kb,
        rss_peak_kb,
    })
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let growth = if self.rss_last_kb >= self.rss_first_kb {
            format!("+{}", self.rss_last_kb.saturating_sub(self.rss_first_kb))
        } else {
            format!("-{}", self.rss_first_kb.saturating_sub(self.rss_last_kb))
        };
        writeln!(
            f,
            "perf cpu: avg {}%, peak {}%, {:.1}s over {:.1}s",
            tenths(self.cpu_average_tenths),
            tenths(self.cpu_peak_tenths),
            self.cpu.as_secs_f64(),
            self.wall.as_secs_f64()
        )?;
        writeln!(
            f,
            "perf rss: first {} kB, last {} kB, peak {} kB, growth {growth} kB",
            self.rss_first_kb, self.rss_last_kb, self.rss_peak_kb
        )?;
        write!(f, "perf samples: {}", self.samples)
    }
}

/// 数字がどういう状況で取れたか｡数字と一緒に出す — ロック中の fixture の
/// idle を本番の idle と見比べる取り違えを､後から見つけられるように｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Conditions {
    /// `debug` か `release`｡
    pub(crate) build: &'static str,
    /// `locked` / `unlocked`､`ioreg` が答えなければ `unknown`｡
    pub(crate) screen: &'static str,
    /// fixture の window だけ true｡occluded でも描き続ける｡
    pub(crate) draws_while_occluded: bool,
}

impl Conditions {
    /// 今の状況を読む｡`ioreg` を 1 回起こす｡
    pub(crate) fn observe(draws_while_occluded: bool) -> Self {
        let screen = match activity::probe() {
            Ok(Activity::Away) => "locked",
            Ok(Activity::Present) => "unlocked",
            Err(_) => "unknown",
        };
        Self {
            build: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            screen,
            draws_while_occluded,
        }
    }
}

impl fmt::Display for Conditions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "perf conditions: build {}, screen {}, draws while occluded: {}",
            self.build,
            self.screen,
            if self.draws_while_occluded {
                "yes"
            } else {
                "no"
            }
        )
    }
}

/// 1 秒ごとに sample を取り､`plan.seconds` 経ったら要約を出してアプリを
/// 終える｡`plan` が無ければ何もしない｡
///
/// window を開いた後に呼ぶ｡`ps` は background executor で起こすので､
/// 描画のスレッドを待たせない｡測定条件は走り終わった時点で読む —
/// 数字が取れていた間の状態に一番近いのはそこだ｡
pub(crate) fn start(cx: &mut App, plan: Option<Plan>) {
    let Some(plan) = plan else {
        return;
    };
    let pid = std::process::id();
    let budget = Duration::from_secs(plan.seconds);
    cx.spawn(async move |cx| {
        let started = Instant::now();
        let mut samples: Vec<Sample> = Vec::new();
        println!("{TSV_HEADER}");
        loop {
            match cx
                .background_executor()
                .spawn(async move { read(pid) })
                .await
            {
                Ok(reading) => {
                    let sample = Sample {
                        elapsed: started.elapsed(),
                        reading,
                    };
                    println!("{}", tsv_row(&sample, samples.last()));
                    samples.push(sample);
                }
                Err(error) => log::warn(&format!("perf: {error:#}")),
            }
            if started.elapsed() >= budget {
                break;
            }
            cx.background_executor().timer(INTERVAL).await;
        }
        let conditions = Conditions::observe(plan.draws_while_occluded);
        let report = match summarize(&samples) {
            Some(summary) => format!("{conditions}\n{summary}"),
            None => format!("{conditions}\nperf: fewer than two samples, nothing to summarize"),
        };
        eprintln!("{report}");
        for line in report.lines() {
            log::info(line);
        }
        // `Err` はアプリがもう畳まれているときだけで､そのときは終わって
        // いるのだから､することは無い｡
        if cx.update(|cx| cx.quit()).is_err() {
            log::warn("perf: the app was already gone when the run ended");
        }
    })
    .detach();
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
        assert_eq!(
            parse_cpu_time("12:34.56"),
            Some(Duration::from_millis(754_560))
        );
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
        assert_eq!(
            parse_reading("1 0:00.00 12.0").map(|r| r.pcpu_tenths),
            Some(120)
        );
        assert_eq!(
            parse_reading("1 0:00.00 0.0").map(|r| r.pcpu_tenths),
            Some(0)
        );
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

    // --- arm ---

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    fn fixture_startup() -> Startup {
        let loaded = crate::fixture::load(std::path::Path::new("fixtures/timeline.json")).unwrap();
        Startup::Fixture(Box::new(loaded))
    }

    #[test]
    fn no_perf_flag_means_no_measurement() {
        assert_eq!(arm(&args(&["twigpui"]), &Startup::Live), Ok(None));
        assert_eq!(arm(&args(&["twigpui"]), &fixture_startup()), Ok(None));
    }

    #[test]
    fn perf_flag_takes_a_number_of_seconds_and_remembers_the_fixture_window() {
        assert_eq!(
            arm(&args(&["twigpui", "--perf", "60"]), &fixture_startup()),
            Ok(Some(Plan {
                seconds: 60,
                draws_while_occluded: true,
            }))
        );
    }

    #[test]
    fn perf_flag_without_a_value_or_with_a_bad_one_is_an_error() {
        let startup = fixture_startup();
        assert!(arm(&args(&["twigpui", "--perf"]), &startup).is_err());
        assert!(arm(&args(&["twigpui", "--perf", "soon"]), &startup).is_err());
        assert!(arm(&args(&["twigpui", "--perf", "0"]), &startup).is_err());
    }

    #[test]
    fn perf_flag_refuses_a_live_window_because_it_bills() {
        let error = arm(&args(&["twigpui", "--perf", "60"]), &Startup::Live).unwrap_err();
        assert!(error.contains("--fixture"), "{error}");
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
        assert!(
            text.contains("cpu: avg 1.0%, peak 1.0%, 0.6s over 60.0s"),
            "{text}"
        );
        assert!(
            text.contains("rss: first 84000 kB, last 85024 kB, peak 85024 kB, growth +1024 kB"),
            "{text}"
        );
        assert!(text.contains("samples: 2"), "{text}");
    }

    #[test]
    fn conditions_say_what_the_numbers_were_taken_under() {
        let conditions = Conditions {
            build: "debug",
            screen: "locked",
            draws_while_occluded: true,
        };
        assert_eq!(
            conditions.to_string(),
            "perf conditions: build debug, screen locked, draws while occluded: yes"
        );
    }
}
