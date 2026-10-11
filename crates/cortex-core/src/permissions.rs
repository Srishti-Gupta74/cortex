//! Security permissions, risk levels, human-in-the-loop approval policies, and dangerous operation interception.

use crate::{CortexError, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::IsTerminal;
use std::path::Path;
use std::str::FromStr;

/// Risk classification tier for tools and operations.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    /// Safe or read-only operations (e.g. `read_file`, `git status`, `git diff`).
    #[default]
    Low,
    /// Potentially destructive or irreversible operations (e.g. `git push`, file deletions, outbound network, package installations, shell commands outside workspace).
    High,
}

impl RiskLevel {
    /// Returns true if this operation tier is classified as high risk.
    pub fn is_high_risk(&self) -> bool {
        matches!(self, Self::High)
    }

    /// Returns true if this operation tier is classified as low risk.
    pub fn is_low_risk(&self) -> bool {
        matches!(self, Self::Low)
    }
}

/// Interactive approval mode governing how high-risk operations are authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalMode {
    /// Interactive CLI/TUI prompt asking the user for explicit authorization (yes/no/always allow for session).
    #[default]
    Prompt,
    /// Automatically reject high-risk operations with a permission error.
    Deny,
    /// Permit high-risk operations without interactive confirmation (for non-interactive headless or sandboxed automation).
    Allow,
}

impl ApprovalMode {
    /// Returns the canonical string representation of this approval mode.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Deny => "deny",
            Self::Allow => "allow",
        }
    }
}

impl fmt::Display for ApprovalMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for ApprovalMode {
    type Err = CortexError;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "prompt" | "ask" | "confirm" => Ok(Self::Prompt),
            "deny" | "reject" | "block" => Ok(Self::Deny),
            "allow" | "auto" | "accept" | "yolo" => Ok(Self::Allow),
            other => Err(CortexError::Validation(format!(
                "invalid approval mode '{}'; expected 'prompt', 'deny', or 'allow'",
                other
            ))),
        }
    }
}

/// Operator authorization decision for a proposed high-risk tool execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Authorize this specific operation one time.
    AllowOnce,
    /// Authorize this operation and all subsequent high-risk operations for the duration of this session.
    AllowAlways,
    /// Reject this operation.
    Deny,
}

impl ApprovalDecision {
    /// Returns true if the decision permits execution.
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::AllowOnce | Self::AllowAlways)
    }
}

impl fmt::Display for ApprovalDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AllowOnce => write!(f, "allow_once"),
            Self::AllowAlways => write!(f, "allow_always"),
            Self::Deny => write!(f, "deny"),
        }
    }
}

impl FromStr for ApprovalDecision {
    type Err = CortexError;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "y" | "yes" | "allow" | "allow_once" | "once" | "ok" => Ok(Self::AllowOnce),
            "a" | "always" | "allow_always" | "all" => Ok(Self::AllowAlways),
            "n" | "no" | "deny" | "reject" => Ok(Self::Deny),
            other => Err(CortexError::Validation(format!(
                "invalid approval decision '{}'; expected 'yes', 'no', or 'always'",
                other
            ))),
        }
    }
}

/// Metadata describing an intercepted high-risk operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HighRiskOperation {
    /// Name of the proposed tool (e.g. `shell`, `git_push`, `delete_file`).
    pub tool_name: String,
    /// Classification category (e.g. "git_push", "file_deletion", "network_access", "package_installation", "outside_workspace").
    pub category: String,
    /// Human-readable explanation of why this operation is high risk.
    pub reason: String,
    /// Detailed description or parameters preview of the operation.
    pub description: String,
}

impl HighRiskOperation {
    /// Create a new high-risk operation descriptor.
    pub fn new(
        tool_name: impl Into<String>,
        category: impl Into<String>,
        reason: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            category: category.into(),
            reason: reason.into(),
            description: description.into(),
        }
    }
}

/// Check whether the current process standard input is connected to an interactive terminal (TTY).
pub fn is_interactive_terminal() -> bool {
    std::io::stdin().is_terminal()
}

