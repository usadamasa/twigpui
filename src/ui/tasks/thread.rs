//! "Show thread" (#12): 1 つの reply の親の chain を読む｡API のクレジットを
//! 使う｡
//!
//! [`super::fetch`] から切り出した｡あちらは timeline そのもの (`state`) を
//! 埋める読み取りで､reload の cooldown､失敗の notice､poll のバッファと
//! 絡み合っている｡こちらは reply ごとの `threads` だけを埋め､そのどれにも
//! 触れない｡

// `use crate::ui::*` ではなく書き下す: 名指しする `ui` の import が
// clippy の `wildcard_imports` が列挙できる程度に少ない｡
use crate::ui::{Context, ThreadFetchState, TimelineView, cache, oauth};

impl TimelineView {
    /// 一つの reply のために "Show thread" のクレジットを使う (#12): 親の
    /// chain を辿り (取得済みならキャッシュから､でなければネットワーク
    /// から､最大 `thread::MAX_THREAD_DEPTH` リクエスト)､結果を描画する｡
    /// client 無しでは何もしない — その状態で toggle は出ないが､
    /// [`Self::reload`] の流儀に合わせてここでも守る｡
    ///
    /// `reply_post_id` は展開される側の reply (キャッシュ/状態のキー);
    /// `first_parent_id` はその直接の親の id — ただで判明している
    /// `TimelineItem::replied_to` の `post_id` — で､そこから辿り始める｡
    pub(in crate::ui) fn show_thread(
        &mut self,
        reply_post_id: String,
        first_parent_id: String,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };

        self.threads
            .insert(reply_post_id.clone(), ThreadFetchState::Loading);
        cx.notify();

        let paths = self.paths.clone();
        let key = reply_post_id.clone();
        let fetch_key = reply_post_id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    cache::fetch_thread(
                        &paths,
                        &client,
                        &reply_post_id,
                        &first_parent_id,
                        oauth::unix_now(),
                    )
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.refresh_usage(cx);
                let state = match result {
                    Ok(chain) => ThreadFetchState::Loaded(chain),
                    Err(error) => ThreadFetchState::Failed(format!("{error:#}").into()),
                };
                this.threads.insert(key.clone(), state);
                this.thread_fetches.remove(&key);
                cx.notify();
            });
        });
        self.thread_fetches.insert(fetch_key, task);
    }
}
