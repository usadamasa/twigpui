//! 読み手が画面の前にいるかどうか､そしていつ戻ってきたか (#204)｡
//!
//! auto-refresh は開けっぱなしのウィンドウにリズムを足すもので
//! (`ui::auto_refresh` を見よ)､画面がロックされていれば足す先が無い｡
//! ここはその 1 ビットと､それが変わった瞬間の記憶を持つ｡
//!
//! 分け方は `ui::auto_refresh` との対だ｡あちらは *cadence* — いつ次の
//! ポーリングに支払うか — を決める｡こちらは読み手が **いるか** を決める｡
//! 前者は後者を [`Activity`] と [`Presence::resumed_at`] という 2 つの
//! 入力として受け取るだけで､`ioreg` も遷移のログも見ない｡
//!
//! # なぜ通知ではなくコマンドなのか
//!
//! macOS がロックを知らせる正規の経路は
//! `NSDistributedNotificationCenter` の `com.apple.screenIsLocked` だ｡
//! objc2 では observer を登録するメソッドがどれも `unsafe fn` で､この
//! クレートは `unsafe_code` を **forbid** している — `forbid` は
//! `#[allow]` で覆せないので､これは好みではなく壁だ｡[`super::browser`]
//! が `NSWorkspace` ではなく `open(1)` を spawn しているのとまったく
//! 同じ壁で､同じ答えを取る｡
//!
//! 読むのは `IOKit` の registry だ｡Root エントリの `IOConsoleLocked` は
//! loginwindow が画面をロックしたときに立ち､`ioreg(8)` が特権も
//! entitlement も無しにそれを印字する｡問い合わせは auto-refresh ループの
//! 1 起床につき 1 回 — 平常時は 1 分に 1 回 — なので､通知を購読するのと
//! コストの桁が変わらない｡
//!
//! # sleep に通知が要らない理由
//!
//! `NSWorkspace` の sleep/wake 通知も同じ壁に当たるが､こちらはそもそも
//! 要らない｡眠っている間このプロセスは 1 命令も進まないので､リクエストは
//! 出ようがない｡知る必要があるのは **wake の瞬間** だけで､それは寝坊した
//! timer として観測できる — [`slept_through`] を見よ｡引数 2 つの純関数で
//! 足りる｡
//!
//! # 継ぎ目
//!
//! プロセスを起こすのは [`probe`] だけで､[`locked_in`] と
//! [`slept_through`] と [`Presence`] は純粋だ｡[`super::browser`] の
//! `is_openable` / `open` と同じ分け方｡

use std::process::Command;

use anyhow::{Context as _, Result};

use crate::log;

/// `ioreg(8)` の絶対パス｡
///
/// `PATH` を引かない｡Finder や Dock から起動された `.app` の `PATH` は
/// ログインシェルのものではなく launchd の既定
/// (`/usr/bin:/bin:/usr/sbin:/sbin`) で､そこに何が並ぶかはこのアプリが
/// 決められない｡`open(1)` と違ってこれは数分おきに静かに呼ばれるものなので､
/// 解決に失敗しても誰も気づかない — 絶対パスなら失敗のしようがない｡
const IOREG: &str = "/usr/sbin/ioreg";

/// 画面がロックされているとき､次に見るまで待つ長さ｡
///
/// ロックが解けたことに気づくまでの最大の遅れであり､ロック中に `ioreg` を
/// spawn する間隔でもある｡飛んでいる fetch を待つ数秒
/// (`ui::auto_refresh` の `BUSY_RECHECK_SECONDS`) よりずっと長いのは､
/// こちらが分どころか一晩続きうる状態だからだ｡ロックされたままのマシンで
/// 5 秒おきにプロセスを起こす理由は無い｡
pub(crate) const AWAY_RECHECK_SECONDS: i64 = 60;

/// timer が要求した期限をこれだけ過ぎて戻ってきたら､その間マシンは
/// 眠っていたと読む｡
///
/// 眠っていないマシンでも timer は少し遅れる — 負荷､executor の混雑｡
/// だから閾値は「遅れ」と「別の時代から戻ってきた」を分けられるだけ
/// 離してある｡短すぎる値にすると､忙しいだけの瞬間が wake として記録され､
/// ポーリングが 1 interval 押しやられる｡
const SLEPT_THROUGH_SECONDS: i64 = 30;

