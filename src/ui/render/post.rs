//! post の本文の部品 (#241): byline､quote card､thread の chain､media の
//! 寸法と badge､avatar の placeholder､x.com への URL､エンゲージメントの
//! 件数､timestamp｡

use chrono::DateTime;
use chrono_tz::Asia::Tokyo;
use gpui::{Pixels, Size, size};

use crate::ui::*;

/// `@name`｡expansion に著者が居なければ何も返さない — 裸の `@` は壊れた
/// 行に見えてしまう｡
pub(in crate::ui) fn byline(author_username: &str) -> String {
    if author_username.is_empty() {
        String::new()
    } else {
        format!("@{author_username}")
    }
}

/// "@name reposted"､repost したユーザーの screen name が expansion に
/// 無ければ "Reposted" だけ — 裸の `@` を描くのではなく [`byline`] の
/// 著者不在時の fallback を写している｡
pub(in crate::ui) fn repost_banner_label(reposted_by: &str) -> String {
    if reposted_by.is_empty() {
        "Reposted".to_string()
    } else {
        format!("@{reposted_by} reposted")
    }
}

/// quote 元を､quote 自身のテキストの下に枠付きのカードとして埋め込む
/// (#13)｡塗りは新しい色のスロットを足さず `bg_header` を使い回す — これ
/// はすでにアプリの「区別された領域」の背景 (ヘッダのバー) であり､カード
/// は `theme.bg` の上に直に載るので､自前のパレット項目を持たずとも明確に
/// 別のブロックとして読める｡
///
/// `media` は quote された post のサムネイルの格子 (#123)｡カードが読む
/// 対象ではなく小さな preview である場所では `None` になる: composer の
/// "replying to" と "quoting" の帯はどちらも､画像がすでに画面にある行の
/// 直下に座る｡
pub(in crate::ui) fn quote_card(
    quoted: &QuotedPost,
    theme: Theme,
    media: Option<AnyElement>,
) -> impl IntoElement {
    let byline = byline(&quoted.author_username);

    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .mt_1()
        .rounded(theme::RADIUS_CONTROL)
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.bg_header))
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .child(quoted.author_name.clone()),
                )
                .child(div().text_color(rgb(theme.text_muted)).child(byline)),
        )
        .child(div().child(quoted.text.clone()))
        .children(media)
}

/// "Replying to @name"｡親の著者が解決できなければ (削除済み､保護済み､
/// あるいは単に expand されていない) 一般的な fallback になる — 裸の
/// "Replying to @" を描くのではなく [`repost_banner_label`] の著者不在時
/// の fallback を写している (#12)｡
pub(in crate::ui) fn reply_banner_label(replied_to: &RepliedTo) -> String {
    if replied_to.author_username.is_empty() {
        "Replying to a post".to_string()
    } else {
        format!("Replying to @{}", replied_to.author_username)
    }
}

/// [`thread_toggle_row`] のクリックできるラベル｡いまの状態 (まだ何も
/// 読めていないが fetch は走っている) に toggle が無ければ `None` — その
/// 場合は代わりに `TimelineView::thread_section` が素の "Loading thread…"
/// の notice を描く｡`state: None` は「一度も要求していない」で (取得を
/// 差し出し､#12 の「費用は予測できねばならない」要件に従い最悪の費用を
/// 先に明示する); `Some(Failed(_))` は再試行を差し出す｡
pub(in crate::ui) fn thread_action_label(state: Option<&ThreadFetchState>) -> Option<&'static str> {
    match state {
        None => Some("Show thread (up to 5 requests)"),
        Some(ThreadFetchState::Failed(_)) => Some("Retry"),
        Some(ThreadFetchState::Loading | ThreadFetchState::Loaded(_)) => None,
    }
}

