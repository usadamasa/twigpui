//! カラーテーマ (#19)｡[`Theme`] は UI の描画ヘルパーが必要とする色スロットを
//! すべて 1 つの `Copy` な値へ束ねる｡おかげでライフタイムもグローバルも無しに
//! `TimelineView` の自由関数へ通せる｡
//!
//! [`ThemeMode`] はユーザーに見える設定 (`light` / `dark` / `system`) で､
//! [`crate::config::Config`] が `config.toml` / `X_THEME` から解決する｡
//! [`ThemeMode::resolve`] がそれを具体的な [`Theme`] へ変える｡OS の外観を
//! 参照するのは `System` のときだけで､gpui の `Window::appearance()` を使う｡
//!
//! ## light パレットのコントラスト
//!
//! WCAG 2 の相対輝度の式 — 線形化した sRGB チャンネルに対する
//! `(L1 + 0.05) / (L2 + 0.05)` — で [`Theme::light`] の値に対して計算した:
//!
//! | 組 | 比 | AA テキストの閾値 (4.5:1) |
//! | --- | --- | --- |
//! | `bg` の上の `text` | 18.5:1 | 合格 |
//! | `bg` の上の `text_muted` | 6.9:1 | 合格 |
//! | `bg` の上の `text_tertiary` (#95) | 5.1:1 | 合格 |
//! | `accent` の上の `button_label` (idle のボタン) | 5.7:1 | 合格 |
//! | `button_busy_bg` の上の `button_label` (busy のボタン) | 6.9:1 | 合格 |
//! | `bg` の上の `danger` | 5.8:1 | 合格 |
//! | `bg` の上の `warning` (#18) | 5.0:1 | 合格 |
//! | `bg` の上の `like` (#95) | 5.8:1 | 合格 |
//! | `bg` の上の `repost` (#95) | 4.9:1 | 合格 |
//!
//! ## これらが macOS のシステムカラーそのものではない理由 (#95)
//!
//! #95 は見た目を「macOS に倣う」と定めた｡*色相*はシステムパレットから
//! 取っている — accent は systemBlue､`like` は systemRed､`repost` は
//! systemGreen､それとラベルの 4 段階のランプ — が輝度は取っていない｡
//! Apple 自身の値は白い背景では上の表を落ちる｡systemBlue (`#007AFF`) は
//! 3.6:1 止まり､systemGreen (`#34C759`) は 1.8:1 しかなく､
//! `secondaryLabelColor` (黒のアルファ 50%) は 3.9:1｡このプロジェクトは #19
//! 以来すべてのテキストの組について AA を文書化しており､見た目の変更はそれを
//! 捨てる理由にならない｡だからシステムの色相は保ったまま通るまで暗くした｡
//! 誰にも読めないテキストを出さずに macOS のパレットとして読める｡

use gpui::{App, Pixels, Window, WindowAppearance, px};

/// 本文テキスト｡macOS 自身の body スタイルは 13pt であって､ウィンドウが
/// かつてグローバルに設定していた 14px の `text_sm` ではない (#95)｡
pub(crate) const TEXT_BODY: Pixels = px(13.0);

/// ハンドル､タイムスタンプ､エンゲージメント数､それとステータスバー —
/// macOS の補助的なサイズは 11 に置かれている (#95)｡
pub(crate) const TEXT_META: Pixels = px(11.0);

/// ボタン､フィールド､その他コントロールとして読めるものすべて｡
pub(crate) const RADIUS_CONTROL: Pixels = px(6.0);

/// 画像のサムネイル｡コントロールより 1 段きつい (#95)｡
pub(crate) const RADIUS_THUMB: Pixels = px(5.0);

/// ツールバーのアイコンを描く大きさ (#95) — body ではなく meta のテキスト
/// サイズに合わせてある｡ツールバーのアイコンは散文ではなくコントロールの
/// ラベルだからだ｡
pub(crate) const ICON_SIZE: Pixels = px(15.0);

/// 添付画像を横に並べた 1 段の幅の上限 (#256)｡1 枚ならその画像の幅の上限｡
///
/// 本番ウィンドウ (429px) の本文列は 365px ほどで､それより狭くしてあるのは
/// 右に余白を残して左寄せに見せるためだ — 画像を列の中央に置くのではなく､
/// 本文と同じ左端から始める (Tumblr の配置)｡小さい画像はここまで拡大する｡
pub(crate) const MEDIA_MAX_WIDTH: Pixels = px(320.0);