/// Classify a proposed tool execution into a risk tier based on tool name, arguments, and workspace boundary.
///
/// High-risk classifications include:
/// 1. `git push` operations
/// 2. File deletions
/// 3. Outbound network requests
/// 4. Arbitrary package installations (e.g. `pip`, `npm`, `cargo`)
/// 5. Shell commands targeting paths outside the assigned workspace
///
/// Low-risk classifications include:
/// - Read-only inspection tools (e.g. `read_file`, `git status`, `git diff`, `list_dir`)
pub fn classify_operation(
    tool_name: &str,
    arguments: &serde_json::Value,
    workspace_root: Option<&Path>,
) -> (RiskLevel, Option<HighRiskOperation>) {
    let lower_tool = tool_name.to_lowercase();

    // 1. Git push detection
    if lower_tool == "git_push" {
        return (
            RiskLevel::High,
            Some(HighRiskOperation::new(
                tool_name,
                "git_push",
                "attempting to push git commits to remote repository",
                format!("tool: {}, args: {}", tool_name, arguments),
            )),
        );
    }
    if lower_tool == "git" {
        if let Some(op) = arguments.get("op").and_then(|v| v.as_str()) {
            if op.eq_ignore_ascii_case("push") {
                return (
                    RiskLevel::High,
                    Some(HighRiskOperation::new(
                        tool_name,
                        "git_push",
                        "attempting to push git commits to remote repository",
                        format!("git op: push, args: {}", arguments),
                    )),
                );
            }
        }
        if let Some(cmd) = arguments.get("command").and_then(|v| v.as_str()) {
            if cmd.contains("push") {
                return (
                    RiskLevel::High,
                    Some(HighRiskOperation::new(
                        tool_name,
                        "git_push",
                        "attempting to push git commits to remote repository",
                        format!("git command: {}", cmd),
                    )),
                );
            }
        }
    }

    // 2. File deletion detection
    if lower_tool == "delete_file"
        || lower_tool == "remove_file"
        || lower_tool == "rm"
        || lower_tool == "unlink"
    {
        let path_str = arguments
            .get("path")
            .or_else(|| arguments.get("file"))
            .and_then(|p| p.as_str())
            .unwrap_or("<unknown>");
        return (
            RiskLevel::High,
            Some(HighRiskOperation::new(
                tool_name,
                "file_deletion",
                "attempting to delete file or directory",
                format!("target path: {}", path_str),
            )),
        );
    }

    // 3. Outbound network request detection
    if lower_tool.contains("network")
        || lower_tool.contains("http")
        || lower_tool.contains("fetch")
        || lower_tool.contains("curl")
        || lower_tool.contains("download")
        || lower_tool.contains("web_search")
    {
        let dest = arguments
            .get("url")
            .or_else(|| arguments.get("destination"))
            .or_else(|| arguments.get("query"))
            .and_then(|u| u.as_str())
            .unwrap_or("<unspecified network destination>");
        return (
            RiskLevel::High,
            Some(HighRiskOperation::new(
                tool_name,
                "network_access",
                "attempting outbound network connection or web request",
                format!("destination: {}", dest),
            )),
        );
    }

    if let Some(url) = arguments.get("url").and_then(|u| u.as_str()) {
        if !url.trim().is_empty() {
            return (
                RiskLevel::High,
                Some(HighRiskOperation::new(
                    tool_name,
                    "network_access",
                    "attempting outbound network connection via URL argument",
                    format!("url: {}", url),
                )),
            );
        }
    }

    // 4. Shell / Subprocess inspection
    if lower_tool == "shell"
        || lower_tool == "bash"
        || lower_tool == "execute_command"
        || lower_tool == "exec"
    {
        if let Some(cmd) = arguments.get("command").and_then(|c| c.as_str()) {
            // Check for git push in shell
            if is_git_push_command(cmd) {
                return (
                    RiskLevel::High,
                    Some(HighRiskOperation::new(
                        tool_name,
                        "git_push",
                        "shell command attempts to push git commits to remote repository",
                        cmd,
                    )),
                );
            }

            // Check for file deletion in shell
            if is_deletion_command(cmd) {
                return (
                    RiskLevel::High,
                    Some(HighRiskOperation::new(
                        tool_name,
                        "file_deletion",
                        "shell command attempts to delete files or directories",
                        cmd,
                    )),
                );
            }

            // Check for outbound network in shell
            if is_network_command(cmd) {
                return (
                    RiskLevel::High,
                    Some(HighRiskOperation::new(
                        tool_name,
                        "network_access",
                        "shell command invokes network transfer utility",
                        cmd,
                    )),
                );
            }

            // Check for package installation in shell
            if is_package_install_command(cmd) {
                return (
                    RiskLevel::High,
                    Some(HighRiskOperation::new(
                        tool_name,
                        "package_installation",
                        "shell command attempts package installation",
                        cmd,
                    )),
                );
            }

            // Check for paths outside workspace in shell
            if is_outside_workspace_command(cmd, workspace_root) {
                return (
                    RiskLevel::High,
                    Some(HighRiskOperation::new(
                        tool_name,
                        "outside_workspace",
                        "shell command targets paths outside workspace boundary",
                        cmd,
                    )),
                );
            }
        }
    }

    // Check path arguments in general tools for traversal or outside workspace
    if let Some(path_val) = arguments
        .get("path")
        .or_else(|| arguments.get("file"))
        .or_else(|| arguments.get("target"))
        .and_then(|p| p.as_str())
    {
        if path_val.contains("..") {
            return (
                RiskLevel::High,
                Some(HighRiskOperation::new(
                    tool_name,
                    "outside_workspace",
                    "path parameter attempts parent directory traversal ('..')",
                    format!("path: {}", path_val),
                )),
            );
        }
        if let Some(ws) = workspace_root {
            let p = Path::new(path_val);
            if p.is_absolute() && !p.starts_with(ws) {
                return (
                    RiskLevel::High,
                    Some(HighRiskOperation::new(
                        tool_name,
                        "outside_workspace",
                        "path parameter targets location outside designated workspace root",
                        format!("path: {}, workspace: {}", path_val, ws.display()),
                    )),
                );
            }
        }
    }

    // Check low-risk inspection operations
    if matches!(
        lower_tool.as_str(),
        "read_file"
            | "list_dir"
            | "list_directory"
            | "git_status"
            | "git_diff"
            | "git_log"
            | "grep"
            | "file_search"
            | "search_files"
    ) {
        return (RiskLevel::Low, None);
    }

    (RiskLevel::Low, None)
}