/// 読み手が画面を見られる状態にあるかどうか｡
///
/// 「見ているか」ではなく「見られるか」だ｡他のアプリを前面にしている
/// 読み手は [`Self::Present`] のままでいる — auto-refresh を止める理由は
/// 「読まれない」ことであって「今この瞬間フォーカスが無い」ことではない｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Activity {
    /// 画面が開いている｡auto-refresh は普段どおり動く｡
    Present,
    /// 画面がロックされている｡auto-refresh は何も送らない｡
    Away,
}

/// `ioreg(8)` に今の状態を尋ねる｡
///
/// 失敗は握り潰さずに返す｡倒す先を決めるのは [`Presence::observe`] だ｡
///
/// ユニットテストはしない — 実プロセスを起こすからだ｡カバレッジは
/// [`locked_in`] が担う｡[`super::browser::open`] と同じ慣習｡
pub(crate) fn probe() -> Result<Activity> {
    // シェルを経由しない｡引数は定数だけなので注入の余地はそもそも無いが､
    // `browser` が引いたのと同じ線をここでも引いておく｡
    let output = Command::new(IOREG)
        .args(["-n", "Root", "-d1"])
        .output()
        .with_context(|| format!("could not run {IOREG}"))?;
    if !output.status.success() {
        anyhow::bail!("{IOREG} exited with {}", output.status);
    }
    // 非 UTF-8 のバイトは registry の他のプロパティ (デバイス名など) から
    // 来うる｡探しているのは ASCII の 1 行なので､置換文字に潰して構わない｡
    let report = String::from_utf8_lossy(&output.stdout);
    let locked = locked_in(&report)
        .with_context(|| format!("{IOREG} printed no IOConsoleLocked property"))?;
    Ok(if locked {
        Activity::Away
    } else {
        Activity::Present
    })
}

/// `ioreg -n Root -d1` の出力から画面ロックの状態を読む｡
/// そのプロパティが出力に無ければ `None`｡
///
/// 探すのは 1 行だけだ:
///
/// ```text
///       "IOConsoleLocked" = No
/// ```
///
/// XML plist (`ioreg -a`) ではなく既定の書式を読む｡`<key>` と値が別々の
/// 行に出る XML と違い､こちらは鍵と値が 1 行に収まっていて､行の走査
/// だけで済む｡どちらの書式でも出力の大半は `IOKitDiagnostics` の 1 行で､
/// このプロパティはそれより前に出るので､見つけた時点で読むのをやめる｡
///
/// 見つからなかったことを `false` ではなく `None` として返す｡macOS が
/// このプロパティの名前を変えたなら､それは「ロックされていない」では
/// なく「もう答えられない」であり､呼び出し側はその 2 つに別々の反応を
/// する ([`Presence::observe`] を見よ)｡
fn locked_in(report: &str) -> Option<bool> {
    for line in report.lines() {
        let Some(value) = line.trim_start().strip_prefix("\"IOConsoleLocked\" = ") else {
            continue;
        };
        return match value.trim_end() {
            "Yes" => Some(true),
            "No" => Some(false),
            _ => None,
        };
    }
    None
}

/// timer が `expected_wake_at` を大きく過ぎて戻ってきたか — つまり
/// その間マシンが眠っていたか｡
///
/// これで sleep を知るのに足りるのは､眠っている間このプロセスが
/// 1 命令も進まないからだ｡通知を購読しても伝えられるのは「起きた」ことで､
/// それは寝坊した timer とまったく同じ事実だ｡違うのは､こちらは引数 2 つの
/// 純関数で､macOS も run loop も要らないことだけ｡
///
/// 起点は timer を仕掛ける直前に読んだ壁時計であって､`Tick::Wait` が
/// 名指した期限ではない｡ループは眠る長さを切り詰めるので､2 つは普段から
/// 食い違っている｡
fn slept_through(expected_wake_at: i64, now: i64) -> bool {
    now.saturating_sub(expected_wake_at) > SLEPT_THROUGH_SECONDS
}

