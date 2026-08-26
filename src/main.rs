//! twigpui — gpui で書いた macOS 専用､開発用途のみの X (Twitter)
//! タイムラインビューア｡
//!
//! この binary crate がアプリケーション全体である: `ui` がウィンドウを描き､
//! `menu` がキーバインドとメニューバーを持ち､`x_api` が X と話し､
//! `cache`/`usage`/`rate_limit` がリクエスト単位の課金を抑え､そしてこの
//! モジュールがエントリポイントと､headless な `--fetch-only` /
//! `--fetch-post` / `--usage` の経路を担う｡

// テストの中の `unwrap` は読める assertion であって潜んだ panic ではない —
// Cargo.toml の厳しい lint は実際に出荷されるコードに向いている｡#47 は同じ
// 理屈をもう三つに広げる: fixture をリテラルの index で引くこと､リテラルの
// 文字列を slice すること､到達してはならない `match` の腕での `panic!` は､
// テストの中ではどれも *assertion* である｡panic するテストは失敗したテスト
// であり､それは仕組みが働いている証拠だ — これらの lint が `src/` で見つける
// ためにある､リモート入力に潜んだクラッシュではない｡
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic
    )
)]

mod activity;
mod assets;
mod avatar;
mod browser;
mod cache;
mod compose;
mod config;
mod fixture;
mod image_cache;
mod like;
mod log;
mod menu;
mod oauth;
mod paths;
mod perf;
mod profile;
mod rate_limit;
mod repost;
mod sync;
mod theme;
mod thread;
mod toggle;
mod ui;
mod url;
mod usage;
mod x_api;

use std::collections::HashSet;
use std::io::IsTerminal as _;

use gpui::{
    AppContext as _, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};

