//! 手元の更新 (#241): usage と like/repost の記録をディスクから読み直し､
//! 見えている timeline に足りない画像を `pbs.twimg.com` から落とす｡
//! どれも API のクレジットは使わない｡

// 列挙ではなく glob にしているのは [`crate::ui::render`] と
// [`crate::ui::auto_refresh`] に合わせたもの｡
use crate::ui::*;

impl TimelineView {
    /// ヘッダの usage 要約をディスクから読み直す (#18)｡何が引き金だった
    /// かとは独立だ — どの fetch 経路 (reload､"Load older"､"Show thread"
    /// の探索) も追跡している件数を動かしうる｡`x_api::client::XClient::get`
    /// がリクエスト自体の成否によらず実際の HTTP 送信をすべて記録する
    /// からだ｡引き金になった fetch に畳み込まず単独で spawn する:
    /// `usage.json` の読み取りが失敗しても､fetch もろとも失敗させるので
    /// はなく､ヘッダは前に出していたものをそのまま出しつづける｡
    pub(in crate::ui) fn refresh_usage(&mut self, cx: &mut Context<'_, Self>) {
        let paths = self.paths.clone();
        self.usage_refresh = Some(cx.spawn(async move |this, cx| {
            let now = oauth::unix_now();
            let result = cx
                .background_executor()
                .spawn(async move {
                    usage::load_all(&paths).map(|entries| usage::totals(&entries, now))
                })
                .await;

            if let Ok(totals) = result {
                let _ = this.update(cx, |this, cx| {
                    this.usage_totals = totals;
                    cx.notify();
                });
            }
        }));
    }

    /// 見えている timeline が変わるたび､ローカルの repost 記録から
    /// `self.reposted_ids` を読み直す (#15) — [`Self::refresh_usage`] の
    /// 型をそのまま写したものだ: 遅いディスク読み取りが描画を止めないよう
    /// background executor で読み､失敗した読み取りは､乗ってきた fetch
    /// もろとも失敗させるのではなくすでに出ているものを残す｡「これを
    /// repost したか」の出所はプロジェクト内でこのファイルだけなので
    /// (#15 が存在する理由そのもの — X API 自体にそんなフィールドは無い)､
    /// ここでの読み取りが古くても失われても､過小・過大に報告できるのは
    /// *このアプリ自身の* repost だけだ｡他の client からのものは決して
    /// 含まないが､この issue はそれをいずれにせよ対象外としている｡
    pub(super) fn refresh_reposted_ids(&mut self, cx: &mut Context<'_, Self>) {
        let paths = self.paths.clone();
        self.reposted_ids_refresh = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { repost::load_all(&paths) })
                .await;

