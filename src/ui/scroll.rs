//! 手で scroll したときの動き (#175): ホイールは目標へ滑らかに寄り､
//! trackpad は OS の言うとおりに動き､どちらも端では rubber band になる｡
//!
//! 形は [`super::auto_refresh`] と同じ: 上半分は gpui に触れない純粋な
//! モデル [`Scroller`] で､入力の delta と経過時間を渡すと次の offset を
//! 返すだけなので､加速・減速・端の clamp は単調性と収束をここでテスト
//! できる｡下半分はそれを画面につなぐ `impl TimelineView` — event を
//! 横取りする canvas と､フレームのループだ｡
//!
//! # gpui が入力をどこまで運んでくるか (#175 の「先に確認すること」)
//!
//! gpui 0.2.2 の `Div` は `overflow_y_scroll` の要素に自分でホイールの
//! listener を張り､bubble phase で `offset += delta.pixel_delta(line_height)`
//! を足す (`elements/div.rs` の `paint_scroll_listener`)｡滑らかにする
//! 段も､端で跳ねる段も無い — clamp は次の prepaint でされるので､端を
//! 越えた入力は黙って捨てられる｡
//!
//! macOS 側 (`platform/mac/events.rs`) は `hasPreciseScrollingDeltas` で
//! 2 通りに分ける:
//!
//! - trackpad と Magic Mouse は `ScrollDelta::Pixels`｡OS がすでに加速を
//!   掛け､指を離したあとの慣性も OS が同じ event の列として送り続ける｡
//!   gpui は `phase()` しか読まず `momentumPhase()` を読まないので､慣性の
//!   event は `TouchPhase::Moved` として届き､慣性が終わった印は来ない｡
//! - ホイールのマウスは `ScrollDelta::Lines`｡これも OS が加速済みの値で､
//!   gpui が行の高さを掛けて px にする｡1 ノッチが行の高さの数倍に飛ぶ
//!   のに､次の入力まで何も起きない — それが「機敏すぎる」の正体だ｡
//!
//! だからここでは係数を重ねない｡`Pixels` は 1:1 で通す (慣性を二重に
//! 足さない)｡`Lines` は目標へ積み上げ､位置は指数関数で目標へ寄る —
//! 小さな入力は小さく､立て続けの入力は位置が追いつかないぶんだけ速く
//! 動き､入力が止まれば減速して止まる｡どちらも端を越えたぶんは rubber
//! band に回し､指が離れるか入力が止まれば戻る｡

use gpui::TouchPhase;

// [`super::list_sync`] と同じく書き下す: ここが `ui` から借りるものは
// clippy の `wildcard_imports` が列挙できる程度に少ない｡
use super::{Context, Duration, IntoElement, Styled, TimelineView, px};

/// アニメーションの 1 フレームの長さ､秒 — 60Hz のディスプレイに追随
/// する｡timer の間隔であり､[`Scroller::step`] へ渡す経過時間でもある｡
/// glide (#22) も同じ刻みで歩く｡
pub(super) const FRAME_S: f32 = 0.016;

/// このモデルが置いた offset から一覧がどれだけ離れていたら､誰か他の人
/// (`ScrollToTop`､reload の補正､pill) が飛ばしたと読むか､px｡glide が
/// 読み手の手を検知するのと同じ閾値｡
const GRAB_PX: f32 = 1.0;

/// ホイールの目標へ寄る時定数､秒｡位置と目標の差は 1 時定数ごとに
/// 1/e になるので､100px のティックは 0.1 秒で 63px､0.3 秒で 95px 進む｡
/// Chrome の smooth scrolling と同じ桁で､短すぎれば元の「飛ぶ」に戻り､
/// 長すぎれば入力に遅れて付いてくる｡
const WHEEL_TAU_S: f32 = 0.09;

/// 端を越えたぶんの見た目の上限､px｡rubber band はこの先へは伸びない｡
pub(super) const PULL_LIMIT_PX: f32 = 96.;