fn main() {
    let config = match config::Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            report_startup_error(&format!("{error:#}"));
            std::process::exit(1);
        }
    };
    // `Config::from_env` がこれらのディレクトリを既に解決して作っている —
    // ここで計算し直せば (安価で､env var に対して純粋)､OAuth トークン
    // ストアのためだけに `Paths` を `Config` に通さずに済む｡
    let paths = match paths::Paths::from_env() {
        Ok(paths) => paths,
        Err(error) => {
            report_startup_error(&format!("{error:#}"));
            std::process::exit(1);
        }
    };

    // ウィンドウ無しで認証情報と接続性を確かめるための headless な取得｡
    if std::env::args().any(|arg| arg == "--fetch-only") {
        std::process::exit(fetch_only(&config, &paths));
    }

    // headless な単一 post の参照 (#42): `--fetch-post <id-or-url>[,...]`｡
    // このフラグは在るかどうかだけでなく後続の値が要るので､上の boolean な
    // フラグのような `std::env::args().any(...)` ではなく､一度 `Vec` に
    // 集めている｡
    let args: Vec<String> = std::env::args().collect();
    match fetch_post_arg(&args, "--fetch-post") {
        FetchPostArg::Absent => {}
        FetchPostArg::Value(arg) => std::process::exit(fetch_post(&config, &paths, arg)),
        FetchPostArg::MissingValue => {
            eprintln!(
                "--fetch-post requires a value: a post id, a status URL \
                 (https://x.com/<user>/status/<id>), or a comma-separated \
                 list of either."
            );
            std::process::exit(1);
        }
    }

    // #163: `--sync-list` はこのアプリがフォローしているアカウントを､設定
    // された List へミラーする｡既定では dry-run で — 両側を読み､plan を
    // 書いて印字する — `--apply` で書き込みを送り､`--prune` で削除も含める｡
    // 上のどのフラグとも同じく headless である: 両側の読み取りがアカウント
    // 単位で課金されるため､これをタイマーで走らせてはならないと #163 自身の
    // 設計が定めている｡
    if args.iter().any(|arg| arg == "--sync-list") {
        let request = sync::Request {
            apply: args.iter().any(|arg| arg == "--apply"),
            prune: args.iter().any(|arg| arg == "--prune"),
        };
        std::process::exit(sync::run_cli(&config, &paths, request));
    }

    // ヘッダが見せるのと同じ usage の数字を JSON で印字する (#18)｡読むのは
    // `state_dir` 配下の `usage.json` だけで — ネットワーク呼び出しは無い
    // ので､クレジットが尽きている間を含めていつ走らせても安全である｡
    if std::env::args().any(|arg| arg == "--usage") {
        std::process::exit(usage_only(&config, &paths));
    }

    // #146: `--fixture <path>` はアカウントではなくファイルからウィンドウを
    // 埋める｡ウィンドウが開く前のここで解決するので､fixture が無かったり
    // 壊れていたりしたときは､説明の無い空のウィンドウとしてではなく､打ち
    // 込んだ端末の上で失敗する｡
    let startup = match fetch_post_arg(&args, "--fixture") {
        FetchPostArg::Absent => ui::Startup::Live,
        FetchPostArg::Value(path) => match fixture::load(std::path::Path::new(path)) {
            Ok(loaded) => ui::Startup::Fixture(Box::new(loaded)),
            Err(error) => {
                eprintln!("--fixture: {error:#}");
                std::process::exit(1);
            }
        },
        FetchPostArg::MissingValue => {
            eprintln!("--fixture requires a path to a fixture JSON file.");
            std::process::exit(1);
        }
    };

    // `--perf <seconds>`: このプロセス自身の RSS と CPU を測る (`perf.rs`)｡
    let perf = perf::arm(&args, &startup).unwrap_or_else(|message| {
        eprintln!("{message}");
        std::process::exit(1);
    });

    // #49: ここから先､知る価値のあることは stderr だけでなくログファイルへ
    // も出す — Finder から起動した `.app` では stderr がどこにも行かないので
    // (#40, #45)､それが唯一の記録になる｡
    log::init(&paths, config.log_level);
    log::info(&startup_banner(
        env!("CARGO_PKG_VERSION"),
        env!("TWIGPUI_GIT_HASH"),
    ));

    // #95: ツールバーが描くアイコン｡gpui は `svg()` のパスをこれを通して
    // 解決するので､これが無いとどのアイコンも何も描かれない｡
    Application::new()
        .with_assets(assets::Assets)
        .run(move |cx| {
            // #38: gpui-component のグローバルなキーバインド､テーマ､その他
            // App 単位の状態を登録する (それ自身の `init` の doc を見よ) —
            // そのウィジェット (composer のテキスト入力) を構築できるように
            // なる前に､一度だけ必要である｡
            gpui_component::init(cx);
            // #58: twigpui 自身のキーバインドを､同じ理由で gpui-component の
            // ものと並べて登録する — それらへ dispatch するウィンドウが存在
            // する前に､一度だけ｡
            menu::init(cx);
            // #99: メニューバーと､その裏にあってウィンドウが持てない唯一の
            // アクション｡メニュー項目はフォーカスのあるウィンドウへ dispatch
            // するので Reload/New Post/Submit Post は timeline 自身のハンドラ
            // へ届く｡だが quit はウィンドウが一つもフォーカスされていなくても
            // 働かねばならず､それを登録するのが `App::on_action` で､root に
            // 置いたハンドラではそうならない｡どちらもウィンドウが開く前に走る:
            // 下でウィンドウを開けそこねたアプリにも､メニューバーはある｡
            cx.on_action(|_: &menu::Quit, cx| cx.quit());
            cx.set_menus(menu::menus());
            // #139: 最後のウィンドウを閉じるとアプリが終わる｡gpui は独自に
            // プロセスを生かし続ける — もう一枚ウィンドウを頼めるアプリには
            // 正しいが､このアプリには誤りで､`cmd-w` は画面に何も無いまま
            // プロセスを走らせ､`cmd-q` だけがそこへ届く状態を残していた｡
            // 決め打ちせず数えているので､二枚目のウィンドウがあっても最後の
            // 一枚が出るまでは終わらない｡
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let bounds = Bounds::centered(None, size(px(560.0), px(820.0)), cx);
            // #169: タイトルがどのインストールかを名乗るので､開発用の
            // ウィンドウと本物がウィンドウ一覧で見分けられる — そして
            // スクリーンショットをタイトルで狙える｡`Profile::current()` を
            // 直に呼ばず `paths` から読むので､タイトルが一方のプロファイル
            // を名乗りながら読んでいるファイルはもう一方､とはなり得ない｡
            let title = paths.profile().window_title();
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(title.into()),
                    ..Default::default()
                }),
                ..Default::default()
            };

            // fork した gpui の patch (Cargo.toml の `[patch.crates-io]`):
            // fixture の window は画面がロックされていても描き続ける｡
            // `open_window` の前でなければならない理由は
            // `ui::Startup::draws_while_occluded` を見よ｡
            gpui::set_draw_while_occluded(startup.draws_while_occluded());

            let opened = cx.open_window(options, |window, cx| {
                let timeline =
                    cx.new(|cx| ui::TimelineView::new(config, paths, startup, window, cx));
                // #38: gpui-component のウィジェットはウィンドウの root を
                // 遡り､そこに `Root` があることを期待する — テキスト入力は
                // 最初の render でそれを尋ね､root view が他の何かなら
                // `Root::read` はそのまま panic する｡timeline を直に root に
                // すると､起動時にアプリが落ちた｡
                cx.new(|cx| gpui_component::Root::new(timeline, window, cx))
            });
            if let Err(error) = opened {
                log::error(&format!("could not open the window: {error:#}"));
                cx.quit();
                return;
            }

            cx.activate(true);
            perf::start(cx, perf);
        });
}

