//! TUI event handler — applies AppEvents to App state.
//!
//! Pure state transitions, no I/O. Testable without a terminal.

use crossterm::event::{KeyCode, KeyModifiers};

use super::app::App;
use super::event::AppEvent;

/// Apply one event to the app state.
pub fn apply_event(app: &mut App, event: AppEvent) {
    match event {
        AppEvent::Envelope(env) => {
            app.ingest_envelope(&env);
        }
        AppEvent::Snapshot(snap) => {
            app.apply_snapshot(snap);
        }
        AppEvent::SnapshotError(msg) => {
            app.snapshot_error = Some(msg);
        }
        AppEvent::WaveTasks {
            wave_id,
            done,
            total,
        } => {
            // Cache the counts and update the matching row (P1-3). Pure state.
            app.wave_counts.insert(wave_id.clone(), (done, total));
            if let Some(row) = app.waves.iter_mut().find(|w| w.wave_id == wave_id) {
                row.done = done;
                row.total = total;
            }
        }
        AppEvent::Key(key) => handle_key(app, key),
        AppEvent::Resize(_, _) => {
            // ratatui handles resize via draw; no state change needed
        }
        AppEvent::Tick => {
            // Idle tick — could decay live states here in the future
        }
    }
}

fn handle_key(app: &mut App, key: crossterm::event::KeyEvent) {
    // Ctrl+C always quits
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return;
    }

    // Filter editing mode captures all keys
    if app.filter_editing {
        handle_filter_key(app, key);
        return;
    }

    // Help popup: any key closes it
    if app.show_help {
        app.show_help = false;
        return;
    }

    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('?') => app.show_help = true,
        KeyCode::Tab => app.focus = app.focus.next(),
        KeyCode::BackTab => app.focus = app.focus.prev(),
        KeyCode::Char('1') => app.focus = super::model::PanelFocus::Agents,
        KeyCode::Char('2') => app.focus = super::model::PanelFocus::Sessions,
        KeyCode::Char('3') => app.focus = super::model::PanelFocus::Waves,
        KeyCode::Char('4') => app.focus = super::model::PanelFocus::Feed,
        KeyCode::Char('r') => {
            // P0-2: request an immediate API refresh. The run loop drains
            // this flag and signals the ticker. Handler stays pure.
            app.snapshot_error = None;
            app.refresh_requested = true;
        }
        KeyCode::Char('/') => {
            app.filter_editing = true;
            app.feed_filter.clear();
        }
        KeyCode::Esc => {
            app.feed_filter.clear();
        }
        KeyCode::Char('j') | KeyCode::Down => move_selection(app, 1),
        KeyCode::Char('k') | KeyCode::Up => move_selection(app, -1),
        KeyCode::PageDown => {
            if app.focus == super::model::PanelFocus::Feed {
                app.feed_scroll = app.feed_scroll.saturating_add(10);
            }
        }
        KeyCode::PageUp => {
            if app.focus == super::model::PanelFocus::Feed {
                app.feed_scroll = app.feed_scroll.saturating_sub(10);
                app.feed_follow = false; // scrolled up → disengage auto-follow
            }
        }
        KeyCode::Char('G') | KeyCode::End => {
            if app.focus == super::model::PanelFocus::Feed {
                app.feed_follow = true; // jump to bottom → re-engage auto-follow
            }
        }
        _ => {}
    }
}

fn handle_filter_key(app: &mut App, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.filter_editing = false;
            app.feed_filter.clear();
        }
        KeyCode::Enter => {
            app.filter_editing = false;
        }
        KeyCode::Backspace => {
            app.feed_filter.pop();
        }
        KeyCode::Char(c) => {
            app.feed_filter.push(c);
        }
        _ => {}
    }
}

fn move_selection(app: &mut App, delta: i32) {
    match app.focus {
        super::model::PanelFocus::Agents => {
            let len = app.agents.len();
            if len > 0 {
                app.agent_sel = wrap_index(app.agent_sel, delta, len);
            }
        }
        super::model::PanelFocus::Sessions => {
            let len = app.sessions.len();
            if len > 0 {
                app.session_sel = wrap_index(app.session_sel, delta, len);
            }
        }
        super::model::PanelFocus::Waves => {
            let len = app.waves.len();
            if len > 0 {
                app.wave_sel = wrap_index(app.wave_sel, delta, len);
            }
        }
        super::model::PanelFocus::Feed => {
            if delta > 0 {
                app.feed_scroll = app.feed_scroll.saturating_add(1);
            } else {
                app.feed_scroll = app.feed_scroll.saturating_sub(1);
                app.feed_follow = false; // scrolled up → disengage auto-follow
            }
        }
    }
}

