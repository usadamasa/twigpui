//! background sync の記憶: *いつ* 支出してよいかについて知っていることの
//! すべてを 1 ファイルにまとめ､純粋関数 1 つだけが動かす (#197, #198)｡
//!
//! # なぜ 1 つの struct なのか
//!
//! 以前は sync のペース配分が 4 箇所に散っていた: 最後の diff 時刻はここ､
//! rate limit の期限は `ui::list_sync` の loop 変数､残件数はまた別の変数､
//! そして 15 分 window は `rate_limit.json`｡そのあいだの受け渡しが情報を
//! 落としていた｡#198 が最も分かりやすい例だ: loop は毎分起きて決め直すが､
//! 待てと言われたせいで何もすることが無かった tick は `Idle` を返し､
//! `Idle` を settle するとたった今待っていたその期限が消えていた｡refusal の
//! 2 分後には loop がまた送っていた｡アプリの再起動でも消えた: #197 が扱う
//! 20 時間のあいだに release build は 8 回起動されており､どの起動も即座に
//! 同じ上限へ送り込んでいた｡
//!
//! そこで期限は disk 上の state のフィールドにし､どのフィールドを変えるのも
//! [`settle`] だけにした｡それが知らないもの — `Idle` の tick — には手を
//! 触れない｡
//!
//! # いつ明けるか言わない上限から後退する
//!
//! #193 は `POST /2/lists/:id/members` が `remaining` 300 のうち 299 で
//! 拒否するのを実測した: `x-rate-limit-*` ヘッダが記述しない制限だ｡そこで
//! 入れた 900 秒の待機は refusal 1 回への当て推量だった｡その後 #197 で
//! 同じ refusal が 20 時間以上繰り返すのを見たが､固定待機ではその間ずっと
//! 15 分ごとに拒否される — しかも write なので課金されうる — request を
//! 投げることになる｡[`opaque_backoff_seconds`] は連続する opaque な refusal
//! ごとに待機を倍にし､6 時間で頭打ちにする: 下がったままの上限に対しては
//! 1 日あたり無駄な request が最大 4 回､明けた上限に気づくまでが最大 6 時間だ｡
//!
//! 連続回数を戻すのは届いた write だけで､それ以外では戻らない｡write の
//! 上限が下がっていても read は動き続ける (#197: diff は成功し､add は
//! しなかった) ので､diff の成功を上限が明けた証拠に数えてはならない｡
//!
//! # そもそもゆっくり送る
//!
//! #197 のロックは 18 分ほどでおよそ 100〜140 件の addition のあとに来た —
//! 毎分 7 件､1 秒間隔で 20 件ずつの batch で送っていた｡
//! [`APPLY_PAUSE_SECONDS`] と `sync_writes_per_batch` (既定 2､上限の
//! 24 時間での回復を実測して以来 config のつまみ) が持続レートを抑える｡
//! 既定値が上限より下かどうかは分かっていない｡狙いは､それが上限を踏む
//! 当のものにならないことだ｡きれいに走ったあとにつまみを上げるのが上限の
//! 大きさを探る公認のやり方で — 答えがどちらでも梯子が吸収する｡
//!
//! # 速度だけでは足りない
//!
//! 毎分 1 件まで落としても拒否は出続けた｡毎分 7 件よりはるかに遅いのに
//! 止められる以上､残る違いは速度ではなく規則正しさになる｡
//!
//! そこで間隔を範囲にした｡二層ある｡
//!
//! - batch と batch のあいだ: [`apply_pause_seconds`] が 90〜300 秒を引く｡
//! - batch の中: [`write_gap`] が write ごとに 3〜20 秒を引く｡
//!
//! どちらも上へしか振れない｡`sync_writes_per_batch` は上限なので､揺らぎが
//! それより速い瞬間を作ってはならない｡
//!
//! 継ぎ目は `rate_limit::backoff_delay` に倣う: 長さを決める関数は純粋で
//! `f64` を受け取り､`getrandom` を触るのは
//! `rate_limit::random_jitter_fraction` だけ｡持続レートはおよそ
//! `writes_per_batch / 195 秒`｡

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::schedule::Outcome;

