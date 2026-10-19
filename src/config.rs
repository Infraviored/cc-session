// User configuration: Claude profiles to scan and launch modes to resume with.
//
// Read from `$XDG_CONFIG_HOME/cc-session/config.toml` (usually
// `~/.config/cc-session/config.toml`). Everything is optional; without a file
// cc-session scans the single default Claude home and offers two launch modes,
// "default" and "skip permissions".

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// A Claude Code config directory whose sessions are listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub config_dir: PathBuf,
}

/// A named set of extra arguments passed to `claude` when resuming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchMode {
    pub name: String,
    pub args: Vec<String>,
}

/// Which shell the resume command is written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ShellSyntax {
    Posix,
    #[value(name = "powershell")]
    PowerShell,
}

impl ShellSyntax {
    /// PowerShell on Windows, a POSIX shell everywhere else.
    pub fn native() -> Self {
        if cfg!(windows) {
            ShellSyntax::PowerShell
        } else {
            ShellSyntax::Posix
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Command that starts Claude Code, run through an interactive shell.
    pub command: String,
    /// Shell the resume command is written for.
    pub syntax: ShellSyntax,
    pub profiles: Vec<Profile>,
    pub modes: Vec<LaunchMode>,
    /// Index into `modes` used by Enter.
    pub mode: usize,
    /// Profile to resume in, overriding the one a session was found in.
    /// Needed when profiles share one `projects` directory (symlinked), so
    /// the transcript alone can't tell which profile it belongs to.
    pub profile_override: Option<usize>,
    /// Whether the profiles have more than one distinct `projects` dir.
    /// Computed once: it is asked for every row of every frame.
    multi_profile: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    command: Option<String>,
    mode: Option<String>,
    #[serde(default)]
    profiles: Vec<RawProfile>,
    #[serde(default)]
    modes: Vec<RawMode>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    name: String,
    config_dir: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMode {
    name: String,
    #[serde(default)]
    args: Vec<String>,
}

pub const SKIP_PERMISSIONS: &str = "--dangerously-skip-permissions";

impl Default for Config {
    fn default() -> Self {
        Config {
            command: "claude".to_string(),
            syntax: ShellSyntax::native(),
            profiles: vec![Profile {
                name: "default".to_string(),
                config_dir: crate::discovery::get_claude_home(),
            }],
            modes: default_modes(),
            mode: 0,
            profile_override: None,
            multi_profile: false,
        }
    }
}

fn default_modes() -> Vec<LaunchMode> {
    vec![
        LaunchMode {
            name: "default".to_string(),
            args: Vec::new(),
        },
        LaunchMode {
            name: "skip permissions".to_string(),
            args: vec![SKIP_PERMISSIONS.to_string()],
        },
    ]
}

/// `~/.config/cc-session`, or `$XDG_CONFIG_HOME/cc-session`.
pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .map(|d| d.join("cc-session"))
}

/// Default location of the config file.
pub fn default_config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

/// File remembering the launch mode and profile last picked in the menu:
/// the mode name on the first line, the profile name (or nothing for
/// "auto") on the second.
fn state_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("state"))
}

/// Expand a leading `~` to the home directory.
fn expand_home(path: &str) -> PathBuf {
    let is_sep = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    match path.strip_prefix("~") {
        Some(rest) if rest.is_empty() || rest.starts_with(is_sep) => dirs::home_dir()
            .map(|h| h.join(rest.trim_start_matches(is_sep)))
            .unwrap_or_else(|| PathBuf::from(path)),
        _ => PathBuf::from(path),
    }
}

