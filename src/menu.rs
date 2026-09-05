//! キーバインド (#58) と macOS のメニューバー (#99)｡
//!
//! `ui` から切り出してあるのは､ここがフロントエンドのうち timeline の描画に
//! 関わらない唯一の部分だからだ: `main` はこのすべてをウィンドウが存在する
//! 前に登録し､メニューバーは個々のウィンドウより長く生きる｡ここに置くこと
//! で､キーストロークが名指される場所がすべて一つのファイルに収まる｡

gpui::actions!(
    twigpui,
    [
        /// timeline をリロードする (#58)｡API リクエストを費やすので `cmd-r`
        /// に割り当ててある — どのアプリも共有するリロードの所作であり､誰かが
        /// 誤って叩く鍵ではない｡
        Reload,
        /// composer へフォーカスを移す (#58)｡
        FocusComposer,
        /// composer からフォーカスを外す (#58)｡下書きには触れない｡
        BlurComposer,
        /// アプリケーションを終了する (#99)｡gpui は独自の quit アクションを
        /// 持たず､それが無いとアプリメニューには `cmd-q` を吊るす先が無い —
        /// twigpui が Dock からしか終了できなくなっていたのはそのためだ｡
        /// ウィンドウの root ではなく `App` の層で `main` が扱う: グローバル
        /// なハンドラはどのウィンドウもフォーカスを持たないときにも発火し､
        /// まさにそのときこそ人は `cmd-q` に手を伸ばす｡
        Quit,
        /// About パネルを見せる (#99) — どの macOS アプリケーションにもある
        /// アプリメニューのもう半分｡
        ShowAbout,
        /// ウィンドウを Dock へしまう (#109)｡`cmd-m` に割り当て｡
        Minimize,
        /// ウィンドウを閉じる (#109)｡`cmd-w` に割り当て｡ウィンドウが一枚なら
        /// `cmd-q` と同じくアプリが終わる — [`CLOSE_WINDOW`] を見よ｡
        CloseWindow,
        /// timeline を最新の post まで戻す (#22)｡`cmd-up` に割り当て｡
        /// 完全にローカルで — 何も費やさない｡
        ScrollToTop,
        /// auto-refresh が既に取得した post を見せる (#21)｡`cmd-shift-r` に
        /// 割り当て｡[`Reload`] の対であり､金の話では意図的にその逆だ:
        /// `cmd-r` は取得を買い､`cmd-shift-r` は既に買って支払い済みのものを
        /// 明かす｡何も費やさない｡
        ShowNewPosts,
        /// poll が拾った新しい post が､先頭にいる読み手のところへひとりでに
        /// 流れ込むかどうかを切り替える (#22)｡`cmd-shift-f` に割り当て｡
        /// 純粋に見せ方の話で — poll そのものは `auto_refresh` のスイッチな
        /// ので､どちらに倒しても何も費やさない｡
        ToggleFollowNewPosts,
        /// list sync の確認ダイアログを開く (#248)｡footer の入口から移した:
        /// 手で同期を始めるのはまれで､そのために footer の幅を使い続ける
        /// 理由が無い｡鍵は持たない — 確認の先で API リクエストを費やす｡
        SyncList,
        /// ウィンドウが手元に無い間､背景を透かすかどうかを切り替える (#267)｡
        /// `cmd-alt-t` に割り当て — Stickies の Translucent と同じ鍵｡
        /// 見え方だけの話で､何も費やさない｡
        ToggleTranslucent,
    ]
);

/// timeline の root 要素が担うキーコンテキスト (#58) — 下のバインドは
/// [`QUIT`] (#99) を除きすべて､グローバルに登録するのではなくこれへスコープ
/// してある｡将来の単一キーのバインドが､別の view がフォーカスを持つ間に
/// 発火しないようにするためだ｡
pub(crate) const KEY_CONTEXT: &str = "Timeline";

