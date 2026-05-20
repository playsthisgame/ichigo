use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
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

use crate::config::ChainConfig;

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
        /// Print Verbose response
        #[arg(long)]
        verbose: bool,
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
    /// Print a shell completion script to stdout
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
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
        Commands::New { name, method, url, global } => cmd_new(name, method, url, global),
        Commands::Run { name, vars , verbose } => cmd_run(name, vars, verbose),
        Commands::List => cmd_list(),
        Commands::Show { name } => cmd_show(name),
        Commands::Delete { name } => cmd_delete(name),
        Commands::Test { name, vars, iterations } => cmd_test(name, vars, iterations),
        Commands::Completions { shell } => cmd_completions(shell),
        Commands::Copy { name, new_name , global} => cmd_copy(name, new_name, global),
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

fn cmd_run(name: String, vars: Vec<String>, verbose: bool) -> Result<()> {
    let path = resolve_config_path(&name)
        .with_context(|| format!("Request '{}' not found", name))?;
    let content = fs::read_to_string(&path)?;
    if !content.contains("steps:") {
        let config = RequestConfig::load(&name)?;
        runner::run_request(&config, &parse_vars(&vars), verbose, false)?;
        Ok(())
    } else {
        let config = ChainConfig::load(&name)?;

        let mut current_vars = parse_vars(&vars);
        let steps_len = config.steps.len();
        for (i, step) in config.steps.iter().enumerate() {
            let is_last = i == steps_len - 1;
            if verbose {
                println!("{} {}", "▸".cyan().bold(), step.name.bold());
                println!();
            }
            let extracted = runner::run_request(&step, &current_vars, verbose, !is_last)?;
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
    // get the path of the existing config
    let path = if global {
        global_config_path(&name)
    } else {
        local_config_path(&name)
    };

    // if it doesn't exists then bail
    if !path.exists() {
        anyhow::bail!("Request '{}' does not exists at {}", name, path.display());
    }

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
    
    // load the config and update the name
    let mut config = RequestConfig::load(&name)?;
    config.name = new_name;
    
    // convert the config to a string
    let content = serde_yaml::to_string(&config)?;

    // write the contents to the new path
    fs::write(&new_path, &content)?;

    println!("{} {}", "Created".green().bold(), new_path.display());
    Ok(())

}

const ZSH_COMPLETION: &str = r#"_ichigo_configs() {
    local -a configs
    local f
    if [[ -d "$PWD/.ichigo" ]]; then
        for f in "$PWD/.ichigo"/*.yaml(N); do
            configs+=("${f:t:r}")
        done
    fi
    local global_dir="${HOME}/.config/ichigo"
    if [[ -d "$global_dir" ]]; then
        for f in "$global_dir"/*.yaml(N); do
            configs+=("${f:t:r}")
        done
    fi
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
            local configs=()
            if [[ -d "$PWD/.ichigo" ]]; then
                for f in "$PWD/.ichigo"/*.yaml; do
                    [[ -f "$f" ]] && configs+=("$(basename "${f%.yaml}")")
                done
            fi
            local global_dir="${HOME}/.config/ichigo"
            if [[ -d "$global_dir" ]]; then
                for f in "$global_dir"/*.yaml; do
                    [[ -f "$f" ]] && configs+=("$(basename "${f%.yaml}")")
                done
            fi
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
    set -l local_dir (pwd)/.ichigo
    set -l global_dir $HOME/.config/ichigo
    for f in $local_dir/*.yaml $global_dir/*.yaml
        if test -f $f
            echo (basename $f .yaml)
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
