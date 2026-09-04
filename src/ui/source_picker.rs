//! ツールバーの source picker (#43, #192): ウィンドウがどの timeline の
//! *集合* を見せるか､その選択が再起動をどう生き延びるか､pull-down の
//! メニュー項目がどこから名前を得るか｡
//!
//! #164 まではここが単一選択の segmented control だった｡所有リストが
//! 十数本あるアカウントでは既定幅 (429px) のツールバーが壊れ (#192)､
//! かつ「合成レーン」(#43) は複数選択そのものを要求する｡どちらも
//! macOS の pull-down button + チェックマーク付きメニューで一度に解決する
//! (他アプリ調査と HIG の根拠は `PLAN.md` を見よ)｡
//!
//! 並びは [`super::list_sync`] と同じ: まず純粋な関数とそのテスト､続いて
//! ウィンドウに触れるかリクエストを使う部分の `impl TimelineView`
//! ブロック｡
//!
//! # 金がかかるのはどこか
//!
//! トグルは即座にキャッシュから再合成するだけで何も送らない｡一度も
//! 読まれていない source — キャッシュファイルがそもそも無い場合 — だけが
//! reload に落ちる｡それは初回起動が出すのと同じリクエストだ｡すでに読んだ
//! source の間を行き来する分には､何度やってもリクエストは 0 だ｡
//! `ui` のテストの `switching_between_cached_sources_sends_nothing` が
//! それを押さえている｡
//!
//! 区画に名前を付けるのはリクエストを 1 つ使う: `GET
//! /2/users/:id/owned_lists` は返された list ごとに課金される
//! (`x-api-budget`)｡ウィンドウの独断で送られることは決してない — メニューは
//! いくらかかるかを言うボタンを末尾に出し､結果は TTL 無しでキャッシュ
//! されるので､再び使う唯一の道はもう一度ボタンを押すことだ｡
//!
//! # 起動時に何が勝つか
//!
//! 保存された `active` (無ければ `selected`) が `config.list_id` に勝ち､
//! したがって `X_LIST_ID` にも勝つ｡これは通常の「ファイルより環境変数」の
//! ルールの逆であり､意図的だ: dev プロファイルは常に既定の list を持つので
//! (`Profile::default_list_id`)､設定が勝てば dev ビルドは起動のたびにその
//! list へ戻り､picker の選択がウィンドウより長生きすることは決してなくなる｡
//! 設定は誰も選ばなかったときのウィンドウの *出発点* であり､選択はより後の､
//! より具体的な決定だ｡

use std::path::Path;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

// `use super::*` ではなく書き下している｡理由は [`super::list_sync`] と同じ｡
use super::{
    Context, ReloadNotice, ReloadTrigger, Startup, TimelineState, TimelineView, lane, log, oauth,
};
use crate::cache::{self, TimelineSource};
use crate::paths::Paths;
use crate::x_api::ListSummary;

/// picker が起動をまたいで覚えていること: ウィンドウが最後に切り替えた
/// ときに表示していた timeline だ｡
///
/// [`TimelineSource`] に `Serialize` を付けるのではなく専用の型にしてある｡
/// こちらはディスクに書かれ､後のビルドが読み戻すからだ｡cache モジュールの
/// 履歴 (#97､schema バージョンを足して外した) が警告になっている:
/// ディスク上の形は､内部の enum の derive が今日たまたま吐くものであっては
/// ならない｡`kind` タグは､3 つ目の variant が list id と取り違えられるのを
/// 防ぐ｡
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Selection {
    /// `GET /2/users/:id/timelines/reverse_chronological` — Home｡
    Home,
    /// この list に対する `GET /2/lists/:id/tweets`｡
    List {
        /// list の id｡数字だけ｡
        id: String,
    },
}

impl Selection {
    /// `source` を名指す selection｡
    pub(super) fn of(source: &TimelineSource) -> Self {
        match source {
            TimelineSource::Home => Self::Home,
            TimelineSource::List(id) => Self::List { id: id.clone() },
        }
    }

