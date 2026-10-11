//! Keyboard event handling and navigation dispatch for the Cortex TUI.

use crate::app::{ActiveTab, App, PortalInputMode};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Process a single keyboard event and update [`App`] state accordingly.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        if app.active_tab == ActiveTab::Chat {
            if app.chat_is_running {
                if app.is_chat_cancelling() {
                    app.should_quit = true;
                    return;
                }
                app.cancel_chat_agent();
                return;
            } else if !app.chat_input.is_empty() {
                app.chat_input_clear();
                return;
            }
        }
        app.should_quit = true;
        return;
    }

    // Global shortcut: Ctrl+V toggles the Live Agent Visualizer HUD across all screens
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && (key.code == KeyCode::Char('v') || key.code == KeyCode::Char('V'))
    {
        app.toggle_visualizer();
        return;
    }

    // Modal navigation when the Live Agent Visualizer HUD is open
    if app.visualizer.is_open {
        match key.code {
            KeyCode::Esc | KeyCode::Char('v') | KeyCode::Char('V') => {
                app.close_visualizer();
                return;
            }
            KeyCode::Tab => {
                app.cycle_visualizer_tab();
                return;
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                if app.chat_is_running {
                    app.cancel_chat_agent();
                }
                return;
            }
            _ => {
                // Trap all other keys while HUD is open
                return;
            }
        }
    }

    // Dedicated routing for the interactive Chat harness tab
    if app.active_tab == ActiveTab::Chat {
        // Handle Ctrl shortcuts first
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('t') | KeyCode::Char('T') => {
                    app.toggle_thinking_expanded();
                    return;
                }
                KeyCode::Char('o') | KeyCode::Char('O') => {
                    app.toggle_tool_calls_expanded();
                    return;
                }
                KeyCode::Char('m') | KeyCode::Char('M') => {
                    app.toggle_coordination_panel();
                    return;
                }
                KeyCode::Char('f') | KeyCode::Char('F') => {
                    app.cycle_coordination_filter();
                    return;
                }
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    app.cycle_stage_filter();
                    return;
                }
                KeyCode::Char('u') | KeyCode::Char('U') => {
                    app.chat_input_clear();
                    return;
                }
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    app.chat_input_home();
                    return;
                }
                KeyCode::Char('e') | KeyCode::Char('E') => {
                    app.chat_input_end();
                    return;
                }
                KeyCode::Char('j') | KeyCode::Char('J') => {
                    app.chat_input_insert('\n');
                    return;
                }
                _ => {}
            }
        }

        if app.chat_input.starts_with('/') && !app.chat_input.contains(' ') {
            app.update_autocomplete();
        } else {
            app.autocomplete_state.is_open = false;
        }

        // Check floating autocomplete popup navigation first
        if app.autocomplete_state.is_open {
            match key.code {
                KeyCode::Up => {
                    app.autocomplete_prev();
                    return;
                }
                KeyCode::Down => {
                    app.autocomplete_next();
                    return;
                }
                KeyCode::Tab | KeyCode::Enter => {
                    app.autocomplete_accept();
                    return;
                }
                KeyCode::Esc => {
                    app.autocomplete_close();
                    return;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Tab => {
                if app.chat_input.starts_with('/') {
                    app.update_autocomplete();
                    if app.autocomplete_state.is_open {
                        app.autocomplete_accept();
                    } else if let Some(completed) =
                        crate::commands::autocomplete_command(&app.chat_input)
                    {
                        app.chat_input = completed;
                        app.chat_cursor = app.chat_input.len();
                    }
                } else if app.chat_input.is_empty() {
                    app.chat_input = "/".to_string();
                    app.chat_cursor = 1;
                    app.update_autocomplete();
                } else {
                    app.chat_input_insert(' ');
                    app.chat_input_insert(' ');
                }
            }
            KeyCode::BackTab => {}
            KeyCode::Esc => {
                if app.autocomplete_state.is_open {
                    app.autocomplete_close();
                } else if app.chat_is_running {
                    app.cancel_chat_agent();
                } else if !app.chat_input.is_empty() {
                    app.chat_input_clear();
                }
            }
            KeyCode::Enter => {
                if key.modifiers.contains(KeyModifiers::ALT)
                    || key.modifiers.contains(KeyModifiers::SHIFT)
                {
                    app.chat_input_insert('\n');
                } else if app.chat_input.ends_with('\\') {
                    app.chat_input.pop();
                    app.chat_cursor = app.chat_input.len();
                    app.chat_input_insert('\n');
                } else {
                    app.dispatch_chat();
                }
            }
            KeyCode::Backspace => {
                app.chat_input_backspace();
            }
            KeyCode::Delete => {
                app.chat_input_delete();
            }
            KeyCode::Left => {
                app.chat_input_left();
            }
            KeyCode::Right => {
                app.chat_input_right();
            }
            KeyCode::Home => {
                app.chat_input_home();
            }
            KeyCode::End => {
                app.chat_input_end();
            }
            KeyCode::PageUp => {
                app.chat_scroll_up();
            }
            KeyCode::PageDown => {
                app.chat_scroll_down();
            }
            KeyCode::Up => {
                if app.chat_input.contains('\n') {
                    app.chat_input_up();
                } else if !app.chat_history.is_empty()
                    && (app.chat_input.is_empty() || app.chat_history_idx.is_some())
                {
                    app.chat_history_prev();
                } else {
                    app.chat_scroll_up();
                }
            }
            KeyCode::Down => {
                if app.chat_input.contains('\n') {
                    app.chat_input_down();
                } else if app.chat_history_idx.is_some() {
                    app.chat_history_next();
                } else {
                    app.chat_scroll_down();
                }
            }
            KeyCode::Char(c) => {
                app.chat_input_insert(c);
            }
            _ => {}
        }
        return;
    }

    // Dedicated routing for Portals tab when inside an active input or edit mode
    if app.active_tab == ActiveTab::Portals && app.portal_input_mode != PortalInputMode::Normal {
        match key.code {
            KeyCode::Esc => {
                app.cancel_portal_input();
            }
            KeyCode::Enter => {
                app.confirm_portal_input();
            }
            KeyCode::Backspace => {
                app.portal_input_buffer.pop();
            }
            KeyCode::Char(c) => {
                app.portal_input_buffer.push(c);
            }
            _ => {}
        }
        return;
    }

    // Default navigation for overview tabs and Portals in normal mode
    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') => {
            app.should_quit = true;
        }
        KeyCode::Tab => {
            app.next_tab();
        }
        KeyCode::BackTab => {
            app.prev_tab();
        }
        KeyCode::Char('1') => {
            app.set_tab(ActiveTab::Dashboard);
        }
        KeyCode::Char('2') => {
            app.set_tab(ActiveTab::Agents);
        }
        KeyCode::Char('3') => {
            app.set_tab(ActiveTab::ActiveRun);
        }
        KeyCode::Char('4') => {
            app.set_tab(ActiveTab::Events);
        }
        KeyCode::Char('5') => {
            app.set_tab(ActiveTab::History);
        }
        KeyCode::Char('6') => {
            app.set_tab(ActiveTab::Tasks);
        }
        KeyCode::Char('7') => {
            app.set_tab(ActiveTab::Chat);
        }
        KeyCode::Char('8') => {
            app.set_tab(ActiveTab::Portals);
        }
        KeyCode::Char('9') => {
            app.set_tab(ActiveTab::Scheduler);
        }
        KeyCode::Char('v') | KeyCode::Char('V') => {
            app.toggle_visualizer();
        }
        KeyCode::Char('r') | KeyCode::Char('R') => {
            app.refresh();
            app.status_message = Some("Refreshed state from SQLite store.".to_string());
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.prev_item();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.next_item();
        }
        KeyCode::Enter => {
            app.select_current();
        }
        // Agents lifecycle action shortcuts
        KeyCode::Char('s') if app.active_tab == ActiveTab::Agents => {
            app.start_selected_agent();
        }
        KeyCode::Char('x') if app.active_tab == ActiveTab::Agents => {
            app.stop_selected_agent();
        }
        KeyCode::Char('p') if app.active_tab == ActiveTab::Agents => {
            app.pause_selected_agent();
        }
        // Scheduler quick action shortcuts
        KeyCode::Char('p') if app.active_tab == ActiveTab::Scheduler => {
            app.toggle_pause_selected_cron_job();
        }
        KeyCode::Char('t') if app.active_tab == ActiveTab::Scheduler => {
            app.trigger_selected_cron_job();
        }
        // Portals quick action shortcuts in normal mode
        KeyCode::Char('a') if app.active_tab == ActiveTab::Portals => {
            app.start_adding_portal();
        }
        KeyCode::Char('e') if app.active_tab == ActiveTab::Portals => {
            app.start_editing_key();
        }
        KeyCode::Char('b') if app.active_tab == ActiveTab::Portals => {
            app.start_editing_base_url();
        }
        KeyCode::Char('d') if app.active_tab == ActiveTab::Portals => {
            app.delete_selected_portal();
        }
        KeyCode::Char('t') if app.active_tab == ActiveTab::Portals => {
            app.test_selected_portal();
        }
        KeyCode::Char(' ') if app.active_tab == ActiveTab::Portals => {
            app.activate_selected_portal();
        }
        KeyCode::Esc if app.active_tab == ActiveTab::Scheduler => {
            if app.cron_drawer_open {
                app.close_cron_drawer();
            } else {
                app.set_tab(ActiveTab::Dashboard);
            }
        }
        KeyCode::Esc if app.active_tab != ActiveTab::Dashboard => {
            app.set_tab(ActiveTab::Dashboard);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: crossterm::event::KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn make_ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::CONTROL,
            kind: crossterm::event::KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn test_quit_keys() {
        let mut app = App::new(None);
        assert!(!app.should_quit);

        handle_key(&mut app, make_ctrl_key(KeyCode::Char('c')));
        assert!(app.should_quit);

        let mut app2 = App::new(None);
        app2.set_tab(ActiveTab::Dashboard);
        handle_key(&mut app2, make_key(KeyCode::Char('q')));
        assert!(app2.should_quit);
    }

    #[test]
    fn test_tab_keys() {
        let mut app = App::new(None);
        assert_eq!(app.active_tab, ActiveTab::Chat);

        // Tab in empty chat inserts "/"
        handle_key(&mut app, make_key(KeyCode::Tab));
        assert_eq!(app.chat_input, "/");

        // Tab autocompletes /m to /model
        app.chat_input = "/m".to_string();
        app.chat_cursor = 2;
        handle_key(&mut app, make_key(KeyCode::Tab));
        assert_eq!(app.chat_input, "/model ");

        // Also test legacy overview tab cycling when active_tab is Dashboard
        let mut app2 = App::new(None);
        app2.set_tab(ActiveTab::Dashboard);
        handle_key(&mut app2, make_key(KeyCode::Tab));
        assert_eq!(app2.active_tab, ActiveTab::Agents);

        handle_key(&mut app2, make_key(KeyCode::Char('9')));
        assert_eq!(app2.active_tab, ActiveTab::Scheduler);

        handle_key(&mut app2, make_key(KeyCode::Enter));
        assert!(app2.cron_drawer_open);

        handle_key(&mut app2, make_key(KeyCode::Esc));
        assert!(!app2.cron_drawer_open);

        handle_key(&mut app2, make_key(KeyCode::Esc));
        assert_eq!(app2.active_tab, ActiveTab::Dashboard);
    }

    #[test]
    fn test_chat_tab_input_handling() {
        let mut app = App::new(None);
        assert_eq!(app.active_tab, ActiveTab::Chat);

        handle_key(&mut app, make_key(KeyCode::Char('h')));
        handle_key(&mut app, make_key(KeyCode::Char('e')));
        handle_key(&mut app, make_key(KeyCode::Char('l')));
        handle_key(&mut app, make_key(KeyCode::Char('l')));
        handle_key(&mut app, make_key(KeyCode::Char('o')));
        assert_eq!(app.chat_input, "hello");

        handle_key(&mut app, make_key(KeyCode::Backspace));
        assert_eq!(app.chat_input, "hell");

        handle_key(&mut app, make_key(KeyCode::Esc));
        assert_eq!(app.chat_input, "");
        assert_eq!(app.active_tab, ActiveTab::Chat);

        // Esc on empty input stays in chat
        handle_key(&mut app, make_key(KeyCode::Esc));
        assert_eq!(app.active_tab, ActiveTab::Chat);
    }

    #[test]
    fn test_portal_tab_shortcuts_and_editing() {
        let mut app = App::new(None);
        app.set_tab(ActiveTab::Portals);
        assert_eq!(app.selected_portal_idx, 0);

        handle_key(&mut app, make_key(KeyCode::Down));
        assert_eq!(app.selected_portal_idx, 1);

        // Activate portal with Space
        handle_key(&mut app, make_key(KeyCode::Char(' ')));
        // Enter edit key mode with 'e'
        app.portals[1].api_key = None;
        handle_key(&mut app, make_key(KeyCode::Char('e')));
        assert_eq!(app.portal_input_mode, PortalInputMode::EditingKey);

        // Type key
        handle_key(&mut app, make_key(KeyCode::Char('x')));
        handle_key(&mut app, make_key(KeyCode::Enter));
        assert_eq!(app.portal_input_mode, PortalInputMode::Normal);
        assert_eq!(app.portals[1].api_key, Some("x".to_string()));
    }

    #[test]
    fn test_ctrl_c_cancels_or_clears_without_quitting() {
        let mut app = App::new(None);
        app.chat_input = "draft task text".to_string();
        app.chat_cursor = app.chat_input.len();

        // Ctrl+C with non-empty input should clear input without quitting
        handle_key(&mut app, make_ctrl_key(KeyCode::Char('c')));
        assert!(!app.should_quit);
        assert_eq!(app.chat_input, "");

        // Next Ctrl+C with empty input should quit
        handle_key(&mut app, make_ctrl_key(KeyCode::Char('c')));
        assert!(app.should_quit);
    }

    #[test]
    fn test_ctrl_o_and_multiline_enter() {
        let mut app = App::new(None);
        assert!(!app.tool_calls_expanded);

        // Ctrl+O toggles tool calls expansion
        handle_key(&mut app, make_ctrl_key(KeyCode::Char('o')));
        assert!(app.tool_calls_expanded);
        handle_key(&mut app, make_ctrl_key(KeyCode::Char('o')));
        assert!(!app.tool_calls_expanded);

        // Backslash + Enter creates newline for multi-line input
        app.chat_input = "line 1\\".to_string();
        handle_key(&mut app, make_key(KeyCode::Enter));
        assert_eq!(app.chat_input, "line 1\n");

        // Alt+Enter also creates newline
        let alt_enter = KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::ALT,
            kind: crossterm::event::KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        handle_key(&mut app, alt_enter);
        assert_eq!(app.chat_input, "line 1\n\n");
    }

    #[test]
    fn test_double_ctrl_c_force_quits_running_agent() {
        let mut app = App::new(None);
        let cancel_token = cortex_runtime::CancellationToken::new();
        let (_tx, rx) = std::sync::mpsc::channel();
        app.chat_handle = Some(crate::app::AgentExecutionHandle {
            cancel_token,
            receiver: rx,
            run_id: Some("run_test".to_string()),
        });
        app.chat_is_running = true;

        // First Ctrl+C triggers graceful cancellation without quitting
        handle_key(&mut app, make_ctrl_key(KeyCode::Char('c')));
        assert!(!app.should_quit);
        assert!(app.is_chat_cancelling());

        // Second Ctrl+C forces quit immediately
        handle_key(&mut app, make_ctrl_key(KeyCode::Char('c')));
        assert!(app.should_quit);
    }

    #[test]
    fn test_visualizer_key_events() {
        let mut app = App::new(None);
        assert!(!app.visualizer.is_open);

        // Ctrl+V toggles visualizer open
        handle_key(&mut app, make_ctrl_key(KeyCode::Char('v')));
        assert!(app.visualizer.is_open);

        // Tab cycles visualizer sub-tab
        assert_eq!(app.visualizer.sub_tab, 0);
        handle_key(&mut app, make_key(KeyCode::Tab));
        assert_eq!(app.visualizer.sub_tab, 1);
        handle_key(&mut app, make_key(KeyCode::Tab));
        assert_eq!(app.visualizer.sub_tab, 2);
        handle_key(&mut app, make_key(KeyCode::Tab));
        assert_eq!(app.visualizer.sub_tab, 0);

        // 'v' closes visualizer when open
        handle_key(&mut app, make_key(KeyCode::Char('v')));
        assert!(!app.visualizer.is_open);

        // Ctrl+V opens again, Esc closes
        handle_key(&mut app, make_ctrl_key(KeyCode::Char('v')));
        assert!(app.visualizer.is_open);
        handle_key(&mut app, make_key(KeyCode::Esc));
        assert!(!app.visualizer.is_open);

        // Non-chat tab 'v' key toggles visualizer
        app.set_tab(ActiveTab::Dashboard);
        handle_key(&mut app, make_key(KeyCode::Char('v')));
        assert!(app.visualizer.is_open);
    }
}
