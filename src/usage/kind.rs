//! 課金される resource の種別 (#162): X の単価は種別ごとに 10 倍違う
//! (Posts $0.005､Users $0.010､Owned Reads $0.001､書き込みは per request)
//! ので､合算せず種別ごとに持つ｡ここは「どの [`Endpoint`] がどの種別で
//! 課金されるか」の対応表 ([`Endpoint::kind`]) と､レスポンス body から
//! 課金対象の id を取り出す仕事 ([`extract_resource_ids`]) を持つ｡

use crate::rate_limit::Endpoint;

/// 課金される resource の種別 (#162)｡
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ResourceKind {
    /// 他人の post — home timeline､単一ユーザーの timeline､list の
    /// timeline､`GET /2/tweets?ids=`｡$0.005 / resource｡
    Posts,
    /// 他人のアカウント情報 — screen name の解決､フォロー一覧､list の
    /// メンバー｡$0.010 / resource｡
    Users,
    /// 自分のデータ (Owned Reads) — `/2/users/me`､自分が所有する list｡
    /// $0.001 / resource｡
    Owned,
    /// 書き込み — post/repost/like の作成・削除､list メンバーの追加・削除｡
    /// per resource ではなく per request｡
    Write,
}

impl ResourceKind {
    /// [`build_report`](super::report::build_report) が種別ごとの内訳を
    /// 一巡できるように — [`Endpoint::ALL`] と同じ理由で並べてある｡
    pub(crate) const ALL: [Self; 4] = [Self::Posts, Self::Users, Self::Owned, Self::Write];

    /// `usage.json` の `dedup` と `--usage` の `by_kind` が使う文字列キー｡
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Posts => "posts",
            Self::Users => "users",
            Self::Owned => "owned",
            Self::Write => "write",
        }
    }
}

impl Endpoint {
    /// この endpoint が課金される resource の種別 (#162 issue の対応表そのもの)｡
    ///
    /// 2 箇所は安全側に倒してある — 実際には Owned Reads ($0.001) の可能性が
    /// あるが､著者を見て仕分けるコストを払わず高い方の単価で数える:
    ///
    /// - `Timeline`: 見ている相手が自分自身の post でも Posts ($0.005) に倒す｡
    ///   ponytail: 天井は「自分の timeline を見るだけで 5 倍の見積りになる」｡
    ///   上げるなら､返ってきた post の `author_id` と signed-in user id を
    ///   比較して Owned/Posts を仕分ける枝を [`extract_resource_ids`] の隣に足す｡
    /// - `Following` / `ListMembers`: 自分のフォロー一覧も Users ($0.010) に
    ///   倒す｡同じ理由､同じ ponytail｡
    ///
    /// 網羅的な `match` にしてあるので､新しい [`Endpoint`] variant を足して
    /// ここを更新し忘れるとコンパイルが落ちる — [`Endpoint::ALL`] の doc が
    /// 同じ理由で挙げている失敗モードだ｡
    pub(crate) fn kind(self) -> ResourceKind {
        // ponytail-red(#162): まだ対応表を実装していない｡すべて Posts を
        // 返す誤った既定値で､id 抽出とセットで RED を確認するための stub｡
        let _ = self;
        ResourceKind::Posts
    }
}

