//! sync が X に頼む 6 つのことと､write と write のあいだの待ち｡
//!
//! [`super::run`] と [`super::auto`] が [`XClient`] に求めるのはこれだけだ｡
//! その分を trait に括り出してあるので､両 module は `&dyn ListSyncApi` を
//! 受け取り､テストは HTTP を張らずにページと write の結果を仕込める｡
//! `XClient` 側は既存のメソッドへ委譲するだけで判断を 1 つも持たない —
//! transport はフィクスチャ JSON を通した `x_api::client` のテストが見ている｡
//!
//! 待ちも trait に載せてある｡[`super::run::apply_some`] は 2 件目以降の
//! write の前に 3〜20 秒待つので､これが無ければ batch を 2 件流すテストは
//! suite をその分だけ止める｡fake は渡された [`Duration`] を記録して
//! すぐ返る｡

use anyhow::Result;
use std::time::Duration;

use crate::paths::Paths;
use crate::x_api::XClient;
use crate::x_api::model::User;

/// list sync が X に対して行う操作｡
///
/// ページを返す 2 つは 1 ページと次の cursor を返し､cursor が `None` に
/// なったところで終わる — [`super::run`] の `read_all` が回す形だ｡
///
/// メソッド名は委譲先の [`XClient`] とわざとずらしてある｡揃えると
/// `clippy::same_name_method` が deny で止めるし ([`XClient`] 側に
/// `#[allow]` を置けば client.rs 全体でその lint が効かなくなる)､
/// `XClient` の値に対する `client.following(..)` がどちらを呼ぶのかは､
/// 名前がずれていればそもそも問いにならない｡
pub(crate) trait ListSyncApi {
    /// `user_id` が follow しているアカウントを 1 ページ｡
    fn following_page(
        &self,
        paths: &Paths,
        user_id: &str,
        cursor: Option<&str>,
        now: i64,
    ) -> Result<(Vec<User>, Option<String>)>;

    /// `list_id` の member を 1 ページ｡
    fn list_members_page(
        &self,
        paths: &Paths,
        list_id: &str,
        cursor: Option<&str>,
        now: i64,
    ) -> Result<(Vec<User>, Option<String>)>;

    /// screen name から user id を引く｡#169 の sync seed が使う｡
    fn lookup_user_id(&self, paths: &Paths, username: &str, now: i64) -> Result<String>;

    /// サインイン中のアカウント｡
    fn signed_in_user(&self, paths: &Paths, now: i64) -> Result<User>;

    /// `user_id` を list に足す｡
    fn add_member(&self, paths: &Paths, list_id: &str, user_id: &str, now: i64) -> Result<()>;

    /// `user_id` を list から外す｡
    fn remove_member(&self, paths: &Paths, list_id: &str, user_id: &str, now: i64) -> Result<()>;

    /// batch の中で write と write のあいだに置く間｡長さは
    /// [`super::state::write_gap`] が引き､ここは待つだけだ｡
    ///
    /// 既定が実際に眠るので本番の経路はこれを実装しない｡テストは上書きして
    /// 記録する｡
    fn pause_between_writes(&self, gap: Duration) {
        std::thread::sleep(gap);
    }
}

impl ListSyncApi for XClient {
    fn following_page(
        &self,
        paths: &Paths,
        user_id: &str,
        cursor: Option<&str>,
        now: i64,
    ) -> Result<(Vec<User>, Option<String>)> {
        self.following(paths, user_id, cursor, now)
    }

    fn list_members_page(
        &self,
        paths: &Paths,
        list_id: &str,
        cursor: Option<&str>,
        now: i64,
    ) -> Result<(Vec<User>, Option<String>)> {
        self.list_members(paths, list_id, cursor, now)
    }

    fn lookup_user_id(&self, paths: &Paths, username: &str, now: i64) -> Result<String> {
        self.user_id_by_username(paths, username, now)
    }

    fn signed_in_user(&self, paths: &Paths, now: i64) -> Result<User> {
        self.me(paths, now)
    }

    fn add_member(&self, paths: &Paths, list_id: &str, user_id: &str, now: i64) -> Result<()> {
        self.add_list_member(paths, list_id, user_id, now)
    }

