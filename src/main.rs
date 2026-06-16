use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use colored::Colorize;
use std::collections::HashMap;
use std::fs;

mod config;
mod runner;
mod tester;
mod tui;
mod utils;

use config::{
    global_config_path, global_dir, list_requests, local_config_path, local_dir,
    prune_empty_parents, resolve_config_path, RequestConfig,
};

use crate::config::ChainConfig;

#[derive(Parser)]
#[command(name = "ichigo")]
#[command(about = "A CLI HTTP client — store requests in .ichigo/ or ~/.config/ichigo/")]
#[command(version)]
struct Cli {
    /// Subcommand to run; opens the interactive TUI when omitted
    #[command(subcommand)]
    command: Option<Commands>,
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
        /// Print Verbose response
        #[arg(long)]
        verbose: bool,
        #[arg(short = 'p', long = "profile")]
        profile: Option<String>,
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
        #[arg(short = 'p', long = "profile")]
        profile: Option<String>,
    },
    /// Print a shell completion script to stdout
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
    /// Open the interactive TUI (same as running with no subcommand)
    #[command(hide = true)]
    Tui,
    /// Makes a copy of an existing config with a new name
    Copy {
        /// Name of the request to copy
        name: String,
        /// Name of the new request
        new_name: String,
        /// Save to ~/.config/ichigo/ instead of .ichigo/
        #[arg(short, long)]
        global: bool,
    },
}

#[derive(ValueEnum, Clone)]
enum Shell {
    Zsh,
    Bash,
    Fish,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::New { name, method, url, global }) => cmd_new(name, method, url, global),
        Some(Commands::Run { name, vars , verbose, profile }) => cmd_run(name, vars, verbose, profile),
        Some(Commands::List) => cmd_list(),
        Some(Commands::Show { name }) => cmd_show(name),
        Some(Commands::Delete { name }) => cmd_delete(name),
        Some(Commands::Test { name, vars, iterations, profile }) => cmd_test(name, vars, iterations, profile),
        Some(Commands::Completions { shell }) => cmd_completions(shell),
        Some(Commands::Tui) | None => tui::run(),
        Some(Commands::Copy { name, new_name , global}) => cmd_copy(name, new_name, global),
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

fn cmd_run(name: String, vars: Vec<String>, verbose: bool, profile: Option<String>) -> Result<()> {
    if !is_chain_request(&name)? {
        let config = RequestConfig::load(&name)?;
        let mut vars_map = parse_vars(&vars);
        runner::run_request(&config, &mut vars_map, verbose, false, profile)?;
        Ok(())
    } else {
        let config = ChainConfig::load(&name)?;

        let mut current_vars = parse_vars(&vars);
        let steps_len = config.steps.len();
        for (i, step) in config.steps.iter().enumerate() {
            let is_last = i == steps_len - 1;
            if verbose {
                println!("{} {}", "▸".cyan().bold(), step.name.bold());
            }
            let extracted = runner::run_request(step, &mut current_vars, verbose, !is_last, None)?;
            current_vars.extend(extracted);
        }
        Ok(())
    }
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
        if is_chain_request(&entry.name).unwrap_or(false) {
            println!("  {:<7} {}{}", "CHAIN".magenta(), entry.name.bold(), scope);
            continue;
        }
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
    let gd = global_dir();
    let base = if path.starts_with(&gd) { gd } else { local_dir() };
    fs::remove_file(&path)?;
    prune_empty_parents(&path, &base);
    println!("{} '{}'", "Deleted".green().bold(), name);
    Ok(())
}

fn cmd_test(name: String, vars: Vec<String>, iterations: usize, profile: Option<String>) -> Result<()> {
    if is_chain_request(&name).unwrap_or(false) {
        anyhow::bail!("'{}' is a chain config — testing chains is not supported", name);
    }
    let config = RequestConfig::load(&name)?;
    tester::run_tester(&config, &mut parse_vars(&vars), iterations, profile)
}

fn cmd_completions(shell: Shell) -> Result<()> {
    let script = match shell {
        Shell::Zsh => ZSH_COMPLETION,
        Shell::Bash => BASH_COMPLETION,
        Shell::Fish => FISH_COMPLETION,
    };
    print!("{}", script);
    Ok(())
}

