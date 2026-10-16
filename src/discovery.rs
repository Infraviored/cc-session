// Session discovery: scanning ~/.claude/projects/ for session JSONL files

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde::Deserialize;

use crate::config::{distinct_projects_dirs, Profile};
use crate::session::{
    clean_message, clean_message_multiline, is_meta_message, ConversationMessage, MessageRole,
    Session, SessionFileEntry,
};

/// Return the Claude home directory.
///
/// Checks the `CLAUDE_HOME` env var first, then Claude Code's own
/// `CLAUDE_CONFIG_DIR`, then falls back to `~/.claude`.
pub fn get_claude_home() -> PathBuf {
    for var in ["CLAUDE_HOME", "CLAUDE_CONFIG_DIR"] {
        if let Some(home) = std::env::var_os(var).filter(|v| !v.is_empty()) {
            return PathBuf::from(home);
        }
    }
    dirs::home_dir()
        .expect("could not determine home directory")
        .join(".claude")
}

/// All session transcripts under `claude_home/projects/<project>/`.
pub fn session_files(claude_home: &Path) -> Vec<PathBuf> {
    let mut jsonl_files: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = fs::read_dir(claude_home.join("projects")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Ok(files) = fs::read_dir(&path) {
                    for file in files.flatten() {
                        let fpath = file.path();
                        if fpath.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                            jsonl_files.push(fpath);
                        }
                    }
                }
            }
        }
    }
    jsonl_files
}

/// Discover all sessions under `claude_home/projects/`.
#[allow(dead_code)]
pub fn discover_sessions(claude_home: &Path) -> Vec<Session> {
    discover_profiles(&[Profile {
        name: String::new(),
        config_dir: claude_home.to_path_buf(),
    }])
}

/// Discover the sessions of every profile, newest first, each tagged with
/// the name of the profile it was found in.
pub fn discover_profiles(profiles: &[Profile]) -> Vec<Session> {
    let jsonl_files: Vec<(PathBuf, &str)> = distinct_projects_dirs(profiles)
        .into_iter()
        .flat_map(|p| {
            session_files(&p.config_dir)
                .into_iter()
                .map(move |f| (f, p.name.as_str()))
        })
        .collect();

    // Parse files in parallel
    let mut sessions: Vec<Session> = jsonl_files
        .par_iter()
        .filter_map(|(path, profile)| {
            let mut session = parse_session_file(path)?;
            session.profile = profile.to_string();
            Some(session)
        })
        .collect();

    // Sort by timestamp descending (newest first)
    sessions.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    sessions
}

/// Session titles found so far in one transcript, and how far it was read.
#[derive(Default, Clone)]
struct TitleScan {
    offset: u64,
    custom: Option<String>,
    agent: Option<String>,
    ai: Option<String>,
}

#[derive(Deserialize)]
struct TitleEntry {
    #[serde(rename = "customTitle")]
    custom_title: Option<String>,
    #[serde(rename = "agentName")]
    agent_name: Option<String>,
    #[serde(rename = "aiTitle")]
    ai_title: Option<String>,
}

/// The session's display name: the latest `/rename` title, else the latest
/// Remote Control agent name, else Claude's generated title.
///
/// These entries can sit anywhere in the transcript, so the whole file has
/// to be read -- but only once: the scan position is cached per file and the
/// periodic refresh only reads what was appended since.
pub fn session_title(path: &Path) -> Option<String> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, TitleScan>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mut scan = cache
        .lock()
        .ok()
        .and_then(|c| c.get(path).cloned())
        .unwrap_or_default();

    if let Ok(mut file) = fs::File::open(path) {
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len < scan.offset {
            // Rewritten from scratch; start over.
            scan = TitleScan::default();
        }
        if len > scan.offset && file.seek(SeekFrom::Start(scan.offset)).is_ok() {
            let mut reader = BufReader::new(file);
            let mut line = Vec::new();
            while let Ok(n) = reader.read_until(b'\n', &mut line) {
                // Stop before a half-written last line; it's read next time.
                if n == 0 || line.last() != Some(&b'\n') {
                    break;
                }
                scan.offset += n as u64;
                let text = String::from_utf8_lossy(&line);
                if text.contains("\"customTitle\"")
                    || text.contains("\"agentName\"")
                    || text.contains("\"aiTitle\"")
                {
                    if let Ok(e) = serde_json::from_str::<TitleEntry>(&text) {
                        let keep = |t: Option<String>| t.filter(|t| !t.trim().is_empty());
                        scan.custom = keep(e.custom_title).or(scan.custom);
                        scan.agent = keep(e.agent_name).or(scan.agent);
                        scan.ai = keep(e.ai_title).or(scan.ai);
                    }
                }
                line.clear();
            }
        }
    }

    let name = scan.custom.clone().or(scan.agent.clone()).or(scan.ai.clone());
    if let Ok(mut c) = cache.lock() {
        // Two scans of one file can race; keep the one that got further.
        if c.get(path).map_or(true, |old| old.offset <= scan.offset) {
            c.insert(path.to_path_buf(), scan);
        }
    }
    name
}