/// 添付画像 1 枚の高さの上限 (#256)｡縦長の 1 枚はここで止まる｡
///
/// 上限が抑えるのは 1 枚ごとの高さであって､post 全体ではない: 横長を
/// ようけ積んだ post は縦に長くなる (#95 で嫌った「添付だけでウィンドウが
/// 埋まる」は､横長 4 枚の縦積みなら再び起きる)｡それは読める大きさで
/// 見せることの対価として #256 が受け入れた — 横 1 段に押し込んだ横長は
/// 40px そこそこになり､サムネイルとしても読めなかった｡
pub(crate) const MEDIA_MAX_HEIGHT: Pixels = px(240.0);

/// 横に並んだ添付画像どうしの隙間 (#256)｡
pub(crate) const MEDIA_GAP: Pixels = px(4.0);

/// timeline の 1 行の水平パディング｡
pub(crate) const ROW_PAD_X: Pixels = px(12.0);

/// timeline の 1 行の垂直パディング｡
pub(crate) const ROW_PAD_Y: Pixels = px(8.0);

/// 行の区切り線を左端からどれだけ字下げするか｡アバターの下ではなくテキストの
/// 始まる位置から引くためで､Mail と Messages が使うのと同じインセットだ｡
/// [`AVATAR_SIZE`] + [`ROW_PAD_X`] + 行の gap (#95)｡
pub(crate) const SEPARATOR_INSET: Pixels = px(52.0);

/// ウィンドウ上端のツールバーの帯 (#95)｡
pub(crate) const TOOLBAR_HEIGHT: Pixels = px(44.0);

/// ウィンドウ下端のステータスバー (#95)｡
pub(crate) const STATUS_BAR_HEIGHT: Pixels = px(24.0);

/// 空でフォーカスも無い composer の入力欄の高さ (#153) — 1 行ぶん｡
///
/// 入力ウィジェット (`gpui_component::Input`) の 1 行 (`1.25rem` = 20px) と
/// 上下の余白 (8px ずつ) と枠線 (1px ずつ) の和｡ウィジェット側の定数は
/// 公開されていないので､ここに写してある｡ずれれば
/// `the_composer_folds_to_one_line_until_it_is_used` が 40px の上限で落ちる｡
/// 広がった状態 (2 行以上) はウィジェットの `auto_grow` に任せ､ここでは
/// 決めない｡
pub(crate) const COMPOSER_FOLDED_HEIGHT: Pixels = px(38.0);

/// ステータスバーの 1 段上に座る list sync の行 (#205)｡
///
/// フェードの最中も含め､行が出ている間はずっとこの高さ｡0 から補間すると
/// フレームごとに timeline が押し上げられ､読んでいる行が指の下で滑る｡
pub(crate) const SYNC_ROW_HEIGHT: Pixels = px(22.0);

/// 手動 sync の確認ダイアログの幅 (#205)｡
///
/// ウィンドウ幅への割合ではなく固定値｡中身は短い文が 3 つで､幅の広い
/// ウィンドウで引き伸ばすと 1 行が読み返しにくくなる｡
pub(crate) const SYNC_DIALOG_WIDTH: Pixels = px(360.0);

/// ダイアログの背後を覆う膜 (#205)｡`rgba` なので下 8 bit が不透明度｡
///
/// 明暗どちらのテーマでも同じ黒を使う｡覆いは色ではなく「ここは今 触れない」
/// という合図で､明るいテーマで白く覆うと伝わらない｡
pub(crate) const SCRIM: u32 = 0x0000_0099;

/// アバターを描くときの角丸の半径 (#98)｡
///
/// `ui.rs` ではなくここに置いたのは､アバターを描く 2 か所 — ダウンロードした
/// 画像と､イニシャルを載せたプレースホルダー — が食い違えないようにするためで､
/// `AVATAR_SIZE` が 1 つの定数なのと同じ理由だ｡形が同じでなければ､ダウンロード
/// が届いた瞬間に行が目に見えて変わる｡
///
/// [`AVATAR_SIZE`] の 32px に対して寸法を決め､[`RADIUS_CONTROL`] に合わせて
/// ある (#95)｡macOS では小さな正方形の画像はこの半径でアプリアイコンとして
/// 読めるし､コントロールの半径を使えば､ほとんど一致する 2 つの丸めではなく
/// 1 つの丸めがウィンドウに残る｡ボタンも共有する — ピル型は #95 で無くなった｡
pub(crate) const AVATAR_RADIUS: Pixels = RADIUS_CONTROL;

