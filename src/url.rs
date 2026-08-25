//! URL を `format!` ではなく部品から組み立てる (#165)｡
//!
//! このクレートが送る URL はどれもかつてフォーマット文字列だった: パスも
//! `?` も `&` も値も一緒に書き並べ､`percent_encode` は `oauth::pkce` では
//! 必要な値に手で呼び､`x_api::client` ではどの値にも呼んでいなかった｡
//! この配置にはコンパイラが検査できるものが何ひとつない｡#161 がその証拠だ:
//! スコープを 1 つ足すのにテスト内の長い URL リテラルを手で編集することに
//! なり､`&` の書き忘れやエンコード漏れは､よくてランタイムの 400 だった｡
//!
//! だからここでは値は値として渡し､区切り文字とエスケープはこの
//! モジュールの仕事にする｡
//!
//! # エスケープ方針が 2 つあるのは､仕様が 2 つあるから
//!
//! [`Escaping::Form`] は RFC 3986 の `unreserved` 集合の外をすべて
//! エスケープする｡OAuth 2.0 §3.1 が求めるのがこれだ — 認可リクエストの
//! パラメータは `application/x-www-form-urlencoded` なので､`scope` の
//! 空白は `%20` として､`redirect_uri` の `:` と `/` はエスケープされた
//! 形で送られなければならない｡
//!
//! [`Escaping::Api`] は同じ集合に 1 文字だけ足したもので､**カンマを素の
//! まま残す**｡英数字でない X API のクエリ値はどれもカンマ区切りのリスト
//! (`tweet.fields`, `expansions`, `ids`) で､docs.x.com はそのカンマを
//! 素で書いている｡`%2C` は仕様に従うサーバならどれも同じものにデコード
//! するが､このクレートは有料の API に対してそれを試せない｡だから
//! エンドポイントがずっと受け取ってきたバイトを､これからも受け取らせる｡
//!
//! # `url` クレートを使わない理由
//!
//! すでに依存ツリーには入っている (gpui → git2 → url) ので､ビルド時間を
//! 理由にどちらとも言えない｡理由はこの仕事ができないことだ:
//! `url::form_urlencoded` は空白を `+`､カンマを `%2C` として直列化する｡
//! どちらも今このクレートが送っているものと違い､しかも設定で変えられない
//! — 採用すれば､ビルダーを得るために回線に乗る URL をすべて変える羽目に
//! なる｡#165 が求めたことの正反対だ｡

use std::fmt::Write as _;

/// 値がそのまま保てる文字はどれか — 2 つある理由はモジュールの doc を
/// 参照｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Escaping {
    /// RFC 3986 の `unreserved` に素のカンマを足したもの｡X API 用｡
    Api,
    /// RFC 3986 の `unreserved` だけ｡OAuth の authorize URL 用｡
    Form,
}

impl Escaping {
    /// `byte` がそれ自身として現れてよいかどうか｡
    ///
    /// `unreserved` 集合は `ALPHA / DIGIT / "-" / "." / "_" / "~"` で､
    /// 両方針が共有する｡カンマが [`Escaping::Api`] の唯一の追加分だ｡
    const fn keeps(self, byte: u8) -> bool {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => true,
            b',' => matches!(self, Self::Api),
            _ => false,
        }
    }

    /// この方針で `value` をパーセントエンコードする｡
    fn escape(self, value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        for byte in value.bytes() {
            if self.keeps(byte) {
                out.push(char::from(byte));
            } else {
                // `String` への `write!` は失敗しない｡
                let _ = write!(out, "%{byte:02X}");
            }
        }
        out
    }
}

/// 組み立て中の URL (#165)｡
///
/// **順序は保たれる**｡これは偶然ではなく構造を支えている: このクレートが
/// 何を送るかを固定するテストは URL 文字列を丸ごと比較するので､ここに
/// `HashMap` を置けばテストがランダムに落ちる｡パラメータは入れた順に
/// 出てくる｡
#[derive(Debug)]
pub(crate) struct Url {
    escaping: Escaping,
    /// base にすでに含まれるスキーム・ホスト・パスと､[`Url::segment`] が
    /// 追加した分｡
    prefix: String,
    /// クエリパラメータ｡入る時点でエスケープ済み｡
    query: Vec<(String, String)>,
}