/// batch と batch のあいだに待つ長さの下限｡秒｡実際の待ちは
/// [`apply_pause_seconds`] が batch ごとに引き直す｡
///
/// 60 秒から上げた｡毎分ちょうど 1 件でも拒否は出続けたので､#197 の上限
/// とは別の何かが効いている｡
pub(crate) const APPLY_PAUSE_SECONDS: i64 = 90;

/// 揺らぎが [`APPLY_PAUSE_SECONDS`] に足しうる最大の秒数｡間は 90〜300 秒に
/// 散り､平均は 195 秒｡
///
/// 幅が下限の 2 倍を超えるのは意図的｡狭い揺らぎは周期を隠さない — 60±5 秒は
/// 目盛りの粗い 60 秒周期でしかない｡
pub(crate) const APPLY_PAUSE_SPREAD_SECONDS: i64 = 210;

/// batch の中で write と write のあいだに空ける最小の長さ｡秒｡
///
/// これが無いと batch は同じ秒のうちに全件を投げる｡#197 のロックはまさに
/// その形 — 1 秒間隔で 20 件ずつ — の後に来た｡
pub(crate) const WRITE_GAP_FLOOR_SECONDS: u64 = 3;

/// 揺らぎがその間に足しうる最大の秒数｡間は 3〜20 秒に散る｡
///
/// pause を伸ばすのとは役目が別｡これがあるので `sync_writes_per_batch` を
/// 上げても一定間隔の連射にはならず､短いひとかたまりと長い沈黙になる｡
pub(crate) const WRITE_GAP_SPREAD_SECONDS: u64 = 17;

/// opaque な refusal 1 回で後退する上限: 6 時間｡1 日下がったままの上限が
/// 96 回ではなく 4 回の request で済むだけ長く､明けた上限に同じ 6 時間の
/// うちに気づけるだけ短い｡
pub(crate) const OPAQUE_BACKOFF_CEILING_SECONDS: i64 = 21_600;

/// background sync の時計とペース配分｡
/// [`crate::paths::Paths::sync_state_file`] に書かれる｡
///
/// どのフィールドも `#[serde(default)]` だ: 前のバージョンを走らせた
/// マシンのファイルは `last_diff_at` しか持たず､diff を 1 回払うのではなく
/// 読み込めなければならない｡
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct SyncState {
    /// 最後に diff を *試みた* 時刻｡失敗した試行でもこれが動く理由は
    /// [`super::auto`] を見よ｡
    #[serde(default)]
    pub last_diff_at: Option<i64>,
    /// いつまで何も送ってはならないか — rate limit の期限､opaque な
    /// refusal のあとの backoff､あるいは失敗した tick が得た interval｡
    /// 再起動でも守られるよう永続化してある (#198)｡
    #[serde(default)]
    pub blocked_until: Option<i64>,
    /// 次の write の batch まで自分に課した間｡[`apply_pause_seconds`] が
    /// batch ごとに引き直す｡
    ///
    /// これは拒否ではないので `blocked_until` とは分ける｡
    /// `ui::list_sync::status_of` は block を「rate limited」と読むため､
    /// 相乗りさせると普通のペース待ちが拒否として表示される｡
    ///
    /// 永続化する理由は `blocked_until` と同じ｡追いつきの途中で再起動しても
    /// 間が消えてはならない (#197 の 20 時間で release build は 8 回起動され､
    /// どの起動も即座に送っていた)｡
    #[serde(default)]
    pub paused_until: Option<i64>,
    /// あいだに write が 1 件も届いていない opaque な refusal の連続回数｡
    /// [`opaque_backoff_seconds`] を駆動し､catch-up が何時間も拒否されて
    /// いるときに status bar が出すのがこれだ (#197)｡
    #[serde(default)]
    pub refusals: u32,
}

