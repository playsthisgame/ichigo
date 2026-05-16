use anyhow::{Context, Result};
use colored::Colorize;
use std::collections::HashMap;

use crate::{config::RequestConfig, utils::send_request};

pub fn run_request(config: &RequestConfig, vars: &HashMap<String, String>) -> Result<()> {
    let response = send_request(config, vars)?;

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
