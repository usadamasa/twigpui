//! ウィンドウの位置と大きさの記憶 (#211)｡
//!
//! 起動のたびにウィンドウを動かし直さずに済むよう､live なウィンドウの矩形を
//! 覚えて次の起動で開き直す｡覚えるのは矩形だけで､最大化やフルスクリーンで
//! あったことは覚えない — [`WindowBounds`] の 3 つの variant はどれも中身に
//! restore size を持つので､そこだけを取る｡次の起動はいつも
//! [`WindowBounds::Windowed`] で開く｡
//!
//! 保存値がどのディスプレイとも重ならないときは既定の中央配置へ落ちる｡
//! 外付けディスプレイを外した後に画面の外へウィンドウが開いて､掴む縁も
//! 見えないまま探すことになるのを防ぐ｡
//!
//! ファイルは [`crate::paths::Paths::window_state_file`]｡読みも書きも
//! live な起動だけで､fixture は塞いである
//! ([`crate::ui::Startup`] を見よ)｡

use anyhow::{Context as _, Result};
use gpui::{App, Bounds, Pixels, Point, Size, WindowBounds, px, size};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::paths::Paths;
use crate::ui::Startup;

/// 記憶が無いとき (初回起動､保存値が画面の外) にウィンドウを開く大きさ｡
const DEFAULT_SIZE: Size<Pixels> = size(px(560.0), px(820.0));

/// 保存されたウィンドウの矩形 (#211)｡
///
/// gpui の [`Bounds`] を直に serde へ載せず自前の struct にしてある｡
/// ディスクの形は gpui の版で変わってはならないし､この 4 つの数は
/// 論理ピクセルであるという以上の意味を持たない｡
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedBounds {
    /// 左上の x 座標 (論理ピクセル)｡複数ディスプレイでは負にもなる｡
    pub x: f32,
    /// 左上の y 座標 (論理ピクセル)｡
    pub y: f32,
    /// 幅 (論理ピクセル)｡
    pub width: f32,
    /// 高さ (論理ピクセル)｡
    pub height: f32,
}

impl SavedBounds {
    /// gpui の矩形へ戻す｡
    pub(crate) fn to_bounds(self) -> Bounds<Pixels> {
        Bounds {
            origin: Point {
                x: px(self.x),
                y: px(self.y),
            },
            size: size(px(self.width), px(self.height)),
        }
    }
}

impl From<WindowBounds> for SavedBounds {
    /// 3 つの variant すべてから中身の矩形を取る｡最大化とフルスクリーンの
    /// 中身は restore size — ウィンドウへ戻したときの矩形なので､次の起動で
    /// そのまま開ける｡
    fn from(bounds: WindowBounds) -> Self {
        let bounds = match bounds {
            WindowBounds::Windowed(bounds)
            | WindowBounds::Maximized(bounds)
            | WindowBounds::Fullscreen(bounds) => bounds,
        };
        Self {
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            width: f32::from(bounds.size.width),
            height: f32::from(bounds.size.height),
        }
    }
}

/// [`crate::paths::Paths::window_state_file`] の中身すべて｡
///
/// フィールドは 1 つだが､それでも struct にしてある — `sync::SyncState` や
/// `ui::list_picker::SelectionState` と同じ理由で､ウィンドウについて次に
/// 覚える価値のあるものが､ファイルの形を変えずに入る｡
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub(crate) struct WindowState {
    /// 最後に観測したウィンドウの矩形｡一度も観測していなければ `None`｡
    #[serde(default)]
    pub bounds: Option<SavedBounds>,
}

/// 覚えたウィンドウの状態を `path` から読み戻す｡
///
/// [`crate::ui::list_picker::load_selection`] と同じく失敗しない｡これを
/// 失う代償はウィンドウを一度動かすことなので､ファイルが無い・壊れている
/// 場合はウィンドウが開くのを止めるエラーではなく既定値になる｡
pub(crate) fn load(path: &Path) -> WindowState {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return WindowState::default();
    };
    serde_json::from_str(&contents).unwrap_or_default()
}

/// ウィンドウの状態を `path` へ書く｡
pub(crate) fn save(path: &Path, state: &WindowState) -> Result<()> {
    let json =
        serde_json::to_string_pretty(state).context("could not serialize the window state")?;
    std::fs::write(path, json).with_context(|| format!("could not write {}", path.display()))
}

/// 保存値をそのまま使ってよいかを決める純関数｡
///
/// `displays` のいずれかと重なる矩形だけを返す｡どれとも重ならなければ
/// `None` で､呼び出し側は既定の中央配置へ落ちる｡大きさが正でないもの
/// (0 や NaN) も `None` になる: [`Bounds::intersects`] は幅 0 の矩形を
/// ディスプレイの内側にあると答えるので､重なりだけでは掴めないウィンドウを
/// 弾けない｡
pub(crate) fn restore(
    saved: Option<&SavedBounds>,
    displays: &[Bounds<Pixels>],
) -> Option<Bounds<Pixels>> {
    let bounds = saved?.to_bounds();
    let has_size = bounds.size.width > px(0.0) && bounds.size.height > px(0.0);
    (has_size && displays.iter().any(|display| bounds.intersects(display))).then_some(bounds)
}

