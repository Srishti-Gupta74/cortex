//! Cancellation must remain observable after the shell exits but pipes stay open.

use cortex_core::{CortexError, RunId};
use cortex_runtime::tool::Tool;
use cortex_runtime::tools::ShellTool;
use cortex_runtime::workspace::Workspace;
use cortex_runtime::CancellationToken;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn shell_child_fixture() {
    let Ok(marker) = std::env::var("CORTEX_CANCELLATION_FIXTURE") else {
        return;
    };
    std::fs::write(marker, "ready").unwrap();
    // Inherits stdout/stderr from the shell; the parent test kills this worker.
    std::thread::sleep(Duration::from_secs(15));
}

#[test]
fn cancels_orphaned_output_pipes() {
    let root = std::env::temp_dir().join(RunId::generate().as_str());
    let workspace = Arc::new(Workspace::new(&root).unwrap());
    let marker = root.join("ready");
    let executable = std::env::current_exe().unwrap();
    let executable = executable.to_string_lossy();
    let executable = executable.strip_prefix(r"\\?\").unwrap_or(&executable);
    let command = if cfg!(windows) {
        std::fs::write(root.join("fixture.cmd"), format!("@echo off\r\nset \"CORTEX_CANCELLATION_FIXTURE={}\"\r\nstart /b \"\" \"{}\" --exact shell_child_fixture --nocapture\r\n", marker.display(), executable)).unwrap();
        "fixture.cmd".to_string()
    } else {
        format!(
            "CORTEX_CANCELLATION_FIXTURE='{}' '{}' --exact shell_child_fixture --nocapture &",
            marker.display(),
            executable
        )
    };
    let token = CancellationToken::new();
    let cancel_token = token.clone();
    let canceller = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !marker.exists() {
            assert!(Instant::now() < deadline, "shell descendant never started");
            std::thread::yield_now();
        }
        let start = Instant::now();
        cancel_token.cancel();
        start
    });
    let result = ShellTool::new(workspace)
        .execute_with_cancellation(&json!({"command": command}), Some(&token));
    let start = canceller
        .join()
        .unwrap_or_else(|_| panic!("canceller failed; shell result: {result:?}"));
    assert!(
        matches!(result, Err(CortexError::Cancelled(_))),
        "{result:?}"
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn precancelled_shell_does_not_spawn() {
    let root = std::env::temp_dir().join(RunId::generate().as_str());
    let workspace = Arc::new(Workspace::new(&root).unwrap());
    let token = CancellationToken::new();
    token.cancel();
    let result = ShellTool::new(workspace).execute_with_cancellation(
        &json!({"command": "echo side-effect > marker"}),
        Some(&token),
    );
    assert!(matches!(result, Err(CortexError::Cancelled(_))));
    assert!(!root.join("marker").exists());
    let _ = std::fs::remove_dir_all(root);
}