/// ログの 1 行目 (#231)｡`starting twigpui 0.1.0 (abc1234)`｡
///
/// version だけでは足りない｡`0.1.0` は当分動かないし､`.app` は
/// `scripts/build-app-bundle.sh` で組み直され､手元には worktree が何本も
/// 並ぶ｡どのビルドのログを読んでいるかを決めるのは commit のほうだ｡
///
/// `hash` は `build.rs` が埋める｡git が使えなければ `unknown`､追跡中の
/// ファイルに未コミットの差分があれば `abc1234-dirty` になる｡
fn startup_banner(version: &str, hash: &str) -> String {
    format!("starting twigpui {version} ({hash})")
}

/// 致命的な起動エラーを stderr へ､そして — stderr を読む端末が付いて
/// いないときは — ネイティブのアラートダイアログとしても報告する｡
///
/// `cargo run` には素の `eprintln!` で足りるが､Finder/Spotlight/Dock から
/// 起動した `.app` (#40) には端末が無い: stderr は誰の目にも触れない先へ
/// 行くので､そうでなければプロセスは目に見える症状も無く消える｡それこそ
/// #40 が挙げる「説明の無い空白ウィンドウ」の失敗である｡`gpui` のウィンドウ
/// ではなく `osascript` を使うのは､これが `Application::new()` の *前* に
/// 走るからだ — `gpui` のアラートを吊るす window server への接続はまだ無い
/// が､`osascript` は普通の macOS アプリであること以上をこのプロセスに
/// 求めない｡
///
/// メッセージは常に `config.toml` の在り処を名指しする｡最も多い原因
/// (認証情報が何も設定されていない) に対する具体的な直し方がそれだから
/// である — README の "Setup" と "`config.toml`" の節を見よ｡
fn report_startup_error(message: &str) {
    let config_hint = paths::Paths::from_env().map_or_else(
        |_| "~/.config/twigpui/config.toml".to_string(),
        |paths| paths.settings_file().display().to_string(),
    );
    let full_message = format!(
        "twigpui could not start: {message} Configuration lives in \
         {config_hint} (non-secret settings, e.g. oauth_client_id) or the \
         X_OAUTH_CLIENT_ID environment variable — see the \
         README's Setup section."
    );
    eprintln!("configuration error: {full_message}");

    if std::io::stderr().is_terminal() {
        // 端末が付いている (`cargo run` やバイナリ直起動) — 上の eprintln!
        // は既に見えているので､その上にダイアログを重ねてもノイズにしか
        // ならない｡
        return;
    }

    let script = format!(
        "display alert \"twigpui\" message {} as critical",
        applescript_quote(&full_message)
    );
    // best-effort: `osascript` 自体が無かったり失敗したりしたら､エラーを
    // 表に出すためにこのプロセスができることはもう無い｡それでも､出せない
    // ダイアログを待ってぶら下がるのではなく､非ゼロで終了せねばならない｡
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .status();
}