impl SyncState {
    /// `blocked_until` がまだ `now` より先かどうか｡
    pub(crate) fn is_blocked(&self, now: i64) -> bool {
        self.blocked_until.is_some_and(|until| until > now)
    }
}

/// 連続 `refusals` 回目の opaque な refusal のあと､どれだけ待つか｡
///
/// `rate_limit::OPAQUE_LIMIT_BACKOFF_SECONDS` (15 分) から倍々にし､
/// [`OPAQUE_BACKOFF_CEILING_SECONDS`] で止まる: 15m, 30m, 1h, 2h, 4h､
/// 以降の refusal はすべて 6h｡`refusals` が 0 なら 1 回目として扱う —
/// この関数が答えるのは「今どれだけ待つか」であり､0 回目の待機など
/// 存在しないからだ｡
pub(crate) fn opaque_backoff_seconds(refusals: u32) -> i64 {
    let floor = crate::rate_limit::OPAQUE_LIMIT_BACKOFF_SECONDS;
    let doublings = refusals.saturating_sub(1);
    // 数回倍にした時点で天井がとうに勝っているので､shift はファイル由来の
    // u32 に任せず境界を付けてある｡
    let factor = 1i64 << doublings.min(16);
    floor
        .saturating_mul(factor)
        .min(OPAQUE_BACKOFF_CEILING_SECONDS)
}

/// `span` のうち `fraction` が指す長さ｡`fraction` は `0.0..=1.0` へ丸める｡
/// 供給源が何を返しても揺らぎが下限を割らないようにするため｡
fn scaled(span: i64, fraction: f64) -> i64 {
    let fraction = fraction.clamp(0.0, 1.0);
    // `span` はこのモジュールの定数 (3 桁) なので f64 が正確に表せる｡
    // 積も同じ桁に留まるため､切り捨てが落とすのは小数部だけ｡
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    let scaled = (span as f64 * fraction) as i64;
    scaled
}

/// 続きのある batch のあと､次の batch まで待つ長さ｡秒｡
///
/// [`APPLY_PAUSE_SECONDS`] から [`APPLY_PAUSE_SPREAD_SECONDS`] だけ上へ散る｡
/// 本番では `rate_limit::random_jitter_fraction` が batch ごとに引き直す｡
pub(crate) fn apply_pause_seconds(jitter_fraction: f64) -> i64 {
    APPLY_PAUSE_SECONDS.saturating_add(scaled(APPLY_PAUSE_SPREAD_SECONDS, jitter_fraction))
}

/// batch の中で write と write のあいだに眠る長さ｡
///
/// [`WRITE_GAP_FLOOR_SECONDS`] から [`WRITE_GAP_SPREAD_SECONDS`] だけ上へ
/// 散る｡引き直すのは write ごと｡同じ間を n 回繰り返せば､それもまた一定
/// 周期になる｡
pub(crate) fn write_gap(jitter_fraction: f64) -> std::time::Duration {
    let spread = i64::try_from(WRITE_GAP_SPREAD_SECONDS).unwrap_or(0);
    let extra = u64::try_from(scaled(spread, jitter_fraction)).unwrap_or(0);
    std::time::Duration::from_secs(WRITE_GAP_FLOOR_SECONDS.saturating_add(extra))
}

/// [`settle`] が待ち時間を決めるのに要る 2 つの長さ｡
///
/// 引数として並べずに struct にする｡どちらも秒なので､`now` の隣に置くと
/// 呼び出し側が黙って取り違える — [`super::schedule::Situation`] と同じ理由｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Spacing {
    /// `config.sync_interval_seconds`｡失敗した tick が得る待ちでもある｡
    pub interval_seconds: u32,
    /// 続きのある batch のあとに待つ長さ｡[`apply_pause_seconds`] が
    /// 呼び出しごとに引く｡
    pub apply_pause_seconds: i64,
}