    fn remove_member(&self, paths: &Paths, list_id: &str, user_id: &str, now: i64) -> Result<()> {
        self.remove_list_member(paths, list_id, user_id, now)
    }
}

/// [`super::run`] と [`super::auto`] のテストが共有する道具｡
///
/// 二つの module が同じ fake を必要とするので､どちらかの `#[cfg(test)]` に
/// 置いて片方から見えなくするのではなく trait の隣に置いてある｡
#[cfg(test)]
pub(super) mod fake {
    use anyhow::{Result, anyhow};
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::ListSyncApi;
    use crate::paths::Paths;
    use crate::profile::Profile;
    use crate::x_api::model::User;

    /// fake が受けた呼び出し 1 件｡どの順で何を訊かれたかを assert する｡
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum Call {
        /// follow list を 1 ページ｡持つのは渡された cursor｡
        Following(Option<String>),
        /// list の member を 1 ページ｡
        Members(Option<String>),
        /// screen name の解決｡
        Lookup(String),
        /// サインイン中のアカウント｡
        Me,
        /// list への追加｡
        Add(String),
        /// list からの削除｡
        Remove(String),
    }

    /// 1 ページ分の答え: アカウントと次の cursor｡
    pub(crate) type Page = Result<(Vec<User>, Option<String>)>;

    /// 仕込んだ答えを順に返し､呼ばれ方だけを記録する [`ListSyncApi`]｡
    ///
    /// 仕込みが尽きたら `Err` を返す — 黙って空ページを返せば､テストが
    /// 期待した以上に呼んだことが「何も見つからなかった」に化ける｡
    #[derive(Debug, Default)]
    pub(crate) struct FakeApi {
        following: RefCell<Vec<Page>>,
        members: RefCell<Vec<Page>>,
        lookups: RefCell<Vec<Result<String>>>,
        me: RefCell<Vec<Result<User>>>,
        writes: RefCell<Vec<Result<()>>>,
        calls: RefCell<Vec<Call>>,
        pauses: RefCell<Vec<Duration>>,
    }

    impl FakeApi {
        pub(crate) fn new() -> Self {
            Self::default()
        }

        /// follow list の read が返すページを順に｡
        pub(crate) fn following(self, pages: Vec<Page>) -> Self {
            *self.following.borrow_mut() = pages;
            self
        }

        /// list member の read が返すページを順に｡
        pub(crate) fn members(self, pages: Vec<Page>) -> Self {
            *self.members.borrow_mut() = pages;
            self
        }

        /// screen name の解決が返す id を順に｡
        pub(crate) fn lookups(self, ids: Vec<Result<String>>) -> Self {
            *self.lookups.borrow_mut() = ids;
            self
        }

        /// `/me` が返すもの｡
        pub(crate) fn me(self, user: Result<User>) -> Self {
            *self.me.borrow_mut() = vec![user];
            self
        }

        /// write が順に返すもの｡add と remove で 1 本の列を共有するので､
        /// 交互に混ざった batch の n 件目がどうなるかをそのまま書ける｡
        pub(crate) fn writes(self, results: Vec<Result<()>>) -> Self {
            *self.writes.borrow_mut() = results;
            self
        }

        /// 受けた呼び出しを順に｡
        pub(crate) fn calls(&self) -> Vec<Call> {
            self.calls.borrow().clone()
        }

        /// 記録した待ちを順に｡本物はここで眠る｡
        pub(crate) fn pauses(&self) -> Vec<Duration> {
            self.pauses.borrow().clone()
        }
    }

    /// 列の先頭を取る｡尽きていたら何が足りなかったかを言う｡
    fn take<T>(queue: &RefCell<Vec<Result<T>>>, what: &str) -> Result<T> {
        let mut queue = queue.borrow_mut();
        if queue.is_empty() {
            return Err(anyhow!("the fake was asked for a {what} it was not given"));
        }
        queue.remove(0)
    }

