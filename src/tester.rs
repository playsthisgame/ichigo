use std::collections::HashMap;
use std::time::{Duration, Instant};
use anyhow::Result;
use colored::Colorize;
use reqwest::blocking::Response;

use crate::{config::RequestConfig, utils::send_request};


pub fn run_tester(config: &RequestConfig, vars: &HashMap<String,String>, iterations: usize) -> Result<()> {
    let mut results: HashMap<u16, Vec<Response>> = HashMap::new();
    let mut timings: Vec<Duration> = Vec::with_capacity(iterations);

    for _ in 0..iterations {
        let start = Instant::now();
        let response = send_request(config, vars,false)?;
        timings.push(start.elapsed());
        results.entry(response.status().as_u16()).or_insert_with(Vec::new).push(response);
    }

    println!("{}", "=== Results ===".bold().cyan());
    let mut statuses: Vec<u16> = results.keys().copied().collect();
    statuses.sort();
    for status in statuses {
        let responses = &results[&status];
        let reason = reqwest::StatusCode::from_u16(status)
            .ok()
            .and_then(|s| s.canonical_reason())
            .unwrap_or("Unknown");
        let label = format!("{} {}", status, reason);
        let count = format!("x {}", responses.len());
        if status < 300 {
            println!("  {}  {}", label.green().bold(), count.dimmed());
        } else if status < 500 {
            println!("  {}  {}", label.yellow().bold(), count.dimmed());
        } else {
            println!("  {}  {}", label.red().bold(), count.dimmed());
        }
    }

    let min = timings.iter().min().unwrap();
    let max = timings.iter().max().unwrap();
    let avg = timings.iter().sum::<Duration>() / timings.len() as u32;

    println!("{}", "=== Timings ===".bold().cyan());
    println!("  {}  {}", "avg:".dimmed(), format!("{:.2?}", avg).yellow());
    println!("  {}  {}", "min:".dimmed(), format!("{:.2?}", min).green());
    println!("  {}  {}", "max:".dimmed(), format!("{:.2?}", max).red());

    Ok(())
}