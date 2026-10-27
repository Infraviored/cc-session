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
    /// Command run instead of the global one, e.g. a sandbox launcher such
    /// as `xclaude`.
    pub command: Option<String>,
    /// Whether to resume in the session's profile via `CLAUDE_CONFIG_DIR`.
    /// Off for launchers that pick the Claude account themselves.
    pub pass_profile: bool,
}

impl LaunchMode {
    fn new(name: &str, args: Vec<String>) -> Self {
        LaunchMode {
            name: name.to_string(),
            args,
            command: None,
            pass_profile: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Command that starts Claude Code, run through an interactive shell.
    pub command: String,
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
    command: Option<String>,
    pass_profile: Option<bool>,
}

pub const SKIP_PERMISSIONS: &str = "--dangerously-skip-permissions";

impl Default for Config {
    fn default() -> Self {
        Config {
            command: "claude".to_string(),
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
        LaunchMode::new("default", Vec::new()),
        LaunchMode::new("skip permissions", vec![SKIP_PERMISSIONS.to_string()]),
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
    match path.strip_prefix("~") {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => dirs::home_dir()
            .map(|h| h.join(rest.trim_start_matches('/')))
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
                    command: m.command.filter(|c| !c.trim().is_empty()),
                    pass_profile: m.pass_profile.unwrap_or(true),
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
    /// Modes with their own command (a sandbox launcher) don't count: a
    /// command-line flag means plain Claude.
    pub fn select_mode_with_arg(&mut self, arg: &str) {
        if let Some(i) = self
            .modes
            .iter()
            .position(|m| m.command.is_none() && m.args.iter().any(|a| a == arg))
        {
            self.mode = i;
        } else {
            self.modes.push(LaunchMode::new(
                &arg.trim_start_matches('-').replace('-', " "),
                vec![arg.to_string()],
            ));
            self.mode = self.modes.len() - 1;
        }
    }

    pub fn current_mode(&self) -> &LaunchMode {
        &self.modes[self.mode]
    }

    /// Command `mode` starts Claude with: its own, else the global one.
    pub fn command_for<'a>(&'a self, mode: &'a LaunchMode) -> &'a str {
        mode.command.as_deref().unwrap_or(&self.command)
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
    /// with `mode`'s command and extra arguments.
    pub fn resume_command(&self, session: &crate::session::Session, mode: &LaunchMode) -> String {
        let mut cmd = format!("cd {} && ", shell_quote(&session.cwd));
        let profile = match self.profile_override {
            _ if !mode.pass_profile => None,
            Some(i) => self.profiles.get(i),
            None => self.profile(&session.profile),
        };
        if let Some(profile) = profile {
            // Only name the config dir when it isn't Claude's own default:
            // CLAUDE_CONFIG_DIR=~/.claude makes Claude read ~/.claude/.claude.json
            // instead of ~/.claude.json, i.e. a different profile.
            let default_home = dirs::home_dir().map(|h| h.join(".claude"));
            if Some(&profile.config_dir) != default_home.as_ref() {
                cmd.push_str(&format!(
                    "CLAUDE_CONFIG_DIR={} ",
                    shell_quote(&profile.config_dir.to_string_lossy())
                ));
            }
        }
        cmd.push_str(self.command_for(mode));
        for arg in &mode.args {
            cmd.push(' ');
            cmd.push_str(&shell_quote(arg));
        }
        cmd.push_str(" -r ");
        cmd.push_str(&shell_quote(&session.id));
        cmd
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
        let c = parse(
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
        assert_eq!(
            c.resume_command(&session, &c.modes[1]),
            "cd '/home/x/my proj' && CLAUDE_CONFIG_DIR='/opt/work claude' claude \
             --dangerously-skip-permissions -r abc"
        );
    }

    fn session_in(profile: &str) -> crate::session::Session {
        crate::session::Session {
            id: "abc".into(),
            project_path: "/p".into(),
            project_name: "p".into(),
            git_branch: None,
            timestamp: chrono::Utc::now(),
            first_message: String::new(),
            last_assistant_line: String::new(),
            cwd: "/home/x/proj".into(),
            project_exists: true,
            name: None,
            profile: profile.into(),
            file_path: PathBuf::new(),
        }
    }

    const XCLAUDE: &str = r#"
        [[profiles]]
        name = "private"
        config_dir = "/opt/private"

        [[profiles]]
        name = "work"
        config_dir = "/opt/work"

        [[modes]]
        name = "skip permissions"
        args = ["--dangerously-skip-permissions"]

        [[modes]]
        name = "xclaude"
        command = "xclaude"
        pass_profile = false
        "#;

    #[test]
    fn mode_command_replaces_global_and_skips_profile() {
        let mut c = parse(XCLAUDE).unwrap();
        // An explicit profile pick doesn't reach a launcher that picks its own.
        c.profile_override = Some(1);
        assert_eq!(
            c.resume_command(&session_in("work"), &c.modes[1]),
            "cd /home/x/proj && xclaude -r abc"
        );
        assert_eq!(c.command_for(&c.modes[1]), "xclaude");
        assert_eq!(c.command_for(&c.modes[0]), "claude");
    }

    #[test]
    fn modes_without_new_keys_keep_passing_profile() {
        let c = parse(XCLAUDE).unwrap();
        assert!(c.modes[0].pass_profile);
        assert_eq!(c.modes[0].command, None);
        assert_eq!(
            c.resume_command(&session_in("work"), &c.modes[0]),
            "cd /home/x/proj && CLAUDE_CONFIG_DIR=/opt/work claude \
             --dangerously-skip-permissions -r abc"
        );
    }

    #[test]
    fn skip_permissions_flag_never_picks_a_launcher_mode() {
        let mut c = parse(
            r#"
            [[modes]]
            name = "xclaude skip"
            command = "xclaude"
            args = ["--dangerously-skip-permissions"]
            "#,
        )
        .unwrap();
        c.select_mode_with_arg(SKIP_PERMISSIONS);
        assert_eq!(c.current_mode().command, None);
        assert_eq!(c.current_mode().args, vec![SKIP_PERMISSIONS]);
    }

    #[test]
    fn blank_mode_command_falls_back_to_global() {
        let c = parse("[[modes]]\nname = \"m\"\ncommand = \" \"").unwrap();
        assert_eq!(c.command_for(c.current_mode()), "claude");
    }

    #[test]
    fn shell_quote_quotes_when_needed() {
        assert_eq!(shell_quote("/home/a/b"), "/home/a/b");
        assert_eq!(shell_quote("/home/a b"), "'/home/a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