/// `text` を二重引用符で囲む `AppleScript` の文字列リテラル用に escape する｡
/// `AppleScript` に `\n` の escape は無く､文字列リテラル内の生の改行は構文
/// エラーになるので､埋め込まれた改行は escape せずスペースへ潰す｡
fn applescript_quote(text: &str) -> String {
    let flattened = text.replace(['\n', '\r'], " ");
    let escaped = flattened.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// ウィンドウが起動時にするのと同じやり方でトークンを解決する｡ただし
/// ブラウザは決して開かない — `--fetch-only` は headless に走らせるための
/// もので､例えば認証情報がまだ有効かを確かめるスクリプトの中で使う｡
///
/// ウィンドウを開くのと違い､これは常に最低一回の API リクエストを費やす:
/// README に言うそのすべての眼目は認証情報と接続性が実際に働くことを確かめ
/// ることで､キャッシュだけの描画では検証できない｡それでも素の取得ではなく
/// `cache::reload` を通すので､user id がキャッシュにあれば二回でなく一回の
/// リクエストで済み､増分の `since_id` がレスポンスを小さく保つ — どちらが
/// 起きたかは下の eprintln! を見よ｡
fn fetch_only(config: &config::Config, paths: &paths::Paths) -> i32 {
    let resolution = match oauth::resolve_credential(config, paths, oauth::unix_now()) {
        Ok(resolution) => resolution,
        Err(error) => {
            eprintln!("could not resolve a credential: {error:#}");
            return 1;
        }
    };
    // #54: 更新できなかった保存済みセッションは､ここでも声に出して言う価値
    // がある — headless な実行には､代わりにそれを見せるヘッダのバナーが
    // 無い｡
    if let Some(demotion) = &resolution.demotion {
        eprintln!("{}", oauth::describe_demotion(demotion));
    }
    let Some(credential) = resolution.credential else {
        // #33: 今や認証情報を得る道はサインインだけで､それにはブラウザが
        // 要る — headless な実行が開いてよいものではない｡
        eprintln!(
            "no signed-in session is available. Run twigpui without --fetch-only and click \
             \"Sign in with X\" once; this flag reuses the session that leaves behind."
        );
        return 1;
    };

    let client = x_api::XClient::renewing(credential.session);
    match cache::reload(
        paths,
        &client,
        &config.target_username,
        config.max_results,
        oauth::unix_now(),
    ) {
        Ok(cache::Reloaded {
            items,
            user_id_cache_hit,
        }) => {
            eprintln!(
                "cache: user id {} ({} request{} spent)",
                if user_id_cache_hit {
                    "cache hit"
                } else {
                    "cache miss, resolved via the API"
                },
                if user_id_cache_hit { 1 } else { 2 },
                if user_id_cache_hit { "" } else { "s" }
            );
            println!("{} post(s) from @{}", items.len(), config.target_username);
            for item in &items {
                println!(
                    "\n[{}] {} (@{})\n{}",
                    item.created_at.as_deref().unwrap_or("-"),
                    item.author_name,
                    item.author_username,
                    item.text
                );
            }
            0
        }
        Err(error) => {
            eprintln!("fetch failed: {error:#}");
            1
        }
    }
}

/// argv の中に `--fetch-post` (#42) を探した結果が着地しうる三つの状態:
/// フラグが現れなかった､現れたが後ろに何も無かった — 例えば
/// `twigpui --fetch-post` が最後の引数だった場合 — あるいは値を伴って現れた｡
/// `Option<Option<&str>>` ではなく名前付きの enum にしてある: clippy 自身の
/// `option_option` lint が入れ子の形を拒むのは､まさに三つの状態は `None` /
/// `Some(None)` / `Some(Some(_))` よりも三つの variant として読むほうが
/// 分かりやすいからだ｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FetchPostArg<'a> {
    Absent,
    MissingValue,
    Value(&'a str),
}

/// `args` の中に `flag` を見つけ､その後ろに続くものを [`FetchPostArg`] と
/// して分類する｡フラグ名について総称にしてあるのは､`--fetch-post` という
/// リテラルの文字列に依存せずテストが動かせるようにするためだけで､この
/// クレートの他のどこも別のフラグでは呼ばない｡
fn fetch_post_arg<'a>(args: &'a [String], flag: &str) -> FetchPostArg<'a> {
    let Some(index) = args.iter().position(|arg| arg == flag) else {
        return FetchPostArg::Absent;
    };
    // `saturating_add` (#47): argv が `usize::MAX` の長さになることはあり
    // 得ないが､saturating の形にしておけば､それは overflow ではなく何も
    // 見つからない参照になる｡
    match args.get(index.saturating_add(1)) {
        Some(value) => FetchPostArg::Value(value.as_str()),
        None => FetchPostArg::MissingValue,
    }
}