/// rubber band の硬さ｡引いた量 `x` に対して見た目は
/// `LIMIT * (1 - 1 / (x * STIFFNESS / LIMIT + 1))` で､最初の数十 px は
/// ほぼこの比率で付いてきて､先へ行くほど渋くなる｡
const PULL_STIFFNESS: f32 = 0.55;

/// ホイールが端を突いたとき､余った距離のうち band に回す割合｡trackpad の
/// 引っ張りより控えめにする — ノッチ 1 つが 100px を越えることがあり､
/// そのまま回すと端に着くたびに大きく跳ねる｡
const WHEEL_PULL: f32 = 0.4;

/// band が戻る時定数､秒｡指を離した瞬間からこの速さで縮む｡
const PULL_TAU_S: f32 = 0.08;

/// 目標や band の残りがこれを切ったら吸着して終わりにする距離､px｡
/// 指数関数はそれ自体ではゼロに届かない｡
const SNAP_PX: f32 = 0.5;

/// 手動 scroll の状態｡offset は gpui のもので､最上部が 0､下へ行くほど
/// 負に大きく､`floor` (`-max_offset`) が末尾｡delta も gpui と同じ向きで､
/// 正が最上部側へ (内容が下へ) 動く｡
#[derive(Debug, Default)]
pub(super) struct Scroller {
    /// ホイールが向かっている offset｡`None` なら追従中ではない｡
    target: Option<f32>,
    /// 端を越えて引いた生の量､px｡正は最上部を越えて下へ引いた､負は
    /// 末尾を越えて上へ引いた｡見た目は [`Self::shift`] が丸める｡
    pull: f32,
    /// trackpad に指が触れているか (`Started` から `Ended` まで)｡触れて
    /// いるあいだ band は戻らない｡
    touching: bool,
    /// 直前にこのモデルが一覧に置いた offset｡次に渡される offset がここ
    /// から [`GRAB_PX`] より離れていれば､誰か他の人が一覧を飛ばしたと
    /// いうことで､古い目標へ引き戻さないよう目標も band も捨てる｡
    placed: Option<f32>,
}

/// [`Scroller::step`] が 1 フレームぶん決めたもの｡
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Motion {
    /// 一覧に置く offset｡`floor..=0` に収まっている｡
    pub offset: f32,
    /// 一覧の見た目をずらす量､px｡正で下へ (最上部の band)､負で上へ
    /// (末尾の band)｡端に触れていなければ 0｡
    pub shift: f32,
    /// もう動くものが無い — ループはここで止まってよい｡
    pub done: bool,
}

impl Scroller {
    /// ホイールのティック (#175)｡`delta` は px に直した後の値｡目標に
    /// 積み上げるだけで､位置は次の [`Self::step`] から寄っていく｡端を
    /// 越えたぶんは band へ｡
    pub(super) fn wheel(&mut self, offset: f32, floor: f32, delta: f32) {
        self.resync(offset);
        let want = self.target.unwrap_or(offset) + delta;
        let (clamped, excess) = clamp_with_excess(want, floor);
        self.pull += excess * WHEEL_PULL;
        self.target = Some(clamped);
    }

    /// trackpad の 1 event (#175)｡OS が決めた距離をそのまま足し､今すぐ
    /// 置くべき offset を返す｡ホイールの目標があれば捨てる — 指が触れた
    /// 一覧は指のものだ｡band が伸びているときの反対向きの入力は､まず
    /// band を縮めてから残りで一覧を動かす｡
    pub(super) fn pan(&mut self, offset: f32, floor: f32, delta: f32, phase: TouchPhase) -> f32 {
        self.resync(offset);
        self.target = None;
        match phase {
            TouchPhase::Started => self.touching = true,
            TouchPhase::Ended => self.touching = false,
            TouchPhase::Moved => {}
        }
        let mut delta = delta;
        if self.is_pulled() && (self.pull > 0.) != (delta > 0.) {
            let remaining = self.pull + delta;
            if (remaining > 0.) == (self.pull > 0.) {
                self.pull = remaining;
                self.placed = Some(offset);
                return offset;
            }
            self.pull = 0.;
            delta = remaining;
        }
        let (clamped, excess) = clamp_with_excess(offset + delta, floor);
        self.pull += excess;
        self.placed = Some(clamped);
        clamped
    }