    /// この selection が名指す source｡ファイルの list id が
    /// `Config::resolve` なら受け付けないものなら `None` — 手で編集された
    /// ファイルは､書いてある内容からリクエスト URL を組むのではなく
    /// フォールバックすべきだ｡
    pub(super) fn into_source(self) -> Option<TimelineSource> {
        match self {
            Self::Home => Some(TimelineSource::Home),
            Self::List { id } => (!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
                .then_some(TimelineSource::List(id)),
        }
    }
}

/// ドロップダウンの開閉 (#192, #43)｡`bool` ではなく専用の 2 値 enum に
/// してある — `TimelineView` はすでに clippy の `struct_excessive_bools`
/// (上限 3) に達する本数の `bool` フィールドを持っており､これ以上増やさない｡
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum SourcePickerVisibility {
    #[default]
    Closed,
    Open,
}

impl SourcePickerVisibility {
    pub(super) fn is_open(self) -> bool {
        matches!(self, Self::Open)
    }

    pub(super) fn toggled(self) -> Self {
        match self {
            Self::Closed => Self::Open,
            Self::Open => Self::Closed,
        }
    }
}

/// [`Paths::selection_file`] の中身すべて｡
///
/// `selected` は #164 が単一選択だった頃の名残で､`active` (#43 の複数選択)
/// が空のときのフォールバックとしてだけ読む｡新しいビルドは `active` に
/// 現在の集合を書き､互換のため先頭要素を `selected` へも鏡写しする｡
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct SelectionState {
    /// 最後に切り替えた先の timeline (後方互換用)｡`active` が空のときだけ読む｡
    #[serde(default)]
    pub selected: Option<Selection>,
    /// 表示中の source 集合 (#43)｡空なら `selected` にフォールバックする｡
    #[serde(default)]
    pub active: Vec<Selection>,
}

/// picker が保存した選択を `path` から読み戻す｡
///
/// [`crate::sync::load_state`] と同じく失敗しない｡理由も同じだ: これを
/// 失う代償はクリック 1 回なので､ファイルが無い・壊れている場合は
/// ウィンドウが開くのを止めるエラーではなく既定値になる｡
pub(crate) fn load_selection(path: &Path) -> SelectionState {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return SelectionState::default();
    };
    serde_json::from_str(&contents).unwrap_or_default()
}

/// picker の選択を `path` へ書く｡
pub(crate) fn save_selection(path: &Path, state: &SelectionState) -> Result<()> {
    let json = serde_json::to_string_pretty(state).context("could not serialize the selection")?;
    std::fs::write(path, json).with_context(|| format!("could not write {}", path.display()))
}

/// ウィンドウがどの timeline の集合で開くか (#161, #164, #43)｡
///
/// まず保存された `active`､次に保存された `selected` (後方互換)､次に
/// `config.list_id`､最後に Home — ファイルが設定に勝つ理由はモジュール doc を
/// 参照｡保存された list は､アカウントが今もそれを所有しているかどうかに
/// 関わらず尊重される: どの list id でも読めるし､#161 の設定された list も
/// 所有している必要は無い｡`active` の要素に不正な list id (手編集など) しか
/// 無く `filter_map` の結果が空になった場合も､次の段 (`selected`) へ
/// フォールバックする — 非空 invariant を守るためにここで panic はしない｡
pub(super) fn initial_sources(
    state: SelectionState,
    configured_list_id: Option<&str>,
) -> Vec<TimelineSource> {
    let active: Vec<TimelineSource> = state
        .active
        .into_iter()
        .filter_map(Selection::into_source)
        .collect();
    if !active.is_empty() {
        return active;
    }
    if let Some(source) = state.selected.and_then(Selection::into_source) {
        return vec![source];
    }
    match configured_list_id {
        Some(list_id) => vec![TimelineSource::List(list_id.to_string())],
        None => vec![TimelineSource::Home],
    }
}

/// `startup` で起動するウィンドウが尊重すべき保存された選択: live な
/// ウィンドウならファイルのもの､fixture なら無し｡fixture は定義上毎回
/// 同じ画面であり (`fixture-visual-check`)､前回の live 実行が残した state
/// ファイルが､どの区画を持ち上げて描くかを変えられてはならない｡書き込み側も
/// 同じように塞いである｡`TimelineView::selection_file` が `None` になる
/// ことによってだ｡
pub(super) fn saved_selection_for(startup: &Startup, paths: &Paths) -> SelectionState {
    match startup {
        Startup::Live => load_selection(&paths.selection_file()),
        Startup::Fixture(_) => SelectionState::default(),
    }
}

