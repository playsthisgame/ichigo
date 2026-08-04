//! Renders a `RequestConfig` as a paste-ready cURL command.
//!
//! Pure string work: no IO, no clipboard, no TUI types. It takes the same two
//! inputs as `utils::send_request` — a config and a resolved variable map — and
//! must stay in step with it, since a command that disagrees with what ichigo
//! actually sends is worse than no command at all.
//!
//! Substitution is shared (`utils::interpolate`). The rules duplicated from
//! `send_request`, and therefore the real drift risk, are:
//!   * query parameters are folded into the URL rather than sent separately
//!   * a body contributes a `Content-Type` header on top of `config.headers`

use std::collections::HashMap;

use reqwest::Url;

use crate::config::RequestConfig;
use crate::utils::interpolate;

/// Builds a multi-line cURL command equivalent to the request `send_request`
/// would issue for this config and these variables.
pub fn to_curl(config: &RequestConfig, vars: &HashMap<String, String>) -> String {
    let method = config.method.to_uppercase();

    // curl already defaults to GET; spelling it out is noise.
    let mut head = String::from("curl");
    if method != "GET" {
        head.push_str(&format!(" -X {}", method));
    }
    head.push(' ');
    head.push_str(&shell_quote(&build_url(config, vars)));

    let mut parts = vec![head];

    // Headers come out of a HashMap, so iteration order is arbitrary. Sort:
    // a command that differs between invocations can't be diffed or tested.
    let mut headers: Vec<(String, String)> = config
        .headers
        .iter()
        .map(|(k, v)| (interpolate(k, vars), interpolate(v, vars)))
        .collect();
    headers.sort();
    for (name, value) in headers {
        parts.push(format!("-H {}", shell_quote(&format!("{}: {}", name, value))));
    }

    if let Some(body) = &config.body {
        // Mirrors send_request, which sets Content-Type from the body in
        // addition to whatever `headers` already carries.
        parts.push(format!(
            "-H {}",
            shell_quote(&format!("Content-Type: {}", body.content_type))
        ));
        // --data-raw rather than -d: -d strips newlines (corrupting multi-line
        // JSON) and reads a leading '@' as a filename.
        parts.push(format!(
            "--data-raw {}",
            shell_quote(&interpolate(&body.data, vars))
        ));
    }

    parts.join(" \\\n  ")
}

/// Interpolates the URL and folds the query map into it, matching how
/// `send_request` hands the query to reqwest.
fn build_url(config: &RequestConfig, vars: &HashMap<String, String>) -> String {
    let base = interpolate(&config.url, vars);
    if config.query.is_empty() {
        return base;
    }

    let mut query: Vec<(String, String)> = config
        .query
        .iter()
        .map(|(k, v)| (interpolate(k, vars), interpolate(v, vars)))
        .collect();
    query.sort();

    // Url::parse normalises as it goes — lowercasing the host among other
    // things — which would rewrite an unresolved `{{HOST}}` to `{{host}}` and
    // leave the user a placeholder they can't find. A URL still carrying
    // placeholders can't be sent anyway, so keep it verbatim.
    let parsed = if base.contains("{{") { None } else { Url::parse(&base).ok() };

    match parsed {
        Some(mut url) => {
            {
                let mut pairs = url.query_pairs_mut();
                for (k, v) in &query {
                    pairs.append_pair(k, v);
                }
            }
            url.to_string()
        }
        // Placeholder-bearing or genuinely unparseable. Silently dropping the
        // query would make the command wrong in a way that's hard to notice,
        // so encode it through the same serializer against a dummy base and
        // graft it onto the URL as written.
        None => {
            let mut dummy = Url::parse("http://placeholder.invalid/").expect("static URL parses");
            {
                let mut pairs = dummy.query_pairs_mut();
                for (k, v) in &query {
                    pairs.append_pair(k, v);
                }
            }
            let encoded = dummy.query().unwrap_or_default();
            let separator = if base.contains('?') { '&' } else { '?' };
            format!("{}{}{}", base, separator, encoded)
        }
    }
}

