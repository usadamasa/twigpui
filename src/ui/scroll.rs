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
//! 足さない)｡`Lines` は目標へ積み上げ､位置は spring で目標へ寄る —
//! 小さな入力は小さく､立て続けの入力は位置が追いつかないぶんだけ速く
//! 動き､入力が止まれば減速して止まる｡どちらも端を越えたぶんは rubber
//! band に回し､指が離れるか入力が止まれば戻る｡
//!
//! # なぜ spring で､なぜ見た目の px を動かすのか
//!
//! 動かすものは 2 つ (ホイールの位置と band) あって､どちらも
//! [`harmonica::Spring`] の臨界減衰 — 速度を状態に持ち､静止から加速し､
//! 行きすぎない｡指数関数で寄せると 1 フレーム目がいちばん速く､目には
//! 弾かれたように映る｡
//!
//! band は生の引っ張り量ではなく**見た目の px** を spring で戻す｡最初の
//! 実装は逆で､生の量を減らして見た目は飽和する関数に通していた｡慣性の
//! event が数千 px 積み上げると､見た目はしばらく上限に貼りついたまま
//! 動かず — 端で「大きく出て､止まって､それから戻る」に見えた｡見た目を
//! 直接動かせば､戻る時間は伸びた距離 ([`PULL_LIMIT_PX`] 以下) だけで
//! 決まり､どれだけ積み上がっても変わらない｡
//!
//! band への入力も 2 通りに分かれる｡指が触れているあいだは**位置** —
//! 引いたぶんだけ伸び､離すまでそこにいる｡指が離れていれば **勢い** —
//! 端に当たった速さを spring の初速に渡して､あとは任せる｡慣性は 1 回の
//! 弾みが数十の event に分かれて届くので､その全部を band に足すと
//! 慣性が尽きるまで端に貼りつく｡受け取るのは最初の 1 回だけにする｡

use gpui::TouchPhase;
use harmonica::Spring;

// [`super::list_sync`] と同じく書き下す: ここが `ui` から借りるものは
// clippy の `wildcard_imports` が列挙できる程度に少ない｡
use super::{Context, Duration, IntoElement, Styled, TimelineView, px};

/// 画面上の長さ､px｡offset も band も delta もこれで､向きは gpui と同じ
/// (正が最上部側)｡
///
/// このモジュールの数はどれも `f32` で､px と px/s と rad/s と秒が同じ顔で
/// 並ぶ｡引数を 1 つ入れ替えても型は通り､ずれはコンパイラではなく画面に
/// 出る｡別名を付けて読み手に単位を渡し､並びが 3 つを越えるところ
/// ([`advance`]) は [`Spin`] と [`Drift`] にまとめて順番そのものを消す｡
type Px = f32;

/// 速さ､px/s｡spring が状態として持つ｡
type PxPerSecond = f32;

/// spring の固有角周波数､rad/s｡
type RadPerSecond = f32;

/// 減衰比｡1 で臨界減衰､下回ると行き過ぎてから戻る｡単位は無い｡
type Damping = f32;

/// 時間､秒｡
type Seconds = f32;

/// rubber band の硬さ｡引くほど渋くなる曲線の傾きを決める｡
type Stiffness = f32;

/// 入ってきた量のうち､どれだけを取るかの割合 (0..=1)｡
type Share = f32;

/// 静止した spring に初速を与えたときの､`速度 / 固有角周波数` に対する
/// 頂点の比｡減衰比だけで決まる｡
type PeakRatio = f32;

/// spring の性格 — 固有角周波数と減衰比の対｡どちらも `f32` なので､
/// [`advance`] の引数として並べず 1 つにまとめる｡
#[derive(Debug, Clone, Copy)]
struct Spin {
    frequency: RadPerSecond,
    damping: Damping,
}

/// spring の状態 — 位置と速度の対｡[`Spin`] と同じ理由で 1 つにする｡
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Drift {
    position: Px,
    speed: PxPerSecond,
}