/// 読み手が離れて戻ってきたことについて､auto-refresh ループが起床の
/// あいだ覚えていること｡
///
/// `TimelineView` のフィールドではなくループ変数として持たれる｡画面は
/// これを何も表示しないし (ポーリングが画面を取らないのと同じ理由)､
/// ループが終われば一緒に消えてよい｡
///
/// 覚えている 3 つはどれも「同じことを二度言わない」ためのものだ:
/// 遷移だけをログに出す､probe の失敗は一度だけ報告する､そして復帰時刻は
/// 上書きされるまで持ち回る｡
#[derive(Debug)]
pub(crate) struct Presence {
    /// 直前の起床で見た状態｡毎分 "the screen is locked" と書く代わりに､
    /// 変わった瞬間だけ書くために持つ｡
    last_seen: Activity,
    /// 読み手が戻ってきたと分かった時刻 — [`Self::resumed_at`] を見よ｡
    resumed_at: Option<i64>,
    /// probe の失敗をすでに報告したか｡`ioreg` が答えないマシンで
    /// ログを毎分 1 行ずつ埋めないために持つ｡
    reported_failure: bool,
}

impl Presence {
    /// ループが始まるときの記憶: 読み手はいて､まだ一度も離れていない｡
    ///
    /// 起動時にすでにロックされていれば､最初の [`Self::observe`] が
    /// それを遷移として見つけて 1 行残す｡
    pub(crate) fn present() -> Self {
        Self {
            last_seen: Activity::Present,
            resumed_at: None,
            reported_failure: false,
        }
    }

    /// 読み手が戻ってきたと分かった時刻｡ロックが解けたことに気づいた
    /// 瞬間か､マシンが sleep から戻った瞬間で､まだ一度も離れていなければ
    /// `None`｡
    ///
    /// これが「滞留した tick をまとめて発火させない」の全部だ｡ロックの
    /// 間に期限は何度も過ぎるが､どれも `Wait` にしかならないので溜まらない｡
    /// 溜まらない代わりに残るのは､ロックの **前** に打たれた
    /// `last_reload_at` — 8 時間眠ったマシンではとうに期限切れで､復帰した
    /// 瞬間に `Poll` を意味する｡これを cadence の anchor に混ぜることで､
    /// 起点が「最後に取った時刻」ではなく「戻ってきた時刻」になる｡
    pub(crate) fn resumed_at(&self) -> Option<i64> {
        self.resumed_at
    }

    /// この起床の probe の結果を取り込み､cadence が使う [`Activity`] を
    /// 返す｡変わっていればログを 1 行残し､戻ってきたのなら復帰時刻を打つ｡
    ///
    /// probe が答えられなかったときは [`Activity::Present`] へ倒す —
    /// つまり #204 の前とまったく同じ振る舞いだ｡検知が壊れたときに
    /// 取りうる態度は「止める」か「これまでどおり」かの 2 つで､黙って
    /// 古びるタイムラインのほうが余分な request より説明のつかない故障だ｡
    ///
    /// `interval_seconds` はログのためだけに要る｡なぜ再開が今この瞬間の
    /// ポーリングではないのかは､ログを読む人がいちばん先に抱く疑問だ｡
    pub(crate) fn observe(
        &mut self,
        probed: Result<Activity>,
        now: i64,
        interval_seconds: u32,
    ) -> Activity {
        let activity = match probed {
            Ok(activity) => activity,
            Err(error) => {
                if !self.reported_failure {
                    self.reported_failure = true;
                    log::warn(&format!(
                        "auto-refresh cannot tell whether the screen is locked, \
                         so it keeps polling: {error:#}"
                    ));
                }
                Activity::Present
            }
        };

        match (self.last_seen, activity) {
            (Activity::Present, Activity::Away) => {
                log::info("auto-refresh paused: the screen is locked");
            }
            (Activity::Away, Activity::Present) => {
                self.resumed_at = Some(now);
                log::info(&format!(
                    "auto-refresh resumed: the screen is unlocked, \
                     next poll in {interval_seconds}s"
                ));
            }
            _ => {}
        }
        self.last_seen = activity;
        activity
    }

