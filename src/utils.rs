use std::{collections::HashMap, env};
use std::time::Duration;
use reqwest::blocking::{Client, Response};
use anyhow::{Context, Result};
use colored::Colorize;

use crate::config::RequestConfig;

/// Builds and fires the HTTP request. This function defines what actually goes
/// on the wire, so `crate::curl::to_curl` mirrors it — keep the two in step,
/// particularly the query handling and the body's Content-Type header.
pub fn send_request(config: & RequestConfig, vars: &HashMap<String,String>, verbose: bool) -> Result<Response>{
    let url = interpolate(&config.url, vars);
    let method = config.method.to_uppercase();

    let headers: Vec<(String, String)> = config
        .headers
        .iter()
        .map(|(k, v)| (interpolate(k, vars), interpolate(v, vars)))
        .collect();

    let query: Vec<(String, String)> = config
        .query
        .iter()
        .map(|(k, v)| (interpolate(k, vars), interpolate(v, vars)))
        .collect();

    let client = Client::builder()
        .timeout(Duration::from_mins(5))
        .build()
        .context("Failed to build HTTP client")?;

    let mut req = match method.as_str() {
        "GET" => client.get(&url),
        "POST" => client.post(&url),
        "PUT" => client.put(&url),
        "PATCH" => client.patch(&url),
        "DELETE" => client.delete(&url),
        "HEAD" => client.head(&url),
        "OPTIONS" => client.request(reqwest::Method::OPTIONS, &url),
        m => anyhow::bail!("Unsupported HTTP method: {}", m),
    };

    if verbose {
        println!("{} {}", method.as_str().magenta().bold(), url.bold());
        for header in &headers {
            println!("  {} {}", format!("{}:", header.0).dimmed(), header.1);
        }
        for q in &query {
            println!("  {} {}", format!("?{}:", q.0).dimmed(), q.1);
        }
        println!();
    }

    for (k, v) in &headers {
        req = req.header(k.as_str(), v.as_str());
    }

    if !&query.is_empty() {
        req = req.query(&query);
    }

    if let Some(body) = &config.body {
        let data = interpolate(&body.data, vars);
        req = req
            .header("Content-Type", body.content_type.as_str())
            .body(data);
    }

    req.send().with_context(|| format!("Failed to connect to {}",url))

}

/// Substitutes `{{VAR}}` placeholders: the supplied map first, then the process
/// environment, leaving anything unresolved as a literal `{{VAR}}`.
///
/// Shared with `crate::curl` so a generated cURL command substitutes exactly
/// what a real request would.
pub(crate) fn interpolate(s: &str, vars: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        result.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        if let Some(end) = rest.find("}}") {
            let var_name = &rest[..end];
            let value = vars
                .get(var_name)
                .cloned()
                .or_else(|| env::var(var_name).ok())
                .unwrap_or_else(|| format!("{{{{{}}}}}", var_name));
            result.push_str(&value);
            rest = &rest[end + 2..];
        } else {
            result.push_str("{{");
        }
    }
    result.push_str(rest);
    result
}
/// Renders a response's headers as one `name: value` line each.
///
/// Sorted by name, because `HeaderMap`'s iteration order is unspecified — a
/// pane that reordered itself between two runs of the same request would read
/// as a bug in the server. The sort is stable, so a header sent more than once
/// (`Set-Cookie`) keeps its values in the order they arrived. A value that is
/// not UTF-8 is shown lossily rather than dropped: a mangled `Set-Cookie` is
/// still evidence that one was sent.
///
/// The result is plain text with no trailing newline, which is what lets the
/// response pane treat header lines as ordinary body lines — the filter,
/// the cursor, and `V`/`y` need no exception for them.
pub fn format_response_headers(headers: &reqwest::header::HeaderMap) -> String {
    let mut rendered: Vec<(&str, String)> = headers
        .iter()
        .map(|(name, value)| {
            let text = value.to_str().map(str::to_string).unwrap_or_else(|_| {
                String::from_utf8_lossy(value.as_bytes()).into_owned()
            });
            (name.as_str(), text)
        })
        .collect();
    rendered.sort_by(|a, b| a.0.cmp(b.0));
    rendered
        .iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

    fn map(pairs: &[(&str, &[u8])]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_bytes(value).unwrap(),
            );
        }
        headers
    }

    #[test]
    fn formats_one_line_per_header_sorted_by_name() {
        let headers = map(&[
            ("x-ratelimit-remaining", b"41"),
            ("content-type", b"application/json"),
            ("etag", b"\"abc\""),
        ]);
        assert_eq!(
            format_response_headers(&headers),
            "content-type: application/json\netag: \"abc\"\nx-ratelimit-remaining: 41"
        );
    }

    #[test]
    fn keeps_repeated_headers_in_arrival_order() {
        let headers = map(&[("set-cookie", b"a=1"), ("set-cookie", b"b=2")]);
        assert_eq!(format_response_headers(&headers), "set-cookie: a=1\nset-cookie: b=2");
    }

    #[test]
    fn shows_a_non_utf8_value_lossily_rather_than_dropping_it() {
        let headers = map(&[("x-odd", &[0xff, 0xfe])]);
        assert_eq!(format_response_headers(&headers), "x-odd: \u{fffd}\u{fffd}");
    }

    #[test]
    fn no_headers_is_the_empty_string() {
        assert_eq!(format_response_headers(&HeaderMap::new()), "");
    }
}
