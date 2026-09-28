//! background sync の write のペース配分 (#231): 3 つの範囲と､そこから
//! 1 回分を引く関数､config からの解決｡
//!
//! # なぜ範囲なのか
//!
//! #197 のロックは毎分 7 件の write のあとに来た｡毎分 1 件へ落としても
//! 拒否は止まらず､固定の間隔は速度によらず機械の周期に見える､というのが
//! 残った説明だ｡そこで write と write の間 ([`WritePacing::gap_seconds`])､
//! 続けて送る件数 ([`WritePacing::batch_writes`])､batch のあとの休み
//! ([`WritePacing::cooldown_seconds`]) をどれも `min-max` の範囲にし､
//! 毎回引き直す｡人が数件足して手を止める形をなぞっている｡
//!
//! 効いているかどうかは後から log で読む ([`super::auto`] の module doc の
//! 「tick が log に残すもの」を見よ)｡そのために 3 つの範囲は定数ではなく
//! config にあり､起動の行に出る: 範囲を変えて refusal の間隔がどう動いたかを
//! log だけで突き合わせられるように｡
//!
//! 継ぎ目は `rate_limit::backoff_delay` に倣う: [`Span::draw`] は純粋で
//! `f64` を受け取り､`getrandom` を触るのは
//! `rate_limit::random_jitter_fraction` だけ｡

use std::fmt;

use anyhow::{Context as _, Result, bail};

/// `min-max` の閉区間｡秒にも件数にも使う — 単位は持ち主のフィールド名が
/// 言う｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    pub min: u32,
    pub max: u32,
}

impl Span {
    pub(crate) const fn new(min: u32, max: u32) -> Self {
        Self { min, max }
    }

    /// `"3-20"` を読む｡片側だけ､数でないもの､`min > max` は拒む｡
    ///
    /// 単独の数を `n-n` と読まないのは意図してのこと: `sync_writes_per_batch = 2`
    /// の癖で打った `"2"` を固定値として黙って通せば､範囲を書いたつもりの
    /// 人に固定の周期を渡すことになる｡固定にしたければ `"2-2"` と書く｡
    pub(crate) fn parse(raw: &str) -> Result<Self> {
        let Some((min, max)) = raw.split_once('-') else {
            bail!("expected a range written as min-max, got {raw:?}");
        };
        let number = |side: &str| {
            side.trim()
                .parse::<u32>()
                .with_context(|| format!("expected a range written as min-max, got {raw:?}"))
        };
        let span = Self::new(number(min)?, number(max)?);
        if span.min > span.max {
            bail!("the range {raw:?} runs backwards: min must not exceed max");
        }
        Ok(span)
    }

    /// `fraction` が指す 1 点: 0.0 で `min`､1.0 で `max`｡`fraction` は
    /// `0.0..=1.0` へ丸めるので､供給源が何を返しても範囲を出ない｡
    pub(crate) fn draw(self, fraction: f64) -> u32 {
        let fraction = fraction.clamp(0.0, 1.0);
        // 幅は u32 なので f64 が正確に表せる｡積はそれより小さいため､
        // 切り捨てが落とすのは小数部だけで､結果は幅を超えない｡
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let extra = (f64::from(self.max.saturating_sub(self.min)) * fraction) as u32;
        self.min.saturating_add(extra)
    }

    /// `self` が `bounds` の中に収まっているか｡
    fn within(self, bounds: Self) -> bool {
        self.min >= bounds.min && self.max <= bounds.max
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.min, self.max)
    }
}

/// background sync が write をどう散らすか｡3 つとも config 由来で
/// ([`Self::resolve`])､どれも tick ごとに引き直す｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WritePacing {
    /// batch の中で write と write のあいだに置く秒数｡
    pub gap_seconds: Span,
    /// cooldown を挟まずに続けて送る件数｡
    pub batch_writes: Span,
    /// batch を送り切ったあと次の batch まで休む秒数｡
    pub cooldown_seconds: Span,
}

/// `batch_writes` が受け付ける最大値: 20 は X の *文書化された* 書き込みの
/// 窓 (15 分で 300 回) を 1 分へならした数｡gap が最短で並んでもそこには
/// 届かないが､超えても速くなるのは refusal だけなので残してある — `2` の
/// つもりで打った `25` は､バーストではなくキー名を挙げた error になるべき｡
const MAX_BATCH_WRITES: u32 = 20;