/// クリックできる "Show thread" / "Retry" の行 (#12)｡`load_older_row`
/// と同じ体裁だ — すでに描かれた post に対する二次的な操作なので､完全な
/// ボタンではなくリンク色のクリックできる行にしてある｡
pub(in crate::ui) fn thread_toggle_row(
    reply_post_id: String,
    first_parent_id: String,
    label: &str,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> impl IntoElement {
    div()
        .addressable(format!("show-thread-{reply_post_id}"))
        .text_color(rgb(theme.accent))
        .cursor_pointer()
        .child(label.to_string())
        .on_click(cx.listener(move |this, _event, _window, cx| {
            this.show_thread(reply_post_id.clone(), first_parent_id.clone(), cx);
        }))
}

/// 組み上がった親の chain (#12)｡最も古い祖先が先頭で､このファイルに
/// すでにある他の「埋め込まれた post」の扱いと見た目を揃えるため､どれも
/// [`quote_card`] と同じように描く｡空で cap もされていない chain は､
/// 最初の親の fetch が何も見つけなかったとき (削除済み､保護済み､その他
/// 不在) にだけ起きる — #12 の「まともに描けねばならない」要件だ — ので､
/// その場合は黙って何も出さず専用のメッセージを出す｡
pub(in crate::ui) fn render_thread_chain(chain: &ThreadChain, theme: Theme) -> AnyElement {
    if chain.items.is_empty() && !chain.capped {
        return div()
            .text_color(rgb(theme.text_muted))
            .child("The parent post is no longer available.")
            .into_any_element();
    }

    div()
        .flex()
        .flex_col()
        .gap_1()
        .children(
            chain
                .items
                .iter()
                .map(|thread_item| thread_row(thread_item, theme)),
        )
        .when(chain.capped, |column| {
            column.child(div().text_color(rgb(theme.text_muted)).child(format!(
                "Reached the {}-level limit — earlier replies in this thread \
                         aren't shown.",
                thread::MAX_THREAD_DEPTH
            )))
        })
        .into_any_element()
}

fn thread_row(thread_item: &thread::ThreadItem, theme: Theme) -> impl IntoElement {
    let byline = byline(&thread_item.author_username);

    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.bg_header))
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .child(thread_item.author_name.clone()),
                )
                .child(div().text_color(rgb(theme.text_muted)).child(byline)),
        )
        .child(div().child(thread_item.text.clone()))
}

/// 一つの行が描く添付画像の数 (#65)｡X は post あたり四枚まで許し､それは
/// timeline の行が timeline の行でなくなる手前に収まる限度でもある｡
pub(in crate::ui) const MAX_RENDERED_MEDIA: usize = 4;

/// 添付画像の寸法の上限と隙間 (#256)｡値は #95 の他の寸法と一緒に `theme`
/// にある｡
pub(in crate::ui) use crate::theme::{MEDIA_GAP, MEDIA_MAX_HEIGHT, MEDIA_MAX_WIDTH};

/// 縦横比 (幅 / 高さ) を収める範囲 (#256)｡高さ 1px のバナーや細い縦帯が
/// 段の高さを 0 へ落としたり､幅の予算を独り占めしたりしないよう､ここで
/// 止める｡
const MEDIA_ASPECT_RANGE: std::ops::RangeInclusive<f32> = 0.1..=10.0;

/// `media` の縦横比 (幅 / 高さ) (#256)｡API は `media.fields=width,height`
/// で寸法を返すが､古いキャッシュの行には無いことがある — 無いときと 0 は
/// 正方形として扱う｡結果は [`MEDIA_ASPECT_RANGE`] に収める｡
pub(in crate::ui) fn media_aspect(media: &PostMedia) -> f32 {
    /// `u32` から `f32` への cast は精度を落とすので通らない｡X の画像は
    /// `u16` に収まるのでそちらを経由し､収まらないものは上限に丸める｡
    fn side(pixels: Option<u32>) -> Option<f32> {
        let pixels = pixels.filter(|&pixels| pixels > 0)?;
        Some(f32::from(u16::try_from(pixels).unwrap_or(u16::MAX)))
    }
    match (side(media.width), side(media.height)) {
        (Some(width), Some(height)) => {
            (width / height).clamp(*MEDIA_ASPECT_RANGE.start(), *MEDIA_ASPECT_RANGE.end())
        }
        _ => 1.0,
    }
}

