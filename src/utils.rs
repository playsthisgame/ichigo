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