impl Url {
    /// X API 向けの URL — [`Escaping::Api`] を参照｡
    pub(crate) fn api(base: &str) -> Self {
        Self::with(base, Escaping::Api)
    }

    /// パラメータが OAuth のフォームエンコードに従う URL —
    /// [`Escaping::Form`] を参照｡
    pub(crate) fn form(base: &str) -> Self {
        Self::with(base, Escaping::Form)
    }

    fn with(base: &str, escaping: Escaping) -> Self {
        Self {
            escaping,
            prefix: base.to_string(),
            query: Vec::new(),
        }
    }

    /// パスセグメントを 1 つ､エスケープして追加する｡
    ///
    /// 方針によらず常に [`Escaping::Form`] の集合でエスケープする｡この
    /// クレートが組み立てるパスセグメントにリストは無く､id・ユーザー名・
    /// 決まった語だけだからだ｡したがって今日ここへ届く値にとって､この
    /// エスケープは何もしないのと同じだ — list id は全桁が数字であることを
    /// 検証済みで (`Config::resolve`)､post と user の id は数値､
    /// ユーザー名は `@` を剥いである｡これがあるのは､そうでない値のためだ:
    /// セグメントはあるアカウントの timeline を別のアカウントのものから
    /// 隔てるものなので､その中に紛れ込んだ `/` がパスの一部になれては
    /// ならない｡
    pub(crate) fn segment(mut self, segment: &str) -> Self {
        self.prefix.push('/');
        self.prefix.push_str(&Escaping::Form.escape(segment));
        self
    }

    /// クエリパラメータを 1 つ追加する｡
    ///
    /// キーは渡されたまま書く: このクレートのキーはどれもビルダーか
    /// `const` のリテラルで､レスポンスや設定ファイルが供給するものでは
    /// ないので､エスケープしたところで既に正しい名前を壊すことしか
    /// できない｡
    pub(crate) fn param(mut self, key: &str, value: &str) -> Self {
        self.query
            .push((key.to_string(), self.escaping.escape(value)));
        self
    }

    /// 値が数値のクエリパラメータを 1 つ追加する｡
    pub(crate) fn number(self, key: &str, value: u32) -> Self {
        self.param(key, &value.to_string())
    }

    /// 決まったパラメータの組を追加する — 複数のエンドポイントが共有する
    /// `*.fields`/`expansions` の集合｡
    pub(crate) fn params(mut self, pairs: &[(&str, &str)]) -> Self {
        for (key, value) in pairs {
            self = self.param(key, value);
        }
        self
    }

    /// 値があるときだけクエリパラメータを 1 つ追加する｡
    ///
    /// このクレートの省略可能なカーソルがどれも取る形 (`since_id`,
    /// `pagination_token`) であり､以前の `format!` 組み立てが毎回
    /// `match` や `if let` で書き下していた形でもある｡
    pub(crate) fn maybe(self, key: &str, value: Option<&str>) -> Self {
        match value {
            Some(value) => self.param(key, value),
            None => self,
        }
    }