/// `aspects` の写真を横 1 段に並べたときの各枚の寸法 (#256)｡
///
/// Tumblr の photoset と同じ組み方: 高さを揃え､幅は縦横比に比例させる｡
/// 高さは隙間を除いた [`MEDIA_MAX_WIDTH`] を縦横比の和で割ったもので､
/// [`MEDIA_MAX_HEIGHT`] を越えない｡1 枚ならこれは「最大値の箱に収まる
/// ように拡大・縮小する」と同じ式になる｡
///
/// 寸法を API の値だけから決めるので､画像が届いても行は組み直されない —
/// 画像が着くたびに読み手の下で組み直される timeline は､埋まるのを待つ
/// 枠を見せる timeline より悪い (#65)｡
pub(in crate::ui) fn media_row_sizes(aspects: &[f32]) -> Vec<Size<Pixels>> {
    if aspects.is_empty() {
        return Vec::new();
    }
    // `usize` から `f32` への cast を避けて､隙間は 2 枚目以降の 1 枚に 1 つ｡
    let gaps: f32 = aspects.iter().skip(1).map(|_| f32::from(MEDIA_GAP)).sum();
    let total_aspect: f32 = aspects.iter().sum();
    let height =
        ((f32::from(MEDIA_MAX_WIDTH) - gaps) / total_aspect).min(f32::from(MEDIA_MAX_HEIGHT));
    aspects
        .iter()
        .map(|&aspect| size(px(aspect * height), px(height)))
        .collect()
}

/// 複数枚の写真をどちらの向きに並べるか (#256)｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum MediaArrangement {
    /// 横 1 段 — 縦長どうしのとき｡
    Row,
    /// 縦積み — それ以外｡
    Column,
}

/// `aspects` の写真をどう並べるか (#256): 縦長 (縦横比 < 1) が過半数なら
/// 横 1 段､それ以外は縦積み｡縦長は幅が余るので隣が置けるが､横長を横に
/// 並べると 1 枚 1 枚が読めないほど小さくなる｡引き分けが縦積みなのは､段に
/// 入れていちばん縮むのが横長だからだ｡1 枚はどちらの向きでも同じ寸法に
/// なる｡
pub(in crate::ui) fn media_arrangement(aspects: &[f32]) -> MediaArrangement {
    let portraits = aspects.iter().filter(|&&aspect| aspect < 1.0).count();
    if portraits.saturating_mul(2) > aspects.len() {
        MediaArrangement::Row
    } else {
        MediaArrangement::Column
    }
}

/// 縦積みの各枚の寸法 (#256): どの枚も 1 枚のときと同じ式で
/// [`MEDIA_MAX_WIDTH`] × [`MEDIA_MAX_HEIGHT`] の箱に収まる｡
pub(in crate::ui) fn media_column_sizes(aspects: &[f32]) -> Vec<Size<Pixels>> {
    aspects
        .iter()
        .flat_map(|&aspect| media_row_sizes(&[aspect]))
        .collect()
}

/// 写真でないサムネイルの下に見せるバッジ (#65)｡素の写真なら `None` で､
/// このアプリが知らない `type` でも `None` だ｡そちらが前方互換の向きで
/// ある: X が後から生む media type は､誰にも解せないラベルとしてではなく
/// 素の静止画として描かれるべきだ｡
pub(in crate::ui) fn media_badge(kind: Option<&str>) -> Option<&'static str> {
    match kind {
        Some("video") => Some("Video"),
        Some("animated_gif") => Some("GIF"),
        _ => None,
    }
}

/// 投稿者のアバターを描く大きさ (#64)｡定数が一つなのは､placeholder が
/// 画像と厳密に一致せねばならないからだ — ダウンロードが届いた瞬間に組み
/// 直る行は､アバターがまったく無いよりも悪い｡
///
/// 大きさを揃えるだけでは足りない (#103): `post_row` はアバターを `flex_1`
/// の本文の隣に据えるので､本文の内容が使える幅を越えると flex の既定の
/// `flex-shrink: 1` が素の `.size(AVATAR_SIZE)` 要素を潰す｡`img` と
/// placeholder の `div` には､大きさそのものだけでなく､この大きさと並べて
/// `flex_shrink_0` も要る｡両者が一致せねばならない三つ目が形であり､だから
/// こそリテラルではなく [`theme::AVATAR_RADIUS`] なのだ (#98)｡
///
/// 値そのものは､どちらもそこから導かれる radius と行の区切り線の inset と
/// 一緒に `theme` にある (#95)｡
pub(in crate::ui) use crate::theme::AVATAR_SIZE;