/// Parse a single JSONL session file and extract the most recent user message.
fn parse_session_file(path: &Path) -> Option<Session> {
    let session_id = path.file_stem()?.to_str()?.to_string();

    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    // Branch comes from the first user entry; cwd too, unless that directory
    // has since been moved -- see resolve_cwd.
    let mut cwd = String::new();
    let mut git_branch: Option<String> = None;
    let mut found_metadata = false;

    for line in reader.lines().take(50) {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let entry: SessionFileEntry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        if entry.entry_type != "user" {
            continue;
        }

        cwd = entry.cwd.clone().unwrap_or_default();
        git_branch = entry.git_branch.clone();
        found_metadata = true;
        break;
    }

    if !found_metadata {
        return None;
    }
    let cwd = resolve_cwd(path, cwd);

    // The messages shown in the list are the most recent ones, not the
    // first -- read backwards from the end of the file instead of scanning
    // it whole, since a session's JSONL can run into the hundreds of MB.
    let (last_user, last_assistant) = read_tail_previews(path);
    let first_message = last_user.unwrap_or_default();
    let last_assistant_line = last_assistant.unwrap_or_default();
    let timestamp: DateTime<Utc> = Utc::now();

    let project_name = Path::new(&cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let project_exists = Path::new(&cwd).exists();

    // Sort/display by last write to the file, not the first message's
    // timestamp -- otherwise a session that started hours ago but is still
    // being actively written never bubbles to the top of the list.
    let timestamp = fs::metadata(path)
        .and_then(|m| m.modified())
        .map(DateTime::<Utc>::from)
        .unwrap_or(timestamp);

    Some(Session {
        id: session_id,
        project_path: cwd.clone(),
        project_name,
        git_branch,
        timestamp,
        first_message,
        last_assistant_line,
        cwd,
        project_exists,
        name: session_title(path),
        profile: String::new(),
        file_path: path.to_path_buf(),
    })
}

/// Pick a working directory that still exists for a session.
///
/// A session's first `cwd` is usually right, but when its folder is moved and
/// the session is resumed from the new place, the early entries keep the old
/// path and only later ones carry the new one. Falls back to the last `cwd` in
/// the file that exists, then to the directory the transcript's project folder
/// name decodes to. Returns `first` unchanged if nothing better turns up.
///
/// The fallbacks read the whole transcript and walk the filesystem, so their
/// result is cached per transcript for the rest of the run -- the periodic
/// refresh would otherwise repeat them every few seconds.
pub fn resolve_cwd(path: &Path, first: String) -> String {
    if first.is_empty() || Path::new(&first).is_dir() {
        return first;
    }
    static CACHE: OnceLock<Mutex<HashMap<(PathBuf, String), String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = (path.to_path_buf(), first);
    if let Some(found) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return found;
    }
    let found = last_existing_cwd(path, &key.1)
        .or_else(|| {
            path.parent()
                .and_then(|d| d.file_name())
                .and_then(|n| n.to_str())
                .and_then(decode_project_dir)
        })
        .unwrap_or_else(|| key.1.clone());
    if let Ok(mut c) = cache.lock() {
        c.insert(key, found.clone());
    }
    found
}

/// Scan every line for a `"cwd":"..."` value and return the last one (other
/// than `skip`) that is an existing directory. Plain substring search, not a
/// JSON parse, since this reads the whole file.
fn last_existing_cwd(path: &Path, skip: &str) -> Option<String> {
    const KEY: &str = "\"cwd\":\"";
    let reader = BufReader::new(fs::File::open(path).ok()?);
    let mut prev = skip.to_string();
    let mut best = None;
    for line in reader.lines().map_while(Result::ok) {
        let Some(start) = line.find(KEY).map(|i| i + KEY.len()) else {
            continue;
        };
        let Some(len) = line[start..].find('"') else {
            continue;
        };
        let cwd = &line[start..start + len];
        if cwd == prev {
            continue;
        }
        prev = cwd.to_string();
        if Path::new(cwd).is_dir() {
            best = Some(cwd.to_string());
        }
    }
    best
}

