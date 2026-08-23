//! Building URLs from parts instead of from `format!` (#165).
//!
//! Every URL this crate sends used to be a format string: the path, the
//! `?`, the `&`s and the values all typed out together, with
//! `percent_encode` called by hand on the values that needed it in
//! `oauth::pkce` and on none of them in `x_api::client`. Nothing about
//! that arrangement could be checked by the compiler. #161 is the
//! evidence: adding one scope meant editing a long URL literal in a test
//! by hand, and a forgotten `&` or a missed encode would have been a
//! runtime 400 at best.
//!
//! So a value goes in as a value here, and the separators and the
//! escaping are this module's business.
//!
//! # Two escaping policies, because there are two specifications
//!
//! [`Escaping::Form`] escapes everything outside RFC 3986's `unreserved`
//! set. That is what OAuth 2.0 §3.1 asks for — the authorization request's
//! parameters are `application/x-www-form-urlencoded`, so a space in
//! `scope` has to travel as `%20` and the `:` and `/` in `redirect_uri`
//! have to travel escaped.
//!
//! [`Escaping::Api`] is the same set plus one character: **a comma is left
//! raw**. Every X API query value that is not alphanumeric is a
//! comma-separated list — `tweet.fields`, `expansions`, `ids` — and
//! docs.x.com writes those commas raw. `%2C` decodes to the same thing on
//! any conformant server, but this crate cannot test that against a paid
//! API, so the byte the endpoint has always received is the byte it keeps
//! receiving.
//!
//! # Why not the `url` crate
//!
//! It is already in the dependency tree (gpui → git2 → url), so there is
//! no build-time argument either way. The argument is that it cannot do
//! this job: `url::form_urlencoded` serializes a space as `+` and a comma
//! as `%2C`. Both differ from what this crate sends today, and neither is
//! configurable — adopting it would mean changing every URL on the wire to
//! get a builder, which is the opposite of what #165 asked for.

use std::fmt::Write as _;

/// Which characters a value is allowed to keep — see the module doc for
/// why there are two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Escaping {
    /// RFC 3986 `unreserved` plus a raw comma. The X API.
    Api,
    /// RFC 3986 `unreserved` and nothing else. OAuth's authorize URL.
    Form,
}

impl Escaping {
    /// Whether `byte` may appear as itself.
    ///
    /// The `unreserved` set is `ALPHA / DIGIT / "-" / "." / "_" / "~"`,
    /// shared by both policies; the comma is [`Escaping::Api`]'s only
    /// addition.
    const fn keeps(self, byte: u8) -> bool {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => true,
            b',' => matches!(self, Self::Api),
            _ => false,
        }
    }

    /// Percent-encode `value` under this policy.
    fn escape(self, value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        for byte in value.bytes() {
            if self.keeps(byte) {
                out.push(char::from(byte));
            } else {
                // `write!` to a `String` is infallible.
                let _ = write!(out, "%{byte:02X}");
            }
        }
        out
    }
}

/// A URL under construction (#165).
///
/// **Order is preserved**, and that is load-bearing rather than
/// incidental: the tests that pin what this crate sends compare whole URL
/// strings, and a `HashMap` here would make them fail at random. Parameters
/// come out in the order they went in.
#[derive(Debug)]
pub(crate) struct Url {
    escaping: Escaping,
    /// The scheme, host and any path already in the base, plus whatever
    /// [`Url::segment`] has appended.
    prefix: String,
    /// Query parameters, escaped on the way in.
    query: Vec<(String, String)>,
}

impl Url {
    /// A URL against the X API — see [`Escaping::Api`].
    pub(crate) fn api(base: &str) -> Self {
        Self::with(base, Escaping::Api)
    }

    /// A URL whose parameters follow OAuth's form encoding — see
    /// [`Escaping::Form`].
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

    /// Append one path segment, escaped.
    ///
    /// Always escaped with [`Escaping::Form`]'s set regardless of the
    /// policy, since no path segment this crate builds is a list: they are
    /// ids, usernames and fixed words. The escaping is therefore a no-op
    /// for every value that reaches it today — a list id is validated to
    /// be all digits (`Config::resolve`), post and user ids are numeric,
    /// and a username has had its `@` stripped. It is here for the value
    /// that is not: a segment is what separates one account's timeline
    /// from another's, so a `/` arriving inside one must not be able to
    /// become part of the path.
    pub(crate) fn segment(mut self, segment: &str) -> Self {
        self.prefix.push('/');
        self.prefix.push_str(&Escaping::Form.escape(segment));
        self
    }

    /// Add one query parameter.
    ///
    /// The key is written as given: every key in this crate is a literal
    /// from a builder or a `const`, never anything a response or a config
    /// file supplies, so escaping one would only be able to corrupt a name
    /// that was already correct.
    pub(crate) fn param(mut self, key: &str, value: &str) -> Self {
        self.query
            .push((key.to_string(), self.escaping.escape(value)));
        self
    }

    /// Add one query parameter whose value is a number.
    pub(crate) fn number(self, key: &str, value: u32) -> Self {
        self.param(key, &value.to_string())
    }

    /// Add a fixed group of parameters — the `*.fields`/`expansions` sets
    /// that several endpoints share.
    pub(crate) fn params(mut self, pairs: &[(&str, &str)]) -> Self {
        for (key, value) in pairs {
            self = self.param(key, value);
        }
        self
    }

    /// Add one query parameter only when there is a value for it.
    ///
    /// The shape every optional cursor in this crate has — `since_id`,
    /// `pagination_token` — and the one the old `format!` builders spelled
    /// out with a `match` or an `if let` each time.
    pub(crate) fn maybe(self, key: &str, value: Option<&str>) -> Self {
        match value {
            Some(value) => self.param(key, value),
            None => self,
        }
    }

    /// The finished URL.
    ///
    /// No `?` at all when nothing was added, so an endpoint with no query
    /// (`/2/users/me`, `POST /2/tweets`) comes out exactly as it did when
    /// it was a bare `format!`.
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

    // The pinned URL tests in `x_api::client` and `oauth::pkce` compare
    // whole strings, so a map here would make them fail at random.
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

    // A segment is what separates one account's data from another's. An
    // id is digits today and the escaping is a no-op, but a `/` arriving
    // inside one must not be able to become part of the path.
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

    // --- escaping ---

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

    // The one character the two policies disagree about, and the whole
    // reason there are two: every non-alphanumeric X API query value is a
    // comma-separated list, and this is the byte those endpoints have
    // always been sent.
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

    // A value that could restructure the query if it were passed through.
    // Neither policy may keep these: `&` would start a parameter that was
    // never asked for and `=` would split one in half.
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
        // UTF-8 percent-encoding is per byte, not per character.
        assert_eq!(Escaping::Form.escape("あ"), "%E3%81%82");
    }
}
