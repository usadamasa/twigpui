//! fixture を window 無しで描いて PNG に書き出す (`--png`, #221)｡
//!
//! ## なぜ window を開かないのか
//!
//! 画面がロックされている間､macOS は `CVDisplayLink` の生成を拒む (-6661)｡
//! gpui は occluded な window の display link を張らないので､ロック中に開いた
//! window は 1 フレームも描かず､screen capture は真っ黒になる｡離席中に
//! 見た目を確かめる手段がそれで消える｡
//!
//! gpui の `HeadlessAppContext` は `TestPlatform` の window を本物の text
//! system と Metal で描き､`Scene` を直接テクスチャへ落とす｡display link も
//! window server も通らないので､ロックの有無に左右されない｡
//!
//! ## なぜ feature の裏なのか
//!
//! `HeadlessAppContext` も `gpui_platform::current_headless_renderer` も gpui の
//! `test-support` の内側にある｡`cargo test` なら dev-dependency で届くが､
//! libtest はテストを main thread で走らせない (`--test-threads=1` でも別
//! スレッド｡実測)｡そこから `MacPlatform::new` を呼ぶと "Mac platform not
//! created on main thread" で落ちる — 本物の CoreText の text system は
//! そこからしか取れない｡
//! main thread を持っているのは `main` だけなので､この経路は bin の中にあり､
//! `test-support` を本番のバイナリへ入れないために `headless-shot` feature の
//! 裏に置いてある (Cargo.toml を見よ)｡
//!
//! ## 呼び方
//!
//! ```sh
//! cargo run --features headless-shot -- \
//!     --fixture fixtures/timeline.json --png ./tmp/shot.png
//! ```
//!
//! `--width` で幅を変える (既定は本番の window と同じ 429px)｡

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use gpui::{AppContext as _, HeadlessAppContext, px, size};

use crate::ui::Startup;
use crate::{FetchPostArg, config, fetch_post_arg, fixture, paths, ui};

/// 撮る window の既定の幅 (px)｡本番の window がこの幅で､枠の予算もこれを
/// 門にしている｡`--width 560` で `window_state::DEFAULT_SIZE` の幅も撮れる｡
const DEFAULT_WIDTH: f32 = 429.0;

/// 撮る window の高さ (px)｡`window_state::DEFAULT_SIZE` と同じにしてある —
/// 行の折り返しと､画面に何行収まるかを本物と揃えるため｡
const HEIGHT: f32 = 820.0;

/// `--png <path>` があれば fixture を撮ってプロセスを終える｡無ければ
/// そのまま戻り､呼び出し元は window を開く経路へ進む｡
///
/// 終了コードを返さずここで終えるのは `main` の都合で､`--fetch-only` の
/// ような `exit(f(..))` の形にすると `main` が `too_many_lines` を越える｡
///
/// `--fixture` と組でしか動かない｡live の起動は window を開くだけで課金
/// されるので (`x-api-budget`)､撮るためにそれを走らせる道は用意しない｡
pub(crate) fn run_if_asked(args: &[String], config: &config::Config, paths: &paths::Paths) {
    let out = match fetch_post_arg(args, "--png") {
        FetchPostArg::Absent => return,
        FetchPostArg::MissingValue => {
            eprintln!("--png requires a path to write the PNG to.");
            std::process::exit(1);
        }
        FetchPostArg::Value(out) => out,
    };
    let FetchPostArg::Value(fixture_path) = fetch_post_arg(args, "--fixture") else {
        eprintln!("--png only runs with --fixture: a live window bills requests on startup.");
        std::process::exit(1);
    };
    let width = match width_of(args) {
        Ok(width) => width,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };

    match capture(
        Path::new(fixture_path),
        width,
        config.clone(),
        paths.clone(),
        Path::new(out),
    ) {
        Ok((pixel_width, pixel_height)) => {
            eprintln!("wrote {out} ({pixel_width}x{pixel_height} px)");
            std::process::exit(0);
        }
        Err(error) => {
            eprintln!("--png: {error:#}");
            std::process::exit(1);
        }
    }
}

/// `--width <px>` を読む｡無ければ [`DEFAULT_WIDTH`]｡
fn width_of(args: &[String]) -> Result<f32, String> {
    match fetch_post_arg(args, "--width") {
        FetchPostArg::Absent => Ok(DEFAULT_WIDTH),
        FetchPostArg::MissingValue => Err("--width requires a number of pixels.".to_string()),
        FetchPostArg::Value(text) => match text.parse::<f32>() {
            Ok(width) if width.is_finite() && width > 0. => Ok(width),
            _ => Err(format!(
                "--width: {text:?} is not a positive number of pixels."
            )),
        },
    }
}

/// fixture を 1 枚描いて `out` へ書き､書いた PNG の寸法を返す｡
///
/// window を組む道筋は `main` と同じ — 同じ key binding を登録し､同じ
/// `TimelineView` を `gpui_component::Root` に包む｡別の描画経路を持たせると､
/// 撮ったものが本物の window の証拠でなくなる｡
///
/// 描くのは 1 回では足りない｡1 回目でアバターと添付の読み込みが走り出し､
/// `run_until_parked` がそれを終わらせ､次の回がその画像を置いた画面を描く｡
fn capture(
    fixture_path: &Path,
    width: f32,
    config: config::Config,
    paths: paths::Paths,
    out: &Path,
) -> Result<(u32, u32)> {
    let loaded = fixture::load(fixture_path)?;
    let startup = Startup::Fixture(Box::new(loaded));

    // 本物の text system は platform から取る｡`current_platform(true)` は
    // headless なので window も NSApplication も作らない｡
    let platform = gpui_platform::current_platform(true);
    let mut cx = HeadlessAppContext::with_platform(
        platform.text_system(),
        Arc::new(crate::assets::Assets),
        gpui_platform::current_headless_renderer,
    );

    cx.update(crate::register_key_bindings);
    let window = cx
        .open_window(size(px(width), px(HEIGHT)), |window, cx| {
            let timeline = cx.new(|cx| ui::TimelineView::new(config, paths, startup, window, cx));
            cx.new(|cx| gpui_component::Root::new(timeline, window, cx))
        })
        .context("could not open the headless window")?;

    // 画像はディスクから読む｡`allow_parking` が無いと､その await で
    // `run_until_parked` が止まる｡
    //
    // 時計は 50ms ずつ 10 回進める｡sync の行などの fade (180ms) は tick ごとに
    // 次の timer を張るので､一度に進めても 1 段しか進まない｡合計 500ms は
    // `pending` が届く 5 秒より手前｡
    cx.allow_parking();
    for _ in 0..10 {
        cx.advance_clock(std::time::Duration::from_millis(50));
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            let _ = window.draw(cx);
        })
        .context("could not draw the headless window")?;
    }

    let image = cx
        .capture_screenshot(window.into())
        .context("could not capture the headless window")?;
    let dimensions = image.dimensions();
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    image
        .save(out)
        .with_context(|| format!("could not write {}", out.display()))?;
    Ok(dimensions)
}