/// [`settle`] が残すもの: 永続化する state と､loop が次に起きるべき時刻｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Settled {
    pub state: SyncState,
    /// 次の tick を走らせてよい最も早い時刻｡呼び出し側はこれより早く
    /// 起きて決め直してよい — `schedule::next_step` は `state` を読んで
    /// `Wait` と言う — が､これより前に tick を走らせてはならない｡
    pub wake_at: i64,
}

/// 直前の tick がしたことを踏まえて state を進める｡読み込んだあとの
/// [`SyncState`] に書き込む唯一の関数だ｡
///
/// tick が完全に失敗したとき `outcome` は `None` になる｡それはすぐの
/// 再試行ではなく interval 丸ごとを得る｡ここまで来る失敗は `rate_limit`
/// 自身のネットワーク再試行を既にくぐり抜けているからで — scope の失効､
/// 削除された list､parse できない plan ファイル — そのどれも 1 秒後に
/// もう一度訊いてよくなるものではない｡interval は `blocked_until` に
/// 記録するので､再起動もそれを再試行しない｡
///
/// `Idle` の tick は何も変えない (#198)｡それはこの state が既に持って
/// いる期限に対して loop が決め直しているだけで､期限を消す権限は無い｡
///
/// refusal は `blocked_until` を動かす｡opaque なものは連続回数も伸ばし､
/// その [`opaque_backoff_seconds`] だけ待つ｡window が説明できるものは
/// window の分だけ待ち､連続回数には触れない｡予定どおり開き直す window は
/// 隠れた上限についてどちらの証拠にもならないからだ｡refusal の直前に
/// write が届いていた場合 (`sent > 0`) は先に連続回数を戻す: 上限は
/// 少し前に明らかに開いていたので､これは 5 回目ではなく 1 回目の refusal だ｡
///
/// 何かを送れた batch は両方を消し — 上限が write を受け付けた — plan に
/// 続きがあれば [`Spacing::apply_pause_seconds`] 後に戻ってくる｡その間は
/// `paused_until` にも記録する｡`wake_at` は loop の変数でしかないので､
/// 再起動がそれを飛ばす｡
///
/// diff は見つけたものを流し切るためにすぐ戻ってくる｡そのとき前の plan が
/// 残した間は消す｡新しい plan の 1 batch 目が､もう存在しない batch の
/// あとを待つ理由は無い｡
pub(crate) fn settle(
    state: SyncState,
    outcome: Option<&Outcome>,
    now: i64,
    spacing: Spacing,
) -> Settled {
    let mut next = state;
    let wake_at = match outcome {
        None => {
            let until = now.saturating_add(i64::from(spacing.interval_seconds));
            next.blocked_until = Some(until);
            until
        }
        Some(Outcome::Idle { until, .. }) => *until,
        Some(Outcome::RateLimited {
            until,
            opaque,
            sent,
            ..
        }) => {
            if *sent > 0 {
                next.refusals = 0;
            }
            let until = if *opaque {
                next.refusals = next.refusals.saturating_add(1);
                now.saturating_add(opaque_backoff_seconds(next.refusals))
            } else {
                *until
            };
            next.blocked_until = Some(until);
            until
        }
        Some(Outcome::Applied { sent, remaining }) => {
            if *sent > 0 {
                next.refusals = 0;
                next.blocked_until = None;
            }
            if *remaining > 0 {
                let until = now.saturating_add(spacing.apply_pause_seconds);
                next.paused_until = Some(until);
                until
            } else {
                next.paused_until = None;
                now
            }
        }
        Some(Outcome::Diffed { .. }) => {
            next.paused_until = None;
            now
        }
    };
    Settled {
        state: next,
        wake_at,
    }
}

/// `path` から state を読み戻す｡
///
/// `load_plan` と違い､壊れたファイルはエラーではなく `Ok(default)` だ｡
/// 二つの失敗は対称ではない: 読めない *plan* は apply を両側の全 read を
/// 支払うところまで押し戻すが､読めない *時計* はどのみち interval 内に
/// 起きるはずだった diff ちょうど 1 回分で済む｡それで loop 全体を落とす
/// 方が高くつく｡loop こそがこの機能だからだ｡
pub(crate) fn load_state(path: &std::path::Path) -> SyncState {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return SyncState::default();
    };
    serde_json::from_str(&contents).unwrap_or_default()
}