/// 一つのバインドを､一度だけ定義する (#99)｡
///
/// メニューバーができる前は､キーストロークは `init` に､そのグリフは
/// [`shortcuts`] に書かれ､両者を結ぶものは何も無かった｡メニュー項目は三つ目
/// の写しになるはずだったので､今は三者が同じ定数を読む: `init` が
/// [`Shortcut::keystroke`] を bind し､ヘッダが [`Shortcut::glyphs`] を印字
/// し､[`menus`] が項目にラベルを付ける｡メニュー自身の key equivalent はここ
/// には一切書かない — macOS が keymap から解決するので､`keystroke` がキーを
/// 名指す唯一の場所であり続ける｡
struct Shortcut {
    /// `gpui::KeyBinding::new` が parse する形のキーストローク｡
    keystroke: &'static str,
    /// このバインドをスコープするキーコンテキスト｡グローバルに登録するなら
    /// `None`｡グローバルなのは [`QUIT`] だけ — [`init`] を見よ｡
    context: Option<&'static str>,
    /// このショートカットのキーバインドを､アクションを閉じ込めて組み立てる
    /// (#119)｡
    ///
    /// `const` は `impl Action` を持てないが､何も捕捉しないクロージャは関数
    /// ポインタへ coerce する — 使うたびにではなくここでアクションを名指す
    /// にはそれで足りる｡以前は `init` と [`menus`] がショートカットと
    /// アクションを手で対にしており､`menu_item(&RELOAD, FocusComposer)` は
    /// Reload というラベルの下で `cmd-n` が composer にフォーカスするメニュー
    /// 項目として型検査を通ってしまった｡
    bind: fn(&'static str, Option<&'static str>) -> gpui::KeyBinding,
    /// このショートカットのメニュー項目を､[`Shortcut::bind`] と同じアクション
    /// を閉じ込めて組み立てる — 対応はこの定数に一度だけ書く｡
    item: fn(&'static str) -> gpui::MenuItem,
    /// メニューバーがアクションをどう名乗るか｡メニューバーに出さないなら
    /// `None`｡言い回しが違うのは意図的だ: メニュー項目は単独で読まれ
    /// ("New Post")､ヘッダの帯は見出しの下のヒントの列として読まれる
    /// ("⌘N Focus the composer")｡
    menu_label: Option<&'static str>,
}

/// timeline をリロードする｡API リクエストを費やすので､誰かが誤って叩く鍵で
/// はなく､どのアプリも共有するリロードの所作を取る｡
const RELOAD: Shortcut = Shortcut {
    keystroke: "cmd-r",
    context: Some(KEY_CONTEXT),
    bind: |keystroke, context| gpui::KeyBinding::new(keystroke, Reload, context),
    item: |label| gpui::MenuItem::action(label, Reload),
    menu_label: Some("Reload"),
};

/// composer へフォーカスを移す｡
const FOCUS_COMPOSER: Shortcut = Shortcut {
    keystroke: "cmd-n",
    context: Some(KEY_CONTEXT),
    bind: |keystroke, context| gpui::KeyBinding::new(keystroke, FocusComposer, context),
    item: |label| gpui::MenuItem::action(label, FocusComposer),
    menu_label: Some("New Post"),
};

/// composer から出る｡メニューバーには無い: 「フォーカスを戻す」は所作で
/// あって､誰かがメニューに探しに行くコマンドではない｡
const BLUR_COMPOSER: Shortcut = Shortcut {
    keystroke: "escape",
    context: Some(KEY_CONTEXT),
    bind: |keystroke, context| gpui::KeyBinding::new(keystroke, BlurComposer, context),
    item: |label| gpui::MenuItem::action(label, BlurComposer),
    menu_label: None,
};

/// 終了する (#99)｡キーコンテキスト無しで登録される唯一のバインドで､ヘッダが
/// 宣伝しない唯一のものでもある — [`init`] を見よ｡
const QUIT: Shortcut = Shortcut {
    keystroke: "cmd-q",
    context: None,
    bind: |keystroke, context| gpui::KeyBinding::new(keystroke, Quit, context),
    item: |label| gpui::MenuItem::action(label, Quit),
    menu_label: Some("Quit twigpui"),
};

/// しまう (#109)｡ヘッダの帯から外してあるのは [`QUIT`] と同じ理由だ: これは
/// macOS の所作であって､このアプリが発明したものではない｡
const MINIMIZE: Shortcut = Shortcut {
    keystroke: "cmd-m",
    context: Some(KEY_CONTEXT),
    bind: |keystroke, context| gpui::KeyBinding::new(keystroke, Minimize, context),
    item: |label| gpui::MenuItem::action(label, Minimize),
    menu_label: Some("Minimize"),
};

/// ウィンドウを閉じる (#109)｡
///
/// ウィンドウが一枚なら [`QUIT`] と同じくアプリが終わる — 一枚ウィンドウの
/// macOS アプリで `cmd-w` がするのはそれなので､ここで違う振る舞いにする値打
/// ちは無い｡未送信の下書きを捨てるという `cmd-q` の危うさ (#14) を共有し､
/// `cmd-q` と同じく確認もしない｡
const CLOSE_WINDOW: Shortcut = Shortcut {
    keystroke: "cmd-w",
    context: Some(KEY_CONTEXT),
    bind: |keystroke, context| gpui::KeyBinding::new(keystroke, CloseWindow, context),
    item: |label| gpui::MenuItem::action(label, CloseWindow),
    menu_label: Some("Close Window"),
};

/// 最新の post へ戻る (#22)｡
///
/// #58 以降の他の追加と違い､ヘッダの帯に載っている: ずっと下までスクロール
/// した読み手が実際に抱く問いに答える､ここで唯一のバインドであり､押しても
/// 費用がかからないからだ｡
const SCROLL_TO_TOP: Shortcut = Shortcut {
    keystroke: "cmd-up",
    context: Some(KEY_CONTEXT),
    bind: |keystroke, context| gpui::KeyBinding::new(keystroke, ScrollToTop, context),
    item: |label| gpui::MenuItem::action(label, ScrollToTop),
    menu_label: Some("Back to Top"),
};

/// auto-refresh が既に取得したものを見せる (#21)｡
///
/// `cmd-shift-r` なのは `cmd-r` の対だからで､対であることこそが要点だ: 二つ
/// は画面に対して同じことを､残高に対して逆のことをする｡Reload は取得を買い､
/// こちらはタイマーが既に買った取得を明かす｡件数を目にした読み手には､決して
/// 費やさずにそれを受け取る道があるわけだ｡そもそも差し出すかどうかはバーの
/// 仕事でこのバインドの仕事ではない — 保留が無ければアクションは no-op だ｡
const SHOW_NEW_POSTS: Shortcut = Shortcut {
    keystroke: "cmd-shift-r",
    context: Some(KEY_CONTEXT),
    bind: |keystroke, context| gpui::KeyBinding::new(keystroke, ShowNewPosts, context),
    item: |label| gpui::MenuItem::action(label, ShowNewPosts),
    menu_label: Some("Show New Posts"),
};

/// 先頭に貼り付く follow を切り替える (#22)｡
///
/// ラベルは状態ではなく言明である — このメニュー API から macOS へチェック
/// マークは渡らないので､どちらへ倒れたかは､リロード完了が使うのと同じバナー
/// で報告する｡どちらに倒しても何も費やさない: そもそもアプリが poll するか
/// どうかは `auto_refresh` のスイッチで､こちらではない｡
const TOGGLE_FOLLOW_NEW_POSTS: Shortcut = Shortcut {
    keystroke: "cmd-shift-f",
    context: Some(KEY_CONTEXT),
    bind: |keystroke, context| gpui::KeyBinding::new(keystroke, ToggleFollowNewPosts, context),
    item: |label| gpui::MenuItem::action(label, ToggleFollowNewPosts),
    menu_label: Some("Follow New Posts"),
};

/// ウィンドウが手元に無い間の透過を切り替える (#267)｡
///
/// 鍵は Stickies の Translucent (⌥⌘T) から借りた｡macOS で「ウィンドウを
/// 透かす」に既に割り当てられている鍵がそれで､同じ所作に同じ鍵を置く｡
/// [`TOGGLE_FOLLOW_NEW_POSTS`] と同じくラベルは状態ではなく言明で､どちらへ
/// 倒れたかはバナーが言う｡
const TOGGLE_TRANSLUCENT: Shortcut = Shortcut {
    keystroke: "cmd-alt-t",
    context: Some(KEY_CONTEXT),
    bind: |keystroke, context| gpui::KeyBinding::new(keystroke, ToggleTranslucent, context),
    item: |label| gpui::MenuItem::action(label, ToggleTranslucent),
    menu_label: Some("Translucent"),
};

/// すべてのバインドを､[`init`] が登録する順で並べたもの (#99)｡
///
/// [`init`] はまさにこの一覧を登録し､[`shortcuts`] はヘッダが宣伝するものへ
/// 絞り込み､[`menus`] は同じ項目からメニュー項目を描く｡#119 以降はどの項目
/// も自身のアクションを持つので､ここに足したショートカットは正しい先へ bind
/// されるか､まったく bind されないかのどちらかだ — 歩調を合わせるべき二つ目
/// の表はもう無い｡
///
/// どの項目がどのメニューに属すかは今も [`menus`] が選ぶので､`menu_label` を
/// 持ちながら `menus` に居場所の無い新しいショートカットはあり得る｡それを
/// 捕まえるのが `every_menu_labelled_shortcut_is_in_the_menu_bar` だ｡
///
/// **この配列から漏れた `Shortcut` は何にも bind されない**｡そして定数自体は
/// そのことを何も語らない — #109 と #22 が `MINIMIZE`､`CLOSE_WINDOW`､
/// `SCROLL_TO_TOP` をこの一覧以外のあらゆる場所へ足した後､三つがここで bind
/// されないまま座っていたのはそのためだ｡今それを捕まえるテストが
/// `every_menu_item_has_a_binding` である｡
const ALL_SHORTCUTS: [&Shortcut; 10] = [
    &RELOAD,
    &FOCUS_COMPOSER,
    &BLUR_COMPOSER,
    &QUIT,
    &MINIMIZE,
    &CLOSE_WINDOW,
    &SCROLL_TO_TOP,
    &SHOW_NEW_POSTS,
    &TOGGLE_FOLLOW_NEW_POSTS,
    &TOGGLE_TRANSLUCENT,
];

/// #58 のキーバインドを登録する｡起動時に一度､`gpui_component::init` (こちら
/// は自身のものを登録する) の隣で呼ばれる｡
///
/// **ここのどのバインドも裸の印字可能キーではない｡** この issue の中心的な
/// 危うさは､利用者が post を打っている最中に裸の `j`/`k`/`n` が発火すること
/// だ｡ここで bind されたものにそれはできない｡どのバインドも `cmd` を伴うか､
/// 何も打たない名前付きキー (`escape`) だからだ｡post の選択が入り裸の文字が
/// 持つに値するようになったら､composer のフォーカスが外すような二つ目のキー
/// コンテキストが要る — その起点が [`KEY_CONTEXT`] である｡
///
/// バインドを並べ直すのではなく [`ALL_SHORTCUTS`] を歩く (#119): 各項目は
/// [`Shortcut::bind`] を通して既に自身のアクションを名指しているので､
/// ショートカットとアクションが対にされる二つ目の場所は無い｡
///
/// キーコンテキストを持たない唯一の項目が [`QUIT`] だ (#99)｡他は timeline に
/// ついての問いに答えるものであり､答える view に属する｡終了はウィンドウの
/// 仕事ではなく､スコープすればフォーカスが他のどこかにあるときの `cmd-q` が
/// 何もしなくなる｡
///
/// **ここには post を送信するものは何も無い (#142)｡** `cmd-enter` は #58 か
/// ら送信していたが､結局みんなが手を伸ばすのは composer のボタンだけだと
/// 分かった｡素の `enter` は bind されたことがなく､今も bind しない｡削除を
/// 越えて残る理由がある: `enter` は改行を入れ続けねばならず､post は取り消せ
/// ない｡キーボードの経路がいつか戻るとしても､満たすべき制約はやはりこれだ｡
pub(crate) fn init(cx: &mut gpui::App) {
    cx.bind_keys(
        ALL_SHORTCUTS
            .iter()
            .map(|shortcut| (shortcut.bind)(shortcut.keystroke, shortcut.context)),
    );
}

/// アプリケーションのメニューバー (#99)｡ウィンドウが開く前に `main` が
/// 登録する｡
///
/// 各項目の key equivalent は [`init`] が登録した keymap から来るので､ここが
/// 名指すのはアクションと言い回しだけだ — キーストロークは決して名指さない｡
pub(crate) fn menus() -> Vec<gpui::Menu> {
    vec![
        gpui::Menu {
            name: "twigpui".into(),
            items: vec![
                gpui::MenuItem::action("About twigpui", ShowAbout),
                gpui::MenuItem::separator(),
            ]
            .into_iter()
            .chain(QUIT.menu_item())
            .collect(),
        },
        gpui::Menu {
            name: "File".into(),
            items: FOCUS_COMPOSER.menu_item().into_iter().collect(),
        },
        gpui::Menu {
            name: "View".into(),
            items: [
                RELOAD.menu_item(),
                // #248: Reload の隣｡どちらも押せば API リクエストを費やす
                // 操作で､こちらは確認ダイアログを挟む｡鍵が無いので
                // `Shortcut` ではなく素の項目 — `every_menu_item_has_a_binding`
                // が名指しで除いている｡
                Some(gpui::MenuItem::action("Sync List…", SyncList)),
                SHOW_NEW_POSTS.menu_item(),
                TOGGLE_FOLLOW_NEW_POSTS.menu_item(),
                SCROLL_TO_TOP.menu_item(),
            ]
            .into_iter()
            .flatten()
            .collect(),
        },
        // この名前は荷重を負っている (#109): gpui の macOS プラットフォーム
        // がメニューを AppKit の `setWindowsMenu_` へ渡すのは､それが厳密に
        // "Window" と呼ばれるときだけだ (`gpui/src/platform/mac/platform.rs`
        // の `create_menu_bar`)｡改名しても `cmd-w`/`cmd-m` は動き続けるが —
        // ただのバインドだからだ — そのメニューは macOS がウィンドウ一覧と
        // して扱うものではなくなる｡
        gpui::Menu {
            name: "Window".into(),
            items: [
                MINIMIZE.menu_item(),
                CLOSE_WINDOW.menu_item(),
                // #267: 常駐させるウィンドウの見え方｡Stickies が同じメニューに
                // 同じ対を置いているので､探す場所もそこになる｡
                Some(gpui::MenuItem::separator()),
                TOGGLE_TRANSLUCENT.menu_item(),
            ]
            .into_iter()
            .flatten()
            .collect(),
        },
    ]
}

/// 一つのショートカットに対応するメニュー項目｡[`Shortcut::menu_label`] が
/// 意図的にメニューバーから外していると言うなら､何も返さない (#99)｡
impl Shortcut {
    /// このショートカットのメニュー項目｡[`Shortcut::menu_label`] が意図的に
    /// メニューバーから外していると言うなら､何も返さない (#99)｡
    ///
    /// アクションの引数を取らない (#119): ラベルと同じ定数から来るので､
    /// 間違えうる二つ目の場所が無い｡
    fn menu_item(&self) -> Option<gpui::MenuItem> {
        self.menu_label.map(self.item)
    }
}

#[cfg(test)]
mod tests {
    use super::{ALL_SHORTCUTS, menus};