fn is_git_push_command(cmd: &str) -> bool {
    let lower = cmd.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    for window in words.windows(2) {
        if window[0] == "git" && window[1] == "push" {
            return true;
        }
    }
    lower.contains("git push") || lower.contains("git-push")
}

fn is_deletion_command(cmd: &str) -> bool {
    let lower = cmd.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    for word in &words {
        let clean = word.trim_matches(&[';', '&', '|', '(', ')'][..]);
        if clean == "rm"
            || clean == "rmdir"
            || clean == "del"
            || clean == "erase"
            || clean == "shred"
        {
            return true;
        }
        if clean == "remove-item" || clean == "ri" {
            return true;
        }
    }
    false
}

fn is_network_command(cmd: &str) -> bool {
    let lower = cmd.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    for word in &words {
        let clean = word.trim_matches(&[';', '&', '|', '(', ')'][..]);
        if matches!(
            clean,
            "curl"
                | "wget"
                | "ssh"
                | "scp"
                | "sftp"
                | "rsync"
                | "nc"
                | "ncat"
                | "netcat"
                | "telnet"
                | "ftp"
        ) {
            return true;
        }
    }
    for window in words.windows(2) {
        if window[0] == "git" && matches!(window[1], "clone" | "fetch" | "pull") {
            return true;
        }
    }
    false
}