/// まだダウンロードされていない､失敗した､あるいはそもそも存在しなかった
/// アバターの代わりに立つもの (#64): 投稿者の頭文字を載せた塗り潰しの円｡
///
/// 空の円盤ではなく頭文字にしたのは､画像が一枚も届く前から timeline の
/// 連続する投稿者をおおむね見分けられるからで — それが #64 の眼目である｡
/// 名前が展開されなかった投稿者には素の円が出る; 見せる文字が無く､
/// でっち上げれば空白よりも悪くなる｡
pub(in crate::ui) fn avatar_placeholder(author_name: &str, theme: Theme) -> AnyElement {
    let initial = avatar_initial(author_name);

    div()
        .size(AVATAR_SIZE)
        .flex_shrink_0()
        .rounded(theme::AVATAR_RADIUS)
        .bg(rgb(theme.border))
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(theme.text_muted))
        .child(initial)
        .into_any_element()
}

/// アバターの placeholder が `author_name` に対して見せる文字 (#64):
/// 先頭の一文字を大文字にしたもの｡名前が展開されなかった投稿者では空に
/// なる — そのとき円は､でっち上げた頭文字を見せるのではなく単独で立つ｡
///
/// バイト単位ではなく `char` 単位なので､マルチバイト文字で始まる名前
/// (そういうものは多い) が文字の途中で切られることも飛ばされることも
/// ない｡`to_uppercase` は Unicode のそれで､字体によっては一文字より多くを
/// 生みうる; それは切り詰めずそのままにしてある｡大文字化した結果を半分で
/// 断てば､短いものではなく誤ったものが出るからだ｡
pub(in crate::ui) fn avatar_initial(author_name: &str) -> String {
    author_name
        .chars()
        .next()
        .map(|first| first.to_uppercase().to_string())
        .unwrap_or_default()
}

/// post 一つに対する x.com の正準 URL (#70)｡[`TimelineItem`] がすでに
/// 持っているものから組み立てる — リクエストは無く､API は一切関わらない｡
///
/// 投稿者が展開されなかった post では `author_username` が空で
/// (`x_api::model::build_item` を見よ)､`x.com//status/…` は 404 になる｡
/// X 自身の id だけの形 `x.com/i/web/status/:id` はサーバ側で投稿者を
/// 解決するので､アプリがその post について最も知らないまさにそのときに
/// 導線を取り下げるのではなく､リンクは働きつづける｡
pub(in crate::ui) fn post_permalink(author_username: &str, post_id: &str) -> String {
    if author_username.is_empty() {
        format!("https://x.com/i/web/status/{post_id}")
    } else {
        format!("https://x.com/{author_username}/status/{post_id}")
    }
}

/// アカウント一つに対する x.com の URL (#70)｡username が解決しなかった
/// ときは `None` — post と違い id だけの逃げ道が無いので､誤った先を指す
/// 代わりにその導線を取り下げる｡
pub(in crate::ui) fn profile_url(author_username: &str) -> Option<String> {
    (!author_username.is_empty()).then(|| format!("https://x.com/{author_username}"))
}

/// 行がアクションの傍らに見せるエンゲージメントの件数 (#67､#95 で作り
/// 直した)｡
///
/// #95 までは本文の下の独立した一行 — "12 replies · 34 reposts ·
/// 56 likes" — で､まさに同じ三つを名指すアクションのラベルが縦に並んだ列
/// の上に載っていた｡#95 は二つを畳み込む: 件数は今や属するアクションの隣に
/// 乗り､独立した行は消えた｡どの post でも一行分の高さが戻ってくる｡
///
/// 各フィールドは､その件数が零のとき､または post が metrics をまったく
/// 持たないとき `None` になる｡だから新しい post は零の連なりではなく素の
/// アクションを描く — 零の部分を落としていた古い一行と同じ規則だ｡件数は
/// 行を取得した時点のスナップショットであり ([`PostMetrics`] を見よ)､
/// ここで読み直すものは何も無い｡
#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::ui) struct RowCounts {
    /// "Reply" の傍ら｡
    pub(in crate::ui) replies: Option<String>,
    /// "Repost" / "Reposted" の傍ら｡
    pub(in crate::ui) reposts: Option<String>,
    /// "Like" / "Liked" の傍ら｡
    pub(in crate::ui) likes: Option<String>,
}

