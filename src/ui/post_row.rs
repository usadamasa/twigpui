//! timeline の 1 行 (#241): 著者のアバター､本文､添付 media のグリッド､
//! action の帯 (reply / repost / like / quote / open / delete)､そして
//! "Show thread" の区画｡行の中で `self` の状態を読む描画メソッドをここに
//! 集めた｡状態を持たない描画の補助は `render.rs`｡
//!
//! `ui/mod.rs` にあったものをそのまま移した｡

use gpui::{Pixels, Size};

use super::lane;
use super::*;

/// サムネイルをクリックしたときの行き先 (#188)｡
#[derive(Debug, PartialEq, Eq)]
pub(in crate::ui) enum MediaClickTarget {
    /// このアプリの viewer ウィンドウで開く ([`super::image_viewer`])｡
    Viewer,
    /// 原寸の画像をブラウザで開く (#70)｡動画・GIF・種類不明のものも含め､
    /// 写真以外はすべてここへ回る｡
    Browser,
}

/// クリックの行き先 (#188)｡静止画だけが viewer､動画と GIF は再生できないので
/// ブラウザのまま｡
pub(in crate::ui) fn media_click_target(kind: Option<&str>) -> MediaClickTarget {
    match kind {
        Some("photo") => MediaClickTarget::Viewer,
        _ => MediaClickTarget::Browser,
    }
}

impl TimelineView {
    /// 一つの post の本文の下に置く添付 media の並び (#65, #256)｡
    ///
    /// サムネイルは最大 [`MAX_RENDERED_MEDIA`] 枚を本文列の左端から並べる｡
    /// 向きは [`media_arrangement`] が縦横比で決める: 縦長どうしは横 1 段､
    /// 横長どうしは縦積み｡各枚の寸法は [`media_row_sizes`] /
    /// [`media_column_sizes`] が API の `width` / `height` だけから決める
    /// ので､どの画像のダウンロードを終えたかに並びの形が依存することは
    /// ありえない｡
    ///
    /// 引用カードの本文列は外側より狭い｡並びは `max_w_full` で列に収め､
    /// セルは `min_w_0` で縮めるので､入りきらない分は枠の右で切れる｡
    /// `items_start` は両向きに掛かる: 縦積みでセルが列いっぱいへ引き
    /// 伸ばされるのも､横 1 段で高さが揃えられるのも止める｡
    fn media_grid(&self, media: &[PostMedia], cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = self.theme;
        let shown: Vec<&PostMedia> = media.iter().take(MAX_RENDERED_MEDIA).collect();
        let aspects: Vec<f32> = shown.iter().map(|media| media_aspect(media)).collect();
        let (grid, sizes) = match media_arrangement(&aspects) {
            MediaArrangement::Row => (div().flex(), media_row_sizes(&aspects)),
            MediaArrangement::Column => (div().flex().flex_col(), media_column_sizes(&aspects)),
        };

        // #188: viewer が動く先は写真だけ｡動画とアニメーション GIF は
        // ここでは再生できないので､今までどおりブラウザへ渡す｡
        let photos: Vec<PostMedia> = media
            .iter()
            .filter(|media| media.kind.as_deref() == Some("photo"))
            .cloned()
            .collect();

        let mut grid = grid.items_start().gap(MEDIA_GAP).max_w_full();
        for (media, size) in shown.into_iter().zip(sizes) {
            grid = grid.child(self.media_cell(media, size, theme, &photos, cx));
        }
        grid.into_any_element()
    }

    /// [`Self::media_grid`]｡描く media が無いときは何も返さない (#123) —
    /// quote card に要るものである｡ほとんどの quote は media を持たず､
    /// 空のグリッドでも card に gap を足してしまうからだ｡
    fn media_grid_for(
        &self,
        media: &[PostMedia],
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        (!media.is_empty()).then(|| self.media_grid(media, cx))
    }