impl Config {
    /// Load the config file at `path`, or the default location if `None`.
    /// A missing default file is not an error; a missing explicit one is.
    pub fn load(path: Option<&Path>) -> Result<Config, String> {
        let (path, explicit) = match path {
            Some(p) => (Some(p.to_path_buf()), true),
            None => (default_config_path(), false),
        };
        let raw = match path.as_deref().map(fs::read_to_string) {
            Some(Ok(text)) => toml::from_str::<RawConfig>(&text).map_err(|e| {
                format!("{}: {e}", path.as_deref().unwrap_or(Path::new("")).display())
            })?,
            Some(Err(e)) if explicit || e.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!(
                    "{}: {e}",
                    path.as_deref().unwrap_or(Path::new("")).display()
                ))
            }
            _ => RawConfig::default(),
        };
        let mut config = Config::from_raw(raw)?;
        if let Some(state) = state_path().and_then(|p| fs::read_to_string(p).ok()) {
            let mut lines = state.lines();
            if let Some(mode) = lines.next() {
                config.select_mode(mode.trim());
            }
            if let Some(profile) = lines.next().map(str::trim).filter(|p| !p.is_empty()) {
                config.profile_override = config.profiles.iter().position(|p| p.name == profile);
            }
        }
        Ok(config)
    }

    fn from_raw(raw: RawConfig) -> Result<Config, String> {
        let mut config = Config::default();
        if let Some(command) = raw.command.filter(|c| !c.trim().is_empty()) {
            config.command = command;
        }
        if !raw.profiles.is_empty() {
            config.profiles = raw
                .profiles
                .into_iter()
                .map(|p| Profile {
                    name: p.name,
                    config_dir: expand_home(&p.config_dir),
                })
                .collect();
        }
        if !raw.modes.is_empty() {
            config.modes = raw
                .modes
                .into_iter()
                .map(|m| LaunchMode {
                    name: m.name,
                    args: m.args,
                })
                .collect();
        }
        config.multi_profile = distinct_projects_dirs(&config.profiles).len() > 1;
        if let Some(name) = raw.mode {
            if !config.select_mode(&name) {
                return Err(format!("mode {name:?} is not defined in [[modes]]"));
            }
        }
        Ok(config)
    }

    /// Make the mode called `name` current. Returns false if there is none.
    pub fn select_mode(&mut self, name: &str) -> bool {
        match self.modes.iter().position(|m| m.name == name) {
            Some(i) => {
                self.mode = i;
                true
            }
            None => false,
        }
    }

    /// Make the first mode passing `arg` current, adding one if none does.
    pub fn select_mode_with_arg(&mut self, arg: &str) {
        if let Some(i) = self.modes.iter().position(|m| m.args.iter().any(|a| a == arg)) {
            self.mode = i;
        } else {
            self.modes.push(LaunchMode {
                name: arg.trim_start_matches('-').replace('-', " "),
                args: vec![arg.to_string()],
            });
            self.mode = self.modes.len() - 1;
        }
    }

    pub fn current_mode(&self) -> &LaunchMode {
        &self.modes[self.mode]
    }

    /// Remember the current mode and profile for the next start. Best effort.
    pub fn save_state(&self) {
        if let Some(path) = state_path() {
            if let Some(dir) = path.parent() {
                let _ = fs::create_dir_all(dir);
            }
            let profile = self
                .profile_override
                .map(|i| self.profiles[i].name.as_str())
                .unwrap_or("");
            let _ = fs::write(path, format!("{}\n{}\n", self.current_mode().name, profile));
        }
    }

    /// Whether sessions come from more than one place (then the list shows
    /// which profile each was found in).
    pub fn multi_profile(&self) -> bool {
        self.multi_profile
    }

    /// Name shown for the profile choice: the override, or "auto".
    pub fn profile_label(&self) -> &str {
        self.profile_override
            .map(|i| self.profiles[i].name.as_str())
            .unwrap_or("auto")
    }

    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// Shell command that resumes `session` in its own directory and profile
    /// with `mode`'s extra arguments.
    pub fn resume_command(&self, session: &crate::session::Session, mode: &LaunchMode) -> String {
        let quote = match self.syntax {
            ShellSyntax::Posix => shell_quote,
            ShellSyntax::PowerShell => ps_quote,
        };
        let mut claude = self.command.clone();
        for arg in &mode.args {
            claude.push(' ');
            claude.push_str(&quote(arg));
        }
        claude.push_str(" -r ");
        claude.push_str(&quote(&session.id));
        let config_dir = {
            let profile = match self.profile_override {
                Some(i) => self.profiles.get(i),
                None => self.profile(&session.profile),
            };
            // Only name the config dir when it isn't Claude's own default:
            // CLAUDE_CONFIG_DIR=~/.claude makes Claude read ~/.claude/.claude.json
            // instead of ~/.claude.json, i.e. a different profile.
            let default_home = dirs::home_dir().map(|h| h.join(".claude"));
            profile
                .filter(|p| Some(&p.config_dir) != default_home.as_ref())
                .map(|p| p.config_dir.to_string_lossy().into_owned())
        };
        let cwd = quote(&session.cwd);
        match (self.syntax, config_dir) {
            (ShellSyntax::Posix, None) => format!("cd {cwd} && {claude}"),
            (ShellSyntax::Posix, Some(dir)) => {
                let dir = shell_quote(&dir);
                format!("cd {cwd} && CLAUDE_CONFIG_DIR={dir} {claude}")
            }
            // `;` rather than `&&`, which Windows PowerShell 5.1 lacks; the
            // `-ErrorAction Stop` keeps claude from starting in the wrong
            // directory. The variable is removed again so it doesn't stick
            // to the calling shell.
            (ShellSyntax::PowerShell, None) => {
                format!("Set-Location -LiteralPath {cwd} -ErrorAction Stop; {claude}")
            }
            // The value is always quoted: on the right of `=`, a bare path
            // would be run as a command instead of assigned.
            (ShellSyntax::PowerShell, Some(dir)) => format!(
                "Set-Location -LiteralPath {cwd} -ErrorAction Stop; \
                 $env:CLAUDE_CONFIG_DIR = '{}'; \
                 try {{ {claude} }} finally {{ \
                 Remove-Item Env:CLAUDE_CONFIG_DIR -ErrorAction SilentlyContinue }}",
                dir.replace('\'', "''")
            ),
        }
    }
}

