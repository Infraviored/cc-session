use std::sync::atomic::Ordering;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::session::Session;

use super::{Action, App, ContentSearchState, DisplayItem, MenuRow, Mode};

/// Handle a key event and return the resulting action.
pub fn handle_input(app: &mut App, key: KeyEvent) -> Action {
    // Ctrl-C always quits
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Action::Quit;
    }

    if app.mode_menu.is_some() {
        return handle_mode_menu(app, key);
    }

    // Alt-M opens the launch-mode menu, from the list or the conversation.
    if key.code == KeyCode::Char('m') && key.modifiers.contains(KeyModifiers::ALT) {
        app.mode_menu = Some(app.config.mode);
        app.menu_launch = None;
        return Action::Continue;
    }

    match app.mode {
        Mode::Browsing => handle_browse(app, key),
        Mode::Conversation => handle_conversation(app, key),
        Mode::ConversationSearch => handle_conversation_search(app, key),
    }
}

fn handle_browse(app: &mut App, key: KeyEvent) -> Action {
    // Alt-G toggles grouped/flat view mode
    if key.code == KeyCode::Char('g') && key.modifiers.contains(KeyModifiers::ALT) {
        app.grouped_mode = !app.grouped_mode;
        app.expanded_projects.clear();
        app.rebuild_display_items();
        app.selected = 0;
        app.scroll_offset = 0;
        return Action::Continue;
    }

    // Alt-C resumes the selected session directly from the list, without
    // going through the conversation detail view first. Plain 'c' is
    // reserved for the live filter (typing searches), so this needs a
    // modifier -- same reasoning as Alt-G above.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::ALT) {
        if let Some(DisplayItem::Session(entry)) = app.display_items.get(app.selected) {
            let session = app.display_session(entry);
            let cmd = app.resume_command(session);
            let cwd = session.cwd.clone();
            let id = session.id.clone();
            return Action::ExecCommand { cmd, cwd, id };
        }
        return Action::Continue;
    }

    match key.code {
        KeyCode::Esc => {
            if !app.filter_query.is_empty() || app.filter_active {
                // First Escape: clear filter and deactivate
                app.cancel_content_search();
                app.filter_query.clear();
                app.filter_active = false;
                // In grouped mode, clear expanded projects so all groups collapse
                if app.grouped_mode {
                    app.expanded_projects.clear();
                }
                app.apply_filter();
                Action::Continue
            } else {
                // Second Escape (filter already empty): quit
                Action::Quit
            }
        }
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
            if let Some(DisplayItem::Session(entry)) = app.display_items.get(app.selected) {
                let session = app.display_session(entry).clone();
                open_launch_menu(app, session);
            }
            Action::Continue
        }
        KeyCode::Enter => {
            if app.selected < app.display_items.len() {
                match &app.display_items[app.selected] {
                    DisplayItem::Header(group) => {
                        // Toggle expand/collapse for this project group
                        let name = group.project_name.clone();
                        if app.expanded_projects.contains(&name) {
                            app.expanded_projects.remove(&name);
                        } else {
                            app.expanded_projects.insert(name);
                        }
                        app.rebuild_display_items();
                        Action::Continue
                    }
                    DisplayItem::Session(_) => {
                        Action::EnterConversation(app.selected)
                    }
                }
            } else {
                Action::Continue
            }
        }
        KeyCode::Down => {
            app.move_down();
            Action::Continue
        }
        KeyCode::Up => {
            app.move_up();
            Action::Continue
        }
        KeyCode::PageDown => {
            // Jump down by a page
            for _ in 0..20 {
                app.move_down();
            }
            Action::Continue
        }
        KeyCode::PageUp => {
            // Jump up by a page
            for _ in 0..20 {
                app.move_up();
            }
            Action::Continue
        }
        KeyCode::Home => {
            app.selected = 0;
            app.scroll_offset = 0;
            Action::Continue
        }
        KeyCode::End => {
            if !app.display_items.is_empty() {
                app.selected = app.display_items.len() - 1;
            }
            Action::Continue
        }
        KeyCode::Backspace => {
            if !app.filter_query.is_empty() {
                app.filter_query.pop();
                app.cancel_flag.store(true, Ordering::Relaxed);
                app.search_receiver = None;
                app.content_results.clear();
                if app.filter_query.is_empty() {
                    app.content_search_state = ContentSearchState::Idle;
                    app.last_keystroke = None;
                    // In grouped mode, clear expanded projects so all groups collapse
                    if app.grouped_mode {
                        app.expanded_projects.clear();
                    }
                } else {
                    app.content_search_state = ContentSearchState::Debouncing;
                    app.last_keystroke = Some(Instant::now());
                    if app.grouped_mode {
                        app.expanded_projects.clear();
                    }
                }
                app.apply_filter();
            }
            Action::Continue
        }
        KeyCode::Char(c) => {
            // First '/' activates filter mode visually without adding to query
            if c == '/' && app.filter_query.is_empty() && !app.filter_active {
                app.filter_active = true;
                return Action::Continue;
            }
            app.filter_active = true;
            app.filter_query.push(c);
            app.cancel_flag.store(true, Ordering::Relaxed);
            app.search_receiver = None;
            app.content_results.clear();
            app.content_search_state = ContentSearchState::Debouncing;
            app.last_keystroke = Some(Instant::now());
            if app.grouped_mode {
                app.expanded_projects.clear();
            }
            app.apply_filter();
            Action::Continue
        }
        _ => Action::Continue,
    }
}