/// state を `path` へ書く｡
pub(crate) fn save_state(path: &std::path::Path, state: &SyncState) -> Result<()> {
    let json = serde_json::to_string_pretty(state).context("could not serialize the sync state")?;
    std::fs::write(path, json).with_context(|| format!("could not write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rate_limit::OPAQUE_LIMIT_BACKOFF_SECONDS;
    use crate::sync::schedule::{Situation, Step, next_step};

    const INTERVAL: u32 = 21_600;
    /// テストが引く「揺らぎの目」｡本番は呼び出しごとに引き直すので､
    /// [`settle`] は待ち時間を渡された長さとしてしか見ない — その一点を
    /// 固定して読みやすくしてある｡
    const PAUSE: i64 = 77;

    fn spacing() -> Spacing {
        Spacing {
            interval_seconds: INTERVAL,
            apply_pause_seconds: PAUSE,
        }
    }

    /// 落ち着いた sync: diff は走り済み､block も pause も refusal も無い｡
    /// 各テストは自分が扱う 1 フィールドだけを上書きする｡
    fn calm() -> SyncState {
        SyncState {
            last_diff_at: Some(1_000),
            blocked_until: None,
            paused_until: None,
            refusals: 0,
        }
    }

    fn opaque(sent: usize) -> Outcome {
        Outcome::RateLimited {
            until: 1_000 + OPAQUE_LIMIT_BACKOFF_SECONDS,
            opaque: true,
            sent,
            remaining: 2_157,
        }
    }

    // --- 梯子 ---

    #[test]
    fn the_first_opaque_refusal_waits_the_floor() {
        assert_eq!(opaque_backoff_seconds(1), OPAQUE_LIMIT_BACKOFF_SECONDS);
    }

    #[test]
    fn each_further_refusal_doubles_the_wait() {
        assert_eq!(opaque_backoff_seconds(2), 1_800);
        assert_eq!(opaque_backoff_seconds(3), 3_600);
        assert_eq!(opaque_backoff_seconds(4), 7_200);
        assert_eq!(opaque_backoff_seconds(5), 14_400);
    }

    #[test]
    fn the_wait_stops_growing_at_six_hours() {
        // 6 回目は 900 × 2⁵ = 28,800 になるはずだが､天井が勝つ｡
        assert_eq!(opaque_backoff_seconds(6), OPAQUE_BACKOFF_CEILING_SECONDS);
        assert_eq!(opaque_backoff_seconds(40), OPAQUE_BACKOFF_CEILING_SECONDS);
        assert_eq!(
            opaque_backoff_seconds(u32::MAX),
            OPAQUE_BACKOFF_CEILING_SECONDS
        );
    }

    #[test]
    fn a_streak_of_zero_is_read_as_the_first_wait() {
        assert_eq!(opaque_backoff_seconds(0), OPAQUE_LIMIT_BACKOFF_SECONDS);
    }

    // --- settle: refusal ---

    #[test]
    fn an_opaque_refusal_starts_a_streak_and_blocks_for_the_floor() {
        let settled = settle(calm(), Some(&opaque(0)), 1_000, spacing());
        assert_eq!(settled.state.refusals, 1);
        assert_eq!(
            settled.state.blocked_until,
            Some(1_000 + OPAQUE_LIMIT_BACKOFF_SECONDS)
        );
        assert_eq!(settled.wake_at, 1_000 + OPAQUE_LIMIT_BACKOFF_SECONDS);
    }

    #[test]
    fn a_second_opaque_refusal_waits_twice_as_long() {
        // #197 の失敗: 20 時間にわたり 15 分ごとに同じ 429｡2 回目が同じ
        // 15 分を待ってはならない｡
        let state = SyncState {
            refusals: 1,
            ..calm()
        };
        let settled = settle(state, Some(&opaque(0)), 10_000, spacing());
        assert_eq!(settled.state.refusals, 2);
        assert_eq!(settled.state.blocked_until, Some(10_000 + 1_800));
    }

    #[test]
    fn a_write_that_landed_before_the_refusal_restarts_the_streak() {
        // 少し前に上限が write を受け付けたので､この refusal は古い連続の
        // 6 回目ではなく新しい連続の 1 回目だ｡
        let state = SyncState {
            refusals: 5,
            ..calm()
        };
        let settled = settle(state, Some(&opaque(3)), 10_000, spacing());
        assert_eq!(settled.state.refusals, 1);
        assert_eq!(
            settled.state.blocked_until,
            Some(10_000 + OPAQUE_LIMIT_BACKOFF_SECONDS)
        );
    }

    #[test]
    fn a_refusal_the_window_explains_waits_for_the_window_and_is_not_a_streak() {
        // 15 分 window を使い切り､いつ開き直すかも言っている｡これは隠れた
        // 上限ではないので､連続回数も伸ばさなければ梯子の分も待たない｡
        let state = SyncState {
            refusals: 3,
            ..calm()
        };
        let outcome = Outcome::RateLimited {
            until: 1_500,
            opaque: false,
            sent: 0,
            remaining: 40,
        };
        let settled = settle(state, Some(&outcome), 1_000, spacing());
        assert_eq!(settled.state.refusals, 3);
        assert_eq!(settled.state.blocked_until, Some(1_500));
        assert_eq!(settled.wake_at, 1_500);
    }

    // --- settle: 連続が終わるのは write のときだけ ---

    #[test]
    fn a_batch_that_sent_something_ends_the_streak_and_the_block() {
        let state = SyncState {
            refusals: 4,
            blocked_until: Some(900),
            ..calm()
        };
        let outcome = Outcome::Applied {
            sent: 2,
            remaining: 100,
        };
        let settled = settle(state, Some(&outcome), 1_000, spacing());
        assert_eq!(settled.state.refusals, 0);
        assert_eq!(settled.state.blocked_until, None);
    }

    #[test]
    fn a_diff_does_not_end_the_streak() {
        // write の上限が下がっていても read は動く (#197: diff は通り､
        // add は通らなかった)｡diff の成功は write が通るかどうかについて
        // 何も言わない｡
        let state = SyncState {
            refusals: 4,
            ..calm()
        };
        let outcome = Outcome::Diffed {
            adds: 3,
            removals: 0,
            members_total: 100,
            held: false,
        };
        let settled = settle(state, Some(&outcome), 1_000, spacing());
        assert_eq!(settled.state.refusals, 4);
        assert_eq!(settled.wake_at, 1_000);
    }

    #[test]
    fn a_batch_that_sent_nothing_changes_nothing() {
        // entry のある plan からは起こりえないが､起きたとしても上限が
        // 明けた証拠にはならない｡
        let state = SyncState {
            refusals: 2,
            blocked_until: Some(900),
            ..calm()
        };
        let outcome = Outcome::Applied {
            sent: 0,
            remaining: 0,
        };
        assert_eq!(settle(state, Some(&outcome), 1_000, spacing()).state, state);
    }

    // --- 揺らぎ ---

    #[test]
    fn the_pause_never_falls_below_its_floor() {
        // 揺らぎは伸ばす側にしかない｡引きの目が悪くても設定より速く
        // 送ってはならない｡
        assert_eq!(apply_pause_seconds(0.0), APPLY_PAUSE_SECONDS);
    }

    #[test]
    fn the_pause_stretches_to_the_top_of_its_spread() {
        assert_eq!(
            apply_pause_seconds(1.0),
            APPLY_PAUSE_SECONDS + APPLY_PAUSE_SPREAD_SECONDS
        );
    }

    #[test]
    fn a_fraction_outside_zero_to_one_is_clamped_rather_than_trusted() {
        // 負の目が pause を floor より下へ引くのは､この機能が防ごうと
        // しているものそのもの｡
        assert_eq!(apply_pause_seconds(-1.0), APPLY_PAUSE_SECONDS);
        assert_eq!(
            apply_pause_seconds(2.0),
            APPLY_PAUSE_SECONDS + APPLY_PAUSE_SPREAD_SECONDS
        );
    }

    #[test]
    fn the_gap_between_writes_stays_inside_its_range() {
        assert_eq!(write_gap(0.0).as_secs(), WRITE_GAP_FLOOR_SECONDS);
        assert_eq!(
            write_gap(1.0).as_secs(),
            WRITE_GAP_FLOOR_SECONDS + WRITE_GAP_SPREAD_SECONDS
        );
        // 中間の目も範囲の中にある｡幅そのものではなく境界を押さえる｡
        let middle = write_gap(0.5).as_secs();
        assert!(
            (WRITE_GAP_FLOOR_SECONDS..=WRITE_GAP_FLOOR_SECONDS + WRITE_GAP_SPREAD_SECONDS)
                .contains(&middle),
            "a mid-range roll left the gap range: {middle}"
        );
    }

    // --- settle: ペース配分 ---

    #[test]
    fn a_batch_with_more_to_send_pauses_before_the_next() {
        // 1 秒に 20 件ではなく毎分 2 件 — module doc を見よ｡
        let outcome = Outcome::Applied {
            sent: 2,
            remaining: 2_155,
        };
        let settled = settle(calm(), Some(&outcome), 1_000, spacing());
        assert_eq!(settled.wake_at, 1_000 + PAUSE);
    }

    #[test]
    fn the_pause_is_recorded_so_a_restart_does_not_skip_it() {
        // #197 が扱う 20 時間で release build は 8 回起動され､どの起動も
        // 即座に送っていた｡wake_at はループ変数だが､これはファイルに残る｡
        let outcome = Outcome::Applied {
            sent: 2,
            remaining: 2_155,
        };
        let settled = settle(calm(), Some(&outcome), 1_000, spacing());
        assert_eq!(settled.state.paused_until, Some(1_000 + PAUSE));
    }

    #[test]
    fn the_batch_that_finishes_the_plan_comes_straight_back() {
        // ペースを配る相手がもういない｡diff が来るかどうかは次の tick が
        // 決める｡
        let outcome = Outcome::Applied {
            sent: 2,
            remaining: 0,
        };
        let settled = settle(calm(), Some(&outcome), 1_000, spacing());
        assert_eq!(settled.wake_at, 1_000);
        assert_eq!(settled.state.paused_until, None);
    }

    #[test]
    fn a_diff_clears_a_pause_the_previous_plan_left_behind() {
        // 期限切れの pause がファイルに残っていると､次の plan の 1 batch
        // 目が理由も無く待たされる｡
        let state = SyncState {
            paused_until: Some(1_060),
            ..calm()
        };
        let outcome = Outcome::Diffed {
            adds: 3,
            removals: 1,
            members_total: 100,
            held: false,
        };
        let settled = settle(state, Some(&outcome), 1_000, spacing());
        assert_eq!(settled.wake_at, 1_000);
        assert_eq!(settled.state.paused_until, None);
    }

    // --- settle: idle と失敗 ---

    #[test]
    fn an_idle_tick_leaves_the_state_exactly_as_it_found_it() {
        // #198｡loop は毎分決め直す｡待てと言われた tick が､待つ理由その
        // ものを消してはならない｡
        let state = SyncState {
            refusals: 2,
            blocked_until: Some(5_000),
            ..calm()
        };
        let outcome = Outcome::Idle {
            until: 5_000,
            pending: 2_157,
        };
        let settled = settle(state, Some(&outcome), 1_060, spacing());
        assert_eq!(settled.state, state);
        assert_eq!(settled.wake_at, 5_000);
    }

    #[test]
    fn a_failed_tick_earns_a_full_interval_and_records_it() {
        // 待つだけでなく記録する: アプリを再起動しても､失効した scope や
        // 削除された list を即座に再試行してはならない｡
        let settled = settle(calm(), None, 1_000, spacing());
        assert_eq!(settled.wake_at, 1_000 + i64::from(INTERVAL));
        assert_eq!(
            settled.state.blocked_until,
            Some(1_000 + i64::from(INTERVAL))
        );
        assert_eq!(settled.state.refusals, 0);
    }

    // #198 の端から端まで: 拒否され､1 分後に起きて待てと言われ､その 1 分
    // 後に起きて — まだ待っている｡古い `settle` が 2 歩目で失っていたのが
    // この並びで､release build が 2 分ごとに演じているのを観測したのも
    // これだ｡
    #[test]
    fn a_refusal_still_holds_two_wake_ups_later() {
        let refused_at = 1_000;
        let first = settle(calm(), Some(&opaque(0)), refused_at, spacing());
        let until = first.state.blocked_until.unwrap();

        let situation = |state: SyncState| Situation {
            last_diff_at: state.last_diff_at,
            interval_seconds: INTERVAL,
            pending: 2_157,
            blocked_until: state.blocked_until,
            paused_until: state.paused_until,
        };
        let woke_at = refused_at + 60;
        assert_eq!(
            next_step(&situation(first.state), woke_at),
            Step::Wait { until }
        );

        let idle = Outcome::Idle {
            until,
            pending: 2_157,
        };
        let second = settle(first.state, Some(&idle), woke_at, spacing());
        assert_eq!(
            next_step(&situation(second.state), woke_at + 60),
            Step::Wait { until },
            "the refusal was forgotten one wake-up after it was handed over"
        );
        assert_eq!(next_step(&situation(second.state), until), Step::Apply);
    }

    // --- ファイル ---

    #[test]
    fn the_state_survives_a_round_trip_through_the_file() {
        let dir = std::env::temp_dir().join(format!("twigpui-sync-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");

        let written = SyncState {
            last_diff_at: Some(1_700_000_000),
            blocked_until: Some(1_700_000_900),
            paused_until: Some(1_700_000_077),
            refusals: 3,
        };
        save_state(&path, &written).unwrap();
        assert_eq!(load_state(&path), written);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_file_reads_as_never_synced() {
        // その結果､初回起動は即座に diff する — 新規インストールに対して
        // schedule が望む挙動だ｡
        let path =
            std::env::temp_dir().join(format!("twigpui-no-state-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_state(&path), SyncState::default());
    }

    #[test]
    fn a_corrupt_file_reads_as_never_synced_rather_than_failing_the_loop() {
        // `load_plan` の規則とは意図的に逆だ: 壊れた時計はどのみち
        // interval 内に来るはずだった diff 1 回分で済むが､それで loop を
        // 落とせば機能そのものが止まる｡
        let path =
            std::env::temp_dir().join(format!("twigpui-bad-state-{}.json", std::process::id()));
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load_state(&path), SyncState::default());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_file_from_before_the_backoff_fields_still_loads() {
        // 前のバージョンを走らせたどのマシンも disk に持っているもの｡
        // ここで時計を失えば､更新後の初回起動で diff 1 回分かかる｡
        let state: SyncState = serde_json::from_str(r#"{"last_diff_at":1787470513}"#).unwrap();
        assert_eq!(state.last_diff_at, Some(1_787_470_513));
        assert_eq!(state.blocked_until, None);
        assert_eq!(state.paused_until, None);
        assert_eq!(state.refusals, 0);
    }

    #[test]
    fn is_blocked_reads_the_deadline_against_now() {
        let state = SyncState {
            blocked_until: Some(2_000),
            ..calm()
        };
        assert!(state.is_blocked(1_999));
        assert!(!state.is_blocked(2_000));
        assert!(!calm().is_blocked(0));
    }
}
