mod clipboard;
mod config;
mod discovery;
mod filter;
mod search;
mod session;
mod theme;
mod tui;

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use config::Config;
use discovery::{apply_filters, discover_profiles};

/// Fast CLI tool for finding and resuming Claude Code sessions.
#[derive(Parser, Debug)]
#[command(
    name = "cc-session",
    version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("BUILD_GIT_HASH"), " ", env!("BUILD_DATE"), ")"),
    about
)]
struct Cli {
    /// Only show sessions newer than duration (e.g. 7d, 2w, 1m)
    #[arg(long)]
    since: Option<String>,

    /// Show at most N sessions
    #[arg(long)]
    last: Option<usize>,

    /// Force light color theme
    #[arg(long = "light", conflicts_with = "dark")]
    light: bool,

    /// Force dark color theme
    #[arg(long = "dark", conflicts_with = "light")]
    dark: bool,

    /// Config file (default: ~/.config/cc-session/config.toml)
    #[arg(long)]
    config: Option<PathBuf>,

    /// Launch mode to resume with, by name (switch inside with Alt-M)
    #[arg(long)]
    mode: Option<String>,

    /// Resume with --dangerously-skip-permissions (selects the launch mode
    /// that passes it)
    #[arg(long = "dangerously-skip-permissions")]
    skip_permissions: bool,

    /// Write session directory to this file on exec (for shell wrapper cd-on-quit)
    #[arg(long = "cd-file", hide = true)]
    cd_file: Option<String>,

    /// Write the resume command to this file instead of running it (used by
    /// the shell function from `cc-session init`)
    #[arg(long = "exec-file", hide = true)]
    exec_file: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Print a shell function that resumes sessions in your current shell,
    /// so it really changes into the session's directory. Add to your rc file:
    /// eval "$(cc-session init bash)"
    Init {
        shell: Shell,
        /// Name of the shell function
        #[arg(long, default_value = "cc-session")]
        name: String,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum Shell {
    Bash,
    Zsh,
}

/// Shell function that runs cc-session with `--exec-file` and then `eval`s
/// the resume command it wrote, so the `cd` happens in the calling shell and
/// the command lands in its history.
fn shell_init(shell: Shell, name: &str) -> String {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "cc-session".to_string());
    let history = match shell {
        Shell::Bash => r#"history -s "$__ccs_cmd""#,
        Shell::Zsh => r#"print -s -- "$__ccs_cmd""#,
    };
    format!(
        r#"unalias {name} 2>/dev/null
{name}() {{
  local __ccs_file __ccs_cmd __ccs_rc
  __ccs_file=$(mktemp) || return
  {exe} --exec-file "$__ccs_file" "$@"
  __ccs_rc=$?
  __ccs_cmd=$(cat "$__ccs_file")
  rm -f "$__ccs_file"
  [ -n "$__ccs_cmd" ] || return $__ccs_rc
  printf '%s\n' "$__ccs_cmd"
  {history}
  eval "$__ccs_cmd"
}}
"#,
        exe = config::shell_quote(&exe),
    )
}

/// Parse a human-friendly duration string into a chrono::Duration.
///
/// Supported suffixes: `d` (days), `w` (weeks), `m` (30-day months).
fn parse_duration(s: &str) -> Result<chrono::Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration string".to_string());
    }

    let (num_str, suffix) = s.split_at(s.len() - 1);
    let num: i64 = num_str
        .parse()
        .map_err(|_| format!("invalid number in duration: {num_str:?}"))?;

    match suffix {
        "d" => Ok(chrono::Duration::days(num)),
        "w" => Ok(chrono::Duration::weeks(num)),
        "m" => Ok(chrono::Duration::days(num * 30)),
        other => Err(format!(
            "unknown duration suffix: {other:?} (expected d, w, or m)"
        )),
    }
}

fn main() {
    let cli = Cli::parse();

    if let Some(Command::Init { shell, name }) = &cli.command {
        print!("{}", shell_init(*shell, name));
        return;
    }

    let mut config = Config::load(cli.config.as_deref()).unwrap_or_else(|e| {
        eprintln!("Invalid config: {e}");
        std::process::exit(1);
    });
    if let Some(name) = &cli.mode {
        if !config.select_mode(name) {
            let names: Vec<&str> = config.modes.iter().map(|m| m.name.as_str()).collect();
            eprintln!("Unknown mode {name:?} (available: {})", names.join(", "));
            std::process::exit(1);
        }
    }
    if cli.skip_permissions {
        config.select_mode_with_arg(config::SKIP_PERMISSIONS);
    }

    if !config.profiles.iter().any(|p| p.config_dir.join("projects").is_dir()) {
        for p in &config.profiles {
            eprintln!(
                "No Claude projects directory found at {}",
                p.config_dir.join("projects").display()
            );
        }
        std::process::exit(2);
    }

    let sessions = discover_profiles(&config.profiles);

    // Apply --since filter
    let since_duration = cli.since.map(|s| {
        parse_duration(&s).unwrap_or_else(|e| {
            eprintln!("Invalid --since value: {e}");
            std::process::exit(1);
        })
    });

    let sessions = apply_filters(sessions, since_duration, cli.last);

    // Determine color theme
    let theme = if cli.light {
        theme::Theme::light()
    } else if cli.dark {
        theme::Theme::dark()
    } else {
        theme::Theme::detect()
    };

    // Interactive TUI
    if let Err(e) = tui::run(sessions, theme, config, cli.cd_file, cli.exec_file) {
        eprintln!("TUI error: {e}");
        std::process::exit(1);
    }
}