fn wrap_index(current: usize, delta: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let cur = current as i32;
    let next = (cur + delta).rem_euclid(len as i32);
    next as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Envelope, MessageKind};
    use crate::tui::model::{PanelFocus, Snapshot};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn test_quit_on_q() {
        let mut app = App::new(100, 120);
        apply_event(&mut app, AppEvent::Key(key(KeyCode::Char('q'))));
        assert!(app.should_quit);
    }

    #[test]
    fn test_quit_on_ctrl_c() {
        let mut app = App::new(100, 120);
        let k = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        apply_event(&mut app, AppEvent::Key(k));
        assert!(app.should_quit);
    }

    #[test]
    fn test_tab_cycles_focus() {
        let mut app = App::new(100, 120);
        assert_eq!(app.focus, PanelFocus::Agents);
        apply_event(&mut app, AppEvent::Key(key(KeyCode::Tab)));
        assert_eq!(app.focus, PanelFocus::Sessions);
        apply_event(&mut app, AppEvent::Key(key(KeyCode::Tab)));
        assert_eq!(app.focus, PanelFocus::Waves);
    }

    #[test]
    fn test_number_keys_jump() {
        let mut app = App::new(100, 120);
        apply_event(&mut app, AppEvent::Key(key(KeyCode::Char('4'))));
        assert_eq!(app.focus, PanelFocus::Feed);
    }

    #[test]
    fn test_feed_ring_buffer_cap() {
        let mut app = App::new(5, 120);
        for i in 0..10 {
            let env = Envelope::new(
                "test",
                "ch",
                MessageKind::Message,
                serde_json::json!({"message": format!("msg {i}")}),
            );
            app.ingest_envelope(&env);
        }
        assert_eq!(app.feed.len(), 5);
        assert_eq!(app.total_envelopes, 10);
        // Oldest should be msg 5
        assert!(app.feed.front().unwrap().summary.contains("5"));
    }

    #[test]
    fn test_filter_editing() {
        let mut app = App::new(100, 120);
        apply_event(&mut app, AppEvent::Key(key(KeyCode::Char('/'))));
        assert!(app.filter_editing);
        apply_event(&mut app, AppEvent::Key(key(KeyCode::Char('w'))));
        apply_event(&mut app, AppEvent::Key(key(KeyCode::Char('o'))));
        assert_eq!(app.feed_filter, "wo");
        apply_event(&mut app, AppEvent::Key(key(KeyCode::Enter)));
        assert!(!app.filter_editing);
        assert_eq!(app.feed_filter, "wo");
    }

    #[test]
    fn test_help_toggle() {
        let mut app = App::new(100, 120);
        apply_event(&mut app, AppEvent::Key(key(KeyCode::Char('?'))));
        assert!(app.show_help);
        apply_event(&mut app, AppEvent::Key(key(KeyCode::Char('x'))));
        assert!(!app.show_help);
    }

    #[test]
    fn test_snapshot_applies() {
        let mut app = App::new(100, 120);
        let snap = Snapshot {
            agents: vec![crate::storage::AgentRecord {
                identity: "w1".to_string(),
                capabilities: vec!["code".to_string()],
                last_seen: chrono::Utc::now(),
                registered_at: chrono::Utc::now(),
                metadata: serde_json::Value::Null,
            }],
            ..Default::default()
        };
        apply_event(&mut app, AppEvent::Snapshot(snap));
        assert_eq!(app.agents.len(), 1);
        assert_eq!(app.agents[0].identity, "w1");
        assert!(app.last_snapshot_at.is_some());
    }

    #[test]
    fn test_r_requests_refresh() {
        let mut app = App::new(100, 120);
        assert!(!app.refresh_requested);
        apply_event(&mut app, AppEvent::Key(key(KeyCode::Char('r'))));
        assert!(app.refresh_requested);
        assert!(app.snapshot_error.is_none());
    }

    #[test]
    fn test_wave_tasks_updates_row_and_cache() {
        let mut app = App::new(100, 120);
        app.waves.push(crate::tui::model::WaveRow {
            wave_id: "w1".to_string(),
            goal: "g".to_string(),
            status: "running".to_string(),
            done: 0,
            total: 0,
        });
        apply_event(
            &mut app,
            AppEvent::WaveTasks {
                wave_id: "w1".to_string(),
                done: 3,
                total: 5,
            },
        );
        assert_eq!(app.wave_counts.get("w1"), Some(&(3, 5)));
        assert_eq!(app.waves[0].done, 3);
        assert_eq!(app.waves[0].total, 5);
    }

    #[test]
    fn test_feed_follow_default_true() {
        let app = App::new(100, 120);
        assert!(app.feed_follow);
    }

    #[test]
    fn test_feed_follow_scroll_up_disengages() {
        let mut app = App::new(100, 120);
        apply_event(&mut app, AppEvent::Key(key(KeyCode::Char('4')))); // focus Feed
        assert!(app.feed_follow);
        apply_event(&mut app, AppEvent::Key(key(KeyCode::Char('k')))); // scroll up
        assert!(!app.feed_follow);
    }

    #[test]
    fn test_feed_follow_g_reengages() {
        let mut app = App::new(100, 120);
        apply_event(&mut app, AppEvent::Key(key(KeyCode::Char('4')))); // focus Feed
        apply_event(&mut app, AppEvent::Key(key(KeyCode::Up))); // scroll up → disengage
        assert!(!app.feed_follow);
        apply_event(&mut app, AppEvent::Key(key(KeyCode::Char('G')))); // re-engage
        assert!(app.feed_follow);

        // End key also re-engages after another scroll-up
        apply_event(&mut app, AppEvent::Key(key(KeyCode::Char('k'))));
        assert!(!app.feed_follow);
        apply_event(&mut app, AppEvent::Key(key(KeyCode::End)));
        assert!(app.feed_follow);
    }
}