/// `sources` に `target` を足す・外す (#43)｡非空 invariant: 最後の 1 つは
/// 外せない — そのクリックは無視する｡表示順を保つため､足すのは末尾へ
/// 追加ではなく `segments` が持つ順序に合わせて呼び出し側が並べ直す
/// (`TimelineView::toggle_source` を見よ)｡
pub(super) fn toggle(
    mut sources: Vec<TimelineSource>,
    target: &TimelineSource,
) -> Vec<TimelineSource> {
    match sources.iter().position(|source| source == target) {
        Some(index) if sources.len() > 1 => {
            sources.remove(index);
        }
        Some(_) => {}
        None => sources.push(target.clone()),
    }
    sources
}

/// ツールバーのトリガーが言うこと (#192, #43)｡1 件なら名前をそのまま､
/// 複数なら表示順で先頭の名前 + `+N`｡`owned` から名前を引けない source
/// (所有していない list) は `segment_label` 相当のフォールバックにはせず
/// id をそのまま出す — トリガーは常に何か読めるものを返す必要がある｡
pub(super) fn trigger_label(sources: &[TimelineSource], owned: &[ListSummary]) -> String {
    let name = |source: &TimelineSource| match source {
        TimelineSource::Home => "Home".to_string(),
        TimelineSource::List(id) => owned
            .iter()
            .find(|list| &list.id == id)
            .map_or_else(|| id.clone(), segment_label),
    };
    match sources {
        [] => String::new(),
        [only] => name(only),
        [first, rest @ ..] => format!("{} +{}", name(first), rest.len()),
    }
}

/// picker が最後に取得した list｡一度も取得していなければ空 — キャッシュ
/// ファイルが読めなかった場合もログ 1 行を残して空だ｡どの list にも名前を
/// 付けられない picker にも Home の区画と残りを取得するボタンはあるので､
/// 読めないキャッシュはウィンドウを失敗させるほどのものではない｡
pub(super) fn cached_lists_or_empty(paths: &Paths) -> Vec<ListSummary> {
    match cache::cached_owned_lists(paths) {
        Ok(lists) => lists.unwrap_or_default(),
        Err(error) => {
            log::warn(&format!("could not read the cached lists: {error:#}"));
            Vec::new()
        }
    }
}

/// picker の区画 1 つ: 何と呼ばれるか､何へ切り替えるか､そしてそれが今
/// 表示中のものかどうか｡
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Segment {
    /// ウィンドウのテストがこの区画を見つけるための要素名
    /// (`render::Addressable`)｡
    pub name: String,
    /// 区画が言うこと｡
    pub label: String,
    /// クリックしたときウィンドウが切り替わる先｡
    pub source: TimelineSource,
    /// これが今表示中の timeline かどうか｡
    pub selected: bool,
}

/// picker の区画を描画順に並べたもの: Home､次に所有する list を API が
/// 並べた順に､そして — 表示中の source に､ウィンドウが所有していない list
/// (通常は #161 の設定された list) が混ざっているときだけ､その list｡選択中の
/// 区画が必ず存在し､メニューから見失われないようにするためだ｡
pub(super) fn segments(current: &[TimelineSource], owned: &[ListSummary]) -> Vec<Segment> {
    let mut segments = vec![segment(TimelineSource::Home, "Home".to_string(), current)];
    for list in owned {
        segments.push(segment(
            TimelineSource::List(list.id.clone()),
            segment_label(list),
            current,
        ));
    }
    for source in current {
        if let TimelineSource::List(id) = source
            && !owned.iter().any(|list| &list.id == id)
        {
            segments.push(segment(source.clone(), "List".to_string(), current));
        }
    }
    segments
}

fn segment(source: TimelineSource, label: String, current: &[TimelineSource]) -> Segment {
    Segment {
        name: segment_name(&source),
        selected: current.contains(&source),
        label,
        source,
    }
}

/// `source` へ切り替える区画の要素名｡
pub(super) fn segment_name(source: &TimelineSource) -> String {
    match source {
        TimelineSource::Home => "tab-home".to_string(),
        TimelineSource::List(id) => format!("tab-list-{id}"),
    }
}

/// list の区画が言うこと: その名前､API が名前を寄越さなかったときは id
/// (それがそもそも許される理由は `ListSummary::name` の doc にある)｡id は
/// 見苦しいラベルだが使える｡空白の区画はどちらでもない｡
pub(super) fn segment_label(list: &ListSummary) -> String {
    if list.name.trim().is_empty() {
        list.id.clone()
    } else {
        list.name.clone()
    }
}

