use std::path::PathBuf;

use cc_session::discovery::{apply_filters, discover_sessions, resolve_cwd};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[test]
fn discover_sessions_from_fixtures() {
    let sessions = discover_sessions(&fixture_dir());
    assert_eq!(sessions.len(), 3, "should find 3 session files");
}

#[test]
fn sessions_sorted_newest_first() {
    let sessions = discover_sessions(&fixture_dir());
    for i in 1..sessions.len() {
        assert!(
            sessions[i - 1].timestamp >= sessions[i].timestamp,
            "sessions should be sorted newest first"
        );
    }
}

#[test]
fn session_id_from_filename() {
    let sessions = discover_sessions(&fixture_dir());
    let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&"11111111-1111-1111-1111-111111111111"));
    assert!(ids.contains(&"22222222-2222-2222-2222-222222222222"));
    assert!(ids.contains(&"33333333-3333-3333-3333-333333333333"));
}

#[test]
fn first_user_message_extracted() {
    let sessions = discover_sessions(&fixture_dir());
    let s = sessions
        .iter()
        .find(|s| s.id == "11111111-1111-1111-1111-111111111111")
        .unwrap();
    assert_eq!(s.first_message, "Help me implement the list endpoints feature");
    assert_eq!(s.git_branch.as_deref(), Some("feat-endpoints"));
}

#[test]
fn array_content_extracted() {
    let sessions = discover_sessions(&fixture_dir());
    let s = sessions
        .iter()
        .find(|s| s.id == "22222222-2222-2222-2222-222222222222")
        .unwrap();
    assert_eq!(s.first_message, "Add OAuth2 support to the client");
}

#[test]
fn apply_last_filter() {
    let sessions = discover_sessions(&fixture_dir());
    let filtered = apply_filters(sessions, None, Some(2));
    assert_eq!(filtered.len(), 2);
}

#[test]
fn empty_directory_returns_empty() {
    let sessions = discover_sessions(&PathBuf::from("/nonexistent/path"));
    assert!(sessions.is_empty());
}

fn user_line(cwd: &str) -> String {
    format!(
        r#"{{"type":"user","cwd":"{cwd}","message":{{"role":"user","content":"hi"}}}}"#
    )
}

#[test]
fn resolve_cwd_keeps_existing_first_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().to_str().unwrap().to_string();
    let file = dir.path().join("s.jsonl");
    std::fs::write(&file, user_line(&live)).unwrap();
    assert_eq!(resolve_cwd(&file, live.clone()), live);
}

#[test]
fn resolve_cwd_uses_last_existing_cwd_after_move() {
    let dir = tempfile::tempdir().unwrap();
    let moved = dir.path().join("moved");
    std::fs::create_dir(&moved).unwrap();
    let moved = moved.to_str().unwrap().to_string();
    let gone = dir.path().join("gone").to_str().unwrap().to_string();
    let file = dir.path().join("s.jsonl");
    let body = [user_line(&gone), user_line(&gone), user_line(&moved)].join("\n");
    std::fs::write(&file, body).unwrap();
    assert_eq!(resolve_cwd(&file, gone), moved);
}

#[test]
fn resolve_cwd_decodes_project_folder_name() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("my.project");
    std::fs::create_dir(&target).unwrap();
    let target = target.to_str().unwrap().to_string();
    let encoded: String = target
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let proj = dir.path().join(encoded);
    std::fs::create_dir(&proj).unwrap();
    let gone = dir.path().join("gone").to_str().unwrap().to_string();
    let file = proj.join("s.jsonl");
    std::fs::write(&file, user_line(&gone)).unwrap();
    assert_eq!(resolve_cwd(&file, gone), target);
}

#[test]
fn session_title_prefers_rename_over_agent_and_ai_title() {
    use cc_session::discovery::session_title;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("s.jsonl");
    std::fs::write(
        &file,
        "{\"type\":\"ai-title\",\"aiTitle\":\"generated\"}\n",
    )
    .unwrap();
    assert_eq!(session_title(&file).as_deref(), Some("generated"));

    // Appended later: picked up by the incremental rescan.
    let mut body = std::fs::read_to_string(&file).unwrap();
    body.push_str("{\"type\":\"agent-name\",\"agentName\":\"agent\"}\n");
    std::fs::write(&file, &body).unwrap();
    assert_eq!(session_title(&file).as_deref(), Some("agent"));

    body.push_str("{\"type\":\"custom-title\",\"customTitle\":\"renamed\"}\n");
    body.push_str("{\"type\":\"ai-title\",\"aiTitle\":\"newer generated\"}\n");
    std::fs::write(&file, &body).unwrap();
    assert_eq!(session_title(&file).as_deref(), Some("renamed"));
}

#[test]
fn sessions_carry_their_transcript_path() {
    for s in discover_sessions(&fixture_dir()) {
        assert!(s.file_path.is_file(), "{}", s.file_path.display());
        assert!(s.file_path.ends_with(format!("{}.jsonl", s.id)));
    }
}