/// この起動でウィンドウを開く矩形 (#211)｡
///
/// live な起動だけが記憶を読む｡fixture は定義上毎回同じ画面であり
/// (`fixture-visual-check`)､前回の live 実行が残した矩形が撮る画面の大きさを
/// 変えられてはならない｡書き込み側も同じように塞いである｡
pub(crate) fn initial_bounds(paths: &Paths, startup: &Startup, cx: &App) -> Bounds<Pixels> {
    let remembered = match startup {
        Startup::Live => {
            let displays: Vec<Bounds<Pixels>> = cx
                .displays()
                .iter()
                .map(|display| display.bounds())
                .collect();
            restore(load(&paths.window_state_file()).bounds.as_ref(), &displays)
        }
        Startup::Fixture(_) => None,
    };
    match remembered {
        Some(bounds) => bounds,
        None => Bounds::centered(None, DEFAULT_SIZE, cx),
    }
}

#[cfg(test)]
mod tests {
    use super::{SavedBounds, WindowState, load, restore, save};
    use gpui::{Bounds, Pixels, Point, WindowBounds, px, size};

    /// 主ディスプレイのつもりの矩形｡原点は 0,0｡
    fn primary() -> Bounds<Pixels> {
        Bounds {
            origin: Point {
                x: px(0.0),
                y: px(0.0),
            },
            size: size(px(1440.0), px(900.0)),
        }
    }

    fn saved(x: f32, y: f32, width: f32, height: f32) -> SavedBounds {
        SavedBounds {
            x,
            y,
            width,
            height,
        }
    }

    fn scratch_file(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "twigpui-window-state-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("window.json")
    }

    #[test]
    fn restores_bounds_that_overlap_a_display() {
        let bounds = restore(Some(&saved(120.0, 80.0, 560.0, 820.0)), &[primary()]);
        assert_eq!(
            bounds,
            Some(Bounds {
                origin: Point {
                    x: px(120.0),
                    y: px(80.0)
                },
                size: size(px(560.0), px(820.0)),
            }),
            "bounds inside the display are usable as they are"
        );
    }

    #[test]
    fn drops_bounds_that_fall_entirely_off_every_display() {
        // 外付けを外した後の起動｡ここを通すと､ウィンドウは掴む縁も見えない
        // 場所に開く｡
        assert_eq!(
            restore(Some(&saved(3000.0, 400.0, 560.0, 820.0)), &[primary()]),
            None,
            "bounds beyond every display fall back to the centered default"
        );
    }

    #[test]
    fn restores_bounds_on_a_display_left_of_the_primary_one() {
        // 主ディスプレイの左に並べたサブディスプレイの原点は負になる｡
        let left = Bounds {
            origin: Point {
                x: px(-1920.0),
                y: px(0.0),
            },
            size: size(px(1920.0), px(1080.0)),
        };
        assert!(
            restore(
                Some(&saved(-1600.0, 200.0, 560.0, 820.0)),
                &[primary(), left]
            )
            .is_some(),
            "a negative origin is a second display, not an off-screen window"
        );
    }

    #[test]
    fn drops_bounds_with_no_size() {
        // `Bounds::intersects` は幅 0 の矩形をディスプレイの内側にあると
        // 答える｡重なりだけでは掴めないウィンドウを弾けない｡
        assert_eq!(
            restore(Some(&saved(100.0, 100.0, 0.0, 820.0)), &[primary()]),
            None,
            "a window with no width cannot be grabbed"
        );
    }

    #[test]
    fn has_nothing_to_restore_without_a_saved_rectangle() {
        assert_eq!(restore(None, &[primary()]), None);
    }

    #[test]
    fn has_nothing_to_restore_without_a_display() {
        assert_eq!(restore(Some(&saved(0.0, 0.0, 560.0, 820.0)), &[]), None);
    }

    #[test]
    fn takes_the_restore_size_out_of_every_window_bounds_variant() {
        // 最大化・フルスクリーンの状態は覚えない｡どの variant からも中身の
        // restore size だけを取るので､次の起動はいつも windowed で開く｡
        let bounds = Bounds {
            origin: Point {
                x: px(10.0),
                y: px(20.0),
            },
            size: size(px(560.0), px(820.0)),
        };
        let expected = SavedBounds {
            x: 10.0,
            y: 20.0,
            width: 560.0,
            height: 820.0,
        };
        for variant in [
            WindowBounds::Windowed(bounds),
            WindowBounds::Maximized(bounds),
            WindowBounds::Fullscreen(bounds),
        ] {
            assert_eq!(
                SavedBounds::from(variant),
                expected,
                "every variant carries the restore size"
            );
        }
    }

    #[test]
    fn a_missing_file_reads_as_no_memory() {
        let path = scratch_file("missing")
            .parent()
            .unwrap()
            .join("absent.json");
        assert_eq!(load(&path), WindowState::default());
    }

    #[test]
    fn a_corrupt_file_reads_as_no_memory() {
        let path = scratch_file("corrupt");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(
            load(&path),
            WindowState::default(),
            "a broken file must not stop the window from opening"
        );
    }

    #[test]
    fn what_save_writes_is_what_load_reads_back() {
        let path = scratch_file("roundtrip");
        let state = WindowState {
            bounds: Some(saved(120.0, 80.0, 560.0, 820.0)),
        };
        save(&path, &state).unwrap();
        assert_eq!(load(&path), state);
    }
}