/// 行が持つ metrics から､その行の [`RowCounts`] を組み立てる｡
pub(in crate::ui) fn row_counts(metrics: Option<&PostMetrics>) -> RowCounts {
    let Some(metrics) = metrics else {
        return RowCounts::default();
    };
    RowCounts {
        replies: non_zero_count(metrics.replies),
        reposts: non_zero_count(metrics.reposts),
        likes: non_zero_count(metrics.likes),
    }
}

/// 件数一つを略記したもの｡零なら `None` — 零が "0" ではなく無である理由は
/// [`RowCounts`] を見よ｡
fn non_zero_count(count: u64) -> Option<String> {
    (count > 0).then(|| compact_count(count))
}

/// アクション一つと､その傍らのエンゲージメント件数 (#95, #156)｡見せる件数が
/// 無ければアクション単独｡
///
/// 件数をアクション自身の要素の一部ではなく兄弟にしてあるのは､数字を
/// クリックしても何も起きないようにするためだ: アクションはリクエストを
/// 費やすトグルであり､ボタンの一部に見える件数は､読み手がただ読むだけの
/// つもりだったアクションの的を広げてしまう｡
///
/// `name` は件数の要素に `{name}-count` の名前を付ける (bounds テストが
/// 引くため)｡`color` は on のとき記号と同じ色にし (idle/pending は
/// `text_muted`)､呼び出し側が `icon_button` へ渡したのと同じ値を渡す｡
pub(in crate::ui) fn with_count(
    name: &str,
    action: AnyElement,
    count: Option<&str>,
    color: u32,
    theme: Theme,
) -> AnyElement {
    let _ = theme;
    let Some(count) = count else {
        return action;
    };

    div()
        .flex()
        .items_center()
        .gap_1()
        .child(action)
        .child(
            div()
                .addressable(format!("{name}-count"))
                .text_color(rgb(color))
                .child(count.to_string()),
        )
        .into_any_element()
}

/// X 自身の UI と同じやり方で件数を略記する — `12345` は `12.3K` になる —
/// 人気の post が七桁の幅でタイムスタンプと byline を押しのけられない
/// ように｡末尾の `.0` は落とす (`1000` は `1.0K` ではなく `1K`); 1000 未満
/// はそのままの数字を見せる｡
fn compact_count(count: u64) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "an abbreviated count is approximate by construction"
    )]
    fn scaled(count: u64, unit: u64, suffix: char) -> String {
        let value = count as f64 / unit as f64;
        // 小数一桁を､丸めずに切り捨てる｡ラベルが post の実際より多くの
        // エンゲージメントを主張しないようにするためだ｡
        let tenths = (value * 10.0).floor() / 10.0;
        if (tenths.fract()).abs() < f64::EPSILON {
            format!("{}{suffix}", tenths.trunc())
        } else {
            format!("{tenths:.1}{suffix}")
        }
    }

    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => scaled(count, 1_000, 'K'),
        _ => scaled(count, 1_000_000, 'M'),
    }
}

/// `2026-08-16T09:00:00.000Z` を JST の `2026-08-16 18:00` にする (#195)｡
///
/// パースできない入力は生の文字列のまま返す｡`created_at` は API から来る
/// 遠隔入力なので､ここで panic させない｡
pub(in crate::ui) fn format_timestamp(created_at: Option<&str>) -> String {
    let Some(raw) = created_at else {
        return String::new();
    };
    match DateTime::parse_from_rfc3339(raw) {
        Ok(at) => at
            .with_timezone(&Tokyo)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        Err(_) => raw.to_string(),
    }
}