    /// `dt_s` 秒進める (#175)｡offset は今一覧にあるものを渡す — ループが
    /// 前のフレームで置いた値とは限らず､reload の補正が動かしていること
    /// もあるからだ｡そのときは [`Self::release`] と同じことをして
    /// `done` を返す｡
    pub(super) fn step(&mut self, offset: f32, floor: f32, dt_s: f32) -> Motion {
        self.resync(offset);
        let mut next = offset;
        if let Some(target) = self.target {
            let target = target.clamp(floor, 0.);
            let gap = target - offset;
            if gap.abs() <= SNAP_PX {
                next = target;
                self.target = None;
            } else {
                next = offset + gap * (1. - (-dt_s / WHEEL_TAU_S).exp());
                self.target = Some(target);
            }
        }
        if self.is_pulled() && !self.touching {
            self.pull *= (-dt_s / PULL_TAU_S).exp();
            if self.shift().abs() < SNAP_PX {
                self.pull = 0.;
            }
        }
        let offset = next.clamp(floor, 0.);
        self.placed = Some(offset);
        Motion {
            offset,
            shift: self.shift(),
            done: self.is_settled(),
        }
    }

    /// 一覧の見た目をずらす量､px — [`Motion::shift`] と同じもの｡
    pub(super) fn shift(&self) -> f32 {
        let magnitude = self.pull.abs();
        let shift = PULL_LIMIT_PX * (1. - 1. / (magnitude * PULL_STIFFNESS / PULL_LIMIT_PX + 1.));
        shift.copysign(self.pull)
    }

    /// 動かすものが何も残っていないか｡指が band を押さえていればまだだ｡
    pub(super) fn is_settled(&self) -> bool {
        self.target.is_none() && !self.is_pulled()
    }

    /// 目標も band も忘れる｡誰か他の人 (`ScrollToTop`､reload の補正) が
    /// 一覧を飛ばしたあとに呼ぶ — 古い目標へ引き戻してはいけない｡
    pub(super) fn release(&mut self) {
        self.target = None;
        self.pull = 0.;
        self.touching = false;
        self.placed = None;
    }

    fn is_pulled(&self) -> bool {
        self.pull.abs() > 0.
    }

    /// 渡された offset が自分の置いたところに無ければ [`Self::release`]｡
    fn resync(&mut self, offset: f32) {
        if let Some(placed) = self.placed
            && (offset - placed).abs() > GRAB_PX
        {
            self.release();
        }
    }
}

/// `want` を `floor..=0` に収めた値と､はみ出した量 (符号付き)｡
fn clamp_with_excess(want: f32, floor: f32) -> (f32, f32) {
    if want > 0. {
        (0., want)
    } else if want < floor {
        (floor, want - floor)
    } else {
        (want, 0.)
    }
}

/// [`Scroller`] を画面につなぐ半分 (#175)｡[`super::auto_refresh`] と同じ
/// 理由で同じファイルにある: event の横取りとフレームのループは 1 つの
/// 機構で､半分ずつ別の場所にあったらどちらも単独では読めない｡
impl TimelineView {
    /// timeline に重ねてホイールの event を横取りする､見えない canvas
    /// (#175)｡
    ///
    /// gpui の `Div` は自分のホイール handler を bubble phase に張る
    /// (モジュール doc を見よ)｡これより後に描かれる要素の listener は
    /// capture phase でそれより先に呼ばれるので､ここで
    /// `stop_propagation` すれば `Div` の handler には届かず､delta が
    /// 二重に足されることはない｡timeline 自身ではなく､band でずれない
    /// 外側の wrapper に `absolute` で重ねる: ずれた要素に張ると､跳ねて
    /// いる最中に露出した端の上ではどの handler にも届かなくなる｡
    ///
    /// hitbox は `Normal` なので､行のクリックは今までどおり行に届く —
    /// hit test は重なった hitbox をすべて集め､`BlockMouse` だけが下を
    /// 隠す｡
    pub(super) fn wheel_capture(cx: &mut Context<'_, Self>) -> impl IntoElement {
        let view = cx.weak_entity();
        gpui::canvas(
            |bounds, window, _cx| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
            move |_bounds, hitbox, window, _cx| {
                // `Div` が `Lines` に掛けるのと同じ行の高さを､同じ描画の
                // 文脈で読む｡dispatch の時点で読むと root の text style に
                // なる｡
                let line_height = window.line_height();
                window.on_mouse_event(move |event: &gpui::ScrollWheelEvent, phase, window, cx| {
                    if phase != gpui::DispatchPhase::Capture || !hitbox.should_handle_scroll(window)
                    {
                        return;
                    }
                    cx.stop_propagation();
                    let _ = view.update(cx, |this, cx| this.on_wheel(event, line_height, cx));
                });
            },
        )
        .absolute()
        .inset_0()
    }