            if let Ok(ids) = result {
                let _ = this.update(cx, |this, cx| {
                    this.reposted_ids = ids;
                    cx.notify();
                });
            }
        }));
    }

    /// ローカルの like 記録から `self.liked_ids` を読み直す (#68) —
    /// [`Self::refresh_reposted_ids`] の like 側の双子で､メインスレッド
    /// の外で読む点も失敗を致命的にしない点も同じ契約だ｡呼ばれる場所も
    /// まったく同じなので､ある行の like ボタンと repost ボタンが別々の
    /// 時点から種を得ることは決してない｡
    pub(super) fn refresh_liked_ids(&mut self, cx: &mut Context<'_, Self>) {
        let paths = self.paths.clone();
        self.liked_ids_refresh = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { like::load_all(&paths) })
                .await;

            if let Ok(ids) = result {
                let _ = this.update(cx, |this, cx| {
                    this.liked_ids = ids;
                    cx.notify();
                });
            }
        }));
    }

    /// 見えている timeline が必要としていて､まだ持っていない avatar を
    /// 落としてくる (#64)｡
    ///
    /// [`Self::refresh_reposted_ids`] と同じ場所すべてから呼ばれるので､
    /// ある行の avatar とそのボタンは同じ時点から来る｡取得は background
    /// executor で URL を 1 本ずつ行い､その都度 map を (ひいては view を)
    /// 更新する — 着いたそばから avatar が現れる方が､一番遅い 1 枚を
    /// timeline 全体で待つよりよい｡失敗した URL はただ欠けたままにする｡
    /// 行は placeholder を保ち､次の reload が取り直す; 読み込めなかった
    /// avatar についてユーザーに言うべき有益なことは何も無い｡
    ///
    /// これらのリクエストは X API ではなく `pbs.twimg.com` へ行く: quota
    /// も credit も無く､#18 の usage 追跡が数えるものは何も無い｡
    fn refresh_avatars(&mut self, cx: &mut Context<'_, Self>) {
        let TimelineState::Loaded(items) = &self.state else {
            return;
        };
        let mut wanted: Vec<String> = Vec::new();
        for url in items
            .iter()
            .filter_map(|item| item.author_avatar_url.as_deref())
        {
            if !self.avatar_paths.contains_key(url) && !wanted.iter().any(|seen| seen == url) {
                wanted.push(url.to_string());
            }
        }
        if wanted.is_empty() {
            return;
        }

        let paths = self.paths.clone();
        self.avatar_fetch = Some(cx.spawn(async move |this, cx| {
            for url in wanted {
                let paths = paths.clone();
                let fetch_url = url.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move { avatar::ensure_cached(&paths, &fetch_url) })
                    .await;

                match result {
                    Ok(path) => {
                        let _ = this.update(cx, |this, cx| {
                            this.avatar_paths.insert(url.clone(), path);
                            cx.notify();
                        });
                    }
                    // #49: どちらにせよ行は placeholder を保つが､黙って
                    // 欠けた avatar は､ログに 1 行無いと後から調べようが
                    // ない類のものそのものだ｡
                    Err(error) => log::warn(&format!("avatar fetch failed: {error:#}")),
                }
            }
        }));
    }

    /// 見えている timeline に欠けている画像を取る (#64, #65) — 著者の
    /// avatar と添付 media の両方だ｡
    ///
    /// timeline を変える箇所すべてで 2 回呼ぶのではなく入口を一つにする:
    /// この二つはまったく同じ瞬間に欲しくなるもので､片方だけ覚えていた
    /// 呼び出し側は行の半分を次の reload まで待たせてしまう｡
    ///
    /// **`self.state` を更新した後に呼ぶこと｡決して前ではない** (#120)｡
    /// 両方とも何が欠けているかを求めるのに `state` を読み､`Loaded` で
    /// なければ何もしないので､先に呼ぶと出ていく側の item 一覧に何が要る
    /// かを尋ねることになる: 起動時は `state` がまだ `Loading` なので何も
    /// 無く､reload では前のバッチの URL になる｡症状は､属している行より
    /// reload 1 回分遅れてしか avatar が現れないことだった｡同じ呼び出し
    /// 箇所にいる兄弟 (`refresh_usage`, `refresh_reposted_ids`,
    /// `refresh_liked_ids`) は代わりにディスクから読み､順序に依存しない｡
    /// それがこれを見落としやすくしていた｡
    pub(in crate::ui) fn refresh_images(&mut self, cx: &mut Context<'_, Self>) {
        self.refresh_avatars(cx);
        self.refresh_media(cx);
    }

    /// 見えている timeline が必要としていて､まだ持っていない添付画像を
    /// 落としてくる (#65) — [`Self::refresh_avatars`] の双子で契約も同じ
    /// だ: timeline 全体で 1 タスク､background executor で URL を 1 本ずつ､
    /// 各サムネイルは着いたそばから現れ､失敗は欠けたままにするので枠は
    /// 残り､次の reload が取り直す｡
    ///
    /// 添付 media は avatar より大きいが同じ経路で届き
    /// (`pbs.twimg.com`､API の quota も credit も無い)､共有の画像
    /// キャッシュ自身のサイズ上限で抑えられている｡
    fn refresh_media(&mut self, cx: &mut Context<'_, Self>) {
        let TimelineState::Loaded(items) = &self.state else {
            return;
        };
        let mut wanted: Vec<String> = Vec::new();
        for url in items
            .iter()
            // #123: quote された post の画像も､行自身のものと同じ経路で
            // 落ちてくる｡これが無いとカードは永久に埋まらない空の枠を
            // 描くことになり､それが置き換えたテキストだけのカードより
            // 悪い｡
            .flat_map(|item| {
                item.media
                    .iter()
                    .chain(item.quoted.iter().flat_map(|quoted| quoted.media.iter()))
            })
            .map(|media| media.url.as_str())
        {
            if !self.media_paths.contains_key(url) && !wanted.iter().any(|seen| seen == url) {
                wanted.push(url.to_string());
            }
        }
        if wanted.is_empty() {
            return;
        }

        let dir = self.paths.media_dir();
        self.media_fetch = Some(cx.spawn(async move |this, cx| {
            for url in wanted {
                let dir = dir.clone();
                let fetch_url = url.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move { image_cache::ensure_cached(&dir, &fetch_url) })
                    .await;

                match result {
                    Ok(path) => {
                        let _ = this.update(cx, |this, cx| {
                            this.media_paths.insert(url.clone(), path);
                            cx.notify();
                        });
                    }
                    Err(error) => log::warn(&format!("media fetch failed: {error:#}")),
                }
            }
        }));
    }
}