    /// 完成した URL｡
    ///
    /// 何も追加されなければ `?` すら付かないので､クエリを持たない
    /// エンドポイント (`/2/users/me`, `POST /2/tweets`) は素の `format!`
    /// だった頃とまったく同じ形で出てくる｡
    pub(crate) fn build(self) -> String {
        let mut out = self.prefix;
        for (index, (key, value)) in self.query.iter().enumerate() {
            out.push(if index == 0 { '?' } else { '&' });
            out.push_str(key);
            out.push('=');
            out.push_str(value);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_with_no_parameters_carries_no_question_mark() {
        assert_eq!(
            Url::api("https://api.x.com/2").segment("users").build(),
            "https://api.x.com/2/users"
        );
    }

    #[test]
    fn the_first_parameter_opens_the_query_and_the_rest_join_it() {
        assert_eq!(
            Url::api("https://example.test")
                .param("a", "1")
                .param("b", "2")
                .build(),
            "https://example.test?a=1&b=2"
        );
    }

    // `x_api::client` と `oauth::pkce` の URL 固定テストは文字列を丸ごと
    // 比較するので､ここにマップを置けばテストがランダムに落ちる｡
    #[test]
    fn parameters_come_out_in_the_order_they_went_in() {
        assert_eq!(
            Url::api("https://example.test")
                .param("z", "1")
                .param("a", "2")
                .param("m", "3")
                .build(),
            "https://example.test?z=1&a=2&m=3"
        );
    }

    #[test]
    fn segments_join_with_slashes() {
        assert_eq!(
            Url::api("https://api.x.com/2")
                .segment("users")
                .segment("2244994945")
                .segment("tweets")
                .build(),
            "https://api.x.com/2/users/2244994945/tweets"
        );
    }

    // セグメントはあるアカウントのデータを別のアカウントのものから隔てる｡
    // 今日の id は数字でエスケープは何もしないのと同じだが､その中に
    // 紛れ込んだ `/` がパスの一部になれてはならない｡
    #[test]
    fn a_slash_inside_a_segment_cannot_become_part_of_the_path() {
        assert_eq!(
            Url::api("https://api.x.com/2")
                .segment("users")
                .segment("a/b")
                .build(),
            "https://api.x.com/2/users/a%2Fb"
        );
    }

    #[test]
    fn an_absent_optional_parameter_adds_nothing() {
        assert_eq!(
            Url::api("https://example.test")
                .param("a", "1")
                .maybe("cursor", None)
                .build(),
            "https://example.test?a=1"
        );
    }

    #[test]
    fn a_present_optional_parameter_is_added_like_any_other() {
        assert_eq!(
            Url::api("https://example.test")
                .param("a", "1")
                .maybe("cursor", Some("abc"))
                .build(),
            "https://example.test?a=1&cursor=abc"
        );
    }

    #[test]
    fn a_group_of_parameters_is_added_in_its_own_order() {
        assert_eq!(
            Url::api("https://example.test")
                .params(&[("one", "1"), ("two", "2")])
                .build(),
            "https://example.test?one=1&two=2"
        );
    }

    #[test]
    fn a_number_needs_no_conversion_at_the_call_site() {
        assert_eq!(
            Url::api("https://example.test")
                .number("max_results", 20)
                .build(),
            "https://example.test?max_results=20"
        );
    }

    // --- エスケープ ---

    #[test]
    fn both_policies_leave_the_unreserved_set_alone() {
        for escaping in [Escaping::Api, Escaping::Form] {
            assert_eq!(escaping.escape("abcXYZ019-._~"), "abcXYZ019-._~");
        }
    }

    #[test]
    fn both_policies_escape_a_space_and_a_slash() {
        for escaping in [Escaping::Api, Escaping::Form] {
            assert_eq!(escaping.escape("a b/c"), "a%20b%2Fc");
        }
    }

    // 2 つの方針が食い違う唯一の文字であり､方針が 2 つある理由そのもの:
    // 英数字でない X API のクエリ値はどれもカンマ区切りのリストで､
    // これがそれらのエンドポイントにずっと送られてきたバイトだ｡
    #[test]
    fn the_api_policy_leaves_a_comma_raw() {
        assert_eq!(
            Escaping::Api.escape("created_at,entities,public_metrics"),
            "created_at,entities,public_metrics"
        );
    }

    #[test]
    fn the_form_policy_escapes_a_comma() {
        assert_eq!(Escaping::Form.escape("a,b"), "a%2Cb");
    }

    // 素通しにするとクエリの構造を組み替えかねない値｡どちらの方針も
    // これらを保ってはならない: `&` は頼んでもいないパラメータを始めて
    // しまい､`=` は 1 つのパラメータを 2 つに割ってしまう｡
    #[test]
    fn neither_policy_lets_a_value_restructure_the_query() {
        for escaping in [Escaping::Api, Escaping::Form] {
            assert_eq!(escaping.escape("a&b=c"), "a%26b%3Dc");
        }
        assert_eq!(
            Url::api("https://example.test")
                .param("q", "a&injected=1")
                .build(),
            "https://example.test?q=a%26injected%3D1"
        );
    }

    #[test]
    fn a_multi_byte_character_is_escaped_one_byte_at_a_time() {
        // UTF-8 のパーセントエンコードは文字単位ではなくバイト単位だ｡
        assert_eq!(Escaping::Form.escape("あ"), "%E3%81%82");
    }
}