    /// 読み手のホイールか trackpad が動いた (#175)｡
    ///
    /// 何より先に glide を drop する: 読み手が触れた一覧は読み手のもので､
    /// 2 つの animation が offset を取り合ってはいけない｡glide 側の
    /// 「置いた場所に無い」検知はその裏の保険として残る｡
    ///
    /// `Pixels` (trackpad) は今すぐ置き､`Lines` (ホイール) は目標に積む —
    /// どちらが何をするかはモジュール doc｡keyboard や `Show New Posts` の
    /// 位置変更はここを通らない (`scroll_to_top_of_item` を直接呼ぶ) ので､
    /// ホイール用の滑らかさはそちらに掛からない｡
    pub(super) fn on_wheel(
        &mut self,
        event: &gpui::ScrollWheelEvent,
        line_height: gpui::Pixels,
        cx: &mut Context<'_, Self>,
    ) {
        self.glide = None;
        let offset = self.list_scroll.offset();
        let y = f32::from(offset.y);
        let floor = -f32::from(self.list_scroll.max_offset().height);
        match event.delta {
            gpui::ScrollDelta::Pixels(delta) => {
                let next = self
                    .scroller
                    .pan(y, floor, f32::from(delta.y), event.touch_phase);
                self.list_scroll.set_offset(gpui::point(offset.x, px(next)));
            }
            gpui::ScrollDelta::Lines(lines) => {
                self.scroller
                    .wheel(y, floor, lines.y * f32::from(line_height));
            }
        }
        self.drive_scroll(cx);
        cx.notify();
    }