/// レスポンス body の `data` から課金対象の id を取り出す (#162)｡
///
/// `data` が配列なら要素ごとの `id`､単一オブジェクト (`/2/users/me` や
/// `/2/users/by/username/:username` が返す形) ならその 1 件の `id` — 配列か
/// オブジェクトかは body の実際の形から判定し､endpoint ごとの表は持たない｡
///
/// `data` が無い・parse できない・要素に `id` が無い場合はそれぞれ黙って
/// 空を返す｡失敗したレスポンスは課金されない
/// (`pricing.md` "Only successful responses that return data are billed") ので､
/// 4xx/5xx の body は `data` を持たず､ここで自然に 0 件になる｡
///
/// `includes` は数えない: リポストの元投稿 (`includes.tweets`) が Post として
/// 課金されていないことは実測済み (`pricing.md` 実測ログ 4)｡**`includes.users`
/// が Users 単価で別課金されているかは未検証** — 外れていたら､ここで
/// `includes.users` も見るよう直す｡
pub(crate) fn extract_resource_ids(body: &str) -> Vec<String> {
    // ponytail-red(#162): まだ抽出していない｡RED を確認するための stub｡
    let _ = body;
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Endpoint::kind (#162 の対応表) ---

    #[test]
    fn maps_each_endpoint_to_its_priced_resource_kind() {
        use ResourceKind::{Owned, Posts, Users, Write};

        let cases = [
            (Endpoint::UserLookup, Users),
            (Endpoint::Me, Owned),
            (Endpoint::Timeline, Posts),
            (Endpoint::HomeTimeline, Posts),
            (Endpoint::ListTimeline, Posts),
            (Endpoint::TweetById, Posts),
            (Endpoint::Following, Users),
            (Endpoint::ListMembers, Users),
            (Endpoint::OwnedLists, Owned),
            (Endpoint::CreatePost, Write),
            (Endpoint::CreateRepost, Write),
            (Endpoint::DeleteRepost, Write),
            (Endpoint::CreateLike, Write),
            (Endpoint::DeleteLike, Write),
            (Endpoint::DeletePost, Write),
            (Endpoint::AddListMember, Write),
            (Endpoint::RemoveListMember, Write),
        ];
        assert_eq!(
            cases.len(),
            Endpoint::ALL.len(),
            "every tracked endpoint must have a case here"
        );
        for (endpoint, expected) in cases {
            assert_eq!(endpoint.kind(), expected, "{endpoint:?}");
        }
    }

    // --- extract_resource_ids ---

    #[test]
    fn extracts_an_id_per_element_of_a_data_array() {
        let body = r#"{"data":[{"id":"1","text":"a"},{"id":"2","text":"b"}]}"#;
        assert_eq!(extract_resource_ids(body), vec!["1", "2"]);
    }

    #[test]
    fn extracts_the_single_id_of_a_data_object_for_users_me() {
        let body = r#"{"data":{"id":"5685672","name":"U","username":"usadamasa"}}"#;
        assert_eq!(extract_resource_ids(body), vec!["5685672"]);
    }

    #[test]
    fn extracts_the_single_id_of_a_data_object_for_username_lookup() {
        // `GET /2/users/by/username/:username` も同じ object 形で返る —
        // endpoint ごとの表ではなく body の形そのものから判定することの根拠｡
        let body = r#"{"data":{"id":"783214","name":"Twitter","username":"Twitter"}}"#;
        assert_eq!(extract_resource_ids(body), vec!["783214"]);
    }

    #[test]
    fn is_empty_when_the_response_has_no_data_field() {
        let body = r#"{"meta":{"result_count":0}}"#;
        assert!(extract_resource_ids(body).is_empty());
    }

    #[test]
    fn is_empty_for_an_unparsable_body() {
        assert!(extract_resource_ids("not json").is_empty());
    }

    #[test]
    fn skips_elements_missing_an_id() {
        let body = r#"{"data":[{"text":"no id"},{"id":"2"}]}"#;
        assert_eq!(extract_resource_ids(body), vec!["2"]);
    }

    #[test]
    fn does_not_count_includes() {
        // #162 の未検証事項: `includes.users` が別課金かは分かっていないが､
        // 数えないのが今の決定 — ここではその決定どおり `includes` を無視する
        // ことだけを確かめる｡
        let body = r#"{"data":[{"id":"1"}],"includes":{"users":[{"id":"99"},{"id":"100"}]}}"#;
        assert_eq!(extract_resource_ids(body), vec!["1"]);
    }
}