/// 1 行の投稿者アバターを描く大きさ (#64)｡#95 で 44px から macOS の小アイコン
/// のサイズへ縮めた — 旧来の値は X 自身の web の timeline から取ったもので､
/// あちらはずっと幅の広いカラム向けに作られている｡
///
/// `ui::render` ではなくここに置いたのは､これから導かれる
/// [`AVATAR_RADIUS`] と [`SEPARATOR_INSET`] の隣に置いておくためだ｡
pub(crate) const AVATAR_SIZE: Pixels = px(32.0);

/// 名前の付いた UI の役割ごとに色スロットを 1 つ｡`ui.rs` に直接置かれていた
/// `BG` / `TEXT` / ... の `u32` 定数を置き換えたものだ｡RGB のチャンネルごとに
/// 区切ってあり､これは clippy が求める桁区切りでもある｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Theme {
    /// ウィンドウ本体の背景｡
    pub(crate) bg: u32,
    /// ヘッダーバーの背景 — ヘッダーが別の領域として読めるよう `bg` とは
    /// 区別してある｡
    pub(crate) bg_header: u32,
    /// ヘッダーと行の区切り線｡
    pub(crate) border: u32,
    /// セグメンテッドコントロールのセグメントが収まる窪んだトラック (#95)｡
    /// `border` を使い回さず独自のスロットにしてある｡区切り線はウィンドウの
    /// 白い背景に対して引くヘアラインで､同じ値をツールバーのグレーに対する
    /// 塗りとして使うと見えなくなるからだ｡
    pub(crate) control_trough: u32,
    /// 主たる本文テキスト｡
    pub(crate) text: u32,
    /// 強調を落としたテキスト (署名行､エンゲージメント数､プレースホルダーの
    /// 断り書き) — macOS の 4 段階のラベルランプの 2 段目 (#95)｡
    pub(crate) text_muted: u32,
    /// そのランプの 3 段目 (#95)｡タイムスタンプと "· replying to" の末尾で､
    /// 読めなければならないが､隣の署名行と張り合ってはいけない｡
    pub(crate) text_tertiary: u32,
    /// 主アクションボタンの idle (クリックできる) ときの塗り｡
    pub(crate) accent: u32,
    /// 主アクションボタンの busy / disabled のときの塗り — `border` を使い
    /// 回さず意図して独自のスロットにしてある｡light テーマのヘアラインの
    /// ボーダーは淡すぎて､ボタンの塗りとしては `button_label` を読めるままに
    /// 保てない (モジュールドキュメントのコントラスト表を参照)｡
    pub(crate) button_busy_bg: u32,
    /// 上のどちらの塗りの状態でも､主アクションボタンの上に描くテキスト｡
    /// `text` を使い回さず独自のスロットにしてある｡light テーマでは本文
    /// テキストはほぼ黒で､`accent` に対するコントラストを満たさないからだ｡
    pub(crate) button_label: u32,
    /// エラーとレートリミットのテキスト｡
    pub(crate) danger: u32,
    /// これが dark パレットかどうか — 色スロットを比べて導き直すのではなく
    /// スロットと並べて持たせてある｡gpui-component 自身の light/dark の
    /// どちらへ向けるかについて､[`sync_gpui_component_theme`] (#38) が単一で
    /// 直接の source of truth を持てるようにするためだ｡
    pub(crate) is_dark: bool,
    /// 今日のリクエスト数が､設定した日次予算に近づいている (まだ達しては
    /// いない) あいだの消費行の色 (#18) — `danger` とは区別してある｡あちらは
    /// 予算を実際に超えた場合 (とエラー) のために取ってあり､2 つの深刻度が
    /// 一目で見分けられるようにするためだ｡
    pub(crate) warning: u32,
    /// いいねした post のアクション (#95) — systemRed の色相を､モジュール
    /// ドキュメントの AA の表を通るまで暗くしたもの｡`accent` を使い回さず
    /// 独自のスロットにしてある｡macOS では「on」の状態はそれが何を意味するかで
    /// 色が付くし､リンクと同じ青に読めるいいねは何も言っていない｡
    pub(crate) like: u32,
    /// リポストした post のアクション (#95) — systemGreen の色相を､`like` と
    /// 同じやり方で同じ理由から暗くしたもの｡
    pub(crate) repost: u32,
}