/// アニメーションの 1 フレームの長さ､秒 — 60Hz のディスプレイに追随
/// する｡timer の間隔であり､[`Scroller::step`] へ渡す経過時間でもある｡
/// glide (#22) も同じ刻みで歩く｡
pub(super) const FRAME_S: Seconds = 0.016;

/// このモデルが置いた offset から一覧がどれだけ離れていたら､誰か他の人
/// (`ScrollToTop`､reload の補正､pill) が飛ばしたと読むか､px｡glide が
/// 読み手の手を検知するのと同じ閾値｡
const GRAB_PX: Px = 1.0;

/// ホイールが目標へ寄る spring｡60px のティックは 0.1 秒で 6 割､0.35 秒で
/// 目標に着く｡Chrome の smooth scrolling と同じ桁で､周波数を上げれば元の
/// 「飛ぶ」に戻り､下げれば入力に遅れて付いてくる｡
///
/// 減衰比は 1 を下回らせない: 目標は読み手が選んだ行き先なので､通り過ぎて
/// から戻ってはいけない｡
const WHEEL_SPIN: Spin = Spin {
    frequency: 20.,
    damping: 1.,
};

/// band が戻る spring｡上限まで伸びたところから 0.19 秒で home に着く —
/// ホイールの追従より速い｡端を突いたのは行き先を選んだ結果ではないので､
/// 見せたら早く畳む｡
///
/// 減衰比は臨界減衰を少しだけ下回らせる｡ちょうど 1 だと止まり方が固く
/// 見える｡頂点の 5% ぶん (上限まで伸びても 2px) だけ home を行き過ぎて
/// 戻る — 一覧を包む clip が無いので行が潜っては困るが､この幅なら行の
/// 高さのはるか下で､目には固さが取れたぶんだけが残る｡
const PULL_SPIN: Spin = Spin {
    frequency: 34.,
    damping: 0.68,
};

/// 端を越えたぶんの見た目の上限､px｡rubber band はこの先へは伸びない｡
/// 一覧は clip されず､伸びたぶんはウィンドウの地が出るだけなので
/// macOS の scroll view よりだいぶ控えめにする — 行 1 つぶんも空けば
/// 「これ以上は無い」は十分に伝わる｡
pub(super) const PULL_LIMIT_PX: Px = 40.;

/// rubber band の硬さ｡引いた量 `x` に対して見た目は
/// `LIMIT * (1 - 1 / (x * STIFFNESS / LIMIT + 1))` で､最初の数十 px は
/// ほぼこの比率で付いてきて､先へ行くほど渋くなる｡
const PULL_STIFFNESS: Stiffness = 0.55;

/// ホイールが端を突いたとき､余った距離のうち band に回す割合｡trackpad の
/// 引っ張りより控えめにする — ノッチ 1 つが 100px を越えることがあり､
/// そのまま回すと端に着くたびに大きく跳ねる｡
const WHEEL_PULL: Share = 0.25;

/// 指が触れていない入力の余りを速度に読み替える時間､秒｡「この 1 event が
/// 1 フレームぶんの距離を運んできた」と読む｡
const FLING_S: Seconds = FRAME_S;

/// 静止した spring に初速 `v` を与えたとき､見た目が伸びる頂点の
/// `v / frequency` に対する比 — `exp(-ζ · arccos ζ / √(1 - ζ²))`｡臨界減衰
/// なら `1/e` (0.368) で､減衰比を下げるほど大きくなる｡[`PULL_SPIN`] の
/// 0.68 でこの値になる｡
const PULL_PEAK_RATIO: PeakRatio = 0.466;

/// 端に当たった勢いの上限､px/s｡ここを越えなければ頂点も
/// [`PULL_LIMIT_PX`] を越えない｡
const PULL_SPEED_MAX: PxPerSecond = PULL_LIMIT_PX * PULL_SPIN.frequency / PULL_PEAK_RATIO;

/// 目標や band の残りがこれを切ったら吸着して終わりにする距離､px｡
/// spring はそれ自体ではゼロに届かない｡
const SNAP_PX: Px = 0.5;