fn handle_conversation(app: &mut App, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Esc => {
            // First Esc: clear highlights if any are active
            if let Some(conv) = &mut app.conversation {
                let has_highlights = !conv.initial_search_terms.is_empty()
                    || conv.search_confirmed;
                if has_highlights {
                    conv.initial_search_terms.clear();
                    conv.search_confirmed = false;
                    conv.search_query.clear();
                    conv.match_positions.clear();
                    conv.rendered_width = 0; // force re-render without highlights
                    return Action::Continue;
                }
            }
            // Second Esc (no highlights): go back to list
            Action::BackToList
        }
        KeyCode::Char('q') => Action::BackToList,
        KeyCode::Char(' ') => {
            if let Some(conv) = &mut app.conversation {
                let max = conv.lines.len().saturating_sub(conv.page_height);
                conv.scroll_offset = (conv.scroll_offset + conv.page_height).min(max);
            }
            Action::Continue
        }
        KeyCode::Char('b') => {
            if let Some(conv) = &mut app.conversation {
                conv.scroll_offset = conv.scroll_offset.saturating_sub(conv.page_height);
            }
            Action::Continue
        }
        KeyCode::Char('g') => {
            if let Some(conv) = &mut app.conversation {
                conv.scroll_offset = 0;
            }
            Action::Continue
        }
        KeyCode::Char('G') => {
            if let Some(conv) = &mut app.conversation {
                let max = conv.lines.len().saturating_sub(conv.page_height);
                conv.scroll_offset = max;
            }
            Action::Continue
        }
        KeyCode::PageDown => {
            if let Some(conv) = &mut app.conversation {
                let max = conv.lines.len().saturating_sub(conv.page_height);
                conv.scroll_offset = (conv.scroll_offset + conv.page_height).min(max);
            }
            Action::Continue
        }
        KeyCode::PageUp => {
            if let Some(conv) = &mut app.conversation {
                conv.scroll_offset = conv.scroll_offset.saturating_sub(conv.page_height);
            }
            Action::Continue
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if let Some(conv) = &mut app.conversation {
                let max = conv.lines.len().saturating_sub(conv.page_height);
                conv.scroll_offset = (conv.scroll_offset + 1).min(max);
            }
            Action::Continue
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if let Some(conv) = &mut app.conversation {
                conv.scroll_offset = conv.scroll_offset.saturating_sub(1);
            }
            Action::Continue
        }
        KeyCode::Char('n') => {
            jump_to_next_match(app);
            Action::Continue
        }
        KeyCode::Char('N') => {
            jump_to_prev_match(app);
            Action::Continue
        }
        KeyCode::Char('/') => {
            if let Some(conv) = &mut app.conversation {
                conv.search_active = true;
                if conv.search_confirmed && !conv.search_query.is_empty() {
                    conv.search_replacing = true;
                    conv.search_cursor = conv.search_query.chars().count();
                } else if !conv.initial_search_terms.is_empty() {
                    conv.search_query = conv.initial_search_terms.join(" ");
                    conv.search_replacing = true;
                    conv.search_cursor = conv.search_query.chars().count();
                } else {
                    conv.search_query.clear();
                    conv.search_replacing = false;
                    conv.search_cursor = 0;
                }
                conv.search_confirmed = false;
                conv.rendered_width = 0;
            }
            app.mode = Mode::ConversationSearch;
            Action::Continue
        }
        KeyCode::Enter => {
            if let Some(conv) = &app.conversation {
                if key.modifiers.contains(KeyModifiers::ALT) {
                    let session = conv.session.clone();
                    open_launch_menu(app, session);
                    Action::Continue
                } else {
                    let cmd = app.resume_command(&conv.session);
                    let cwd = conv.session.cwd.clone();
                    let id = conv.session.id.clone();
                    Action::ExecCommand { cmd, cwd, id }
                }
            } else {
                Action::Continue
            }
        }
        KeyCode::Char('y') => match &app.conversation {
            Some(conv) => Action::CopyCommand(app.resume_command(&conv.session)),
            None => Action::Continue,
        },
        _ => Action::Continue,
    }
}