/// Claude Code's project folder name for a directory: every character that
/// isn't ASCII alphanumeric becomes `-`.
fn encode_project_dir(dir: &str) -> String {
    dir.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Find the existing directory whose encoded name is `name`. The encoding is
/// lossy, so walk the filesystem from `/`, descending only into children whose
/// encoded path is still a prefix of `name`.
fn decode_project_dir(name: &str) -> Option<String> {
    fn walk(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
        let enc = encode_project_dir(dir.to_str()?);
        if enc == name {
            return Some(dir.to_path_buf());
        }
        if depth > 32 {
            return None;
        }
        for entry in fs::read_dir(dir).ok()?.flatten() {
            let child = entry.path();
            let Some(child_str) = child.to_str() else { continue };
            let child_enc = encode_project_dir(child_str);
            let fits = child_enc == name
                || (name.starts_with(&child_enc) && name.as_bytes()[child_enc.len()] == b'-');
            if fits && child.is_dir() {
                if let Some(found) = walk(&child, name, depth + 1) {
                    return Some(found);
                }
            }
        }
        None
    }
    walk(Path::new("/"), name, 0).map(|p| p.to_string_lossy().into_owned())
}

/// Find the most recent non-meta user message and the most recent assistant
/// text reply's last line, reading backwards from the end of the file
/// instead of scanning it whole (doubling the read window until both turn
/// up, capped, so a pathological file can't stall discovery).
fn read_tail_previews(path: &Path) -> (Option<String>, Option<String>) {
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return (None, None),
    };
    let file_len = match file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return (None, None),
    };

    let mut window: u64 = 64 * 1024;
    let max_window: u64 = 8 * 1024 * 1024;
    let mut last_user: Option<String> = None;
    let mut last_assistant: Option<String> = None;

    loop {
        let start = file_len.saturating_sub(window);
        if file.seek(SeekFrom::Start(start)).is_err() {
            return (last_user, last_assistant);
        }
        let mut buf = Vec::with_capacity((file_len - start) as usize);
        if file
            .by_ref()
            .take(file_len - start)
            .read_to_end(&mut buf)
            .is_err()
        {
            return (last_user, last_assistant);
        }
        let text = String::from_utf8_lossy(&buf);

        // The window boundary likely lands mid-line; drop that partial first
        // line unless we're already reading from the true start of the file.
        let lines: Vec<&str> = if start == 0 {
            text.lines().collect()
        } else {
            text.lines().skip(1).collect()
        };

        for line in lines.iter().rev() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let entry: SessionFileEntry = match serde_json::from_str(line) {
                Ok(e) => e,
                Err(_) => continue,
            };
            let raw_text = entry.message.map(|m| m.content.text()).unwrap_or_default();
            if is_meta_message(&raw_text) {
                continue;
            }

            match entry.entry_type.as_str() {
                "user" if last_user.is_none() => {
                    let msg: String = clean_message(&raw_text).chars().take(200).collect();
                    if !msg.is_empty() {
                        last_user = Some(msg);
                    }
                }
                "assistant" if last_assistant.is_none() => {
                    let line: String = clean_message_multiline(&raw_text)
                        .lines()
                        .rev()
                        .map(|l| l.trim())
                        .find(|l| !l.is_empty())
                        .unwrap_or("")
                        .chars()
                        .take(200)
                        .collect();
                    if !line.is_empty() {
                        last_assistant = Some(line);
                    }
                }
                _ => {}
            }

            if last_user.is_some() && last_assistant.is_some() {
                return (last_user, last_assistant);
            }
        }

        if start == 0 || window >= max_window {
            return (last_user, last_assistant);
        }
        window = (window * 2).min(max_window);
    }
}

/// Load all conversation messages (user + assistant) from a session JSONL file.
///
/// Returns messages in chronological order. Skips file-history-snapshot entries,
/// system entries, tool-use blocks, and meta-messages. Consecutive messages from
/// the same role are merged into a single message with paragraphs separated by
/// blank lines.
pub fn load_conversation(session: &Session) -> Vec<ConversationMessage> {
    let file = match fs::File::open(&session.file_path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let reader = BufReader::new(file);
    let mut messages: Vec<ConversationMessage> = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let entry: SessionFileEntry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let role = match entry.entry_type.as_str() {
            "user" => MessageRole::User,
            "assistant" => MessageRole::Assistant,
            _ => continue,
        };

        let raw_text = match &entry.message {
            Some(m) => m.content.text(),
            None => continue,
        };

        // Skip meta messages for user entries
        if role == MessageRole::User && is_meta_message(&raw_text) {
            continue;
        }

        let text = clean_message_multiline(&raw_text);
        if text.is_empty() {
            continue;
        }

        let timestamp: DateTime<Utc> = entry
            .timestamp
            .and_then(|t| t.parse().ok())
            .unwrap_or_else(Utc::now);

        // Merge consecutive messages from the same role, skipping duplicates
        if let Some(last) = messages.last_mut() {
            if last.role == role {
                // Skip if the text is a duplicate of the last segment
                // (happens with skill expansions that get repeated in JSONL)
                if !last.text.ends_with(&text) {
                    last.text.push_str("\n\n");
                    last.text.push_str(&text);
                }
                // Keep the latest timestamp
                last.timestamp = timestamp;
                continue;
            }
        }

        messages.push(ConversationMessage {
            role,
            text,
            timestamp,
        });
    }

    messages
}

/// Apply optional time-based and count-based filters to a session list.
///
/// `since` keeps only sessions newer than `Utc::now() - since`.
/// `last` keeps only the first N sessions (already sorted newest-first).
pub fn apply_filters(
    mut sessions: Vec<Session>,
    since: Option<chrono::Duration>,
    last: Option<usize>,
) -> Vec<Session> {
    if let Some(duration) = since {
        let cutoff = Utc::now() - duration;
        sessions.retain(|s| s.timestamp >= cutoff);
    }
    if let Some(n) = last {
        sessions.truncate(n);
    }
    sessions
}