/// `--fetch-post` (#42) のトークン一つから post id を取り出す: id そのもの
/// (trim すると全部数字)､あるいは status URL の id 部分 — `.../status/<id>`
/// に､それ自体が数字でない何かが続く形 (`/photo/1`､`?s=...` のクエリ文字列､
/// あるいは何も無し)｡`x.com` と `twitter.com` のどちらのリンクでも働く｡
/// 両者は同じ `/status/<id>` のパス形状を共有しているからで､scheme と host
/// は一切見ない｡これはどのトークンなら API へ送る価値があるかを整えるだけで
/// — id が実在するかの検証はしない｡それはリクエスト自体が本当の検査である｡
/// それ以外は空入力 (例えば引数に紛れたカンマ) も含めて `None`｡
fn extract_post_id(token: &str) -> Option<String> {
    let trimmed = token.trim();
    if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Some(trimmed.to_string());
    }
    let after_marker = trimmed.split("/status/").nth(1)?;
    let id: String = after_marker
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if id.is_empty() { None } else { Some(id) }
}

/// `--fetch-post` (#42) の引数を､それが名指す post id へ parse する — 裸の
/// id､完全な status URL､あるいはそのどちらかをカンマで並べた混合｡issue 自身
/// の "決めること" の表が､id だけでなく両方を受け付けると決めている (実際に
/// 手元にあるのはたいていリンクだから)｡各トークンは [`extract_post_id`] が
/// 解決する｡認識できない最初の一つは黙って落とさず parse 全体を失敗させる｡
/// 想定より短い id の列は､ここが一度だけ撃つリクエストが頼まれたより少ない
/// post しか対象にしない結果を､理由の手がかり無しに招くからだ｡
fn parse_post_ids(arg: &str) -> Result<Vec<String>, String> {
    let ids: Vec<String> = arg
        .split(',')
        .map(|token| {
            extract_post_id(token).ok_or_else(|| format!("could not find a post id in {token:?}"))
        })
        .collect::<Result<_, _>>()?;

    // #112: API に拒ませるのではなく､ここで拒む｡上限を超えると X はリクエ
    // スト全体を拒否するので､どちらにせよ呼び出し側は何も払わない -- だが
    // 400 は､件数が原因だという手がかりの無い不透明な API エラーとして届き､
    // 明らかな次の一手 (リトライ) はリクエストを一つ費やして同じように失敗
    // する｡数を言うことが直し方のすべてである｡
    if ids.len() > MAX_POST_IDS {
        return Err(format!(
            "{} ids given; `GET /2/tweets?ids=` accepts at most {MAX_POST_IDS} per request. \
             Split the list and run it again.",
            ids.len()
        ));
    }
    Ok(ids)
}

/// `GET /2/tweets?ids=` が一回のリクエストで受け取る id の数 (#112)｡
///
/// これを超えて分割すると､一回の起動に対し有料のリクエストが複数になる｡
/// それは別の機能であって `--fetch-post` が要るものではない: これは開発時の
/// 参照であり､アプリの中に一度に一握り以上を求めるものは無い｡
/// `cache::fetch_thread` の walk は呼び出しごとに id を一つ渡すので､
/// こちらもここへは届かない｡
const MAX_POST_IDS: usize = 100;

/// `requested` のうち `items` に現れなかったものを — 削除された､保護されて
/// いる､あるいは他の理由で API のレスポンスに無い — `requested` 自身の順で
/// 返す｡`fetch_post` が stderr へ印字する "N of M found" の行にだけ使われる｡
/// stdout は常に返ってきたものだけをそのまま印字し､残りにプレースホルダの
/// 項目は置かない｡
fn missing_ids(requested: &[String], items: &[x_api::TimelineItem]) -> Vec<String> {
    let present: HashSet<&str> = items.iter().map(|item| item.id.as_str()).collect();
    requested
        .iter()
        .filter(|id| !present.contains(id.as_str()))
        .cloned()
        .collect()
}

/// 取得した post を `--fetch-post` (#42) の stdout 向けに整形済み JSON と
/// して描く — 出力形式はこれだけで､人が読む形式を併設しなかった理由は
/// `fetch_post` の doc コメントを見よ｡`serde_json::to_string_pretty` の薄い
/// ラッパで､`fetch_post` のネットワーク呼び出しを通さずに印字されるものの形
/// をテストが確かめられるよう独立させてある｡下の `usage_only` が自身の JSON
/// 出力に対して既に当てているのと同じ理屈だ｡
fn render_fetch_post_json(items: &[x_api::TimelineItem]) -> serde_json::Result<String> {
    serde_json::to_string_pretty(items)
}

