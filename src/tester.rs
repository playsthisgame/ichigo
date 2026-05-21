use std::collections::HashMap;
use std::time::{Duration, Instant};
use anyhow::Result;
use colored::Colorize;

use crate::{config::RequestConfig, utils::send_request};

pub struct StatusCount {
    pub status: u16,
    pub count: usize,
}

pub struct TestResults {
    pub statuses: Vec<StatusCount>,
    pub avg: Duration,
    pub min: Duration,
    pub max: Duration,
    pub timings: Vec<u64>, // per-iteration milliseconds
}

pub fn collect_test_results(
    config: &RequestConfig,
    vars: &HashMap<String, String>,
    iterations: usize,
) -> Result<TestResults> {
    let mut counts: HashMap<u16, usize> = HashMap::new();
    let mut raw: Vec<Duration> = Vec::with_capacity(iterations);

    for _ in 0..iterations {
        let start = Instant::now();
        let response = send_request(config, vars, false)?;
        raw.push(start.elapsed());
        *counts.entry(response.status().as_u16()).or_insert(0) += 1;
    }

    let mut statuses: Vec<StatusCount> = counts
        .into_iter()
        .map(|(status, count)| StatusCount { status, count })
        .collect();
    statuses.sort_by_key(|s| s.status);

    let min = *raw.iter().min().unwrap();
    let max = *raw.iter().max().unwrap();
    let avg = raw.iter().sum::<Duration>() / raw.len() as u32;
    let timings = raw.iter().map(|d| d.as_millis() as u64).collect();

    Ok(TestResults { statuses, avg, min, max, timings })
}

fn ascii_bar(count: usize, max_count: usize) -> String {
    let width = (count * 30 / max_count).max(1);
    "█".repeat(width)
}

fn ascii_sparkline(timings: &[u64]) -> String {
    const CHARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if timings.is_empty() {
        return String::new();
    }
    let min = *timings.iter().min().unwrap();
    let max = *timings.iter().max().unwrap();
    let range = (max - min).max(1);
    timings
        .iter()
        .map(|&t| {
            let idx = ((t - min) * 7 / range) as usize;
            CHARS[idx.min(7)]
        })
        .collect()
}

pub fn run_tester(
    config: &RequestConfig,
    vars: &HashMap<String, String>,
    iterations: usize,
) -> Result<()> {
    let results = collect_test_results(config, vars, iterations)?;
    let max_count = results.statuses.iter().map(|s| s.count).max().unwrap_or(1);

    println!("{}", "=== Results ===".bold().cyan());
    for sc in &results.statuses {
        let reason = reqwest::StatusCode::from_u16(sc.status)
            .ok()
            .and_then(|s| s.canonical_reason())
            .unwrap_or("Unknown");
        let label = format!("{} {:<20}", sc.status, reason);
        let bar = ascii_bar(sc.count, max_count);
        let count = format!("x {}", sc.count);
        if sc.status < 300 {
            println!("  {}  {}  {}", label.green().bold(), bar.green(), count.dimmed());
        } else if sc.status < 500 {
            println!("  {}  {}  {}", label.yellow().bold(), bar.yellow(), count.dimmed());
        } else {
            println!("  {}  {}  {}", label.red().bold(), bar.red(), count.dimmed());
        }
    }

    println!();
    println!("{}", "=== Timings ===".bold().cyan());
    println!("  {}  {}", "avg:".dimmed(), format!("{:.2?}", results.avg).yellow());
    println!("  {}  {}", "min:".dimmed(), format!("{:.2?}", results.min).green());
    println!("  {}  {}", "max:".dimmed(), format!("{:.2?}", results.max).red());
    println!("  {}", ascii_sparkline(&results.timings));

    Ok(())
}