impl WritePacing {
    /// 既定: gap 3-20 秒､batch 1-3 件､cooldown 90-300 秒｡
    ///
    /// #231 より前の固定値 (1 batch 2 件､gap 3-20 秒､batch の間 90-300 秒)
    /// と持続レートを揃えてある: batch の平均は 2 件のままなので､
    /// およそ 2 件 / (195 + 11.5) 秒 ≈ 0.6 件/分｡上限の大きさは今も実測
    /// されていない (#197)｡既定で走らせて refusal が出ないのを見てから
    /// 上げるのは今までと同じで､答えがどちらでも `state` の梯子が吸収する｡
    pub(crate) const DEFAULT: Self = Self {
        gap_seconds: Span::new(3, 20),
        batch_writes: Span::new(1, 3),
        cooldown_seconds: Span::new(90, 300),
    };

    /// 3 つの範囲を解決する: それぞれ env > file > [`Self::DEFAULT`]｡
    ///
    /// 秒の 2 つは `min >= 1` — 0 秒の間は #197 の直前の形 (同じ秒に全件)
    /// そのものだ｡件数は `1..=MAX_BATCH_WRITES`: 0 は「off」ではなく拒む｡
    /// off のためのスイッチは `auto_sync_list` であり､0 件の batch は
    /// 走っていると称しながら plan を決して流し切らない sync になる｡
    pub(crate) fn resolve(
        var: impl Fn(&str) -> Option<String>,
        gap: Option<String>,
        batch: Option<String>,
        cooldown: Option<String>,
    ) -> Result<Self> {
        let seconds = Span::new(1, u32::MAX);
        Ok(Self {
            gap_seconds: resolve_span(
                "X_SYNC_WRITE_GAP_SECONDS",
                "sync_write_gap_seconds",
                &var,
                gap,
                Self::DEFAULT.gap_seconds,
                seconds,
            )?,
            batch_writes: resolve_span(
                "X_SYNC_BATCH_WRITES",
                "sync_batch_writes",
                &var,
                batch,
                Self::DEFAULT.batch_writes,
                Span::new(1, MAX_BATCH_WRITES),
            )?,
            cooldown_seconds: resolve_span(
                "X_SYNC_COOLDOWN_SECONDS",
                "sync_cooldown_seconds",
                &var,
                cooldown,
                Self::DEFAULT.cooldown_seconds,
                seconds,
            )?,
        })
    }
}

impl fmt::Display for WritePacing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "gap {}s, batch {}, cooldown {}s",
            self.gap_seconds, self.batch_writes, self.cooldown_seconds
        )
    }
}

