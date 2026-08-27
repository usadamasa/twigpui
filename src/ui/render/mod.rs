//! 一つの timeline item を要素へ変える部品と､それらの要素が何と言うかを
//! 決める純粋関数｡
//!
//! `ui` から切り出した (#126) のは `src/ui.rs` がサイズの天井に達して
//! 余裕が無くなったからで､こちらは `TimelineView` の状態に一切触れない
//! 側だ: 行がすでに持っているデータの上の自由関数である｡ここのすべてが
//! `pub(crate)` ではなく `ui` の内側に留まるのは — 呼ぶのは `ui` だけであり､
//! 可視性を広げれば分割が台無しになるからだ｡
//!
//! reload を走らせてよい *かどうか* の判断と､失敗したとき何と言うかは
//! 代わりに [`super::reload_policy`] に住む｡
//!
//! # 4 つのファイル (#241)
//!
//! 1 ファイルだった `render.rs` を､部品が窓のどこに出るかで割った｡
//! 子は `pub(in crate::ui)` で書き､ここで再輸出するので､`ui` からの
//! 見え方は `render::xxx` のまま変わらない｡
//!
//! - [`frame`]: 窓の枠 — バナー､notice､toolbar の表題と segment､usage の
//!   行､composer のエラー行｡
//! - [`offers`]: どの操作を差し出すかの述語 — `offers_*` と `is_own_post`｡
//!   純粋関数で､テストはすべてここの対象｡
//! - [`actions`]: 操作の行そのもの — like / repost / reply / quote / open と
//!   リンクの chip｡どれもクリックのために `cx` を取る｡
//! - [`post`]: post の本文の部品 — byline､quote card､thread､media､
//!   avatar､件数､timestamp｡

use gpui::{InteractiveElement, SharedString};

// 子は `pub(super)`: 描画が使うものはここで再輸出するが､純粋関数のテスト
// (`ui/mod.rs` の `tests`) が引くだけのものは再輸出せず､`render::post::…`
// のように子を名指して引く｡再輸出だけして描画が使わない名前は
// `unused_imports` で落ちる｡
pub(super) mod actions;
pub(super) mod frame;
pub(super) mod offers;
pub(super) mod post;

pub(super) use actions::{
    author_link, like_row, link_row, open_post_link, quote_row, reply_row, reply_target_label,
    repost_row,
};
pub(super) use frame::{
    compose_error_message, header_title_element, notice, reload_notice_banner,
    session_notice_banner, sign_in_pill, tab_segment, tab_trough, usage_color, usage_label,
};
pub(super) use offers::{
    offers_delete, offers_like, offers_quote, offers_reauthorize, offers_reply, offers_repost,
};
pub(super) use post::{
    AVATAR_SIZE, MAX_RENDERED_MEDIA, MEDIA_CELL_HEIGHT, RowCounts, avatar_placeholder, byline,
    format_timestamp, media_badge, media_columns, quote_card, render_thread_chain,
    reply_banner_label, repost_banner_label, row_counts, thread_action_label, thread_toggle_row,
    with_count,
};

/// 一つの要素に､gpui とテストの双方が使える名前を一つ与える｡
///
/// これは accessibility ではなく *テスト* のための addressability だ｡
/// gpui 0.2.2 は accessibility tree をまったく持たない — AccessKit も
/// role も無く､X というボタンがどこにあるかをウィンドウへ尋ねる手段も
/// 無い — のでここから screen reader へ届くものは何も無く､ARIA 相当と
/// 呼ぶのは大幅に言い過ぎになる｡
///
/// crate が実際に持っているのは `debug_selector` で､テストが引ける名前
/// ([`gpui::VisualTestContext::debug_bounds`]) の下に要素が実際どこへ
/// 配置されたかを記録し､`cargo test` の外では何にもコンパイルされない｡
/// このモジュールの対話的な要素はどれもすでに一意な `.id(..)` を持って
/// いる; この trait が無いとテスト用に名前を付けるにはその文字列をもう
/// 一度書くことになり､一つの要素に二つの名前があれば､どちらかが編集され
/// た最初の瞬間にずれる｡`addressable` は一度だけ書く｡
///
/// そもそも bounds を持つ意義は #184 にある: テストはその中心をクリック
/// でき､それによって `dispatch_action` が飛ばす唯一の段である gpui の
/// hit test を､座標をどこにも書かずに assert 下へ置ける｡
pub(super) trait Addressable: InteractiveElement + Sized {
    /// この要素に gpui の対話性のための名前と､テストが引く名前を与える｡
    fn addressable(self, name: impl Into<SharedString>) -> gpui::Stateful<Self> {
        let name = name.into();
        // selector が先: これは `Self` を返すが､`id` は消費して
        // `Stateful` にする｡どちらも同じ `Interactivity` へ書くので､
        // 順序は他に何も変えない｡
        self.debug_selector({
            let name = name.clone();
            move || name.to_string()
        })
        .id(gpui::ElementId::Name(name))
    }
}

impl<E: InteractiveElement> Addressable for E {}
