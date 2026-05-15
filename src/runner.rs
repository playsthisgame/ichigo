use anyhow::{Context, Result};
use colored::Colorize;
use reqwest::blocking::Client;
use std::collections::HashMap;
use std::env;

use crate::config::RequestConfig;

pub fn run_request(config: &RequestConfig, vars: &HashMap<String, String>) -> Result<()> {
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

    println!("{} {}", method.bold().blue(), url.cyan());
    for (k, v) in &headers {
        println!("  {}: {}", k.dimmed(), v);
    }
    println!();

    let client = Client::builder()
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

    for (k, v) in &headers {
        req = req.header(k.as_str(), v.as_str());
    }

    if !query.is_empty() {
        req = req.query(&query);
    }

    if let Some(body) = &config.body {
        let data = interpolate(&body.data, vars);
        req = req
            .header("Content-Type", body.content_type.as_str())
            .body(data);
    }

    let response = req
        .send()
        .with_context(|| format!("Failed to connect to {}", url))?;

    let status = response.status();
    let status_line = format!(
        "{} {}",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    );
    let status_colored = if status.is_success() {
        status_line.green().bold()
    } else if status.is_client_error() {
        status_line.yellow().bold()
    } else if status.is_server_error() {
        status_line.red().bold()
    } else {
        status_line.normal().bold()
    };
    println!("{}", status_colored);
    println!();

    let body_text = response.text().context("Failed to read response body")?;

    if body_text.is_empty() {
        println!("{}", "(empty body)".dimmed());
    } else if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body_text) {
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        println!("{}", body_text);
    }

    Ok(())
}

fn interpolate(s: &str, vars: &HashMap<String, String>) -> String {
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