/// `--fetch-post <id-or-url>[,...]` (#42) のために､id で post を一つ以上
/// 取得して JSON で stdout へ印字する — 例えば `x.com` 自身が `WebFetch` に
/// 402 を返す (issue 自身の動機) ため､人が貼り付けなくても Claude Code の
/// セッションが特定の post の本文を読めるようにする｡
///
/// [`x_api::XClient::tweets_by_id`] を通す — `cache::fetch_thread` の親
/// チェーンの walk が使うのと同じ呼び出し (#12) — id ごとに一リクエストでは
/// なく､要求された id をすべてカンマ区切りの `ids=` 一つに繋いで渡す｡
/// `GET /2/tweets?ids=` は既にカンマ区切りの列を受け取るので､その呼び出しを
/// 手を入れずに再利用している: 五つの id の取得でもちょうど一リクエストで､
/// `cache::fetch_thread` と同じ `Endpoint::TweetById` (#10) の下で追跡され､
/// #18 の usage 追跡が自動で数える｡どちらも他のあらゆる読み取りと同じように
/// [`x_api::XClient::get`] を通るからだ｡
///
/// **timeline のキャッシュ (#9) は意図的に完全に迂回する — そこから何も
/// 読まず､そこへ何も書かない｡** あのキャッシュは､リロードのたびに同じ
/// アカウントの timeline を取り直すのを避けるためにある｡任意の post id は
/// その繰り返しアクセスの性質を共有しない: たいていリンクされていた場所から
/// 一度引かれるだけで､二度と引かれない｡キャッシュするなら､eviction の
/// 筋書きの無い id を鍵とする新しいファイルを作るか､無関係な post を何の
/// 関係も無いアカウント単位の timeline ファイルへ押し込むかになる｡最も
/// 擁護しやすい選択は､これが要する一リクエストを常に費やし､結果を決して
/// 永続化しないことだ｡
///
/// 要求された id が何個であっても常にちょうど一リクエストを費やし (上の
/// `tweets_by_id` の理屈を見よ)､要求した id のうち実際にいくつ返ってきたか
/// と並べて stderr へ報告する — `fetch_only` のキャッシュ hit/miss の行に
/// ならったもので — 費用と収穫が曖昧にならないようにする｡それが issue 自身
/// の完了条件である｡stdout へ印字するのは JSON だけだ: issue の動機は端末の
/// 前の人間ではなくこの出力を読むツールであり､下の `--usage` が既に同じ理由
/// で同じ選択をしている｡だから別の `--json` フラグも､人が読む既定も足して
/// いない｡
fn fetch_post(config: &config::Config, paths: &paths::Paths, arg: &str) -> i32 {
    let ids = match parse_post_ids(arg) {
        Ok(ids) => ids,
        Err(message) => {
            eprintln!("--fetch-post: {message}");
            return 1;
        }
    };

    let resolution = match oauth::resolve_credential(config, paths, oauth::unix_now()) {
        Ok(resolution) => resolution,
        Err(error) => {
            eprintln!("could not resolve a credential: {error:#}");
            return 1;
        }
    };
    // #54: 更新できなかった保存済みセッションは､ここでも声に出して言う価値
    // がある — headless な実行には､代わりにそれを見せるヘッダのバナーが
    // 無い｡
    if let Some(demotion) = &resolution.demotion {
        eprintln!("{}", oauth::describe_demotion(demotion));
    }
    let Some(credential) = resolution.credential else {
        // `fetch_only` の同じ箇所を見よ: サインインにはブラウザが要る (#33)｡
        eprintln!(
            "no signed-in session is available. Run twigpui without --fetch-post and click \
             \"Sign in with X\" once; this flag reuses the session that leaves behind."
        );
        return 1;
    };

    let client = x_api::XClient::renewing(credential.session);
    let joined_ids = ids.join(",");
    match client.tweets_by_id(paths, &joined_ids, oauth::unix_now()) {
        Ok(items) => {
            let missing = missing_ids(&ids, &items);
            let missing_note = if missing.is_empty() {
                String::new()
            } else {
                format!(" (missing: {})", missing.join(", "))
            };
            eprintln!(
                "1 API request spent, {} of {} post(s) found{missing_note}",
                items.len(),
                ids.len(),
            );
            match render_fetch_post_json(&items) {
                Ok(json) => {
                    println!("{json}");
                    0
                }
                Err(error) => {
                    eprintln!("could not serialize the fetched post(s): {error}");
                    1
                }
            }
        }
        Err(error) => {
            eprintln!("fetch failed: {error:#}");
            1
        }
    }
}