fn is_package_install_command(cmd: &str) -> bool {
    let lower = cmd.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();

    for window in words.windows(2) {
        let w0 = window[0].trim_matches(&[';', '&', '|'][..]);
        let w1 = window[1].trim_matches(&[';', '&', '|'][..]);

        if (w0 == "pip" || w0 == "pip3" || w0 == "pipx") && w1 == "install" {
            return true;
        }
        if (w0 == "npm" || w0 == "yarn" || w0 == "pnpm" || w0 == "bun")
            && (w1 == "install" || w1 == "i" || w1 == "add")
        {
            return true;
        }
        if w0 == "cargo" && (w1 == "install" || w1 == "add") {
            return true;
        }
        if w0 == "gem" && w1 == "install" {
            return true;
        }
        if (w0 == "apt"
            || w0 == "apt-get"
            || w0 == "dnf"
            || w0 == "yum"
            || w0 == "brew"
            || w0 == "choco"
            || w0 == "winget")
            && w1 == "install"
        {
            return true;
        }
        if w0 == "poetry" && w1 == "add" {
            return true;
        }
    }

    if words
        .iter()
        .any(|w| w.trim_matches(&[';', '&', '|'][..]) == "npx")
    {
        return true;
    }

    false
}

fn is_outside_workspace_command(cmd: &str, workspace_root: Option<&Path>) -> bool {
    if cmd.contains("..") {
        return true;
    }

    if let Some(ws) = workspace_root {
        let ws_str = ws.to_string_lossy().replace('\\', "/");
        let ws_clean = ws_str.trim_end_matches('/');

        let lower = cmd.to_lowercase();
        // Check Unix paths like /etc, /root, /var, /usr
        let sensitive_unix_roots = ["/etc", "/root", "/var", "/usr", "/opt", "/sys", "/proc"];
        for prefix in sensitive_unix_roots {
            if lower.contains(prefix) && !ws_clean.to_lowercase().starts_with(prefix) {
                return true;
            }
        }

        // Check Windows drive root targets like C:\Windows
        let sensitive_win_roots = [
            "c:/windows",
            "c:\\windows",
            "c:/program files",
            "c:\\program files",
        ];
        for prefix in sensitive_win_roots {
            if lower.contains(prefix) && !ws_clean.to_lowercase().starts_with(prefix) {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_risk_level_methods() {
        assert!(RiskLevel::High.is_high_risk());
        assert!(!RiskLevel::High.is_low_risk());
        assert!(RiskLevel::Low.is_low_risk());
        assert!(!RiskLevel::Low.is_high_risk());
    }

    #[test]
    fn test_approval_mode_parsing_and_display() {
        assert_eq!(ApprovalMode::Prompt.as_str(), "prompt");
        assert_eq!(ApprovalMode::Deny.as_str(), "deny");
        assert_eq!(ApprovalMode::Allow.as_str(), "allow");

        assert_eq!(
            "prompt".parse::<ApprovalMode>().unwrap(),
            ApprovalMode::Prompt
        );
        assert_eq!("deny".parse::<ApprovalMode>().unwrap(), ApprovalMode::Deny);
        assert_eq!(
            "allow".parse::<ApprovalMode>().unwrap(),
            ApprovalMode::Allow
        );
        assert_eq!("yolo".parse::<ApprovalMode>().unwrap(), ApprovalMode::Allow);

        assert!("invalid".parse::<ApprovalMode>().is_err());
    }

    #[test]
    fn test_approval_decision_parsing_and_display() {
        assert_eq!(
            "yes".parse::<ApprovalDecision>().unwrap(),
            ApprovalDecision::AllowOnce
        );
        assert_eq!(
            "y".parse::<ApprovalDecision>().unwrap(),
            ApprovalDecision::AllowOnce
        );
        assert_eq!(
            "always".parse::<ApprovalDecision>().unwrap(),
            ApprovalDecision::AllowAlways
        );
        assert_eq!(
            "a".parse::<ApprovalDecision>().unwrap(),
            ApprovalDecision::AllowAlways
        );
        assert_eq!(
            "no".parse::<ApprovalDecision>().unwrap(),
            ApprovalDecision::Deny
        );
        assert_eq!(
            "n".parse::<ApprovalDecision>().unwrap(),
            ApprovalDecision::Deny
        );

        assert!(ApprovalDecision::AllowOnce.is_allowed());
        assert!(ApprovalDecision::AllowAlways.is_allowed());
        assert!(!ApprovalDecision::Deny.is_allowed());
    }

    #[test]
    fn test_classify_low_risk_operations() {
        let (risk, op) = classify_operation("read_file", &json!({"path": "src/main.rs"}), None);
        assert_eq!(risk, RiskLevel::Low);
        assert!(op.is_none());

        let (risk, op) = classify_operation("git_status", &json!({}), None);
        assert_eq!(risk, RiskLevel::Low);
        assert!(op.is_none());

        let (risk, op) = classify_operation("git_diff", &json!({}), None);
        assert_eq!(risk, RiskLevel::Low);
        assert!(op.is_none());
    }

    #[test]
    fn test_classify_git_push() {
        let (risk, op) = classify_operation("git_push", &json!({}), None);
        assert_eq!(risk, RiskLevel::High);
        assert_eq!(op.unwrap().category, "git_push");

        let (risk, op) = classify_operation("git", &json!({"op": "push"}), None);
        assert_eq!(risk, RiskLevel::High);
        assert_eq!(op.unwrap().category, "git_push");

        let (risk, op) =
            classify_operation("shell", &json!({"command": "git push origin main"}), None);
        assert_eq!(risk, RiskLevel::High);
        assert_eq!(op.unwrap().category, "git_push");
    }

    #[test]
    fn test_classify_file_deletions() {
        let (risk, op) = classify_operation("delete_file", &json!({"path": "temp.txt"}), None);
        assert_eq!(risk, RiskLevel::High);
        assert_eq!(op.unwrap().category, "file_deletion");

        let (risk, op) = classify_operation("shell", &json!({"command": "rm -rf build/"}), None);
        assert_eq!(risk, RiskLevel::High);
        assert_eq!(op.unwrap().category, "file_deletion");

        let (risk, op) =
            classify_operation("shell", &json!({"command": "del target\\temp.log"}), None);
        assert_eq!(risk, RiskLevel::High);
        assert_eq!(op.unwrap().category, "file_deletion");
    }

    #[test]
    fn test_classify_network_requests() {
        let (risk, op) = classify_operation("curl", &json!({"url": "https://example.com"}), None);
        assert_eq!(risk, RiskLevel::High);
        assert_eq!(op.unwrap().category, "network_access");

        let (risk, op) = classify_operation(
            "shell",
            &json!({"command": "curl -O https://example.com/data.zip"}),
            None,
        );
        assert_eq!(risk, RiskLevel::High);
        assert_eq!(op.unwrap().category, "network_access");

        let (risk, op) = classify_operation(
            "shell",
            &json!({"command": "git clone https://github.com/foo/bar.git"}),
            None,
        );
        assert_eq!(risk, RiskLevel::High);
        assert_eq!(op.unwrap().category, "network_access");
    }

    #[test]
    fn test_classify_package_installations() {
        let (risk, op) =
            classify_operation("shell", &json!({"command": "pip install requests"}), None);
        assert_eq!(risk, RiskLevel::High);
        assert_eq!(op.unwrap().category, "package_installation");

        let (risk, op) =
            classify_operation("shell", &json!({"command": "npm install lodash"}), None);
        assert_eq!(risk, RiskLevel::High);
        assert_eq!(op.unwrap().category, "package_installation");

        let (risk, op) =
            classify_operation("shell", &json!({"command": "cargo install ripgrep"}), None);
        assert_eq!(risk, RiskLevel::High);
        assert_eq!(op.unwrap().category, "package_installation");
    }

    #[test]
    fn test_classify_outside_workspace_traversal() {
        let (risk, op) = classify_operation("read_file", &json!({"path": "../secret.txt"}), None);
        assert_eq!(risk, RiskLevel::High);
        assert_eq!(op.unwrap().category, "outside_workspace");

        let (risk, op) =
            classify_operation("shell", &json!({"command": "cat ../../etc/passwd"}), None);
        assert_eq!(risk, RiskLevel::High);
        assert_eq!(op.unwrap().category, "outside_workspace");

        let ws = Path::new("/workspace/project");
        let (risk, op) =
            classify_operation("shell", &json!({"command": "ls /etc/hosts"}), Some(ws));
        assert_eq!(risk, RiskLevel::High);
        assert_eq!(op.unwrap().category, "outside_workspace");
    }
}
