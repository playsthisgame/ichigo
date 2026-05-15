use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::collections::HashMap;
use std::fs;

mod config;
mod runner;

use config::{config_path, list_requests, RequestConfig, OXIDE_DIR};

#[derive(Parser)]
#[command(name = "oxide")]
#[command(about = "A CLI HTTP client — store requests in .oxide/, run them on demand")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new request config
    New {
        /// Name for the request (becomes .oxide/<name>.yaml)
        name: String,
        /// HTTP method
        #[arg(short, long, default_value = "GET")]
        method: String,
        /// Target URL
        #[arg(short, long)]
        url: Option<String>,
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::New { name, method, url } => cmd_new(name, method, url),
        Commands::Run { name, vars } => cmd_run(name, vars),
        Commands::List => cmd_list(),
        Commands::Show { name } => cmd_show(name),
        Commands::Delete { name } => cmd_delete(name),
    }
}

fn cmd_new(name: String, method: String, url: Option<String>) -> Result<()> {
    let path = config_path(&name);
    if path.exists() {
        anyhow::bail!("Request '{}' already exists at {}", name, path.display());
    }

    let dir = std::path::PathBuf::from(OXIDE_DIR);
    if !dir.exists() {
        fs::create_dir_all(&dir).context("Failed to create .oxide directory")?;
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

fn cmd_run(name: String, vars: Vec<String>) -> Result<()> {
    let mut var_map = HashMap::new();
    for var in &vars {
        match var.split_once('=') {
            Some((k, v)) => {
                var_map.insert(k.to_string(), v.to_string());
            }
            None => eprintln!(
                "{} ignoring malformed var '{}' (expected KEY=VALUE)",
                "warning:".yellow(),
                var
            ),
        }
    }
    let config = RequestConfig::load(&name)?;
    runner::run_request(&config, &var_map)
}

fn cmd_list() -> Result<()> {
    let requests = list_requests()?;
    if requests.is_empty() {
        println!("No requests found. Use `oxide new <name>` to create one.");
        return Ok(());
    }
    println!("{}", "Requests:".bold());
    for name in &requests {
        match RequestConfig::load(name) {
            Ok(config) => {
                let desc = config
                    .description
                    .filter(|s| !s.is_empty())
                    .map(|s| format!(" — {}", s))
                    .unwrap_or_default();
                println!(
                    "  {:<7} {}{}",
                    config.method.cyan(),
                    name.bold(),
                    desc.dimmed(),
                );
                println!("          {}", config.url.dimmed());
            }
            Err(e) => println!("  {} ({})", name, e),
        }
    }
    Ok(())
}

fn cmd_show(name: String) -> Result<()> {
    let path = config_path(&name);
    let content = fs::read_to_string(&path)
        .with_context(|| format!("Request '{}' not found", name))?;
    print!("{}", content);
    Ok(())
}

fn cmd_delete(name: String) -> Result<()> {
    let path = config_path(&name);
    if !path.exists() {
        anyhow::bail!("Request '{}' not found", name);
    }
    fs::remove_file(&path)?;
    println!("{} '{}'", "Deleted".green().bold(), name);
    Ok(())
}