    // --- #58: キーボードショートカット ---

    #[test]
    fn no_shortcut_is_a_bare_letter() {
        // この issue の中心的な危うさ: 利用者が post を打っている最中に裸の
        // `j`/`k`/`n` が発火すること｡`keystroke` を比べるのは､それが `init`
        // の `KeyBinding::new` へ渡すものだからで､誰かが打っている間に gpui
        // がバインドを発火させるかを決めるのもそれだからだ｡
        //
        // `"escape"` は修飾キー無しでも許す: 名前付きの特殊キーであって､
        // 普通の打鍵が生む文字ではない｡
        for shortcut in ALL_SHORTCUTS {
            let keystroke = shortcut.keystroke;
            assert!(
                keystroke.starts_with("cmd-") || keystroke == "escape",
                "{keystroke} would fire while typing"
            );
        }
    }

    #[test]
    fn load_older_has_no_shortcut() {
        // 押すたびに有料のリクエスト一つで後ろへページする｡打ち間違いで金を
        // 使う鍵は便利ではない (#58)｡`menu_label` に対して確かめているのは､
        // #95 がヘッダのヒントの帯を､そして各ショートカットが持っていた人間
        // 向けの別ラベルを一緒に取り去ったからだ｡
        assert!(
            !ALL_SHORTCUTS.iter().any(|shortcut| shortcut
                .menu_label
                .is_some_and(|label| label.to_lowercase().contains("older"))),
            "\"Load older\" must not be bound"
        );
    }