/// Wraps a value in single quotes so the shell reproduces it literally.
///
/// Single quotes suppress every form of expansion, so the only character
/// needing care is a single quote itself: close, emit an escaped one, reopen.
/// Quoting is unconditional — deciding a value "looks safe" is a bug generator.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Body;

    fn req(method: &str, url: &str) -> RequestConfig {
        RequestConfig {
            name: "t".to_string(),
            method: method.to_string(),
            url: url.to_string(),
            description: None,
            headers: HashMap::new(),
            query: HashMap::new(),
            body: None,
            extract: None,
            profiles: None,
        }
    }

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn get_omits_redundant_method_flag() {
        let out = to_curl(&req("GET", "https://api/x"), &HashMap::new());
        assert!(!out.contains("-X GET"), "got: {out}");
        assert!(out.starts_with("curl 'https://api/x'"), "got: {out}");
    }

    #[test]
    fn non_get_includes_method() {
        let out = to_curl(&req("POST", "https://api/x"), &HashMap::new());
        assert!(out.starts_with("curl -X POST 'https://api/x'"), "got: {out}");
    }

    #[test]
    fn method_is_uppercased() {
        let out = to_curl(&req("delete", "https://api/x"), &HashMap::new());
        assert!(out.contains("-X DELETE"), "got: {out}");
    }

    #[test]
    fn headers_are_emitted_and_sorted() {
        let mut c = req("GET", "https://api/x");
        c.headers.insert("Zeta".to_string(), "1".to_string());
        c.headers.insert("Accept".to_string(), "application/json".to_string());
        c.headers.insert("Middle".to_string(), "2".to_string());
        let out = to_curl(&c, &HashMap::new());
        let accept = out.find("-H 'Accept:").unwrap();
        let middle = out.find("-H 'Middle:").unwrap();
        let zeta = out.find("-H 'Zeta:").unwrap();
        assert!(accept < middle && middle < zeta, "not sorted: {out}");
    }

    #[test]
    fn output_is_stable_across_calls() {
        let mut c = req("GET", "https://api/x");
        for (k, v) in [("A", "1"), ("B", "2"), ("C", "3"), ("D", "4"), ("E", "5")] {
            c.headers.insert(k.to_string(), v.to_string());
            c.query.insert(k.to_lowercase(), v.to_string());
        }
        assert_eq!(to_curl(&c, &HashMap::new()), to_curl(&c, &HashMap::new()));
    }

    #[test]
    fn query_params_are_folded_into_url() {
        let mut c = req("GET", "https://api/x");
        c.query.insert("page".to_string(), "2".to_string());
        let out = to_curl(&c, &HashMap::new());
        assert!(out.contains("https://api/x?page=2"), "got: {out}");
        // Not emitted as a separate flag.
        assert!(!out.contains("--data-urlencode"), "got: {out}");
    }

    #[test]
    fn query_params_are_percent_encoded() {
        let mut c = req("GET", "https://api/x");
        c.query.insert("q".to_string(), "a b&c=d".to_string());
        c.query.insert("né".to_string(), "café".to_string());
        let out = to_curl(&c, &HashMap::new());
        // Raw separators must not leak through unencoded.
        assert!(out.contains("a+b%26c%3Dd"), "got: {out}");
        assert!(out.contains("caf%C3%A9"), "got: {out}");
    }

    #[test]
    fn query_appends_to_existing_query_string() {
        let mut c = req("GET", "https://api/x?existing=1");
        c.query.insert("added".to_string(), "2".to_string());
        let out = to_curl(&c, &HashMap::new());
        assert!(out.contains("existing=1"), "got: {out}");
        assert!(out.contains("added=2"), "got: {out}");
    }

    #[test]
    fn unparseable_url_keeps_its_query() {
        // An unresolved placeholder in the host: the query must not vanish.
        let mut c = req("GET", "http://{{HOST}}/x");
        c.query.insert("page".to_string(), "2".to_string());
        let out = to_curl(&c, &HashMap::new());
        assert!(out.contains("{{HOST}}"), "got: {out}");
        assert!(out.contains("page=2"), "got: {out}");
    }

    #[test]
    fn body_carries_content_type() {
        let mut c = req("POST", "https://api/x");
        c.body = Some(Body {
            content_type: "application/json".to_string(),
            data: r#"{"name":"ada"}"#.to_string(),
        });
        let out = to_curl(&c, &HashMap::new());
        assert!(out.contains("-H 'Content-Type: application/json'"), "got: {out}");
        assert!(out.contains(r#"--data-raw '{"name":"ada"}'"#), "got: {out}");
    }

    #[test]
    fn multiline_body_is_preserved() {
        let mut c = req("POST", "https://api/x");
        c.body = Some(Body {
            content_type: "application/json".to_string(),
            data: "{\n  \"a\": 1\n}".to_string(),
        });
        let out = to_curl(&c, &HashMap::new());
        assert!(out.contains("{\n  \"a\": 1\n}"), "newlines collapsed: {out}");
    }

    #[test]
    fn single_quotes_are_escaped() {
        let mut c = req("GET", "https://api/x");
        c.headers.insert("X-Note".to_string(), "it's here".to_string());
        let out = to_curl(&c, &HashMap::new());
        // Close, escaped quote, reopen — the only escape single quotes allow.
        assert!(out.contains(r"'X-Note: it'\''s here'"), "got: {out}");
    }

    #[test]
    fn shell_metacharacters_are_not_expandable() {
        let mut c = req("GET", "https://api/x");
        c.headers.insert("A".to_string(), "$HOME `id` a b".to_string());
        let out = to_curl(&c, &HashMap::new());
        // Inside single quotes none of these expand.
        assert!(out.contains("-H 'A: $HOME `id` a b'"), "got: {out}");
    }

    #[test]
    fn placeholders_are_substituted_from_vars() {
        let mut c = req("GET", "https://{{HOST}}/x");
        c.headers.insert("Authorization".to_string(), "Bearer {{TOKEN}}".to_string());
        c.query.insert("u".to_string(), "{{USER}}".to_string());
        c.body = Some(Body {
            content_type: "application/json".to_string(),
            data: r#"{"t":"{{TOKEN}}"}"#.to_string(),
        });
        let out = to_curl(&c, &vars(&[("HOST", "api.test"), ("TOKEN", "abc123"), ("USER", "ada")]));
        assert!(out.contains("https://api.test/x"), "got: {out}");
        assert!(out.contains("Bearer abc123"), "got: {out}");
        assert!(out.contains("u=ada"), "got: {out}");
        assert!(out.contains(r#"{"t":"abc123"}"#), "got: {out}");
        assert!(!out.contains("{{"), "placeholder left behind: {out}");
    }

    #[test]
    fn header_names_are_interpolated_too() {
        // send_request interpolates keys as well as values.
        let mut c = req("GET", "https://api/x");
        c.headers.insert("X-{{KIND}}".to_string(), "v".to_string());
        let out = to_curl(&c, &vars(&[("KIND", "Trace")]));
        assert!(out.contains("-H 'X-Trace: v'"), "got: {out}");
    }

    #[test]
    fn unresolved_placeholder_stays_literal() {
        let mut c = req("GET", "https://api/x");
        c.headers.insert("A".to_string(), "{{NOPE_NOT_SET_ANYWHERE}}".to_string());
        let out = to_curl(&c, &HashMap::new());
        assert!(out.contains("{{NOPE_NOT_SET_ANYWHERE}}"), "got: {out}");
    }

    #[test]
    fn environment_is_the_fallback() {
        // Matches interpolate's order: vars map first, then the environment.
        unsafe { std::env::set_var("ICHIGO_CURL_TEST_TOKEN", "from-env") };
        let mut c = req("GET", "https://api/x");
        c.headers.insert("A".to_string(), "{{ICHIGO_CURL_TEST_TOKEN}}".to_string());
        let out = to_curl(&c, &HashMap::new());
        assert!(out.contains("-H 'A: from-env'"), "got: {out}");

        // An explicit var still wins over the environment.
        let out = to_curl(&c, &vars(&[("ICHIGO_CURL_TEST_TOKEN", "from-map")]));
        assert!(out.contains("-H 'A: from-map'"), "got: {out}");
        unsafe { std::env::remove_var("ICHIGO_CURL_TEST_TOKEN") };
    }

    #[test]
    fn continuations_join_flags() {
        let mut c = req("POST", "https://api/x");
        c.headers.insert("A".to_string(), "1".to_string());
        let out = to_curl(&c, &HashMap::new());
        assert!(out.contains(" \\\n  -H "), "got: {out}");
    }
}
