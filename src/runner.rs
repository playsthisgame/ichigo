use anyhow::{Context, Result};
use colored::Colorize;
use colored_json::ToColoredJson;

use std::collections::HashMap;
use std::time::Instant;

use crate::{config::RequestConfig, utils::send_request};

pub fn run_request(config: &RequestConfig, vars: &mut HashMap<String, String>, verbose: bool, is_chain: bool, profile: Option<String>) -> Result<HashMap<String, String>> {
    if !is_chain && let Some(ref profile_name) = profile
        && let Some(profiles) = &config.profiles
            && let Some(found) = profiles.iter().find(|p| p.name.eq_ignore_ascii_case(profile_name)) {
                vars.extend(found.params.clone());
            }

    // Timed around `send_request` alone, which returns once the response's
    // headers are in — the same thing `tester.rs` measures, so a single run and
    // a load test of the same request report the same number rather than two
    // that quietly mean different things. Reading the body is not in it.
    let started = Instant::now();
    let response = send_request(config, vars, verbose)?;
    let elapsed = started.elapsed();

    let status = response.status();

    let is_json = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("application/json"))
        .unwrap_or(false);

    let body_text = response.text().context("Failed to read response body")?;

    // Printed after the body is read rather than before, because the size is
    // part of the line and there is no size until then. Nothing else prints in
    // between, so the summary still leads the output.
    //
    // Only under `--verbose`: a bare `run` prints the body and nothing else, so
    // `ichigo run thing | jq` keeps working.
    if verbose {
        let summary = crate::utils::format_run_summary(
            status,
            elapsed,
            &crate::media::human_size(body_text.len()),
        );
        let summary_colored = if status.is_success() {
            summary.green().bold()
        } else if status.is_client_error() {
            summary.yellow().bold()
        } else if status.is_server_error() {
            summary.red().bold()
        } else {
            summary.normal().bold()
        };
        println!("{}", summary_colored);
        println!();
    }

    if !is_chain || verbose {
        if body_text.is_empty() {
            println!("{}", "(empty body)".dimmed());
        } else if let Ok(colored) = body_text.to_colored_json_auto() {
            println!("{}", colored);
        } else {
            println!("{}", body_text);
        }
    }

    let extracted  = extract_values(&config.extract, &body_text, is_json)?;

    if verbose && !extracted.is_empty() {
        println!();
        for (k, v) in &extracted {
            println!("  {} {}", format!("← {}:", k).cyan().dimmed(), v.bold());
            println!();
        }
    }

    Ok(extracted)
}

pub fn extract_values(
    extract: &Option<HashMap<String, String>>,
    body_text: &str,
    is_json: bool,
) -> Result<HashMap<String, String>> {
    let mut extracted = HashMap::new();

    if let Some(extractions) = extract {
        if !is_json {
            anyhow::bail!("Cannot extract values: response is not JSON");
        }
        let json: serde_json::Value = serde_json::from_str(body_text)?;
        for (var_name, path) in extractions {
            let path = path.strip_prefix("$.").unwrap_or(path);
            let mut current = &json;
            for segment in path.split('.') {
                current = if let Ok(index) = segment.parse::<usize>() {
                    current
                        .get(index)
                        .with_context(|| format!("Index {} out of bounds in response", index))?
                } else {
                    current
                        .get(segment)
                        .with_context(|| format!("Path '{}' not found in response", segment))?
                };
            }
            let value = match current {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            extracted.insert(var_name.clone(), value);
        }
    }
    Ok(extracted)
}