    // --- #99: メニューバー ---

    /// メニューバーにあるすべてのアクション項目の名前｡サブメニューも含む｡
    fn menu_action_names() -> Vec<String> {
        menus()
            .into_iter()
            .flat_map(|menu| menu.items)
            .filter_map(|item| match item {
                gpui::MenuItem::Action { name, .. } => Some(name.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn sync_list_is_in_the_menu_bar_without_a_keystroke() {
        // #248: footer の入口をここへ移した｡手で同期を始めるのはまれで､
        // そのために footer の幅を使い続ける理由が無い｡鍵を持たないのは
        // `load_older_has_no_shortcut` と同じ理由 — 押すたびに課金する｡
        assert!(
            menu_action_names().iter().any(|name| name == "Sync List…"),
            "the way into the sync dialog has to be in the menu bar"
        );
        assert!(
            !ALL_SHORTCUTS
                .iter()
                .any(|shortcut| shortcut.menu_label == Some("Sync List…")),
            "Sync List… must not be bound to a key"
        );
    }

    #[test]
    fn every_menu_item_has_a_binding() {
        // 他のテストが見落としていた向き｡どれも `ALL_SHORTCUTS` から出発
        // して中身を確かめるので､その配列から漏れた `Shortcut` はどのテスト
        // からも見えない -- それでいて `menus()` には現れる｡あちらは定数を
        // 直に名指すからだ｡
        //
        // まさにそれが起きた: #109 の Minimize と Close Window､#22 の
        // Back to Top はメニューバーへは届き､keymap へは届かなかった｡だから
        // `cmd-m`､`cmd-w`､`cmd-up` は何もせず､macOS はどれの傍にも
        // key equivalent を描かなかった｡メニュー項目は自身のアクションを
        // 持つので働いた｡キーストロークには照合する先が無かった｡
        //
        // メニューから内側へ歩くことが､この漏れを見えるようにする｡
        let bound: Vec<&str> = ALL_SHORTCUTS
            .iter()
            .filter_map(|shortcut| shortcut.menu_label)
            .collect();

        for name in menu_action_names() {
            // 設計上ショートカットの無いメニュー項目｡規則で飛ばすのではなく
            // ここに書き下してあるので､三つ目がすり抜けることはない｡
            // `Sync List…` (#248) は API リクエストを費やすので､"Load older"
            // と同じ理由で鍵を持たない — `load_older_has_no_shortcut` を見よ｡
            if name == "About twigpui" || name == "Sync List…" {
                continue;
            }
            assert!(
                bound.contains(&name.as_str()),
                "{name} is in the menu bar but not in ALL_SHORTCUTS, so its keystroke is never bound"
            );
        }
    }

    #[test]
    fn no_keystroke_is_bound_twice() {
        // `no_key_is_bound_twice` はヘッダの四つを覆う｡こちらは `init` が
        // 登録するすべてのバインドを覆う｡ヘッダが宣伝しないもの (#99 の
        // `cmd-q`) も含み､グリフではなく gpui が実際に parse する
        // キーストロークを比べる｡
        let mut keystrokes: Vec<&str> = ALL_SHORTCUTS
            .iter()
            .map(|shortcut| shortcut.keystroke)
            .collect();
        keystrokes.sort_unstable();
        let before = keystrokes.len();
        keystrokes.dedup();
        assert_eq!(keystrokes.len(), before, "two actions share a keystroke");
    }

    #[test]
    fn every_menu_labelled_shortcut_is_in_the_menu_bar() {
        // #99 が防ごうとする drift: バインドがメニューラベルを得ながら
        // メニューには届かないこと｡
        //
        // 確かめるのはこの向きだけだ｡逆 — 裏にショートカットの無いメニュー
        // 項目 — は drift ではなく設計である: `menus()` は About も載せて
        // おり､これはバインドの無いアクションだ｡#95 が､各ショートカットが
        // ヘッダのヒントの帯のために持っていた人間向けの別ラベルを取り去る
        // までは #99 が名前で確かめていた｡件数で見ると本当の drift ではなく
        // About で落ちてしまう｡
        let names = menu_action_names();
        for label in ALL_SHORTCUTS
            .iter()
            .filter_map(|shortcut| shortcut.menu_label)
        {
            assert!(
                names.iter().any(|name| name == label),
                "{label} has a menu label but no menu item"
            );
        }
    }

    #[test]
    fn the_menu_bar_can_quit() {
        // #99 が存在するすべての理由: それ以前､アプリから出る唯一の道は
        // Dock のコンテキストメニューだった｡
        assert!(
            menu_action_names()
                .iter()
                .any(|name| name.to_lowercase().contains("quit")),
            "no menu item quits the app"
        );
    }

    #[test]
    fn the_window_menu_is_named_exactly_window() {
        // gpui がメニューを AppKit の `setWindowsMenu_` へ渡すのは名前が
        // 厳密に一致するときだけだ (#109)｡改名すると `cmd-w`/`cmd-m` は
        // 働いたまま､メニューだけが静かにただのメニューへ格下げされる｡
        // diff から誰かが気づく類の regression ではない｡
        assert!(
            menus().iter().any(|menu| menu.name.as_ref() == "Window"),
            "no menu is named \"Window\""
        );
    }

    #[test]
    fn the_window_menu_can_minimize_and_close() {
        let names = menu_action_names();
        for expected in ["Minimize", "Close Window"] {
            assert!(
                names.iter().any(|name| name == expected),
                "{expected} is missing from the menu bar"
            );
        }
    }

    #[test]
    fn the_window_menu_can_float_and_go_translucent() {
        // #267: Stickies が Window メニューに置いている対 (Floating Window /
        // Translucent) に倣う｡常駐させるウィンドウの居場所と見え方は､どちらも
        // ウィンドウの属性なので View ではなく Window に入る｡
        let window = menus()
            .into_iter()
            .find(|menu| menu.name.as_ref() == "Window")
            .expect("a Window menu");
        let names: Vec<String> = window
            .items
            .into_iter()
            .filter_map(|item| match item {
                gpui::MenuItem::Action { name, .. } => Some(name.to_string()),
                _ => None,
            })
            .collect();
        for expected in ["Float on Top", "Translucent"] {
            assert!(
                names.iter().any(|name| name == expected),
                "{expected} is missing from the Window menu: {names:?}"
            );
        }
    }

    #[test]
    fn no_menu_carries_a_keystroke_in_its_label() {
        // macOS は key equivalent を keymap から描く｡ラベルに "⌘R" と書け
        // ば画面に二度出るうえ､キーストロークが同期を保つべき二つ目のもの
        // になる (#99)｡
        for name in menu_action_names() {
            assert!(!name.contains('⌘'), "{name} spells out its own keystroke");
        }
    }
}
