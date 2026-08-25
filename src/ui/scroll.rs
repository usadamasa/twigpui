//! 手で scroll したときの動き (#175): ホイールは目標へ滑らかに寄り､
//! trackpad は OS の言うとおりに動き､どちらも端では rubber band になる｡

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
        assert!(close(offset, -560.), "one tick of 60px must land at -560, was {offset}");
        assert!(frames <= 60, "a tick must settle within a second, took {frames} frames");
        assert!(frames > 1, "a tick must not land in a single frame");
    }

    #[test]
    fn a_wheel_tick_moves_smoothly_and_never_turns_back() {
        let mut scroller = Scroller::default();
        scroller.wheel(-500., FLOOR, -60.);
        let first = scroller.step(-500., FLOOR, FRAME_S).offset;
        assert!(first < -500., "the first frame must already move, was {first}");
        assert!(first > -560., "the first frame must not jump the whole way, was {first}");
        let mut previous = first;
        loop {
            let motion = scroller.step(previous, FLOOR, FRAME_S);
            assert!(
                motion.offset <= previous,
                "the approach must be monotone: {} after {previous}",
                motion.offset
            );
            assert!(motion.offset >= -560., "the approach must not overshoot, {}", motion.offset);
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
        assert!(close(end, -680.), "three ticks of 60px must land at -680, was {end}");
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
            assert!(offset <= 0., "the list itself must never pass the top, {offset}");
            peak_shift = peak_shift.max(motion.shift);
            if motion.done {
                break;
            }
        }
        assert!(close(offset, 0.), "the wheel must end at the top, was {offset}");
        assert!(peak_shift > 0., "the excess must show as a bounce");
        assert!(close(scroller.shift(), 0.), "and the bounce must relax by itself");
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
            assert!(offset >= FLOOR, "the list must never pass the floor, {offset}");
            lowest_shift = lowest_shift.min(motion.shift);
            if motion.done {
                break;
            }
        }
        assert!(close(offset, FLOOR), "the wheel must end at the floor, was {offset}");
        assert!(lowest_shift < 0., "past the bottom the content lifts, not drops");
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
        assert!(close(end_60, -700.) && close(end_30, -700.), "{end_60} / {end_30}");
    }

    #[test]
    fn a_wheel_target_is_clamped_to_a_floor_that_moved_since() {
        let mut scroller = Scroller::default();
        scroller.wheel(-1_000., FLOOR, -900.);
        // "Load older" が失敗して一覧が縮んだ､あるいはウィンドウが伸びた｡
        let (offset, _) = settle(&mut scroller, -1_000., -1_500.);
        assert!(close(offset, -1_500.), "the target must not point past the new floor, {offset}");
    }

    // --- trackpad: OS が決めた距離を､そのまま ---

    #[test]
    fn a_pan_moves_immediately_by_exactly_its_delta() {
        let mut scroller = Scroller::default();
        let offset = scroller.pan(-500., FLOOR, -30., TouchPhase::Moved);
        assert!(close(offset, -530.), "the OS already accelerated this delta, was {offset}");
        assert!(scroller.is_settled(), "a pan inside the bounds leaves nothing to animate");
    }

    #[test]
    fn a_pan_past_the_top_stretches_the_band_with_resistance_up_to_a_limit() {
        let mut scroller = Scroller::default();
        let offset = scroller.pan(0., FLOOR, 50., TouchPhase::Started);
        assert!(close(offset, 0.), "the list stays put, {offset}");
        let first = scroller.shift();
        assert!(first > 0. && first < 50., "50px of pull shows as less than 50px, was {first}");
        scroller.pan(0., FLOOR, 50., TouchPhase::Moved);
        let second = scroller.shift();
        assert!(
            second > first && second < first * 2.,
            "the second 50px must add less than the first: {first} then {second}"
        );
        scroller.pan(0., FLOOR, 100_000., TouchPhase::Moved);
        assert!(scroller.shift() <= PULL_LIMIT_PX, "the band has a limit, {}", scroller.shift());
    }

    #[test]
    fn the_band_holds_under_a_finger_and_springs_back_once_it_lifts() {
        let mut scroller = Scroller::default();
        scroller.pan(0., FLOOR, 80., TouchPhase::Started);
        let held = scroller.shift();
        for _ in 0..60 {
            let motion = scroller.step(0., FLOOR, FRAME_S);
            assert!(close(motion.shift, held), "held: {} vs {held}", motion.shift);
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
                assert!(motion.shift <= previous, "spring-back is monotone: {}", motion.shift);
                assert!(motion.shift >= 0., "and never crosses zero: {}", motion.shift);
                previous = motion.shift;
                if motion.done {
                    break (offset, frames);
                }
                assert!(frames < 600, "the band must relax within ten seconds");
            }
        };
        assert!(close(offset, 0.) && close(scroller.shift(), 0.), "back home: {offset}");
        assert!(frames > 1, "a spring is not a snap");
    }

    #[test]
    fn pulling_back_releases_the_band_before_the_list_moves() {
        let mut scroller = Scroller::default();
        scroller.pan(0., FLOOR, 50., TouchPhase::Started);
        let stretched = scroller.shift();
        let offset = scroller.pan(0., FLOOR, -20., TouchPhase::Moved);
        assert!(close(offset, 0.), "20px back only eases the band, {offset}");
        assert!(scroller.shift() < stretched, "eased: {} < {stretched}", scroller.shift());
        let offset = scroller.pan(0., FLOOR, -100., TouchPhase::Moved);
        assert!(close(scroller.shift(), 0.), "the band is slack, {}", scroller.shift());
        assert!(close(offset, -70.), "the 70px left over scrolls the list, {offset}");
    }

    #[test]
    fn a_pan_takes_over_from_a_wheel_in_flight() {
        let mut scroller = Scroller::default();
        scroller.wheel(-500., FLOOR, -300.);
        let mid = scroller.step(-500., FLOOR, FRAME_S).offset;
        let grabbed = scroller.pan(mid, FLOOR, 0., TouchPhase::Started);
        assert!(close(grabbed, mid), "a touch holds the list where it is, {grabbed}");
        let motion = scroller.step(grabbed, FLOOR, FRAME_S);
        assert!(close(motion.offset, mid), "and the wheel's target is forgotten, {}", motion.offset);
    }

    #[test]
    fn release_forgets_both_the_target_and_the_band() {
        let mut scroller = Scroller::default();
        scroller.wheel(-500., FLOOR, -300.);
        scroller.pan(0., FLOOR, 50., TouchPhase::Started);
        scroller.release();
        assert!(scroller.is_settled(), "nothing may outlive a jump made by someone else");
        assert!(close(scroller.shift(), 0.), "{}", scroller.shift());
    }
}