/// `usage::build_report` の数字を JSON で stdout へ印字する (#18) — そもそも
/// リクエスト数を `state_dir` の下に永続化する狙いは､外部のツールがウィンド
/// ウを開かずにヘッダが見せるのと同じ数字を読めることにある｡独自のテキスト
/// 形式ではなく JSON なのは､このプロジェクトが永続化する他のすべてで既に
/// `serde_json` に依存しているからで､機械が読む消費者には scrape せねば
/// ならない形式ではなく parse できる構造が要るからだ｡
fn usage_only(config: &config::Config, paths: &paths::Paths) -> i32 {
    let entries = match usage::load_all(paths) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("could not read usage data: {error:#}");
            return 1;
        }
    };

    let report = usage::build_report(
        &entries,
        oauth::unix_now(),
        config.request_price,
        config.daily_request_budget,
    );

    match serde_json::to_string_pretty(&report) {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(error) => {
            eprintln!("could not serialize the usage report: {error:#}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- #231: 起動ログの 1 行目 ---

    #[test]
    fn the_startup_banner_names_the_version_and_the_commit() {
        assert_eq!(
            startup_banner("0.1.0", "abc1234"),
            "starting twigpui 0.1.0 (abc1234)"
        );
    }

    #[test]
    fn a_dirty_tree_and_a_missing_git_both_still_read_as_one_line() {
        // `build.rs` が出しうる残り 2 つ｡どちらも括弧の中に収まる｡
        assert_eq!(
            startup_banner("0.1.0", "abc1234-dirty"),
            "starting twigpui 0.1.0 (abc1234-dirty)"
        );
        assert_eq!(
            startup_banner("0.1.0", "unknown"),
            "starting twigpui 0.1.0 (unknown)"
        );
    }

    #[test]
    fn the_banner_carries_the_hash_this_build_was_stamped_with() {
        // `env!` なので埋め忘れはコンパイルエラーになる｡ここが見るのは
        // 中身が実際に届いているか — `build.rs` が空文字列を出しても
        // `env!` は通ってしまう｡
        let hash = env!("TWIGPUI_GIT_HASH");
        assert!(!hash.is_empty(), "build.rs stamped an empty hash");
        assert!(
            startup_banner(env!("CARGO_PKG_VERSION"), hash).ends_with(&format!("({hash})")),
            "the banner should end with the stamped hash"
        );
    }

    #[test]
    fn applescript_quote_escapes_backslashes_and_quotes() {
        assert_eq!(
            applescript_quote(r#"say "hi" \ bye"#),
            r#""say \"hi\" \\ bye""#
        );
    }

    #[test]
    fn applescript_quote_flattens_embedded_newlines() {
        assert_eq!(
            applescript_quote("line one\nline two"),
            "\"line one line two\""
        );
    }

    // --- #42: --fetch-post の引数の parse ---

    #[test]
    fn fetch_post_arg_is_absent_when_the_flag_is_absent() {
        let args: Vec<String> = ["twigpui", "--fetch-only"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(fetch_post_arg(&args, "--fetch-post"), FetchPostArg::Absent);
    }

    #[test]
    fn fetch_post_arg_is_missing_value_when_the_flag_has_no_value() {
        let args: Vec<String> = ["twigpui", "--fetch-post"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            fetch_post_arg(&args, "--fetch-post"),
            FetchPostArg::MissingValue
        );
    }

    #[test]
    fn fetch_post_arg_returns_the_following_argument() {
        let args: Vec<String> = ["twigpui", "--fetch-post", "123,456"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            fetch_post_arg(&args, "--fetch-post"),
            FetchPostArg::Value("123,456")
        );
    }

    #[test]
    fn extract_post_id_accepts_a_bare_id() {
        assert_eq!(
            extract_post_id("1700000000000000001"),
            Some("1700000000000000001".to_string())
        );
    }

    #[test]
    fn extract_post_id_trims_surrounding_whitespace() {
        assert_eq!(
            extract_post_id("  1700000000000000001  "),
            Some("1700000000000000001".to_string())
        );
    }

    #[test]
    fn extract_post_id_reads_the_id_out_of_an_x_status_url() {
        assert_eq!(
            extract_post_id("https://x.com/jack/status/20"),
            Some("20".to_string())
        );
    }

    #[test]
    fn extract_post_id_reads_the_id_out_of_a_twitter_status_url() {
        assert_eq!(
            extract_post_id("https://twitter.com/jack/status/20"),
            Some("20".to_string())
        );
    }

    #[test]
    fn extract_post_id_stops_at_a_trailing_path_segment() {
        assert_eq!(
            extract_post_id("https://x.com/jack/status/20/photo/1"),
            Some("20".to_string())
        );
    }

    #[test]
    fn extract_post_id_stops_at_a_trailing_query_string() {
        assert_eq!(
            extract_post_id("https://x.com/jack/status/20?s=20"),
            Some("20".to_string())
        );
    }

    #[test]
    fn extract_post_id_rejects_neither_an_id_nor_a_status_url() {
        assert_eq!(extract_post_id("not-a-post"), None);
    }

    #[test]
    fn extract_post_id_rejects_empty_input() {
        assert_eq!(extract_post_id(""), None);
        assert_eq!(extract_post_id("   "), None);
    }

    #[test]
    fn parse_post_ids_accepts_a_single_id() {
        assert_eq!(parse_post_ids("20"), Ok(vec!["20".to_string()]));
    }

    #[test]
    fn parse_post_ids_splits_a_comma_separated_list() {
        assert_eq!(
            parse_post_ids("20,30"),
            Ok(vec!["20".to_string(), "30".to_string()])
        );
    }

    #[test]
    fn parse_post_ids_accepts_a_mix_of_ids_and_urls_with_whitespace() {
        assert_eq!(
            parse_post_ids(" 20 , https://x.com/jack/status/30 "),
            Ok(vec!["20".to_string(), "30".to_string()])
        );
    }

    #[test]
    fn parse_post_ids_accepts_the_maximum_id_count() {
        let arg = (1..=100)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(parse_post_ids(&arg).map(|ids| ids.len()), Ok(100));
    }

    #[test]
    fn parse_post_ids_rejects_more_ids_than_one_request_accepts() {
        // #112: 上限を超えると X はリクエスト全体を拒否するので､間違えても
        // 費用はかからない -- だがメッセージが上限を名指ししなければ､目に
        // 見える症状は不透明な 400 だけになる｡
        let arg = (1..=101)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let message = parse_post_ids(&arg).expect_err("101 ids must be refused");
        assert!(message.contains("101"), "{message} must name the count");
        assert!(message.contains("100"), "{message} must name the limit");
    }

    #[test]
    fn parse_post_ids_rejects_an_empty_argument() {
        assert!(parse_post_ids("").is_err());
    }

    #[test]
    fn parse_post_ids_rejects_a_token_that_is_neither_an_id_nor_a_url() {
        assert!(parse_post_ids("20,not-a-post").is_err());
    }

    fn item(id: &str) -> x_api::TimelineItem {
        x_api::TimelineItem {
            id: id.to_string(),
            text: format!("text of {id}"),
            created_at: None,
            author_name: format!("Author {id}"),
            author_username: format!("author{id}"),
            reposted_by: None,
            quoted: None,
            replied_to: None,
            metrics: None,
            links: Vec::new(),
            author_avatar_url: None,
            original_post_id: None,
            media: Vec::new(),
        }
    }

    #[test]
    fn missing_ids_is_empty_when_every_requested_id_came_back() {
        let requested = vec!["1".to_string(), "2".to_string()];
        let items = vec![item("1"), item("2")];
        assert!(missing_ids(&requested, &items).is_empty());
    }

    #[test]
    fn missing_ids_reports_ids_absent_from_the_response() {
        let requested = vec!["1".to_string(), "2".to_string(), "3".to_string()];
        let items = vec![item("1"), item("3")];
        assert_eq!(missing_ids(&requested, &items), vec!["2".to_string()]);
    }

    #[test]
    fn render_fetch_post_json_prints_the_fetched_posts_as_a_json_array() {
        let items = vec![item("1")];
        let json = render_fetch_post_json(&items).unwrap();
        assert!(json.contains("\"id\": \"1\""));
        assert!(json.contains("\"text\": \"text of 1\""));
        assert!(json.contains("\"author_username\": \"author1\""));
    }

    #[test]
    fn render_fetch_post_json_prints_an_empty_array_for_no_posts() {
        let items: Vec<x_api::TimelineItem> = Vec::new();
        assert_eq!(render_fetch_post_json(&items).unwrap(), "[]");
    }
}