    /// サムネイル一つ: ダウンロードした画像が着いていればそれ､無ければ同じ
    /// 大きさの枠 (#65)｡ちゃんと見る手段の無いサムネイルは機能の半分でしか
    /// ないので､クリックには行き先がある — [`media_click_target`] が言う
    /// とおり､写真は viewer のウィンドウ ([`super::image_viewer`], #188)､
    /// それ以外は原寸の画像をブラウザで (#70)｡動画やアニメーション GIF は
    /// 静止画と､それがどちらかを示す badge を出す; どちらもここでは
    /// 再生されない｡
    ///
    /// 枠は `size` そのもの (#256): [`media_row_sizes`] が API の寸法から出した
    /// 幅と高さを枠にも画像にも与えるので､画像が着く前と後で枠の形は変わら
    /// ない｡塗るのは placeholder だけ — 画像は枠をちょうど埋めるので､枠まで
    /// 塗ると API の寸法と実物がわずかに違うときに灰色の縁が覗く｡
    ///
    /// 幅と高さを両方与えるのは gpui の `img` のためでもある｡画像がデコード
    /// されると `img` は `aspect_ratio` を layout に持ち込むが､taffy はそれを
    /// 片方が auto のときにしか使わない｡片方だけ与えると縦横比が勝って枠と
    /// 喧嘩する (幅いっぱいの縦長の画像が枠を突き抜けて次の行に重なった)｡
    /// 両方が定まっていれば画像は枠に収まり､差は `Contain` が吸収する｡
    fn media_cell(
        &self,
        media: &PostMedia,
        size: Size<Pixels>,
        theme: Theme,
        photos: &[PostMedia],
        cx: &mut Context<'_, TimelineView>,
    ) -> AnyElement {
        let url = media.url.clone();
        // #188: 行き先は `media_click_target` に一本化する｡viewer 向けの
        // ときだけ `photos` の何枚目かを覚えておく — viewer が `←` / `→` で
        // 同じ post の残りへ動けるようにするため｡
        let timeline = cx.entity();
        let viewer = matches!(
            media_click_target(media.kind.as_deref()),
            MediaClickTarget::Viewer
        )
        .then(|| {
            let index = photos
                .iter()
                .position(|photo| photo.url == media.url)
                .unwrap_or(0);
            (photos.to_vec(), index)
        });

        let frame = div()
            .addressable(format!("media-frame-{}", media.url))
            .w(size.width)
            .h(size.height)
            .max_w_full()
            .rounded(theme::RADIUS_THUMB)
            // 狭い列 (引用カード) では枠のほうが縮むので､画像は右で切る｡
            .overflow_hidden();
        let inner = match self.media_paths.get(&media.url) {
            Some(path) => frame
                .child(
                    img(path.clone())
                        .addressable(format!("media-image-{}", media.url))
                        .w(size.width)
                        .h(size.height)
                        .rounded(theme::RADIUS_THUMB)
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element(),
            None => frame.bg(rgb(theme.border)).into_any_element(),
        };

        let mut cell = div()
            .addressable(format!("media-{}", media.url))
            .flex()
            .flex_col()
            // flex item の `min-width` は既定で `auto` (= 中身の幅) なので､
            // 段が列に入りきらないとき枠が縮めるようにしておく｡
            .min_w_0()
            .gap_1()
            .child(inner)
            .on_click(
                cx.listener(move |this, _event, _window, cx| match viewer.clone() {
                    Some((photos, index)) => {
                        image_viewer::open(&timeline, photos, index, cx);
                    }
                    None => this.open_in_browser(url.clone(), cx),
                }),
            );

        if let Some(badge) = media_badge(media.kind.as_deref()) {
            cell = cell.child(div().text_color(rgb(theme.text_muted)).child(badge));
        }
        if let Some(alt) = media.alt_text.as_ref() {
            // hover の裏に隠さず出す: このアプリ自身には screen reader の
            // 経路が無く､目の見える読み手が読める alt text のほうが､誰も
            // たどり着けない alt text より役に立つ｡
            cell = cell.child(
                div()
                    .text_color(rgb(theme.text_muted))
                    .child(format!("Alt: {alt}")),
            );
        }
        cell.into_any_element()
    }

    /// 一つの post の著者のアバター (#64): ディスクに落ちていればその画像､
    /// 無ければ [`avatar_placeholder`] — 二つは同じ大きさなので､画像が
    /// 着いても行は組み直されない｡
    fn avatar(&self, item: &TimelineItem, theme: Theme) -> AnyElement {
        let cached = item
            .author_avatar_url
            .as_deref()
            .and_then(|url| self.avatar_paths.get(url));

        match cached {
            Some(path) => img(path.clone())
                .size(AVATAR_SIZE)
                .flex_shrink_0()
                .rounded(theme::AVATAR_RADIUS)
                .into_any_element(),
            None => avatar_placeholder(&item.author_name, theme),
        }
    }

    /// `item` の出自ラベル (#43)｡[`lane::provenance_label`] の薄いラッパー｡
    fn post_provenance(&self, item: &TimelineItem) -> Option<String> {
        lane::provenance_label(
            self.sources.len(),
            &self.item_provenance,
            &self.owned_lists,
            &item.id,
        )
    }

    pub(super) fn post_row(&self, item: &TimelineItem, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = self.theme;
        let byline = byline(&item.author_username);

        let counts = row_counts(item.metrics.as_ref());

        // #64: アバターは左の独立した列に座るので､下の body は別に組み立てて
        // からその隣に置く｡
        let body = div()
            .flex()
            .flex_col()
            // `flex_1` ではなく `flex_grow` (#43): basis 0 だと一覧がウィンドウ
            // より短いとき､折り返す行の高さが列の余りまで伸びる (4 行の
            // fixture で実測 180px の空白)｡basis auto なら高さは中身で決まる｡
            // `ui` の `a_wrapping_row_keeps_its_height_when_the_lane_is_short`｡
            .flex_grow()
            // #140: `flex_grow` が取るのは *余った* 幅であって､中身より狭く
            // 縮むことは許さない｡flex の子の `min-width` の既定が `auto`
            // だからだ｡そのため長い文が列を行より広く押し広げ､はみ出しは
            // 切り取られていた｡代わりに折り返させるのが `min_w_0` である｡
            //
            // これがあの時点で表に出たのは #103 のせいだ: アバターが
            // `flex_shrink_0` を得る前は､アバターが潰れることではみ出しを
            // 吸収していた｡それを固定したのは正しく､そして余った幅の
            // 行き先として body だけが
            // 残った｡
            .min_w_0()
            .gap_1()
            // #95: meta 行は一本｡著者､byline､timestamp､そして "reposted" /
            // "replying to" のうち当てはまるほうが､みな一緒に並ぶ — #95 まで
            // は後ろの二つが名前の上の全幅の行を占めていて､二行の post が
            // 四行に膨らんでいた｡
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .text_size(theme::TEXT_META)
                    // #70: 著者名と handle は x.com のプロフィールを開く｡
                    // username が展開されなかったときは `profile_url` が
                    // `None` を返し､その場合は行き先の無いリンクにはならず
                    // ただの文字のままになる｡
                    .child(author_link(item, theme, cx))
                    .child(div().text_color(rgb(theme.text_muted)).child(byline))
                    .child(
                        div()
                            .text_color(rgb(theme.text_tertiary))
                            .child(format_timestamp(item.created_at.as_deref())),
                    )
                    // #13: repost は誰が repost したかを言う — この時点で
                    // body が持っているのはすでに *原文* の post であって
                    // (`TimelineResponse::into_items` の join を見よ)､外側の
                    // post 自身の著者ではない｡
                    .when_some(item.reposted_by.as_deref(), |line, reposted_by| {
                        line.child(
                            div()
                                .text_color(rgb(theme.text_tertiary))
                                .child(format!("· {}", repost_banner_label(reposted_by))),
                        )
                    })
                    // #12: この post が誰に返信しているか｡追加のリクエスト
                    // 費用ゼロで出せる — 親の著者は #13 の expansions により
                    // すでに `includes` に入っている｡
                    .when_some(item.replied_to.as_ref(), |line, replied_to| {
                        line.child(
                            div()
                                .text_color(rgb(theme.text_tertiary))
                                .child(format!("· {}", reply_banner_label(replied_to))),
                        )
                    })
                    // #43: 合成レーンでだけ出自を出す。
                    // list 由来の post だけに付き、Home にしか無い post には
                    // 付かない。`item_provenance` は表示専用の派生値で、
                    // 複数 list に載っている post なら表示順で最初の 1 つ
                    // だけを見せる。
                    .when_some(self.post_provenance(item), |line, name| {
                        line.child(
                            div()
                                .max_w(px(120.0))
                                .truncate()
                                .text_color(rgb(theme.text_tertiary))
                                .child(format!("· {name}")),
                        )
                    }),
            )
            .child(div().child(item.text.clone()))
            // #70: 本文中のリンク｡本文が持つ `t.co` の短縮リンクから展開
            // したもの — 本文の中ではなく下に置く理由は `link_row` の doc を
            // 見よ｡
            .when(!item.links.is_empty(), |column| {
                column.child(link_row(&item.links, theme, cx))
            })
            // #65: 添付画像を､body の下のサムネイルとして出す｡
            .when(!item.media.is_empty(), |column| {
                column.child(self.media_grid(&item.media, cx))
            })
            // #13: quote (quote の repost も含む) は引用元を本文の下に枠付き
            // の card として埋め込む｡
            .when_some(item.quoted.as_ref(), |column, quoted| {
                column.child(quote_card(
                    quoted,
                    theme,
                    self.media_grid_for(&quoted.media, cx),
                ))
            })
            // #95: すべての action を横一行に並べ､それぞれが自分の件数を
            // 添える｡これがこの issue の主たる不満だ — 同じ一式が以前は
            // 行の下へ一行一ラベルで積み上がっていた｡
            .child(self.action_row(item, &counts, cx))
            // #12: "Show thread" — 提示するのは reply のときだけだ｡辿る親が
            // あるのはその場合だけだからである｡意図的に `action_row` の一部
            // にしていない: 読み込まれた thread は post の連なり全体へ広がる
            // ので､一行の帯の中には収まらない｡
            .when_some(item.replied_to.as_ref(), |column, replied_to| {
                column.child(self.thread_section(&item.id, replied_to, cx))
            });

        div()
            .addressable(format!("post-row-{}", item.id))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .px(theme::ROW_PAD_X)
                    .py(theme::ROW_PAD_Y)
                    .child(self.avatar(item, theme))
                    .child(body),
            )
            // #95: 区切り線はアバターの下を走らず､本文が始まる位置から
            // 始まる｡macOS 自身の一覧 (Mail､Messages) が使う inset である｡
            // 行自身の下枠ではなく行の兄弟にしてあるのは､この inset を
            // padding として言い直さずに
            // 済ませるためだ｡
            .child(
                div()
                    .h(px(1.0))
                    .ml(theme::SEPARATOR_INSET)
                    .bg(rgb(theme.border)),
            )
            .into_any_element()
    }

    /// 1 つの返信 (#12) についての "Show thread" のトグル､読み込み中/エラーの
    /// 状態､あるいは組み上がったチェーン — `self.threads.get(reply_post_id)` が
    /// 今どれだと言うかによる｡[`Self::post_row`] から切り出したのは読みやすさの
    /// ためだけで､トグルのクリックハンドラのために `cx` は依然として要る｡
    fn thread_section(
        &self,
        reply_post_id: &str,
        replied_to: &RepliedTo,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = self.theme;

        let state = self.threads.get(reply_post_id);

        if let Some(ThreadFetchState::Loaded(chain)) = state {
            return render_thread_chain(chain, theme);
        }
        if matches!(state, Some(ThreadFetchState::Loading)) {
            return div()
                .text_color(rgb(theme.text_muted))
                .child("Loading thread…")
                .into_any_element();
        }

        // ここへ届く状態: `None` (一度も要求していない) と `Failed` — どちらも
        // クリックできるトグルを出す｡違うのはラベルだけで､詳しくは
        // `thread_action_label` を見る｡
        let label = thread_action_label(state).unwrap_or_default();
        let toggle = thread_toggle_row(
            reply_post_id.to_string(),
            replied_to.post_id.clone(),
            label,
            theme,
            cx,
        );

        if let Some(ThreadFetchState::Failed(message)) = state {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_color(rgb(theme.danger)).child(message.clone()))
                .child(toggle)
                .into_any_element()
        } else {
            toggle.into_any_element()
        }
    }
}