    /// [`Scroller`] に動くものがあるあいだ､フレームごとに一覧を置き直す
    /// ループ (#175)｡すでに走っていれば何もしない — 入力のたびに
    /// ループを増やさない｡
    ///
    /// glide と同じく､時刻は壁時計ではなくフレームごとに [`FRAME_S`] を
    /// 足す｡終わったら自分でスロットを空け､次の入力が新しいループを
    /// 始められるようにする｡
    fn drive_scroll(&mut self, cx: &mut Context<'_, Self>) {
        if self.scroll_motion.is_some() || self.scroller.is_settled() {
            return;
        }
        self.scroll_motion = Some(cx.spawn(async move |this, cx| {
            let frame = Duration::from_secs_f32(FRAME_S);
            loop {
                cx.background_executor().timer(frame).await;
                // `Err` はウィンドウが消えたということ｡
                let Ok(done) = this.update(cx, |this, cx| {
                    let offset = this.list_scroll.offset();
                    let floor = -f32::from(this.list_scroll.max_offset().height);
                    let motion = this.scroller.step(f32::from(offset.y), floor, FRAME_S);
                    this.list_scroll
                        .set_offset(gpui::point(offset.x, px(motion.offset)));
                    cx.notify();
                    motion.done
                }) else {
                    return;
                };
                if done {
                    break;
                }
            }
            let _ = this.update(cx, |this, _| this.scroll_motion = None);
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TouchPhase;

    const FLOOR: f32 = -2_000.;

    /// 落ち着くまで 1 フレームずつ進め､最後の offset と要したフレーム数を
    /// 返す｡10 秒で落ち着かなければそれ自体が失敗だ｡
    fn settle(scroller: &mut Scroller, mut offset: f32, floor: f32) -> (f32, usize) {
        for frame in 1..=600 {
            let motion = scroller.step(offset, floor, FRAME_S);
            offset = motion.offset;
            if motion.done {
                return (offset, frame);
            }
        }
        unreachable!("ten seconds passed and the scroller never settled, offset {offset}");
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    // --- ホイール: 滑らかに､しかし正確に ---

    #[test]
    fn a_wheel_tick_ends_exactly_its_delta_away() {
        let mut scroller = Scroller::default();
        scroller.wheel(-500., FLOOR, -60.);
        let (offset, frames) = settle(&mut scroller, -500., FLOOR);
        assert!(
            close(offset, -560.),
            "one tick of 60px must land at -560, was {offset}"
        );
        assert!(
            frames <= 60,
            "a tick must settle within a second, took {frames} frames"
        );
        assert!(frames > 1, "a tick must not land in a single frame");
    }

    #[test]
    fn a_wheel_tick_moves_smoothly_and_never_turns_back() {
        let mut scroller = Scroller::default();
        scroller.wheel(-500., FLOOR, -60.);
        let first = scroller.step(-500., FLOOR, FRAME_S).offset;
        assert!(
            first < -500.,
            "the first frame must already move, was {first}"
        );
        assert!(
            first > -560.,
            "the first frame must not jump the whole way, was {first}"
        );
        let mut previous = first;
        loop {
            let motion = scroller.step(previous, FLOOR, FRAME_S);
            assert!(
                motion.offset <= previous,
                "the approach must be monotone: {} after {previous}",
                motion.offset
            );
            assert!(
                motion.offset >= -560.,
                "the approach must not overshoot, {}",
                motion.offset
            );
            previous = motion.offset;
            if motion.done {
                break;
            }
        }
    }

    // 立て続けのティックは目標に積み上がり､位置がまだ追いついていない
    // ぶんだけ 1 フレームの移動が大きくなる — それが「素早い入力は十分な
    // 距離を動く」の中身で､OS がすでに掛けている加速に係数を重ねはしない｡
    #[test]
    fn quick_wheel_ticks_accumulate_and_move_faster_per_frame() {
        let mut single = Scroller::default();
        single.wheel(-500., FLOOR, -60.);
        let single_first = -500. - single.step(-500., FLOOR, FRAME_S).offset;

        let mut rapid = Scroller::default();
        let mut offset = -500.;
        for _ in 0..3 {
            rapid.wheel(offset, FLOOR, -60.);
            offset = rapid.step(offset, FLOOR, FRAME_S).offset;
        }
        let before = offset;
        let after = rapid.step(offset, FLOOR, FRAME_S).offset;
        assert!(
            before - after > single_first,
            "three quick ticks must move faster per frame ({}) than one ({single_first})",
            before - after
        );
        let (end, _) = settle(&mut rapid, after, FLOOR);
        assert!(
            close(end, -680.),
            "three ticks of 60px must land at -680, was {end}"
        );
    }

    #[test]
    fn a_wheel_past_the_top_stops_at_the_top_and_bounces() {
        let mut scroller = Scroller::default();
        scroller.wheel(-20., FLOOR, 100.);
        let mut offset = -20.;
        let mut peak_shift = 0.0_f32;
        for _ in 0..600 {
            let motion = scroller.step(offset, FLOOR, FRAME_S);
            offset = motion.offset;
            assert!(
                offset <= 0.,
                "the list itself must never pass the top, {offset}"
            );
            peak_shift = peak_shift.max(motion.shift);
            if motion.done {
                break;
            }
        }
        assert!(
            close(offset, 0.),
            "the wheel must end at the top, was {offset}"
        );
        assert!(peak_shift > 0., "the excess must show as a bounce");
        assert!(
            close(scroller.shift(), 0.),
            "and the bounce must relax by itself"
        );
        assert!(scroller.is_settled(), "nothing left to animate");
    }

    #[test]
    fn a_wheel_past_the_bottom_stops_at_the_floor_and_bounces_the_other_way() {
        let mut scroller = Scroller::default();
        scroller.wheel(-1_990., FLOOR, -100.);
        let mut offset = -1_990.;
        let mut lowest_shift = 0.0_f32;
        for _ in 0..600 {
            let motion = scroller.step(offset, FLOOR, FRAME_S);
            offset = motion.offset;
            assert!(
                offset >= FLOOR,
                "the list must never pass the floor, {offset}"
            );
            lowest_shift = lowest_shift.min(motion.shift);
            if motion.done {
                break;
            }
        }
        assert!(
            close(offset, FLOOR),
            "the wheel must end at the floor, was {offset}"
        );
        assert!(
            lowest_shift < 0.,
            "past the bottom the content lifts, not drops"
        );
    }

    // #175: フレーム数や実行環境によって終了位置が大きく変わらない｡
    #[test]
    fn frame_rate_does_not_change_where_a_wheel_ends_or_how_it_gets_there() {
        let mut at_60hz = Scroller::default();
        at_60hz.wheel(-500., FLOOR, -200.);
        let mut offset_60 = -500.;
        for _ in 0..30 {
            offset_60 = at_60hz.step(offset_60, FLOOR, 1. / 60.).offset;
        }
        let mut at_30hz = Scroller::default();
        at_30hz.wheel(-500., FLOOR, -200.);
        let mut offset_30 = -500.;
        for _ in 0..15 {
            offset_30 = at_30hz.step(offset_30, FLOOR, 1. / 30.).offset;
        }
        assert!(
            (offset_60 - offset_30).abs() < 0.5,
            "half a second in, both rates must be at the same place: {offset_60} vs {offset_30}"
        );
        let (end_60, _) = settle(&mut at_60hz, offset_60, FLOOR);
        let (end_30, _) = settle(&mut at_30hz, offset_30, FLOOR);
        assert!(
            close(end_60, -700.) && close(end_30, -700.),
            "{end_60} / {end_30}"
        );
    }

    #[test]
    fn a_wheel_target_is_clamped_to_a_floor_that_moved_since() {
        let mut scroller = Scroller::default();
        scroller.wheel(-1_000., FLOOR, -900.);
        // "Load older" が失敗して一覧が縮んだ､あるいはウィンドウが伸びた｡
        let (offset, _) = settle(&mut scroller, -1_000., -1_500.);
        assert!(
            close(offset, -1_500.),
            "the target must not point past the new floor, {offset}"
        );
    }

    // --- trackpad: OS が決めた距離を､そのまま ---

    #[test]
    fn a_pan_moves_immediately_by_exactly_its_delta() {
        let mut scroller = Scroller::default();
        let offset = scroller.pan(-500., FLOOR, -30., TouchPhase::Moved);
        assert!(
            close(offset, -530.),
            "the OS already accelerated this delta, was {offset}"
        );
        assert!(
            scroller.is_settled(),
            "a pan inside the bounds leaves nothing to animate"
        );
    }

    #[test]
    fn a_pan_past_the_top_stretches_the_band_with_resistance_up_to_a_limit() {
        let mut scroller = Scroller::default();
        let offset = scroller.pan(0., FLOOR, 50., TouchPhase::Started);
        assert!(close(offset, 0.), "the list stays put, {offset}");
        let first = scroller.shift();
        assert!(
            first > 0. && first < 50.,
            "50px of pull shows as less than 50px, was {first}"
        );
        scroller.pan(0., FLOOR, 50., TouchPhase::Moved);
        let second = scroller.shift();
        assert!(
            second > first && second < first * 2.,
            "the second 50px must add less than the first: {first} then {second}"
        );
        scroller.pan(0., FLOOR, 100_000., TouchPhase::Moved);
        assert!(
            scroller.shift() <= PULL_LIMIT_PX,
            "the band has a limit, {}",
            scroller.shift()
        );
    }

    #[test]
    fn the_band_holds_under_a_finger_and_springs_back_once_it_lifts() {
        let mut scroller = Scroller::default();
        scroller.pan(0., FLOOR, 80., TouchPhase::Started);
        let held = scroller.shift();
        for _ in 0..60 {
            let motion = scroller.step(0., FLOOR, FRAME_S);
            assert!(
                close(motion.shift, held),
                "held: {} vs {held}",
                motion.shift
            );
            assert!(!motion.done, "a finger on the glass is not settled");
        }
        scroller.pan(0., FLOOR, 0., TouchPhase::Ended);
        let mut previous = held;
        let (offset, frames) = {
            let mut offset = 0.;
            let mut frames = 0;
            loop {
                frames += 1;
                let motion = scroller.step(offset, FLOOR, FRAME_S);
                offset = motion.offset;
                assert!(
                    motion.shift <= previous,
                    "spring-back is monotone: {}",
                    motion.shift
                );
                assert!(
                    motion.shift >= 0.,
                    "and never crosses zero: {}",
                    motion.shift
                );
                previous = motion.shift;
                if motion.done {
                    break (offset, frames);
                }
                assert!(frames < 600, "the band must relax within ten seconds");
            }
        };
        assert!(
            close(offset, 0.) && close(scroller.shift(), 0.),
            "back home: {offset}"
        );
        assert!(frames > 1, "a spring is not a snap");
    }

    #[test]
    fn pulling_back_releases_the_band_before_the_list_moves() {
        let mut scroller = Scroller::default();
        scroller.pan(0., FLOOR, 50., TouchPhase::Started);
        let stretched = scroller.shift();
        let offset = scroller.pan(0., FLOOR, -20., TouchPhase::Moved);
        assert!(close(offset, 0.), "20px back only eases the band, {offset}");
        assert!(
            scroller.shift() < stretched,
            "eased: {} < {stretched}",
            scroller.shift()
        );
        let offset = scroller.pan(0., FLOOR, -100., TouchPhase::Moved);
        assert!(
            close(scroller.shift(), 0.),
            "the band is slack, {}",
            scroller.shift()
        );
        assert!(
            close(offset, -70.),
            "the 70px left over scrolls the list, {offset}"
        );
    }

    #[test]
    fn a_pan_takes_over_from_a_wheel_in_flight() {
        let mut scroller = Scroller::default();
        scroller.wheel(-500., FLOOR, -300.);
        let mid = scroller.step(-500., FLOOR, FRAME_S).offset;
        let grabbed = scroller.pan(mid, FLOOR, 0., TouchPhase::Started);
        assert!(
            close(grabbed, mid),
            "a touch holds the list where it is, {grabbed}"
        );
        let motion = scroller.step(grabbed, FLOOR, FRAME_S);
        assert!(
            close(motion.offset, mid),
            "and the wheel's target is forgotten, {}",
            motion.offset
        );
    }

    // `ScrollToTop`､pill､reload の補正はどれもモデルを通さずに一覧を
    // 飛ばす｡次に見た offset が置いた場所に無ければ､古い目標へ引き戻さず
    // そこで止まる｡
    #[test]
    fn a_jump_made_by_someone_else_drops_the_target_where_it_landed() {
        let mut scroller = Scroller::default();
        scroller.wheel(-500., FLOOR, -300.);
        let moving = scroller.step(-500., FLOOR, FRAME_S);
        assert!(!moving.done, "still on its way to -800");
        let motion = scroller.step(0., FLOOR, FRAME_S);
        assert!(
            close(motion.offset, 0.),
            "the jump to the top stands, {}",
            motion.offset
        );
        assert!(motion.done, "and nothing pulls the reader back down");
    }

    #[test]
    fn release_forgets_both_the_target_and_the_band() {
        let mut scroller = Scroller::default();
        scroller.wheel(-500., FLOOR, -300.);
        scroller.pan(0., FLOOR, 50., TouchPhase::Started);
        scroller.release();
        assert!(
            scroller.is_settled(),
            "nothing may outlive a jump made by someone else"
        );
        assert!(close(scroller.shift(), 0.), "{}", scroller.shift());
    }
}