/// Keys while the launch menu is open: move, Enter to pick the highlighted
/// row and close, Space or a number to pick and stay, Esc to close. Picks
/// are remembered for the next start.
fn handle_mode_menu(app: &mut App, key: KeyEvent) -> Action {
    let Some(sel) = app.mode_menu else {
        return Action::Continue;
    };
    let rows = app.menu_rows();
    let count = rows.len();
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => close_menu(app),
        KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::ALT) => close_menu(app),
        KeyCode::Up | KeyCode::Char('k') => app.mode_menu = Some((sel + count - 1) % count),
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
            app.mode_menu = Some((sel + 1) % count)
        }
        KeyCode::Char(c @ '1'..='9') if (c as usize - '1' as usize) < count => {
            let idx = c as usize - '1' as usize;
            app.mode_menu = Some(idx);
            pick_row(app, rows[idx]);
        }
        KeyCode::Char(' ') => pick_row(app, rows[sel]),
        KeyCode::Enter => {
            pick_row(app, rows[sel]);
            if let Some(session) = app.menu_launch.take() {
                app.mode_menu = None;
                let cmd = app.resume_command(&session);
                return Action::ExecCommand {
                    cmd,
                    cwd: session.cwd,
                    id: session.id,
                };
            }
            app.mode_menu = None;
        }
        _ => {}
    }
    Action::Continue
}

/// Open the menu for resuming `session`: Enter picks the highlighted row
/// and resumes with the resulting mode and profile.
fn open_launch_menu(app: &mut App, session: Session) {
    app.mode_menu = Some(app.config.mode);
    app.menu_launch = Some(session);
}

fn close_menu(app: &mut App) {
    app.mode_menu = None;
    app.menu_launch = None;
}

pub(crate) fn pick_row_status_message(config: &crate::config::Config) -> String {
    let current = config.current_mode();
    if current.pass_profile {
        format!(
            "Resume with: {} \u{00B7} profile {}",
            current.name,
            config.profile_label()
        )
    } else {
        format!("Resume with: {}", current.name)
    }
}