impl Theme {
    /// #19 より前の twigpui が積んでいたパレット｡そのまま持ち越してあるので､
    /// `dark` へ切り替えれば昔の見た目がそのまま再現される｡
    pub(crate) const fn dark() -> Self {
        Self {
            bg: 0x15_20_2b,
            bg_header: 0x1b_28_36,
            border: 0x38_44_4d,
            control_trough: 0x0f_18_20,
            text: 0xf7_f9_f9,
            text_muted: 0x88_99_a6,
            // 暗い `bg` に対して `text_muted` より 1 段下｡`light` が逆向きに
            // やっていることの鏡像だ (#95)｡
            text_tertiary: 0x6b_7a_86,
            accent: 0x1d_9b_f0,
            // #19 より前のボタンをそのまま再現する｡当時のボタンは無条件に
            // `BORDER` を busy の塗りに､`TEXT` をラベルに使っていた｡
            button_busy_bg: 0x38_44_4d,
            button_label: 0xf7_f9_f9,
            danger: 0xf4_21_2e,
            is_dark: true,
            // Amber-400 あたり｡モジュールドキュメントの light パレットの表が
            // 使うのと同じ WCAG の式で `bg` (0x15_20_2b) に対して約 9.9:1｡
            warning: 0xfb_bf_24,
            // systemRed / systemGreen を暗い地の上向けに明るくしたもの｡
            // `light` がこの 2 つにやることの鏡像だ (#95)｡
            like: 0xff_6b_6b,
            repost: 0x4c_d9_8f,
        }
    }

    /// #19 が既定にする light パレット｡これらの値の裏にあるコントラスト比は
    /// モジュールドキュメントを参照｡
    pub(crate) const fn light() -> Self {
        Self {
            bg: 0xff_ff_ff,
            bg_header: 0xf5_f7_f8,
            border: 0xd7_dc_e0,
            control_trough: 0xe4_e8_eb,
            text: 0x0f_14_19,
            text_muted: 0x54_5b_63,
            // 5.1:1 — `text_muted` より読める範囲で 1 段下｡macOS 自身の
            // tertiaryLabelColor (黒のアルファ 26%､白の上で 3.4:1) では
            // モジュールドキュメントの表を通らない｡
            text_tertiary: 0x6e_6e_73,
            accent: 0x0b_65_c2,
            // 淡いヘアラインのボーダーでは白い `button_label` が読めなく
            // なるので､busy の塗りは代わりに中間のグレーにしてある —
            // モジュールドキュメントのコントラスト表を参照｡
            button_busy_bg: 0x54_5b_63,
            button_label: 0xff_ff_ff,
            danger: 0xc4_1e_3a,
            is_dark: false,
            // Amber-700 あたり｡`bg` (白) に対して約 5.0:1 で､モジュール
            // ドキュメントの表が他のスロットを照らすのと同じ AA テキストの
            // 閾値 (4.5:1) を通る｡
            warning: 0xb4_53_09,
            // systemRed の色相を `danger` の輝度で — 同じ色だ｡「いいね済み」と
            // 「失敗」が隣り合うことは無いからだ｡
            like: 0xc4_1e_3a,
            // systemGreen を 1.8:1 から 4.9:1 へ暗くした (#95)｡
            repost: 0x1f_7a_4d,
        }
    }
}

/// 設定されたままの `theme` の設定値 — [`Theme`] 自体とは別物だ｡`System` は
/// ウィンドウの実際の OS 外観に対して解決されるまで､決まった色の値を
/// 持たないからだ｡
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ThemeMode {
    /// 常に light パレット｡
    #[default]
    Light,
    /// 常に dark パレット｡
    Dark,
    /// gpui の `Window::appearance()` 経由で OS の外観に従う｡
    System,
}