/// ツールバーが list を取得するボタンを出すかどうか: 金を使うための
/// client と､問い合わせる id の両方があるときだけだ｡fixture の
/// ウィンドウはどちらも持たない｡それが fixture を無課金に保っている｡
pub(super) fn offers_list_fetch(has_client: bool, user_known: bool) -> bool {
    has_client && user_known
}

/// 取得ボタンの文言｡リクエストを送るクリックについての `x-api-budget` の
/// ルールに従い､静止している状態では必ず値段を示す｡
pub(super) fn lists_button_label(has_lists: bool, fetching: bool) -> &'static str {
    if fetching {
        "Loading lists…"
    } else if has_lists {
        "Refresh lists (1 request)"
    } else {
        "Load lists (1 request)"
    }
}

/// 切り替えが起動の完了を待たねばならないかどうか｡
///
/// `start` は渡された source のキャッシュを読み､その結果を画面へ出す｡
/// その途中で切り替えると､起動時の行が新しい区画の下に現れてしまう｡
/// 見分け方は､client がまだ無い状態の `Loading` だ — どちらかが変われば
/// 起動は落ち着いている｡
pub(super) fn switch_waits_for_startup(state: &TimelineState, has_client: bool) -> bool {
    matches!(state, TimelineState::Loading) && !has_client
}

impl TimelineView {
    /// `target` を表示中の source 集合へ足す・外す (#164, #43)｡最後の 1 つは
    /// 外せない — [`toggle`] の非空 invariant がそのクリックを無視する｡
    ///
    /// 集合が変わったら前の集合に属していたものはすべて一緒に消える: 飛行中
    /// の reload や "Load older" (その結果が誤った source の下に着地して
    /// しまう)､ページングカーソル (複数選択では意味を持たない — §3.6)､
    /// poll のバッファ (`clear_pending` の doc)､開いているスレッド､そして
    /// スクロール位置｡そのうえでキャッシュ済みの分だけ即座に再合成して
    /// 画面へ出し (off にした分は消え､on にした分は載る)､一度も取得して
    /// いない source だけを reload する — 画面は空にせず
    /// `reloading` フラグとスピナーだけで示す｡
    ///
    /// auto-refresh のループは動かしたままにせず再起動する:
    /// ループは開始時点の `sources` を
    /// 捕まえており､再起動しないと off にした source を poll し続けて
    /// しまう — #43 の完了条件「オフのソースが API リクエストを消費
    /// しない」への違反になる｡
    pub(super) fn toggle_source(&mut self, target: &TimelineSource, cx: &mut Context<'_, Self>) {
        if switch_waits_for_startup(&self.state, self.client.is_some()) {
            return;
        }
        let next = toggle(self.sources.clone(), target);
        if next == self.sources {
            // 非空 invariant で無視された (最後の 1 つを外そうとした)｡
            return;
        }
        self.sources = next;
        self.fetch = None;
        self.reloading = false;
        self.reload_notice = None;
        // §3.6: 複数選択のあいだ `next_page_token` は常に `None` を保つ
        // 不変条件｡`reload_sources` も N > 1 のときは書かないが､ここでも
        // 切り替わった瞬間に明示して二重に守る｡
        if self.sources.len() != 1 {
            self.next_page_token = None;
        }
        self.clear_pending();
        self.threads.clear();
        self.thread_fetches.clear();
        self.list_scroll.scroll_to_top_of_item(0);

        // 読み取り側と同じように塞いである: fixture の区画は存在しない
        // list を名指しており､それを覚えると次の live 起動が 404 を
        // reload しに行くことになる｡
        if let Some(selection_file) = &self.selection_file {
            let remembered = SelectionState {
                selected: self.sources.first().map(Selection::of),
                active: self.sources.iter().map(Selection::of).collect(),
            };
            if let Err(error) = save_selection(selection_file, &remembered) {
                log::warn(&format!(
                    "could not remember the selected timeline: {error:#}"
                ));
            }
        }

        if let Some(user_id) = self.home_user_id.clone() {
            let composed = lane::load_composite_timeline(&self.paths, &self.sources, &user_id);
            self.item_provenance = composed.provenance;
            self.state = TimelineState::Loaded(composed.items);
            cx.notify();
            self.fill_missing_sources(&user_id, ReloadTrigger::UserAction, cx);
        }
        // #43: off にしたぶんを二度と poll しないよう必ず再起動する｡
        self.start_auto_refresh(cx);
        // `state` を差し替えた後で｡理由は `start` と同じ (#120)｡
        self.refresh_images(cx);
        cx.notify();
    }