fn cmd_copy(name: String, new_name: String, global: bool) -> Result<()> {
    let path = resolve_config_path(&name)
        .with_context(|| format!("Request '{}' not found", name))?;

    // get the path of the new config
    let new_path = if global {
        global_config_path(&new_name)
    } else {
        local_config_path(&new_name)
    };

    // if it already exists then bail
    if new_path.exists() {
        anyhow::bail!("Request '{}' already exists at {}", new_name, new_path.display());
    }
    // create the new path if it doesn't exist
    let dir = new_path.parent().unwrap();
    if !dir.exists() {
        fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create directory {}", dir.display()))?;
    }
    
    if is_chain_request(&name).unwrap_or(false) {
        fs::copy(&path, &new_path)?;
    } else {
        let mut config = RequestConfig::load(&name)?;
        config.name = new_name;
        let content = serde_yaml::to_string(&config)?;
        fs::write(&new_path, &content)?;
    }

    println!("{} {}", "Created".green().bold(), new_path.display());
    Ok(())

}

fn is_chain_request(name: &String) -> Result<bool> {
    let path = resolve_config_path(name)
        .with_context(|| format!("Request '{}' not found", name))?;
    let content = fs::read_to_string(&path)?;
    Ok(content.contains("steps:"))
}

const ZSH_COMPLETION: &str = r#"_ichigo_configs() {
    local -a configs
    local base f rel
    for base in "$PWD/.ichigo" "${HOME}/.config/ichigo"; do
        [[ -d "$base" ]] || continue
        while IFS= read -r f; do
            rel="${f#$base/}"
            configs+=("${rel%.yaml}")
        done < <(find "$base" -name "*.yaml" 2>/dev/null)
    done
    _describe 'config name' configs
}

_ichigo() {
    local -a cmds
    cmds=(
        'new:Create a new request config'
        'run:Execute a configured request'
        'list:List all configured requests'
        'show:Print a request config'
        'delete:Delete a request config'
        'test:Run the configured tests'
        'copy:Makes a copy of an existing config with a new name'
        'completions:Print a shell completion script'
    )

    _arguments -C '1: :->cmd' '*:: :->args'

    case $state in
        cmd)
            _describe 'command' cmds
            ;;
        args)
            case $words[1] in
                run|show|delete|test|copy)
                    _arguments '1: :_ichigo_configs'
                    ;;
                completions)
                    _arguments '1: :(zsh bash fish)'
                    ;;
            esac
            ;;
    esac
}

compdef _ichigo ichigo
"#;

const BASH_COMPLETION: &str = r#"_ichigo_completions() {
    local cur="${COMP_WORDS[COMP_CWORD]}"
    local cmd="${COMP_WORDS[1]}"

    case "$cmd" in
        run|show|delete|test|copy)
            local configs=() base f rel
            for base in "$PWD/.ichigo" "${HOME}/.config/ichigo"; do
                [[ -d "$base" ]] || continue
                while IFS= read -r f; do
                    rel="${f#$base/}"
                    configs+=("${rel%.yaml}")
                done < <(find "$base" -name "*.yaml" 2>/dev/null)
            done
            COMPREPLY=($(compgen -W "${configs[*]}" -- "$cur"))
            ;;
        completions)
            COMPREPLY=($(compgen -W "zsh bash fish" -- "$cur"))
            ;;
        *)
            COMPREPLY=($(compgen -W "new run list show delete test copy completions" -- "$cur"))
            ;;
    esac
}

complete -F _ichigo_completions ichigo
"#;

const FISH_COMPLETION: &str = r#"function __ichigo_configs
    for base in (pwd)/.ichigo $HOME/.config/ichigo
        test -d $base || continue
        find $base -name "*.yaml" 2>/dev/null | while read -l f
            set -l rel (string replace -- "$base/" "" $f)
            echo (string replace -r '\.yaml$' '' -- $rel)
        end
    end
end

set -l config_cmds run show delete test copy

complete -c ichigo -f
complete -c ichigo -n "not __fish_seen_subcommand_from new run list show delete test copy completions" \
    -a "new run list show delete test copy completions"
complete -c ichigo -n "__fish_seen_subcommand_from $config_cmds" \
    -a "(__ichigo_configs)"
complete -c ichigo -n "__fish_seen_subcommand_from completions" \
    -a "zsh bash fish"
"#;