    /// timer が寝坊して戻ってきたなら､その間マシンは眠っていた｡
    /// [`slept_through`] を見よ｡
    pub(crate) fn woke(&mut self, expected_wake_at: i64, now: i64, interval_seconds: u32) {
        if !slept_through(expected_wake_at, now) {
            return;
        }
        let slept = now.saturating_sub(expected_wake_at);
        self.resumed_at = Some(now);
        log::info(&format!(
            "auto-refresh resumed: the machine slept for about {slept}s, \
             next poll in {interval_seconds}s"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 実機の `ioreg -n Root -d1` の出力から､形を保った抜粋｡末尾の長い
    /// 1 行は `IOKitDiagnostics` の位置を表していて､探しているプロパティ
    /// がそれより前に出ることを示す｡
    const REPORT: &str = r#"+-o Root  <class IORegistryEntry, id 0x100000100, retain 30>
    {
      "IOKitBuildVersion" = "Darwin Kernel Version 25.5.0"
      "OS Build Version" = "25F84"
      "IOConsoleLocked" = No
      "IOConsoleUsers" = ({"kCGSSessionOnConsoleKey"=Yes,"kCGSSessionUserIDKey"=501})
      "IOKitDiagnostics" = {"Classes"={"ACMKernelService"=6,"AppleARMPE"=1}}
    }
"#;

    fn locked_report() -> String {
        REPORT.replace("\"IOConsoleLocked\" = No", "\"IOConsoleLocked\" = Yes")
    }

    #[test]
    fn an_unlocked_screen_reads_as_unlocked() {
        assert_eq!(locked_in(REPORT), Some(false));
    }

    #[test]
    fn a_locked_screen_reads_as_locked() {
        assert_eq!(locked_in(&locked_report()), Some(true));
    }

    // 鍵が消えたら "ロックされていない" ではなく "答えられない" だ｡
    // `Presence::observe` はこの 2 つに違う反応をする｡
    #[test]
    fn a_report_without_the_property_answers_nothing_rather_than_unlocked() {
        assert_eq!(
            locked_in(&REPORT.replace("\"IOConsoleLocked\" = No\n", "")),
            None
        );
    }

    #[test]
    fn an_empty_report_answers_nothing() {
        assert_eq!(locked_in(""), None);
    }

    // 値の綴りが変われば､それも "答えられない" だ｡`Yes` と `No` 以外を
    // 見て黙って false に倒すと､検知が壊れたことに誰も気づけない｡
    #[test]
    fn an_unfamiliar_value_answers_nothing() {
        assert_eq!(
            locked_in(&REPORT.replace("\"IOConsoleLocked\" = No", "\"IOConsoleLocked\" = 0")),
            None
        );
    }

    // `IOConsoleUsers` の中にも同じ綴りの鍵が現れうる｡照合は行頭 (字下げを
    // 除いた) で固定してあるので､入れ子の 1 行に引っかかることはない｡
    #[test]
    fn a_nested_line_that_merely_mentions_the_key_is_not_the_property() {
        let report = "      \"IOConsoleUsers\" = ({\"IOConsoleLocked\" = Yes})\n";
        assert_eq!(locked_in(report), None);
    }

    #[test]
    fn a_timer_that_returned_on_schedule_is_not_read_as_sleep() {
        assert!(!slept_through(1_000, 1_000));
        assert!(!slept_through(1_000, 1_000 + SLEPT_THROUGH_SECONDS));
    }

    // 閉じた蓋の向こうで壁時計だけが進む｡timer は要求した長さぶんしか
    // 数えていないので､戻ってきたときの遅れがそのまま眠った長さになる｡
    #[test]
    fn a_timer_that_returned_from_another_hour_means_the_machine_slept() {
        assert!(slept_through(1_000, 1_000 + 3_600));
    }

    // 早く戻ってきた timer (時計が後ろへ飛んだ) は sleep ではない｡
    // 引き算が負になっても飽和して `false` に落ちる｡
    #[test]
    fn a_clock_that_jumped_backwards_is_not_sleep() {
        assert!(!slept_through(1_000, 500));
        assert!(!slept_through(i64::MAX, i64::MIN));
    }

    #[test]
    fn a_reader_who_never_left_has_no_resume_to_anchor_to() {
        let mut presence = Presence::present();

        assert_eq!(
            presence.observe(Ok(Activity::Present), 1_000, 300),
            Activity::Present
        );
        assert_eq!(presence.resumed_at(), None);
    }

    // ロックの間は復帰時刻が打たれない — 打たれるのは戻ってきた瞬間だけだ｡
    // ここが「復帰後は最大 1 回だけ schedule される」の入口になる｡
    #[test]
    fn coming_back_from_a_locked_screen_is_recorded_once_it_is_unlocked() {
        let mut presence = Presence::present();

        assert_eq!(
            presence.observe(Ok(Activity::Away), 1_000, 300),
            Activity::Away
        );
        assert_eq!(presence.resumed_at(), None);
        assert_eq!(
            presence.observe(Ok(Activity::Away), 5_000, 300),
            Activity::Away
        );
        assert_eq!(presence.resumed_at(), None);

        assert_eq!(
            presence.observe(Ok(Activity::Present), 9_000, 300),
            Activity::Present
        );
        assert_eq!(presence.resumed_at(), Some(9_000));
    }

    // ロックが続いている間に復帰時刻が動くことは無い｡動けば anchor が
    // 毎分先送りされ､解除しても永遠にポーリングが来ないタイマーになる｡
    #[test]
    fn staying_present_does_not_keep_moving_the_resume_forward() {
        let mut presence = Presence::present();

        presence.observe(Ok(Activity::Away), 1_000, 300);
        presence.observe(Ok(Activity::Present), 2_000, 300);
        presence.observe(Ok(Activity::Present), 3_000, 300);

        assert_eq!(presence.resumed_at(), Some(2_000));
    }

    // 検知が壊れたら､止めるのではなく #204 の前と同じように動きつづける｡
    #[test]
    fn a_probe_that_could_not_answer_keeps_the_polling_going() {
        let mut presence = Presence::present();

        let activity = presence.observe(Err(anyhow::anyhow!("no ioreg here")), 1_000, 300);

        assert_eq!(activity, Activity::Present);
        assert_eq!(presence.resumed_at(), None);
    }

    // ロック中に probe が壊れても "戻ってきた" ことにはなる — それが
    // `Present` へ倒すということだ｡復帰時刻が打たれるので､そこから
    // 1 interval 待って 1 回ポーリングする｡
    #[test]
    fn a_probe_that_breaks_while_locked_resumes_rather_than_polling_at_once() {
        let mut presence = Presence::present();

        presence.observe(Ok(Activity::Away), 1_000, 300);
        presence.observe(Err(anyhow::anyhow!("ioreg went away")), 4_000, 300);

        assert_eq!(presence.resumed_at(), Some(4_000));
    }

    #[test]
    fn a_timer_that_slept_through_records_the_wake_as_a_resume() {
        let mut presence = Presence::present();

        presence.woke(1_000, 1_000 + 3_600, 300);

        assert_eq!(presence.resumed_at(), Some(4_600));
    }

    #[test]
    fn a_timer_that_returned_on_time_leaves_the_resume_alone() {
        let mut presence = Presence::present();

        presence.woke(1_000, 1_002, 300);

        assert_eq!(presence.resumed_at(), None);
    }

    // #204 は timeline の auto-refresh **だけ** を止める｡list sync は
    // バックグラウンドで follow 状態を合わせる仕事で､読み手がいるかどうか
    // とは関係が無いし､止めれば backoff の梯子 (#197, #198) がロックの
    // たびに巻き戻る｡
    //
    // これを固定するのに動かせる入力が無い — sync の判定関数はそもそも
    // activity を引数に取らない｡だから固定するのは呼び出しのほうだ:
    // sync のどのファイルもこのモジュールを呼ばない｡散文で `activity` に
    // 触れるのは自由で (むしろ触れてほしい)､捕まえるのは呼び出しだけ｡
    #[test]
    fn the_list_sync_loop_never_asks_whether_the_reader_is_present() {
        for (name, source) in [
            ("sync/mod.rs", include_str!("sync/mod.rs")),
            ("sync/auto.rs", include_str!("sync/auto.rs")),
            ("sync/run.rs", include_str!("sync/run.rs")),
            ("sync/schedule.rs", include_str!("sync/schedule.rs")),
            ("sync/state.rs", include_str!("sync/state.rs")),
            ("ui/list_sync.rs", include_str!("ui/list_sync.rs")),
        ] {
            assert!(
                !source.contains("activity::"),
                "{name} calls into activity — #204 stops timeline auto-refresh only, \
                 never the list sync loop"
            );
        }
    }
}