    /// list に名前を与える 1 回のリクエストを使い (#164)､返ってきたものを
    /// キャッシュする｡すでに 1 つ飛行中なら拒否する｡
    pub(super) fn fetch_owned_lists(&mut self, cx: &mut Context<'_, Self>) {
        if self.lists_fetch.is_some() {
            return;
        }
        let (Some(client), Some(user_id)) = (self.client.clone(), self.home_user_id.clone()) else {
            return;
        };
        let paths = self.paths.clone();

        self.lists_fetch = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let now = oauth::unix_now();
                    let (lists, next_token) = client.owned_lists(&paths, &user_id, None, now)?;
                    if next_token.is_some() {
                        // 1 ページが picker の語彙のすべてだ —
                        // `XClient::owned_lists` を参照｡黙って切り詰めず
                        // 声に出して言う｡
                        log::warn("the account owns more lists than one page holds; the picker shows the first 100");
                    }
                    cache::save_owned_lists(&paths, &lists, now)?;
                    anyhow::Ok(lists)
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.lists_fetch = None;
                this.refresh_usage(cx);
                match result {
                    Ok(lists) => this.owned_lists = lists,
                    Err(error) => {
                        log::error(&format!("could not load the owned lists: {error:#}"));
                        this.reload_notice = Some(ReloadNotice::Failed(
                            format!("Could not load lists: {error:#}").into(),
                        ));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(id: &str, name: &str) -> ListSummary {
        ListSummary {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("twigpui-selection-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("selection.json")
    }

    // --- 保存された選択 ---

    #[test]
    fn a_selection_round_trips_through_its_file() {
        let path = scratch("roundtrip");
        let state = SelectionState {
            selected: None,
            active: vec![Selection::List {
                id: "2091351590695588200".to_string(),
            }],
        };
        save_selection(&path, &state).unwrap();
        assert_eq!(load_selection(&path), state);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn the_file_names_its_kind_rather_than_leaking_an_enum_shape() {
        // ディスク上の形は後のビルドとの契約なので､derive が吐くものに
        // 任せずここで固定する｡
        let path = scratch("shape");
        save_selection(
            &path,
            &SelectionState {
                selected: Some(Selection::Home),
                active: Vec::new(),
            },
        )
        .unwrap();
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["selected"]["kind"], "home");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_missing_selection_file_is_the_default() {
        let path = scratch("missing");
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_selection(&path), SelectionState::default());
    }

    #[test]
    fn a_corrupt_selection_file_is_the_default() {
        // 選択を失う代償はクリック 1 回｡それでウィンドウを失敗させるほうが
        // 高くつく｡
        let path = scratch("corrupt");
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(load_selection(&path), SelectionState::default());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_legacy_file_without_active_still_reads_as_one_source() {
        // #43 より前の形 (`active` キー無し) を新しいビルドが読めることの
        // テスト｡`#[serde(default)]` が空 `Vec` を補う｡
        let path = scratch("legacy");
        std::fs::write(
            &path,
            br#"{"selected":{"kind":"list","id":"2091351590695588200"}}"#,
        )
        .unwrap();
        let state = load_selection(&path);
        assert_eq!(state.active, Vec::new());
        assert_eq!(
            initial_sources(state, None),
            vec![TimelineSource::List("2091351590695588200".to_string())]
        );
        std::fs::remove_file(&path).unwrap();
    }

    // --- ウィンドウがどの timeline の集合で開くか ---

    fn state(selected: Option<Selection>, active: Vec<Selection>) -> SelectionState {
        SelectionState { selected, active }
    }

    #[test]
    fn nothing_configured_and_nothing_saved_reads_the_home_timeline() {
        assert_eq!(
            initial_sources(state(None, Vec::new()), None),
            vec![TimelineSource::Home]
        );
    }

    #[test]
    fn a_configured_list_replaces_the_home_timeline() {
        // 補うのではなく置き換える (#161): #157 の後､home の timeline には
        // フォールバックする価値のあるものが残っていない｡
        assert_eq!(
            initial_sources(state(None, Vec::new()), Some("2091351590695588200")),
            vec![TimelineSource::List("2091351590695588200".to_string())]
        );
    }

    #[test]
    fn a_saved_active_set_beats_the_configured_list() {
        // dev プロファイルは常に list を設定する｡それが勝てば dev ビルドは
        // 起動のたびに picker の選択を忘れる｡
        assert_eq!(
            initial_sources(
                state(
                    None,
                    vec![
                        Selection::Home,
                        Selection::List {
                            id: "7".to_string()
                        }
                    ]
                ),
                Some("2091351590695588200")
            ),
            vec![TimelineSource::Home, TimelineSource::List("7".to_string())]
        );
    }

    #[test]
    fn a_saved_selected_beats_the_configured_list_when_active_is_empty() {
        // `active` が空なら旧フィールド `selected` へフォールバックする｡
        assert_eq!(
            initial_sources(
                state(
                    Some(Selection::List {
                        id: "7".to_string()
                    }),
                    Vec::new()
                ),
                Some("2091351590695588200")
            ),
            vec![TimelineSource::List("7".to_string())]
        );
        assert_eq!(
            initial_sources(
                state(Some(Selection::Home), Vec::new()),
                Some("2091351590695588200")
            ),
            vec![TimelineSource::Home]
        );
    }

    #[test]
    fn a_saved_list_id_that_is_not_digits_falls_back_to_the_configuration() {
        // `Config::resolve` が `list_id` に適用するのと同じルールを､
        // 人が手で編集できるファイルに適用する｡
        assert_eq!(
            initial_sources(
                state(
                    None,
                    vec![Selection::List {
                        id: "not-a-list".to_string()
                    }]
                ),
                Some("2091351590695588200")
            ),
            vec![TimelineSource::List("2091351590695588200".to_string())]
        );
        assert_eq!(
            initial_sources(
                state(None, vec![Selection::List { id: String::new() }]),
                None
            ),
            vec![TimelineSource::Home]
        );
    }

    #[test]
    fn an_active_set_with_only_invalid_ids_falls_back_rather_than_panicking() {
        // `filter_map` の結果が空になっても非空
        // invariant は panic せず次の段 (`selected`) へフォールバックする｡
        assert_eq!(
            initial_sources(
                state(
                    Some(Selection::Home),
                    vec![Selection::List {
                        id: "not-a-list".to_string()
                    }]
                ),
                None
            ),
            vec![TimelineSource::Home]
        );
    }

    #[test]
    fn a_selection_names_the_source_it_was_taken_from() {
        assert_eq!(Selection::of(&TimelineSource::Home), Selection::Home);
        assert_eq!(
            Selection::of(&TimelineSource::List("7".to_string())),
            Selection::List {
                id: "7".to_string()
            }
        );
    }

    // --- トグル (#43) ---

    #[test]
    fn toggling_an_absent_source_appends_it() {
        let sources = toggle(
            vec![TimelineSource::Home],
            &TimelineSource::List("1".to_string()),
        );
        assert_eq!(
            sources,
            vec![TimelineSource::Home, TimelineSource::List("1".to_string())]
        );
    }

    #[test]
    fn toggling_a_present_source_removes_it() {
        let sources = toggle(
            vec![TimelineSource::Home, TimelineSource::List("1".to_string())],
            &TimelineSource::List("1".to_string()),
        );
        assert_eq!(sources, vec![TimelineSource::Home]);
    }

    #[test]
    fn the_last_source_cannot_be_toggled_off() {
        let sources = toggle(vec![TimelineSource::Home], &TimelineSource::Home);
        assert_eq!(sources, vec![TimelineSource::Home]);
    }

    // --- トリガーのラベル (#192, #43) ---

    #[test]
    fn a_single_selection_shows_its_own_name() {
        assert_eq!(
            trigger_label(&[TimelineSource::Home], &[]),
            "Home".to_string()
        );
        assert_eq!(
            trigger_label(
                &[TimelineSource::List("1".to_string())],
                &[list("1", "rust")]
            ),
            "rust".to_string()
        );
    }

    #[test]
    fn multiple_selections_summarize_as_the_first_name_plus_a_count() {
        assert_eq!(
            trigger_label(
                &[TimelineSource::Home, TimelineSource::List("1".to_string())],
                &[list("1", "rust")]
            ),
            "Home +1".to_string()
        );
        assert_eq!(
            trigger_label(
                &[
                    TimelineSource::Home,
                    TimelineSource::List("1".to_string()),
                    TimelineSource::List("2".to_string())
                ],
                &[list("1", "rust"), list("2", "art")]
            ),
            "Home +2".to_string()
        );
    }

    // --- 区画 ---

    #[test]
    fn home_comes_first_then_the_owned_lists_in_api_order() {
        let current = [TimelineSource::List("2".to_string())];
        let segments = segments(&current, &[list("2", "second"), list("1", "first")]);
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Home", "second", "first"]
        );
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.selected)
                .collect::<Vec<_>>(),
            vec![false, true, false]
        );
        assert_eq!(segments[1].name, "tab-list-2");
        assert_eq!(segments[0].name, "tab-home");
        assert_eq!(segments[0].source, TimelineSource::Home);
    }

    #[test]
    fn multiple_selected_sources_are_all_checked() {
        let current = [TimelineSource::Home, TimelineSource::List("1".to_string())];
        let segments = segments(&current, &[list("1", "mine"), list("2", "other")]);
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.selected)
                .collect::<Vec<_>>(),
            vec![true, true, false]
        );
    }

    #[test]
    fn a_list_the_account_does_not_own_still_gets_a_segment_while_showing() {
        // #161 の設定された list は所有しているものである必要が無い｡
        // これが無いとメニューに持ち上がった区画が 1 つも無くなる｡
        let current = [TimelineSource::List("9".to_string())];
        let segments = segments(&current, &[list("1", "mine")]);
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[2].label, "List");
        assert!(segments[2].selected);
        assert_eq!(segments[2].source, current[0]);
    }