fn pick_row(app: &mut App, row: MenuRow) {
    match row {
        MenuRow::Mode(i) => app.config.mode = i,
        MenuRow::Profile(p) => app.config.profile_override = p,
    }
    app.config.save_state();
    app.status_message = Some((pick_row_status_message(&app.config), Instant::now()));
}

fn handle_conversation_search(app: &mut App, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Esc => {
            if let Some(conv) = &mut app.conversation {
                conv.search_active = false;
                conv.search_query.clear();
                conv.search_confirmed = false;
                conv.search_replacing = false;
                conv.rendered_width = 0;
            }
            app.mode = Mode::Conversation;
            Action::Continue
        }
        KeyCode::Enter => {
            if let Some(conv) = &mut app.conversation {
                conv.search_active = false;
                conv.search_replacing = false;
                if !conv.search_query.is_empty() && !conv.match_positions.is_empty() {
                    conv.search_confirmed = true;
                    conv.current_match = 0;
                    let max = conv.lines.len().saturating_sub(conv.page_height);
                    conv.scroll_offset = conv.match_positions[0]
                        .saturating_sub(conv.page_height / 2)
                        .min(max);
                }
            }
            app.mode = Mode::Conversation;
            Action::Continue
        }
        KeyCode::Backspace => {
            if let Some(conv) = &mut app.conversation {
                if conv.search_replacing {
                    conv.search_query.clear();
                    conv.search_cursor = 0;
                    conv.search_replacing = false;
                } else if conv.search_cursor > 0 {
                    let byte_idx = char_to_byte_index(&conv.search_query, conv.search_cursor - 1);
                    conv.search_query.remove(byte_idx);
                    conv.search_cursor -= 1;
                }
                conv.rendered_width = 0;
            }
            Action::Continue
        }
        KeyCode::Left => {
            if let Some(conv) = &mut app.conversation {
                if conv.search_replacing {
                    conv.search_replacing = false;
                    conv.search_cursor = 0;
                } else if conv.search_cursor > 0 {
                    conv.search_cursor -= 1;
                }
            }
            Action::Continue
        }
        KeyCode::Right => {
            if let Some(conv) = &mut app.conversation {
                let char_count = conv.search_query.chars().count();
                if conv.search_replacing {
                    conv.search_replacing = false;
                    conv.search_cursor = char_count;
                } else if conv.search_cursor < char_count {
                    conv.search_cursor += 1;
                }
            }
            Action::Continue
        }
        KeyCode::Char(c) => {
            if let Some(conv) = &mut app.conversation {
                if conv.search_replacing {
                    conv.search_query.clear();
                    conv.search_cursor = 0;
                    conv.search_replacing = false;
                }
                let byte_idx = char_to_byte_index(&conv.search_query, conv.search_cursor);
                conv.search_query.insert(byte_idx, c);
                conv.search_cursor += 1;
                conv.rendered_width = 0;
            }
            Action::Continue
        }
        _ => Action::Continue,
    }
}

fn jump_to_next_match(app: &mut App) {
    if let Some(conv) = &mut app.conversation {
        if conv.match_positions.is_empty() {
            return;
        }
        conv.current_match = (conv.current_match + 1) % conv.match_positions.len();
        let max = conv.lines.len().saturating_sub(conv.page_height);
        conv.scroll_offset = conv.match_positions[conv.current_match]
            .saturating_sub(conv.page_height / 2)
            .min(max);
    }
}

fn jump_to_prev_match(app: &mut App) {
    if let Some(conv) = &mut app.conversation {
        if conv.match_positions.is_empty() {
            return;
        }
        if conv.current_match == 0 {
            conv.current_match = conv.match_positions.len() - 1;
        } else {
            conv.current_match -= 1;
        }
        let max = conv.lines.len().saturating_sub(conv.page_height);
        conv.scroll_offset = conv.match_positions[conv.current_match]
            .saturating_sub(conv.page_height / 2)
            .min(max);
    }
}

/// Convert a char index to a byte index in a string.
fn char_to_byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(byte_pos, _)| byte_pos)
        .unwrap_or(s.len())
}
