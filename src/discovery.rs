// Session discovery: scanning ~/.claude/projects/ for session JSONL files

use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rayon::prelude::*;

use crate::session::{
    clean_message, clean_message_multiline, is_meta_message, ConversationMessage, MessageRole,
    Session, SessionFileEntry,
};

/// Return the Claude home directory.
///
/// Checks the `CLAUDE_HOME` env var first, then falls back to `~/.claude`.
pub fn get_claude_home() -> PathBuf {
    if let Ok(home) = std::env::var("CLAUDE_HOME") {
        return PathBuf::from(home);
    }
    dirs::home_dir()
        .expect("could not determine home directory")
        .join(".claude")
}

/// Discover all sessions under `claude_home/projects/`.
pub fn discover_sessions(claude_home: &Path) -> Vec<Session> {
    let projects_dir = claude_home.join("projects");
    if !projects_dir.is_dir() {
        return Vec::new();
    }

    // Collect all .jsonl file paths
    let mut jsonl_files: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = fs::read_dir(&projects_dir) {
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

    // Parse files in parallel
    let mut sessions: Vec<Session> = jsonl_files
        .par_iter()
        .filter_map(|path| parse_session_file(path))
        .collect();

    // Sort by timestamp descending (newest first)
    sessions.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    sessions
}

/// Parse a single JSONL session file and extract the most recent user message.
fn parse_session_file(path: &Path) -> Option<Session> {
    let session_id = path.file_stem()?.to_str()?.to_string();

    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    // cwd/branch never change within a session, so the first user entry is
    // enough for metadata -- stop as soon as it's found.
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
    })
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
pub fn load_conversation(claude_home: &Path, session: &Session) -> Vec<ConversationMessage> {
    let encoded_dir = session.project_path.replace('/', "-");
    let file_path = claude_home
        .join("projects")
        .join(&encoded_dir)
        .join(format!("{}.jsonl", session.id));

    let file = match fs::File::open(&file_path) {
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
