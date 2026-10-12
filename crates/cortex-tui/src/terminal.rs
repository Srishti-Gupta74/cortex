//! Terminal initialization, cleanup guard, and panic hook restoration.

use cortex_core::{CortexError, Result};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{stdout, Stdout};
use std::panic;

/// RAII guard ensuring terminal state (raw mode, alternate screen, cursor)
/// is cleanly restored upon exit or unexpected panic.
pub struct TerminalGuard {
    _private: (),
}

impl TerminalGuard {
    /// Initialize the terminal into raw mode and alternate screen buffer.
    pub fn init() -> Result<(Terminal<CrosstermBackend<Stdout>>, Self)> {
        enable_raw_mode().map_err(|e| {
            CortexError::Internal(format!("failed to enable raw terminal mode: {}", e))
        })?;

        let mut out = stdout();
        execute!(out, EnterAlternateScreen, EnableMouseCapture, Hide).map_err(|e| {
            let _ = disable_raw_mode();
            CortexError::Internal(format!("failed to enter alternate screen: {}", e))
        })?;

        // Install panic hook to restore terminal before panic unwind prints
        let original_hook = panic::take_hook();
        panic::set_hook(Box::new(move |panic_info| {
            let _ = disable_raw_mode();
            let _ = execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture, Show);
            original_hook(panic_info);
        }));

        let backend = CrosstermBackend::new(out);
        let terminal = Terminal::new(backend).map_err(|e| {
            let _ = disable_raw_mode();
            let _ = execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture, Show);
            CortexError::Internal(format!("failed to initialize ratatui terminal: {}", e))
        })?;

        Ok((terminal, Self { _private: () }))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture, Show);
    }
}
