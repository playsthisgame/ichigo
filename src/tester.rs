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

fn ascii_bar(count: usize, max_count: usize) -> (String, String) {
    let filled = (count * 30 / max_count).max(1);
    ("█".repeat(filled), "░".repeat(30 - filled))
}

fn print_ascii_line_graph(timings: &[u64]) {
    if timings.is_empty() {
        return;
    }

    const H: usize = 8;
    const W: usize = 60;

    let min_ms = *timings.iter().min().unwrap();
    let max_ms = *timings.iter().max().unwrap();
    let range = (max_ms - min_ms).max(1);
    let n = timings.len();

    let mut grid: Vec<Vec<bool>> = vec![vec![false; W]; H];
    for col in 0..W {
        let idx = (col * n / W).min(n - 1);
        let ms = timings[idx];
        let row = H - 1 - ((ms - min_ms) as usize * (H - 1) / range as usize);
        grid[row][col] = true;
    }

    let label_w = format!("{}ms", max_ms).len().max(5);

    for (r, row) in grid.iter().enumerate() {
        let ms_val = max_ms.saturating_sub(r as u64 * range / (H as u64 - 1));
        let label = if r == 0 || r == H / 2 || r == H - 1 {
            format!("{:>label_w$}", format!("{}ms", ms_val))
        } else {
            " ".repeat(label_w)
        };
        let dots: String = row.iter().map(|&d| if d { '•' } else { ' ' }).collect();
        println!("  {} {} {}", label.dimmed(), "│".dimmed(), dots.cyan());
    }
    println!("  {} {}", " ".repeat(label_w), format!("└{}", "─".repeat(W + 1)).dimmed());
}

pub fn run_tester(
    config: &RequestConfig,
    vars: &mut HashMap<String, String>,
    iterations: usize,
    profile: Option<String>,
) -> Result<()> {
    if let Some(ref profile_name) = profile {
        if let Some(profiles) = &config.profiles {
            if let Some(found) = profiles.iter().find(|p| p.name.eq_ignore_ascii_case(profile_name)) {
                vars.extend(found.params.clone());
            }
        }
    }
    let results = collect_test_results(config, vars, iterations)?;
    let max_count = results.statuses.iter().map(|s| s.count).max().unwrap_or(1);

    println!("{}", "=== Results ===".bold().cyan());
    for sc in &results.statuses {
        let reason = reqwest::StatusCode::from_u16(sc.status)
            .ok()
            .and_then(|s| s.canonical_reason())
            .unwrap_or("Unknown");
        let (filled, empty) = ascii_bar(sc.count, max_count);
        let count = format!("×{}", sc.count);
        if sc.status < 300 {
            println!("  {}  {}{}  {}  {}", format!("{:>3}", sc.status).green().bold(), filled.green(), empty.dimmed(), reason.dimmed(), count.dimmed());
        } else if sc.status < 500 {
            println!("  {}  {}{}  {}  {}", format!("{:>3}", sc.status).yellow().bold(), filled.yellow(), empty.dimmed(), reason.dimmed(), count.dimmed());
        } else {
            println!("  {}  {}{}  {}  {}", format!("{:>3}", sc.status).red().bold(), filled.red(), empty.dimmed(), reason.dimmed(), count.dimmed());
        }
    }

    println!();
    println!("{}", "=== Timings ===".bold().cyan());
    println!("  {}  {}", "avg:".dimmed(), format!("{:.2?}", results.avg).yellow());
    println!("  {}  {}", "min:".dimmed(), format!("{:.2?}", results.min).green());
    println!("  {}  {}", "max:".dimmed(), format!("{:.2?}", results.max).red());
    println!();
    print_ascii_line_graph(&results.timings);

    Ok(())
}