    impl ListSyncApi for FakeApi {
        fn following_page(
            &self,
            _paths: &Paths,
            _user_id: &str,
            cursor: Option<&str>,
            _now: i64,
        ) -> Page {
            self.calls
                .borrow_mut()
                .push(Call::Following(cursor.map(str::to_string)));
            take(&self.following, "follow page")
        }

        fn list_members_page(
            &self,
            _paths: &Paths,
            _list_id: &str,
            cursor: Option<&str>,
            _now: i64,
        ) -> Page {
            self.calls
                .borrow_mut()
                .push(Call::Members(cursor.map(str::to_string)));
            take(&self.members, "member page")
        }

        fn lookup_user_id(&self, _paths: &Paths, username: &str, _now: i64) -> Result<String> {
            self.calls
                .borrow_mut()
                .push(Call::Lookup(username.to_string()));
            take(&self.lookups, "user id")
        }

        fn signed_in_user(&self, _paths: &Paths, _now: i64) -> Result<User> {
            self.calls.borrow_mut().push(Call::Me);
            take(&self.me, "signed-in user")
        }

        fn add_member(
            &self,
            _paths: &Paths,
            _list_id: &str,
            user_id: &str,
            _now: i64,
        ) -> Result<()> {
            self.calls.borrow_mut().push(Call::Add(user_id.to_string()));
            take(&self.writes, "write result")
        }

        fn remove_member(
            &self,
            _paths: &Paths,
            _list_id: &str,
            user_id: &str,
            _now: i64,
        ) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(Call::Remove(user_id.to_string()));
            take(&self.writes, "write result")
        }

        fn pause_between_writes(&self, gap: Duration) {
            self.pauses.borrow_mut().push(gap);
        }
    }

    /// テスト用の 1 アカウント｡`name` は screen name と同じにしてある —
    /// diff が見るのは id だけで､report が見るのは `username` だけだ｡
    pub(crate) fn user(id: &str, username: &str) -> User {
        User {
            id: id.to_string(),
            name: username.to_string(),
            username: username.to_string(),
            profile_image_url: None,
        }
    }

    /// `(id, username)` の並びと次の cursor から 1 ページ｡
    pub(crate) fn page(users: &[(&str, &str)], next: Option<&str>) -> Page {
        Ok((
            users.iter().map(|(id, name)| user(id, name)).collect(),
            next.map(str::to_string),
        ))
    }

    /// 追跡している window が送信前に拒んだ write｡`schedule::apply_outcome`
    /// はこの型を downcast して探すので､素の `anyhow!` では代われない｡
    pub(crate) fn rate_limited(until: i64, opaque: bool) -> anyhow::Error {
        anyhow::Error::new(crate::rate_limit::RateLimited {
            reset_at: Some(until),
            opaque,
        })
    }

    /// `env::temp_dir()` 配下を指す [`Paths`] と､それが使うディレクトリ｡
    ///
    /// `label` ごとに別の root なので､テストが並列に走っても互いの plan や
    /// state を読まない｡profile を明示するのは [`Paths::for_profile`] と
    /// 同じ理由による — sync の seed (#169) は dev にしか無く､テストが
    /// たまたま自分の profile を観測するだけでは両方の枝を踏めない｡
    #[derive(Debug)]
    pub(crate) struct Scratch {
        root: PathBuf,
        paths: Paths,
    }

    impl Scratch {
        /// follow list を実際に読む profile (release)｡
        pub(crate) fn new(label: &str) -> Self {
            Self::for_profile(label, Profile::Release)
        }

        /// seed から follow list を組み立てる profile (dev, #169)｡
        pub(crate) fn dev(label: &str) -> Self {
            Self::for_profile(label, Profile::Dev)
        }

        fn for_profile(label: &str, profile: Profile) -> Self {
            let root = std::env::temp_dir()
                .join(format!("twigpui-test-sync-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let home = root.display().to_string();
            let paths =
                Paths::for_profile(move |key| (key == "HOME").then(|| home.clone()), profile)
                    .unwrap();
            paths.ensure_dirs().unwrap();
            Self { root, paths }
        }

        pub(crate) fn paths(&self) -> &Paths {
            &self.paths
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            // 読み取り専用にした state dir を作るテストがあるので､消せなく
            // ても失敗にはしない｡
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}