/// 範囲 1 つを env > file > `default` で解決し､`bounds` の外なら出所を
/// 挙げて拒む｡
fn resolve_span(
    key: &str,
    file_key: &str,
    var: &impl Fn(&str) -> Option<String>,
    file_value: Option<String>,
    default: Span,
    bounds: Span,
) -> Result<Span> {
    // 空の env は「素通し」: shell に置き去りにされた `X_...=` は値ではない｡
    let (raw, source) = match var(key).filter(|value| !value.trim().is_empty()) {
        Some(raw) => (raw, key),
        None => match file_value {
            Some(raw) => (raw, file_key),
            None => return Ok(default),
        },
    };
    let file_source = format!("{file_key} in config.toml");
    let source = if source == key { key } else { &file_source };
    let span = Span::parse(&raw).with_context(|| format!("{source} is not a valid range"))?;
    if !span.within(bounds) {
        bail!("{source} must stay within {bounds}, got {span}");
    }
    Ok(span)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Span::parse ---

    #[test]
    fn a_range_reads_as_min_and_max() {
        assert_eq!(Span::parse("3-20").unwrap(), Span::new(3, 20));
    }

    #[test]
    fn a_degenerate_range_is_a_fixed_value() {
        // 実験の対照群に要る: 揺らぎを切って固定の間隔で走らせる形｡
        assert_eq!(Span::parse("5-5").unwrap(), Span::new(5, 5));
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(Span::parse(" 3 - 20 ").unwrap(), Span::new(3, 20));
    }

    #[test]
    fn a_range_whose_min_exceeds_its_max_is_rejected() {
        let error = Span::parse("20-3").unwrap_err().to_string();
        assert!(error.contains("20-3"), "{error}");
    }

    #[test]
    fn a_single_number_is_rejected() {
        // `sync_writes_per_batch = 2` の癖で打った `"2"` を 2-2 と読めば､
        // 範囲を書いたつもりの人に黙って固定値を渡すことになる｡
        assert!(Span::parse("2").is_err());
    }

    #[test]
    fn a_non_numeric_range_is_rejected() {
        assert!(Span::parse("a-b").is_err());
        assert!(Span::parse("3-").is_err());
        assert!(Span::parse("-20").is_err());
        assert!(Span::parse("").is_err());
    }

    // --- Span::draw ---

    #[test]
    fn the_draw_covers_both_ends() {
        let span = Span::new(90, 300);
        assert_eq!(span.draw(0.0), 90);
        assert_eq!(span.draw(1.0), 300);
        assert_eq!(span.draw(0.5), 195);
    }

    #[test]
    fn a_fraction_outside_zero_to_one_is_clamped_rather_than_trusted() {
        // 負の目が gap を min より下へ引くのは､この機能が防ごうとしている
        // ものそのもの｡
        let span = Span::new(3, 20);
        assert_eq!(span.draw(-1.0), 3);
        assert_eq!(span.draw(2.0), 20);
    }

    #[test]
    fn a_fixed_span_always_draws_itself() {
        assert_eq!(Span::new(7, 7).draw(0.3), 7);
    }

    #[test]
    fn a_span_prints_as_it_is_written() {
        assert_eq!(Span::new(3, 20).to_string(), "3-20");
        assert_eq!(
            WritePacing::DEFAULT.to_string(),
            "gap 3-20s, batch 1-3, cooldown 90-300s"
        );
    }

    // --- WritePacing::resolve (env > file > default) ---

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        move |key: &str| {
            owned
                .iter()
                .find(|(candidate, _)| candidate == key)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn nothing_set_resolves_to_the_default() {
        let pacing = WritePacing::resolve(vars(&[]), None, None, None).unwrap();
        assert_eq!(pacing, WritePacing::DEFAULT);
    }

    #[test]
    fn the_file_sets_each_range_when_the_env_is_unset() {
        let pacing = WritePacing::resolve(
            vars(&[]),
            Some("5-30".to_string()),
            Some("2-4".to_string()),
            Some("120-600".to_string()),
        )
        .unwrap();
        assert_eq!(pacing.gap_seconds, Span::new(5, 30));
        assert_eq!(pacing.batch_writes, Span::new(2, 4));
        assert_eq!(pacing.cooldown_seconds, Span::new(120, 600));
    }

    #[test]
    fn the_env_wins_over_the_file_for_each_range() {
        let pacing = WritePacing::resolve(
            vars(&[
                ("X_SYNC_WRITE_GAP_SECONDS", "10-40"),
                ("X_SYNC_BATCH_WRITES", "3-6"),
                ("X_SYNC_COOLDOWN_SECONDS", "600-900"),
            ]),
            Some("5-30".to_string()),
            Some("2-4".to_string()),
            Some("120-600".to_string()),
        )
        .unwrap();
        assert_eq!(pacing.gap_seconds, Span::new(10, 40));
        assert_eq!(pacing.batch_writes, Span::new(3, 6));
        assert_eq!(pacing.cooldown_seconds, Span::new(600, 900));
    }

    #[test]
    fn an_empty_env_value_falls_through_to_the_file() {
        // shell に置き去りにされた `X_SYNC_BATCH_WRITES=` は「素通し」で
        // あって､空の範囲ではない｡
        let pacing = WritePacing::resolve(
            vars(&[("X_SYNC_BATCH_WRITES", "")]),
            None,
            Some("2-4".to_string()),
            None,
        )
        .unwrap();
        assert_eq!(pacing.batch_writes, Span::new(2, 4));
    }

    #[test]
    fn a_batch_of_zero_is_rejected() {
        // 0 は "off" ではない — off は `auto_sync_list` の役目だ｡
        let error = WritePacing::resolve(vars(&[("X_SYNC_BATCH_WRITES", "0-3")]), None, None, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("X_SYNC_BATCH_WRITES"), "{error}");
        assert!(error.contains("1-20"), "{error}");
    }

    #[test]
    fn a_batch_past_the_documented_window_is_rejected() {
        let error = WritePacing::resolve(vars(&[]), None, Some("1-21".to_string()), None)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("sync_batch_writes in config.toml"),
            "{error}"
        );
    }

    #[test]
    fn the_documented_window_itself_is_accepted() {
        let pacing = WritePacing::resolve(vars(&[]), None, Some("1-20".to_string()), None).unwrap();
        assert_eq!(pacing.batch_writes, Span::new(1, 20));
    }

    #[test]
    fn a_zero_second_gap_or_cooldown_is_rejected() {
        // 0 秒の間は #197 の直前の形 — 同じ秒に全件 — そのもの｡
        assert!(WritePacing::resolve(vars(&[]), Some("0-20".to_string()), None, None).is_err());
        assert!(WritePacing::resolve(vars(&[]), None, None, Some("0-300".to_string())).is_err());
    }

    #[test]
    fn a_malformed_range_names_where_it_came_from() {
        let error = WritePacing::resolve(
            vars(&[("X_SYNC_COOLDOWN_SECONDS", "slow")]),
            None,
            None,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_SYNC_COOLDOWN_SECONDS"), "{error}");
    }
}