/// 吸着してよい速さの上限､px/s｡距離だけで見ると､目標を高速で通り抜ける
/// フレームをたまたま掴んで急停止させてしまう｡
const SNAP_SPEED: PxPerSecond = 20.;

/// 手動 scroll の状態｡offset は gpui のもので､最上部が 0､下へ行くほど
/// 負に大きく､`floor` (`-max_offset`) が末尾｡delta も gpui と同じ向きで､
/// 正が最上部側へ (内容が下へ) 動く｡
#[derive(Debug, Default)]
pub(super) struct Scroller {
    /// ホイールが向かっている offset｡`None` なら追従中ではない｡
    target: Option<Px>,
    /// 目標へ寄る spring の速度､px/s｡位置は一覧が持っているのでここには
    /// 無い｡ティックをまたいで残るので､立て続けの入力は速度が積み上がった
    /// ところから続く｡
    speed: PxPerSecond,
    /// 端を越えたぶんの band｡`position` が見た目のずれ ([`Motion::shift`] と
    /// 同じもの) で､正は最上部を越えて下へ引いた､負は末尾を越えて上へ
    /// 引いた｡生の引っ張り量は [`pull_of`] が要るときに逆算する｡
    band: Drift,
    /// trackpad に指が触れているか (`Started` から `Ended` まで)｡触れて
    /// いるあいだ band は戻らない｡
    touching: bool,
    /// 今の慣性がもう端に当たったか｡1 回の慣性は数十の event に分かれて
    /// 届くので (`momentumPhase` が読めない)､弾ませるのは最初の 1 回だけ｡
    /// 指が触れ直せば､次の慣性のためにまた降ろす｡
    bounced: bool,
    /// 直前にこのモデルが一覧に置いた offset｡次に渡される offset がここ
    /// から [`GRAB_PX`] より離れていれば､誰か他の人が一覧を飛ばしたと
    /// いうことで､古い目標へ引き戻さないよう目標も band も捨てる｡
    placed: Option<Px>,
}

/// [`Scroller::step`] が 1 フレームぶん決めたもの｡
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Motion {
    /// 一覧に置く offset｡`floor..=0` に収まっている｡
    pub offset: Px,
    /// 一覧の見た目をずらす量､px｡正で下へ (最上部の band)､負で上へ
    /// (末尾の band)｡端に触れていなければ 0｡
    pub shift: Px,
    /// もう動くものが無い — ループはここで止まってよい｡
    pub done: bool,
}

impl Scroller {
    /// ホイールのティック (#175)｡`delta` は px に直した後の値｡目標に
    /// 積み上げるだけで､位置は次の [`Self::step`] から寄っていく｡端を
    /// 越えたぶんは band へ｡
    pub(super) fn wheel(&mut self, offset: Px, floor: Px, delta: Px) {
        self.resync(offset);
        let want = self.target.unwrap_or(offset) + delta;
        let (clamped, excess) = clamp_with_excess(want, floor);
        self.fling(excess * WHEEL_PULL);
        self.target = Some(clamped);
    }

    /// trackpad の 1 event (#175)｡OS が決めた距離をそのまま足し､今すぐ
    /// 置くべき offset を返す｡ホイールの目標があれば捨てる — 指が触れた
    /// 一覧は指のものだ｡band が伸びているときの反対向きの入力は､まず
    /// band を縮めてから残りで一覧を動かす｡
    pub(super) fn pan(&mut self, offset: Px, floor: Px, delta: Px, phase: TouchPhase) -> Px {
        self.resync(offset);
        self.target = None;
        // 速度も一緒に｡残すと､指を離したあとの最初のティックが前の
        // 勢いに引かれて逆へ跳ねる｡
        self.speed = 0.;
        match phase {
            TouchPhase::Started => {
                self.touching = true;
                self.bounced = false;
            }
            TouchPhase::Ended => self.touching = false,
            TouchPhase::Moved => {}
        }
        let mut delta = delta;
        if self.is_pulled() && (self.band.position > 0.) != (delta > 0.) {
            let pulled = pull_of(self.band.position);
            let remaining = pulled + delta;
            if (remaining > 0.) == (pulled > 0.) {
                self.band.position = shift_of(remaining);
                self.placed = Some(offset);
                return offset;
            }
            self.band = Drift::default();
            delta = remaining;
        }
        let (clamped, excess) = clamp_with_excess(offset + delta, floor);
        if self.touching {
            self.stretch(excess);
        } else if !self.bounced && excess.abs() > 0. {
            self.fling(excess);
            self.bounced = true;
        }
        self.placed = Some(clamped);
        clamped
    }