impl ThemeMode {
    /// `theme` の設定値 (`X_THEME` か `config.toml` の `theme` キー) を
    /// パースする｡大文字小文字を区別せず空白を落とす — `Config::resolve` が
    /// 他の文字列設定を扱うのに合わせてある｡それ以外は `None`｡不正な値を
    /// どう報告するかは呼び出し側が決める｡認識できないテーマが起動を失敗
    /// させてはならないからだ (#19)｡
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "system" => Some(Self::System),
            _ => None,
        }
    }

    /// 具体的な [`Theme`] へ解決する｡`appearance` を参照するのは `System` の
    /// ときだけで､`Light` と `Dark` は OS の設定に関わらず固定だ｡
    pub(crate) fn resolve(self, appearance: WindowAppearance) -> Theme {
        match self {
            Self::Light => Theme::light(),
            Self::Dark => Theme::dark(),
            Self::System => match appearance {
                WindowAppearance::Light | WindowAppearance::VibrantLight => Theme::light(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark => Theme::dark(),
            },
        }
    }
}

/// gpui-component 自身のグローバルテーマを､このプロジェクトが解決した
/// パレットへ向ける (#38)｡
///
/// composer のテキスト入力は `gpui_component::input::Input` で､色を
/// `gpui_component::theme::Theme` から読む — このモジュールの [`Theme`] とは
/// 完全に別のグローバルだ｡これが無いとそのグローバルは
/// `gpui_component::init` が最後に置いた場所 (生の OS 外観に同期した状態) に
/// 留まり､このプロジェクトが `config.theme` から解決したばかりのものと
/// 食い違いうる — 例えば light 外観の OS で `config.theme = "dark"` だと､
/// 暗いウィンドウに囲まれた入力が light-on-light で描かれてしまう｡
/// [`ThemeMode::resolve`] の直後に [`crate::ui::TimelineView::new`] から
/// 本物の `Window` を持って 1 度だけ呼ぶ — ここで扱うべき "System" の場合分けは
/// 無く､`theme` はすでに具体的だ｡
///
/// このプロジェクトのパレットへ向けるのは､gpui-component の `Input` が実際に
/// 読む色スロットだけだ (あちらのソースによれば `background`､`foreground`､
/// プレースホルダー用の `muted_foreground`､枠線用の `border`/`input`､
/// `caret`､`selection`､それとフォーカス表示用の `accent`/`primary`/`ring`)｡
/// 他のスロット (メニュー､テーブル､チャート､…) は解決した light/dark モードに
/// 対する gpui-component 自身の既定のままにする｡このアプリはそれらの
/// ウィジェットを一切描かないからだ｡
pub(crate) fn sync_gpui_component_theme(theme: Theme, window: &mut Window, cx: &mut App) {
    use gpui_component::theme::{Theme as ComponentTheme, ThemeMode as ComponentThemeMode};

    let mode = if theme.is_dark {
        ComponentThemeMode::Dark
    } else {
        ComponentThemeMode::Light
    };
    ComponentTheme::change(mode, Some(window), cx);

    let colors = ComponentTheme::global_mut(cx);
    colors.background = gpui::rgb(theme.bg).into();
    colors.foreground = gpui::rgb(theme.text).into();
    colors.muted_foreground = gpui::rgb(theme.text_muted).into();
    colors.muted = gpui::rgb(theme.bg_header).into();
    colors.border = gpui::rgb(theme.border).into();
    colors.input = gpui::rgb(theme.border).into();
    colors.caret = gpui::rgb(theme.text).into();
    colors.selection = gpui::rgb(theme.accent).into();
    colors.accent = gpui::rgb(theme.accent).into();
    colors.accent_foreground = gpui::rgb(theme.button_label).into();
    colors.primary = gpui::rgb(theme.accent).into();
    colors.primary_foreground = gpui::rgb(theme.button_label).into();
    colors.ring = gpui::rgb(theme.accent).into();
    colors.danger = gpui::rgb(theme.danger).into();
}

