use anyhow::{Context, Result};
use colored::Colorize;
use colored_json::ToColoredJson;

use std::collections::HashMap;

use crate::{config::RequestConfig, utils::send_request};

pub fn run_request(config: &RequestConfig, vars: &mut HashMap<String, String>, verbose: bool, is_chain: bool, profile: Option<String>) -> Result<HashMap<String, String>> {
    if !is_chain && let Some(ref profile_name) = profile
        && let Some(profiles) = &config.profiles
            && let Some(found) = profiles.iter().find(|p| p.name.eq_ignore_ascii_case(profile_name)) {
                vars.extend(found.params.clone());
            }

    let response = send_request(config, vars, verbose)?;

    if verbose {
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
    }

    let is_json = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("application/json"))
        .unwrap_or(false);

    let body_text = response.text().context("Failed to read response body")?;

    if !is_chain || verbose {
        if body_text.is_empty() {
            println!("{}", "(empty body)".dimmed());
        } else if let Ok(colored) = body_text.to_colored_json_auto() {
            println!("{}", colored);
        } else {
            println!("{}", body_text);
        }
    }

    let mut extracted = HashMap::new();

    if let Some(extractions) = &config.extract {
        if !is_json {
            anyhow::bail!("Cannot extract values: response is not JSON");
        }
        let json: serde_json::Value = serde_json::from_str(&body_text)?;
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

    if verbose && !extracted.is_empty() {
        println!();
        for (k, v) in &extracted {
            println!("  {} {}", format!("← {}:", k).cyan().dimmed(), v.bold());
            println!();
        }
    }

    Ok(extracted)
}
