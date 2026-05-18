use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::collections::HashMap;
use std::fs;

mod config;
mod runner;
mod tester;
mod utils;

use config::{
    global_config_path, list_requests, local_config_path, resolve_config_path, RequestConfig,
};

#[derive(Parser)]
#[command(name = "ichigo")]
#[command(about = "A CLI HTTP client — store requests in .ichigo/ or ~/.config/ichigo/")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new request config
    New {
        /// Name for the request
        name: String,
        /// HTTP method
        #[arg(short, long, default_value = "GET")]
        method: String,
        /// Target URL
        #[arg(short, long)]
        url: Option<String>,
        /// Save to ~/.config/ichigo/ instead of .ichigo/
        #[arg(short, long)]
        global: bool,
    },
    /// Execute a configured request
    Run {
        /// Name of the request to run
        name: String,
        /// Set a variable: KEY=VALUE (overrides env vars, supports {{KEY}} in config)
        #[arg(short = 'v', long = "var", value_name = "KEY=VALUE")]
        vars: Vec<String>,
    },
    /// List all configured requests
    List,
    /// Print a request config
    Show {
        /// Name of the request
        name: String,
    },
    /// Delete a request config
    Delete {
        /// Name of the request
        name: String,
    },
    /// Run the configured tests
    Test {
        /// Name of the config to test
        name: String,
        /// Set a variable: KEY=VALUE (overrides env vars, supports {{KEY}} in config)
        #[arg(short = 'v', long = "var", value_name = "KEY=VALUE")]
        vars: Vec<String>,
        /// Number of iterations to test
        #[arg(short = 'i', long = "iter")]
        iterations: usize,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::New { name, method, url, global } => cmd_new(name, method, url, global),
        Commands::Run { name, vars } => cmd_run(name, vars),
        Commands::List => cmd_list(),
        Commands::Show { name } => cmd_show(name),
        Commands::Delete { name } => cmd_delete(name),
        Commands::Test { name, vars, iterations } => cmd_test(name, vars, iterations),
    }
}

fn cmd_new(name: String, method: String, url: Option<String>, global: bool) -> Result<()> {
    let path = if global {
        global_config_path(&name)
    } else {
        local_config_path(&name)
    };

    if path.exists() {
        anyhow::bail!("Request '{}' already exists at {}", name, path.display());
    }

    let dir = path.parent().unwrap();
    if !dir.exists() {
        fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create directory {}", dir.display()))?;
    }

    let url = url.unwrap_or_else(|| "https://example.com".to_string());
    let method = method.to_uppercase();
    fs::write(&path, build_template(&name, &method, &url))?;

    println!("{} {}", "Created".green().bold(), path.display());
    Ok(())
}

fn build_template(name: &str, method: &str, url: &str) -> String {
    let body_section = if matches!(method, "POST" | "PUT" | "PATCH") {
        "body:\n  content_type: application/json\n  data: |\n    {\n      \"key\": \"value\"\n    }\n"
    } else {
        "# body:\n#   content_type: application/json\n#   data: |\n#     {\n#       \"key\": \"value\"\n#     }\n"
    };

    format!(
        "name: {name}\nmethod: {method}\nurl: {url}\ndescription: \"\"\n\n\
        # Use {{{{VAR}}}} anywhere in url, headers, query, or body.\n\
        # Values are filled from --var KEY=VALUE flags or environment variables.\n\n\
        headers:\n\
        #   Accept: application/json\n\
        #   Authorization: \"Bearer {{{{TOKEN}}}}\"\n\n\
        query:\n\
        #   page: \"1\"\n\n\
        {body_section}"
    )
}

fn parse_vars(vars: &[String]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for var in vars {
        match var.split_once('=') {
            Some((k, v)) => {
                map.insert(k.to_string(), v.to_string());
            }
            None => eprintln!(
                "{} ignoring malformed var '{}' (expected KEY=VALUE)",
                "warning:".yellow(),
                var
            ),
        }
    }
    map
}

fn cmd_run(name: String, vars: Vec<String>) -> Result<()> {
    let config = RequestConfig::load(&name)?;
    runner::run_request(&config, &parse_vars(&vars))
}

fn cmd_list() -> Result<()> {
    let entries = list_requests()?;
    if entries.is_empty() {
        println!("No requests found. Use `ichigo new <name>` to create one.");
        return Ok(());
    }
    println!("{}", "Requests:".bold());
    for entry in &entries {
        let scope = if entry.global {
            " global".dimmed()
        } else {
            " local".dimmed()
        };
        match RequestConfig::load(&entry.name) {
            Ok(config) => {
                let desc = config
                    .description
                    .filter(|s| !s.is_empty())
                    .map(|s| format!(" — {}", s))
                    .unwrap_or_default();
                println!(
                    "  {:<7} {}{}{}",
                    config.method.cyan(),
                    entry.name.bold(),
                    desc.dimmed(),
                    scope,
                );
                println!("          {}", config.url.dimmed());
            }
            Err(e) => println!("  {} ({})", entry.name, e),
        }
    }
    Ok(())
}

fn cmd_show(name: String) -> Result<()> {
    let path = resolve_config_path(&name)
        .with_context(|| format!("Request '{}' not found", name))?;
    let content = fs::read_to_string(&path)
        .with_context(|| format!("Request '{}' not found", name))?;
    print!("{}", content);
    Ok(())
}

fn cmd_delete(name: String) -> Result<()> {
    let path = resolve_config_path(&name)
        .with_context(|| format!("Request '{}' not found", name))?;
    fs::remove_file(&path)?;
    println!("{} '{}'", "Deleted".green().bold(), name);
    Ok(())
}

fn cmd_test(name: String, vars: Vec<String>, iterations: usize) -> Result<()> {
    let config = RequestConfig::load(&name)?;
    tester::run_tester(&config, &parse_vars(&vars), iterations)
}