    /// `dt_s` 秒進める (#175)｡offset は今一覧にあるものを渡す — ループが
    /// 前のフレームで置いた値とは限らず､reload の補正が動かしていること
    /// もあるからだ｡そのときは [`Self::release`] と同じことをして
    /// `done` を返す｡
    pub(super) fn step(&mut self, offset: Px, floor: Px, dt_s: Seconds) -> Motion {
        self.resync(offset);
        let mut next = offset;
        if let Some(target) = self.target {
            let target = target.clamp(floor, 0.);
            let from = Drift {
                position: offset,
                speed: self.speed,
            };
            let moved = advance(dt_s, WHEEL_SPIN, from, target);
            if (target - moved.position).abs() <= SNAP_PX && moved.speed.abs() <= SNAP_SPEED {
                next = target;
                self.target = None;
                self.speed = 0.;
            } else {
                next = moved.position;
                self.speed = moved.speed;
                self.target = Some(target);
            }
        }
        if self.is_pulled() && !self.touching {
            let moved = advance(dt_s, PULL_SPIN, self.band, 0.);
            if moved.position.abs() <= SNAP_PX && moved.speed.abs() <= SNAP_SPEED {
                self.band = Drift::default();
            } else {
                self.band = Drift {
                    // 勢いを重ねて渡されたときの保険｡[`PULL_SPEED_MAX`] は
                    // 1 回ぶんしか見ていない｡
                    position: moved.position.clamp(-PULL_LIMIT_PX, PULL_LIMIT_PX),
                    speed: moved.speed,
                };
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
    pub(super) fn shift(&self) -> Px {
        self.band.position
    }

    /// 動かすものが何も残っていないか｡指が band を押さえていればまだだ｡
    pub(super) fn is_settled(&self) -> bool {
        self.target.is_none() && !self.is_pulled()
    }

    /// 目標も band も忘れる｡誰か他の人 (`ScrollToTop`､reload の補正) が
    /// 一覧を飛ばしたあとに呼ぶ — 古い目標へ引き戻してはいけない｡
    pub(super) fn release(&mut self) {
        self.target = None;
        self.speed = 0.;
        self.band = Drift::default();
        self.touching = false;
        self.bounced = false;
        self.placed = None;
    }

    /// 指が引っ張っているあいだの band｡生の引っ張り量を `raw` px ぶん
    /// 足して見た目を引き直す — 足すのは生の側なので､すでに伸びている
    /// ところへの入力は先へ行くほど渋くなる｡指が押さえている band は
    /// spring が動かさないので､速度は 0 のまま｡
    fn stretch(&mut self, raw: Px) {
        self.band.position = shift_of(pull_of(self.band.position) + raw);
    }

    /// 指が離れているときに端へ当たったぶん (慣性とホイール)｡位置ではなく
    /// **勢い**を渡し､そこから先は spring に任せる｡
    ///
    /// 位置に足していたときは､慣性の event が届き続けるかぎり band が
    /// 押されつづけ､端に貼りついたまま慣性が尽きるのを待っていた — 手には
    /// それが引っかかりになる｡勢いなら 1 度渡せば済み､
    /// `v / frequency * PULL_PEAK_RATIO` の高さまで伸びて自分で戻る｡速く
    /// 当たれば大きく､そっと当たれば小さく弾むのも､距離で決めていたときには
    /// 出せなかった｡
    ///
    /// すでに弾んでいるあいだの当たりは捨てる｡1 回の慣性は数十の event に
    /// 分かれて届くので､足し込めば位置に足していたのと同じ貼りつきに戻る｡
    fn fling(&mut self, excess: Px) {
        if self.is_pulled() {
            return;
        }
        self.band.speed = (excess / FLING_S).clamp(-PULL_SPEED_MAX, PULL_SPEED_MAX);
    }

    /// band に動くものがあるか｡伸びていなくても勢いを渡された直後は
    /// まだこれから伸びる｡
    fn is_pulled(&self) -> bool {
        self.band.position.abs() > 0. || self.band.speed.abs() > 0.
    }

    /// 渡された offset が自分の置いたところに無ければ [`Self::release`]｡
    fn resync(&mut self, offset: Px) {
        if let Some(placed) = self.placed
            && (offset - placed).abs() > GRAB_PX
        {
            self.release();
        }
    }
}

/// spring を `dt_s` 秒進め､新しい位置と速度を返す｡
///
/// 係数は 1 フレームぶんの閉じた形なので､刻みを半分にして 2 回呼んでも
/// 同じところへ着く — [`Scroller::step`] を 30Hz で回しても 60Hz と同じ
/// 位置になる根拠がここにある｡
///
/// [`harmonica`] は f64 で解く｡px は f32 で持っているので､戻すときに
/// 落ちる桁がある — 24bit の仮数は 1px の 1/100 万まで表せるので､
/// 画面の 1px には届かない｡
#[expect(
    clippy::cast_possible_truncation,
    reason = "a spring solved in f64 comes back to f32 pixels; the discarded \
              bits are far below what a display can show"
)]
fn advance(dt_s: Seconds, spin: Spin, from: Drift, home: Px) -> Drift {
    let spring = Spring::new(
        f64::from(dt_s),
        f64::from(spin.frequency),
        f64::from(spin.damping),
    );
    let (position, speed) = spring.update(
        f64::from(from.position),
        f64::from(from.speed),
        f64::from(home),
    );
    Drift {
        position: position as f32,
        speed: speed as f32,
    }
}

/// 生の引っ張り量から見た目のずれへ｡最初の数十 px はほぼ
/// [`PULL_STIFFNESS`] の比率で付いてきて､先へ行くほど渋くなり､
/// [`PULL_LIMIT_PX`] には届かない｡
fn shift_of(pull: Px) -> Px {
    let magnitude = pull.abs();
    let shift = PULL_LIMIT_PX * (1. - 1. / (magnitude * PULL_STIFFNESS / PULL_LIMIT_PX + 1.));
    shift.copysign(pull)
}

/// [`shift_of`] の逆｡band を伸ばし縮めする入力は生の側で足し引きするので､
/// 見た目からいったん戻す必要がある｡上限に貼りついた見た目を渡されても
/// 無限大にならないよう､ほんの手前で頭打ちにする｡
fn pull_of(shift: Px) -> Px {
    let magnitude = shift.abs().min(PULL_LIMIT_PX * 0.999);
    let pull = PULL_LIMIT_PX * magnitude / (PULL_STIFFNESS * (PULL_LIMIT_PX - magnitude));
    pull.copysign(shift)
}

/// `want` を `floor..=0` に収めた値と､はみ出した量 (符号付き)｡
fn clamp_with_excess(want: Px, floor: Px) -> (Px, Px) {
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
        // #206: 読み手が上へ戻れば､follow の countdown もそこまで進む｡
        self.note_scroll_position();
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
                    this.note_scroll_position();
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

    const FLOOR: Px = -2_000.;

    /// 落ち着くまで 1 フレームずつ進め､最後の offset と要したフレーム数を
    /// 返す｡10 秒で落ち着かなければそれ自体が失敗だ｡
    fn settle(scroller: &mut Scroller, mut offset: Px, floor: Px) -> (Px, usize) {
        for frame in 1..=600 {
            let motion = scroller.step(offset, floor, FRAME_S);
            offset = motion.offset;
            if motion.done {
                return (offset, frame);
            }
        }
        unreachable!("ten seconds passed and the scroller never settled, offset {offset}");
    }

    fn close(a: Px, b: Px) -> bool {
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
        // 戻りは減衰比を 1 から少し下げてあるので､home をわずかに行き
        // 過ぎてから収まる｡行き過ぎが行の高さに届けば先頭の行が composer
        // の下へ潜るので､そこは押さえる｡
        let overshoot_limit = -PULL_LIMIT_PX * 0.1;
        let (offset, frames) = {
            let mut offset = 0.;
            let mut frames = 0;
            loop {
                frames += 1;
                let motion = scroller.step(offset, FLOOR, FRAME_S);
                offset = motion.offset;
                assert!(
                    motion.shift <= held,
                    "the band never stretches further than the finger left it: {}",
                    motion.shift
                );
                assert!(
                    motion.shift >= overshoot_limit,
                    "and swings back past home by a hair at most: {}",
                    motion.shift
                );
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

    // 目標だけを捨てて速度を残すと､指のあとの最初のティックが下向きの
    // 勢いに引かれて逆へ跳ねる — spring は速度を状態に持つので､目標を
    // 捨てるときは速度も一緒に捨てる｡
    #[test]
    fn a_wheel_after_a_pan_starts_from_rest_and_never_lurches_backwards() {
        let mut scroller = Scroller::default();
        scroller.wheel(-500., FLOOR, -600.);
        let mut offset = -500.;
        for _ in 0..8 {
            offset = scroller.step(offset, FLOOR, FRAME_S).offset;
        }
        let grabbed = scroller.pan(offset, FLOOR, 0., TouchPhase::Started);
        scroller.pan(grabbed, FLOOR, 0., TouchPhase::Ended);
        scroller.wheel(grabbed, FLOOR, 60.);
        let motion = scroller.step(grabbed, FLOOR, FRAME_S);
        assert!(
            motion.offset > grabbed,
            "the tick asked to go up, not down: {} from {grabbed}",
            motion.offset
        );
        let (end, _) = settle(&mut scroller, motion.offset, FLOOR);
        assert!(
            close(end, grabbed + 60.),
            "and it lands exactly its delta away, {end} from {grabbed}"
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

    // --- 動きの手触り (#175 の再訪) ---
    //
    // 最初の実装は生の引っ張り量を指数関数で減らし､見た目はそれを飽和する
    // 関数に通していた｡飽和のせいで､慣性の event が積み上げた数千 px の
    // うち最初の何百 px を削るあいだ見た目は上限に貼りついたまま動かず､
    // 端で「大きく出て､しばらく止まって､それから戻る」に見えた｡
    // 見た目の px そのものを spring で戻せば､戻る時間は伸びた距離だけで
    // 決まり､どれだけ積み上がっても変わらない｡

    /// 落ち着くまでの 1 フレームごとの `shift` の減り方を集める｡
    fn relax(scroller: &mut Scroller) -> Vec<Px> {
        let mut previous = scroller.shift();
        let mut steps = Vec::new();
        for _ in 0..600 {
            let motion = scroller.step(0., FLOOR, FRAME_S);
            steps.push(previous - motion.shift);
            previous = motion.shift;
            if motion.done {
                break;
            }
        }
        steps
    }

    // 慣性の event はいくらでも積み上がる｡そこから指を離しても､戻りに
    // かかる時間は見た目の伸びぶんだけで決まる｡
    #[test]
    fn a_band_pulled_far_past_the_limit_still_relaxes_in_a_fifth_of_a_second() {
        let mut scroller = Scroller::default();
        scroller.pan(0., FLOOR, 100_000., TouchPhase::Started);
        scroller.pan(0., FLOOR, 0., TouchPhase::Ended);
        let frames = relax(&mut scroller).len();
        assert!(
            frames <= 14,
            "the band must be home within a fifth of a second, took {frames} frames"
        );
        assert!(close(scroller.shift(), 0.), "{}", scroller.shift());
    }

    // gpui は `momentumPhase` を読まないので､指を離したあとも OS の慣性は
    // `Moved` として届き続ける (モジュール doc)｡1 回の弾みが数十の event に
    // 分かれて届くということで､その全部を band に足すと慣性が尽きるまで
    // 端に貼りついたままになる — 手にはそれが引っかかりになる｡
    #[test]
    fn a_run_of_momentum_events_bounces_the_band_once_and_lets_it_come_home() {
        let mut scroller = Scroller::default();
        scroller.pan(0., FLOOR, 0., TouchPhase::Ended);
        let mut trace = Vec::new();
        let mut delta = 60.;
        for _ in 0..40 {
            scroller.pan(0., FLOOR, delta, TouchPhase::Moved);
            trace.push(scroller.step(0., FLOOR, FRAME_S).shift);
            delta *= 0.9;
        }
        let peak = trace.iter().copied().fold(0.0_f32, f32::max);
        let peaked_at = trace
            .iter()
            .position(|shift| close(*shift, peak))
            .unwrap_or(usize::MAX);
        assert!(
            peak > 0. && peak <= PULL_LIMIT_PX,
            "the run must show as a bounce inside the limit, {peak}"
        );
        assert!(
            peaked_at <= 6,
            "and top out at once, not ride the whole run: frame {peaked_at}"
        );
        let last = trace.last().copied().unwrap_or_default();
        assert!(
            last.abs() < 0.5,
            "the band must be home while the momentum still arrives, {last}"
        );
    }

    // 勢いの上限 [`PULL_SPEED_MAX`] は [`PULL_PEAK_RATIO`] から逆算して
    // いる｡比が過大なら弾みが上限を越え､過小なら上限まで届かない — どちら
    // も見て初めて分かる類のずれなので､両側から挟む｡
    #[test]
    fn the_hardest_bounce_fills_the_limit_without_passing_it() {
        let mut scroller = Scroller::default();
        scroller.pan(0., FLOOR, 0., TouchPhase::Ended);
        scroller.pan(0., FLOOR, 100_000., TouchPhase::Moved);
        let mut peak = 0.0_f32;
        for _ in 0..600 {
            let motion = scroller.step(0., FLOOR, FRAME_S);
            peak = peak.max(motion.shift);
            if motion.done {
                break;
            }
        }
        assert!(
            peak <= PULL_LIMIT_PX,
            "the hardest bounce must stay inside the limit, {peak}"
        );
        assert!(
            peak > PULL_LIMIT_PX * 0.9,
            "and must be worth the limit it was given, {peak}"
        );
    }

    // 離した瞬間に最高速で走り出すのは指数関数の癖で､目には弾かれたように
    // 映る｡spring は静止から加速するので､最初のフレームは山より小さい｡
    #[test]
    fn the_band_eases_out_of_the_pull_instead_of_bolting_on_the_first_frame() {
        let mut scroller = Scroller::default();
        scroller.pan(0., FLOOR, 120., TouchPhase::Started);
        scroller.pan(0., FLOOR, 0., TouchPhase::Ended);
        let steps = relax(&mut scroller);
        let first = steps.first().copied().unwrap_or_default();
        let peak = steps.iter().copied().fold(0.0_f32, f32::max);
        assert!(
            first < peak * 0.75,
            "the first frame ({first}) must be gentler than the fastest ({peak})"
        );
    }

    // ホイールも同じ｡目標へ寄る速さが 1 フレーム目にいきなり最大になると
    // ｢飛ぶ｣に戻る｡
    #[test]
    fn a_wheel_tick_eases_in_instead_of_lurching_on_its_first_frame() {
        let mut scroller = Scroller::default();
        scroller.wheel(-500., FLOOR, -200.);
        let mut offset = -500.;
        let mut steps = Vec::new();
        for _ in 0..600 {
            let motion = scroller.step(offset, FLOOR, FRAME_S);
            steps.push(offset - motion.offset);
            offset = motion.offset;
            if motion.done {
                break;
            }
        }
        let first = steps.first().copied().unwrap_or_default();
        let peak = steps.iter().copied().fold(0.0_f32, f32::max);
        assert!(
            first < peak * 0.75,
            "the first frame ({first}) must be gentler than the fastest ({peak})"
        );
    }
}