/// Profiles with their own `projects` directory: a profile whose directory
/// resolves (through symlinks) to one already listed is dropped, so shared
/// transcripts are found once, under the first profile.
pub fn distinct_projects_dirs(profiles: &[Profile]) -> Vec<&Profile> {
    let mut seen = Vec::new();
    profiles
        .iter()
        .filter(|p| {
            let dir = p.config_dir.join("projects");
            let dir = fs::canonicalize(&dir).unwrap_or(dir);
            if seen.contains(&dir) {
                false
            } else {
                seen.push(dir);
                true
            }
        })
        .collect()
}

/// Single-quote `s` for POSIX shells unless it is plainly safe as is.
pub fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Single-quote `s` for PowerShell unless it is plainly safe as is.
pub fn ps_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./\\:".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Config, String> {
        Config::from_raw(toml::from_str(text).map_err(|e| e.to_string())?)
    }

    #[test]
    fn empty_config_uses_defaults() {
        let c = parse("").unwrap();
        assert_eq!(c.command, "claude");
        assert_eq!(c.profiles.len(), 1);
        assert_eq!(c.modes.len(), 2);
        assert_eq!(c.current_mode().name, "default");
    }

    #[test]
    fn profiles_modes_and_mode_parse() {
        let c = parse(
            r#"
            command = "command claude"
            mode = "yolo"
            [[profiles]]
            name = "private"
            config_dir = "~/.claude"
            [[profiles]]
            name = "work"
            config_dir = "/opt/work"
            [[modes]]
            name = "normal"
            [[modes]]
            name = "yolo"
            args = ["--dangerously-skip-permissions"]
            "#,
        )
        .unwrap();
        assert_eq!(c.command, "command claude");
        assert!(c.multi_profile());
        assert_eq!(c.profiles[1].config_dir, PathBuf::from("/opt/work"));
        assert!(!c.profiles[0].config_dir.starts_with("~"));
        assert_eq!(c.current_mode().args, vec![SKIP_PERMISSIONS]);
    }

    #[test]
    fn unknown_mode_is_an_error() {
        assert!(parse("mode = \"nope\"").is_err());
    }

    #[test]
    fn select_mode_with_arg_adds_missing_mode() {
        let mut c = parse("[[modes]]\nname = \"only\"").unwrap();
        c.select_mode_with_arg(SKIP_PERMISSIONS);
        assert_eq!(c.modes.len(), 2);
        assert_eq!(c.current_mode().args, vec![SKIP_PERMISSIONS]);
    }

    #[test]
    fn resume_command_sets_profile_dir_and_mode_args() {
        let mut c = parse(
            r#"
            [[profiles]]
            name = "work"
            config_dir = "/opt/work claude"
            "#,
        )
        .unwrap();
        let session = crate::session::Session {
            id: "abc".into(),
            project_path: "/p".into(),
            project_name: "p".into(),
            git_branch: None,
            timestamp: chrono::Utc::now(),
            first_message: String::new(),
            last_assistant_line: String::new(),
            cwd: "/home/x/my proj".into(),
            project_exists: true,
            name: None,
            profile: "work".into(),
            file_path: PathBuf::new(),
        };
        c.syntax = ShellSyntax::Posix;
        assert_eq!(
            c.resume_command(&session, &c.modes[1]),
            "cd '/home/x/my proj' && CLAUDE_CONFIG_DIR='/opt/work claude' claude \
             --dangerously-skip-permissions -r abc"
        );
        c.syntax = ShellSyntax::PowerShell;
        assert_eq!(
            c.resume_command(&session, &c.modes[1]),
            "Set-Location -LiteralPath '/home/x/my proj' -ErrorAction Stop; \
             $env:CLAUDE_CONFIG_DIR = '/opt/work claude'; \
             try { claude --dangerously-skip-permissions -r abc } \
             finally { Remove-Item Env:CLAUDE_CONFIG_DIR -ErrorAction SilentlyContinue }"
        );
        // A path that needs no quoting as an argument still gets quoted as
        // the value of the assignment.
        c.profiles[0].config_dir = PathBuf::from(r"C:\Users\x\.claude-business");
        assert!(c
            .resume_command(&session, &c.modes[0])
            .contains(r"$env:CLAUDE_CONFIG_DIR = 'C:\Users\x\.claude-business';"));
    }

    #[test]
    fn shell_quote_quotes_when_needed() {
        assert_eq!(shell_quote("/home/a/b"), "/home/a/b");
        assert_eq!(shell_quote("/home/a b"), "'/home/a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn ps_quote_quotes_when_needed() {
        assert_eq!(ps_quote(r"D:\repos\cc-session"), r"D:\repos\cc-session");
        assert_eq!(ps_quote(r"C:\My Dir"), r"'C:\My Dir'");
        assert_eq!(ps_quote("it's"), "'it''s'");
        assert_eq!(ps_quote("$x"), "'$x'");
    }
}