    #[test]
    fn with_no_lists_cached_the_picker_is_just_home() {
        let segments = segments(&[TimelineSource::Home], &[]);
        assert_eq!(segments.len(), 1);
        assert!(segments[0].selected);
    }

    #[test]
    fn a_nameless_list_is_labelled_by_its_id() {
        assert_eq!(segment_label(&list("7", "")), "7");
        assert_eq!(segment_label(&list("7", "   ")), "7");
        assert_eq!(segment_label(&list("7", "rust")), "rust");
    }

    // --- 取得ボタン ---

    #[test]
    fn the_fetch_is_offered_only_with_a_client_and_a_known_user() {
        assert!(offers_list_fetch(true, true));
        assert!(!offers_list_fetch(false, true), "a fixture window");
        assert!(!offers_list_fetch(true, false), "before /me has resolved");
    }

    #[test]
    fn the_fetch_button_names_its_price() {
        assert_eq!(lists_button_label(false, false), "Load lists (1 request)");
        assert_eq!(lists_button_label(true, false), "Refresh lists (1 request)");
        assert_eq!(lists_button_label(true, true), "Loading lists…");
    }

    #[test]
    fn a_fixture_window_ignores_the_saved_selection() {
        // 前回の live 実行が state ディレクトリに何を残していようと､
        // 毎回同じ画面になる｡
        let home = std::env::temp_dir().join("twigpui-selection-fixture");
        let home_str = home.display().to_string();
        let paths = Paths::from_vars(move |key| (key == "HOME").then(|| home_str.clone())).unwrap();
        paths.ensure_dirs().unwrap();
        save_selection(
            &paths.selection_file(),
            &SelectionState {
                selected: Some(Selection::Home),
                active: Vec::new(),
            },
        )
        .unwrap();

        assert_eq!(
            saved_selection_for(&Startup::Live, &paths).selected,
            Some(Selection::Home)
        );
        let fixture = crate::fixture::Fixture {
            signed_in_as: crate::fixture::FixtureUser {
                id: "1".to_string(),
                username: "a".to_string(),
            },
            items: Vec::new(),
            pending: Vec::new(),
            lists: Vec::new(),
            sync: None,
            sources: Vec::new(),
            list_items: std::collections::BTreeMap::new(),
            picker_open: false,
            liked: Vec::new(),
            reposted: Vec::new(),
        };
        assert_eq!(
            saved_selection_for(&Startup::Fixture(Box::new(fixture)), &paths),
            SelectionState::default()
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn a_switch_waits_only_while_startup_has_neither_client_nor_screen() {
        assert!(switch_waits_for_startup(&TimelineState::Loading, false));
        assert!(!switch_waits_for_startup(&TimelineState::Loading, true));
        assert!(!switch_waits_for_startup(
            &TimelineState::Loaded(Vec::new()),
            false
        ));
        assert!(!switch_waits_for_startup(
            &TimelineState::NotAuthenticated,
            false
        ));
    }
}