impl std::fmt::Display for ThemeMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Theme, ThemeMode};
    use gpui::WindowAppearance;

    #[test]
    fn light_and_dark_are_distinct_in_every_slot() {
        let light = Theme::light();
        let dark = Theme::dark();
        assert_ne!(light.bg, dark.bg);
        assert_ne!(light.bg_header, dark.bg_header);
        assert_ne!(light.border, dark.border);
        assert_ne!(light.control_trough, dark.control_trough);
        assert_ne!(light.text, dark.text);
        assert_ne!(light.text_muted, dark.text_muted);
        assert_ne!(light.text_tertiary, dark.text_tertiary);
        assert_ne!(light.accent, dark.accent);
        assert_ne!(light.like, dark.like);
        assert_ne!(light.repost, dark.repost);
        assert_ne!(light.button_busy_bg, dark.button_busy_bg);
        assert_ne!(light.button_label, dark.button_label);
        assert_ne!(light.danger, dark.danger);
        assert_ne!(light.is_dark, dark.is_dark);
        assert_ne!(light.warning, dark.warning);
    }

    #[test]
    fn warning_is_distinct_from_danger_in_both_palettes() {
        // #18: `usage_color` は「予算に近い」を `warning` へ､「予算を超えた」を
        // `danger` へ対応させる — 2 つの色が同じなら､ヘッダーは 2 つの
        // 深刻度を見た目で区別できない｡
        assert_ne!(Theme::light().warning, Theme::light().danger);
        assert_ne!(Theme::dark().warning, Theme::dark().danger);
    }

    #[test]
    fn an_on_state_is_never_the_same_color_as_a_link() {
        // #95: `like` と `repost` があるのは､「on」のアクションがどの
        // アクションなのかを示すためだ｡どちらかを `accent` — すべてのリンクと
        // 主ボタンがすでに纏っている色 — へ潰せばそれが台無しになるし､
        // 2 つは色相が十分に近いので不注意な編集でそうなりうる｡
        for theme in [Theme::light(), Theme::dark()] {
            assert_ne!(theme.like, theme.accent);
            assert_ne!(theme.repost, theme.accent);
            assert_ne!(theme.like, theme.repost);
        }
    }

    #[test]
    fn the_three_label_steps_are_distinct_within_a_palette() {
        // #95: ランプがランプであるのは段が違うときだけだ｡2 つを同じ値にした
        // パレットは署名行とタイムスタンプを同じに描く｡それこそがこの issue が
        // 直そうとした平板化だ｡
        for theme in [Theme::light(), Theme::dark()] {
            assert_ne!(theme.text, theme.text_muted);
            assert_ne!(theme.text_muted, theme.text_tertiary);
            assert_ne!(theme.text, theme.text_tertiary);
        }
    }

    #[test]
    fn light_and_dark_ignore_the_os_appearance() {
        assert_eq!(
            ThemeMode::Light.resolve(WindowAppearance::Dark),
            Theme::light()
        );
        assert_eq!(
            ThemeMode::Dark.resolve(WindowAppearance::Light),
            Theme::dark()
        );
    }

    #[test]
    fn system_follows_a_light_os_appearance() {
        assert_eq!(
            ThemeMode::System.resolve(WindowAppearance::Light),
            Theme::light()
        );
        assert_eq!(
            ThemeMode::System.resolve(WindowAppearance::VibrantLight),
            Theme::light()
        );
    }

    #[test]
    fn system_follows_a_dark_os_appearance() {
        assert_eq!(
            ThemeMode::System.resolve(WindowAppearance::Dark),
            Theme::dark()
        );
        assert_eq!(
            ThemeMode::System.resolve(WindowAppearance::VibrantDark),
            Theme::dark()
        );
    }

    #[test]
    fn defaults_to_light() {
        assert_eq!(ThemeMode::default(), ThemeMode::Light);
    }

    #[test]
    fn parses_known_values_case_insensitively_and_trims_whitespace() {
        assert_eq!(ThemeMode::parse("light"), Some(ThemeMode::Light));
        assert_eq!(ThemeMode::parse("  LIGHT\n"), Some(ThemeMode::Light));
        assert_eq!(ThemeMode::parse("Dark"), Some(ThemeMode::Dark));
        assert_eq!(ThemeMode::parse("SYSTEM"), Some(ThemeMode::System));
        assert_eq!(ThemeMode::parse(" system "), Some(ThemeMode::System));
    }

    #[test]
    fn rejects_an_unrecognized_value() {
        assert_eq!(ThemeMode::parse("solarized"), None);
        assert_eq!(ThemeMode::parse(""), None);
    }

    #[test]
    fn display_matches_the_parse_keywords() {
        // Config::resolve のフォールバックの警告がこれを埋め込むので､
        // parse() から乖離するのではなく往復できる必要がある｡
        for mode in [ThemeMode::Light, ThemeMode::Dark, ThemeMode::System] {
            assert_eq!(ThemeMode::parse(&mode.to_string()), Some(mode));
        }
    }
}
