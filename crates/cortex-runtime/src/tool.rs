//! Tool trait, definition contracts, schema validation, and tool registry.

use crate::agent::manager::AgentPermissions;
use crate::agent::CancellationToken;
use cortex_core::{CortexError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Security and authorization permission level required by a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionLevel {
    /// Read-only inspection (e.g. read_file, grep, git_status). Allowed without approval.
    #[default]
    ReadOnly,
    /// Workspace filesystem modification (e.g. write_file, edit_file, delete_file).
    WorkspaceWrite,
    /// Subprocess or command execution (e.g. bash, execute_command).
    Execute,
    /// High-risk or potentially destructive action.
    Danger,
}

impl std::fmt::Display for PermissionLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadOnly => write!(f, "read_only"),
            Self::WorkspaceWrite => write!(f, "workspace_write"),
            Self::Execute => write!(f, "execute"),
            Self::Danger => write!(f, "danger"),
        }
    }
}

/// Specification and metadata of an agent tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Unique name of the tool (e.g., "read_file", "shell").
    pub name: String,
    /// Human-readable description of the tool functionality.
    pub description: String,
    /// JSON schema describing expected input parameters.
    pub parameters: serde_json::Value,
    /// Permission level required to execute this tool.
    #[serde(default)]
    pub permission_level: PermissionLevel,
}

impl ToolDefinition {
    /// Create a new [`ToolDefinition`] with parameter schema.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            permission_level: PermissionLevel::ReadOnly,
        }
    }

    /// Attach a specific [`PermissionLevel`] to this tool definition.
    pub fn with_permission(mut self, permission_level: PermissionLevel) -> Self {
        self.permission_level = permission_level;
        self
    }
}

/// Result produced by executing a tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    /// Output content or message produced by the tool.
    pub output: String,
    /// Flag indicating whether the execution produced an error.
    pub is_error: bool,
}

impl ToolResult {
    /// Construct a successful [`ToolResult`].
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: false,
        }
    }

    /// Construct a failed [`ToolResult`].
    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: true,
        }
    }
}

/// Abstract contract for runtime tools.
///
/// All tools must pass through capability checks and runtime schema validation
/// before execution.
pub trait Tool: Send + Sync {
    /// Return the metadata definition of this tool.
    fn definition(&self) -> &ToolDefinition;

    /// Unique name of the tool.
    fn name(&self) -> &str {
        &self.definition().name
    }

    /// Permission level required to execute this tool.
    fn permission_level(&self) -> PermissionLevel {
        self.definition().permission_level
    }

    /// Check if the tool is supported on the current platform/runtime environment.
    fn is_available(&self) -> Result<bool> {
        Ok(true)
    }

    /// Format or pretty-print the execution result for terminal UI display.
    fn format_output(&self, result: &ToolResult) -> String {
        result.output.clone()
    }

    /// Execute the tool with validated JSON input arguments.
    fn execute(&self, input: &serde_json::Value) -> Result<ToolResult>;

    /// Execute the tool with validated JSON input arguments and an optional cooperative cancellation token.
    fn execute_with_cancellation(
        &self,
        input: &serde_json::Value,
        _cancellation_token: Option<&CancellationToken>,
    ) -> Result<ToolResult> {
        self.execute(input)
    }
}

/// Helper function to return human-readable type names of JSON values.
fn value_type_name(val: &serde_json::Value) -> &'static str {
    match val {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                "number"
            }
        }
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Validate input arguments against a JSON Schema specification.
///
/// Supports validation of:
/// - Top-level object structure
/// - Required fields
/// - Property types (`string`, `integer`, `number`, `boolean`, `array`, `object`)
pub fn validate_schema(schema: &serde_json::Value, input: &serde_json::Value) -> Result<()> {
    // If schema is not a JSON object or is empty, accept any input.
    let schema_obj = match schema.as_object() {
        Some(obj) if !obj.is_empty() => obj,
        _ => return Ok(()),
    };

    // If schema specifies "type", validate the input type.
    if let Some(expected_type) = schema_obj.get("type").and_then(|t| t.as_str()) {
        if expected_type == "object" && !input.is_object() {
            return Err(CortexError::Validation(format!(
                "expected JSON object, found {}",
                value_type_name(input)
            )));
        }
    }

    let input_obj = match input.as_object() {
        Some(obj) => obj,
        None => return Ok(()),
    };

    // Validate required fields.
    if let Some(required) = schema_obj.get("required").and_then(|r| r.as_array()) {
        for req in required {
            if let Some(field_name) = req.as_str() {
                if !input_obj.contains_key(field_name) || input_obj[field_name].is_null() {
                    return Err(CortexError::Validation(format!(
                        "missing required parameter: '{}'",
                        field_name
                    )));
                }
            }
        }
    }

    // Validate properties against schema property definitions.
    if let Some(properties) = schema_obj.get("properties").and_then(|p| p.as_object()) {
        for (prop_name, prop_val) in input_obj {
            if let Some(prop_schema) = properties.get(prop_name) {
                if let Some(expected_type) = prop_schema.get("type").and_then(|t| t.as_str()) {
                    let matches = match expected_type {
                        "string" => prop_val.is_string(),
                        "integer" => prop_val.is_i64() || prop_val.is_u64(),
                        "number" => prop_val.is_number(),
                        "boolean" => prop_val.is_boolean(),
                        "array" => prop_val.is_array(),
                        "object" => prop_val.is_object(),
                        _ => true,
                    };

                    if !matches {
                        return Err(CortexError::Validation(format!(
                            "parameter '{}' expected type '{}', found {}",
                            prop_name,
                            expected_type,
                            value_type_name(prop_val)
                        )));
                    }

                    // Recursively validate nested objects
                    if expected_type == "object" && prop_val.is_object() {
                        validate_schema(prop_schema, prop_val)?;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Thread-safe registry for agent tools.
///
/// Manages tool registration, lookup, enumeration, capability authorization, and schema-validated execution.
#[derive(Default)]
pub struct ToolRegistry {
    tools: RwLock<HashMap<String, Arc<dyn Tool>>>,
    permissions: RwLock<Option<AgentPermissions>>,
}

impl ToolRegistry {
    /// Create a new, empty [`ToolRegistry`].
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
            permissions: RwLock::new(None),
        }
    }

    /// Attach capability permissions to this tool registry.
    pub fn with_permissions(self, permissions: AgentPermissions) -> Self {
        if let Ok(mut p) = self.permissions.write() {
            *p = Some(permissions);
        }
        self
    }

    /// Update runtime capability permissions for this registry.
    pub fn set_permissions(&self, permissions: Option<AgentPermissions>) {
        if let Ok(mut p) = self.permissions.write() {
            *p = permissions;
        }
    }

    /// Retrieve the currently configured capability permissions.
    pub fn permissions(&self) -> Option<AgentPermissions> {
        self.permissions.read().ok().and_then(|p| p.clone())
    }

    /// Register a tool instance wrapped in an [`Arc`].
    ///
    /// Returns [`CortexError::Validation`] if a tool with the same name is already registered.
    pub fn register(&self, tool: Arc<dyn Tool>) -> Result<()> {
        let name = tool.name().to_string();
        let mut tools = self.tools.write().map_err(|e| {
            CortexError::Internal(format!("failed to acquire tool registry write lock: {}", e))
        })?;

        if tools.contains_key(&name) {
            return Err(CortexError::Validation(format!(
                "tool '{}' is already registered",
                name
            )));
        }

        tools.insert(name, tool);
        Ok(())
    }

    /// Convenience method to register a tool implementing [`Tool`].
    pub fn register_tool(&self, tool: impl Tool + 'static) -> Result<()> {
        self.register(Arc::new(tool))
    }

    /// Retrieve a reference to a registered tool by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.read().ok()?.get(name).cloned()
    }

    /// Check if a tool with the given name is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.tools
            .read()
            .map(|t| t.contains_key(name))
            .unwrap_or(false)
    }

    /// Return metadata definitions for all registered tools.
    pub fn list(&self) -> Vec<ToolDefinition> {
        self.tools
            .read()
            .map(|t| t.values().map(|tool| tool.definition().clone()).collect())
            .unwrap_or_default()
    }

    /// Return the count of registered tools.
    pub fn count(&self) -> usize {
        self.tools.read().map(|t| t.len()).unwrap_or(0)
    }

    /// Validate input arguments and execute the specified tool.
    ///
    /// Returns [`CortexError::NotFound`] if the tool is not registered.
    /// Returns [`CortexError::Validation`] if argument schema validation fails.
    /// Returns [`CortexError::PermissionDenied`] if capability boundaries are violated.
    pub fn execute(&self, name: &str, input: &serde_json::Value) -> Result<ToolResult> {
        self.execute_with_permissions(name, input, None, None)
    }

    /// Validate input arguments and execute the specified tool with an optional cooperative cancellation token.
    ///
    /// Returns [`CortexError::NotFound`] if the tool is not registered.
    /// Returns [`CortexError::Validation`] if argument schema validation fails.
    /// Returns [`CortexError::PermissionDenied`] if capability boundaries are violated.
    pub fn execute_with_cancellation(
        &self,
        name: &str,
        input: &serde_json::Value,
        cancellation_token: Option<&CancellationToken>,
    ) -> Result<ToolResult> {
        self.execute_with_permissions(name, input, None, cancellation_token)
    }

    /// Validate capability permissions, input schema, and execute the specified tool.
    ///
    /// Enforces capability boundaries (`AgentPermissions`) prior to tool execution.
    /// Returns [`CortexError::PermissionDenied`] if unpermitted capability operations are attempted.
    pub fn execute_with_permissions(
        &self,
        name: &str,
        input: &serde_json::Value,
        permissions: Option<&AgentPermissions>,
        cancellation_token: Option<&CancellationToken>,
    ) -> Result<ToolResult> {
        if cancellation_token.is_some_and(CancellationToken::is_cancelled) {
            return Err(CortexError::Cancelled(
                "tool execution cancelled by request".to_string(),
            ));
        }
        let tool = self
            .get(name)
            .ok_or_else(|| CortexError::NotFound(format!("tool '{}' not found", name)))?;

        if !tool.is_available()? {
            return Err(CortexError::Internal(format!(
                "tool '{}' is not available in current environment",
                name
            )));
        }

        // Enforce capability permissions if present
        let configured = self.permissions();
        let active_perms = permissions.or(configured.as_ref());
        if let Some(perms) = active_perms {
            perms.validate_tool_call(name, input, tool.permission_level())?;
        }

        // Validate arguments against tool parameters schema
        validate_schema(&tool.definition().parameters, input)?;

        // Execute tool
        tool.execute_with_cancellation(input, cancellation_token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    struct EchoTool {
        def: ToolDefinition,
    }

    impl EchoTool {
        fn new() -> Self {
            Self {
                def: ToolDefinition::new(
                    "echo",
                    "Echoes the provided message",
                    json!({
                        "type": "object",
                        "properties": {
                            "message": { "type": "string" },
                            "count": { "type": "integer" }
                        },
                        "required": ["message"]
                    }),
                ),
            }
        }
    }

    impl Tool for EchoTool {
        fn definition(&self) -> &ToolDefinition {
            &self.def
        }

        fn execute(&self, input: &serde_json::Value) -> Result<ToolResult> {
            let message = input["message"].as_str().unwrap_or_default();
            let count = input.get("count").and_then(|c| c.as_i64()).unwrap_or(1);
            let output = (0..count).map(|_| message).collect::<Vec<_>>().join(" ");
            Ok(ToolResult::success(output))
        }
    }

    struct FailingTool {
        def: ToolDefinition,
    }

    impl FailingTool {
        fn new() -> Self {
            Self {
                def: ToolDefinition::new("failing", "Always fails", json!({})),
            }
        }
    }

    impl Tool for FailingTool {
        fn definition(&self) -> &ToolDefinition {
            &self.def
        }

        fn execute(&self, _input: &serde_json::Value) -> Result<ToolResult> {
            Ok(ToolResult::error("execution failed deliberately"))
        }
    }

    #[test]
    fn test_register_and_execute_success() {
        let registry = ToolRegistry::new();
        assert_eq!(registry.count(), 0);

        registry.register_tool(EchoTool::new()).unwrap();
        assert_eq!(registry.count(), 1);
        assert!(registry.contains("echo"));

        let input = json!({
            "message": "hello",
            "count": 2
        });

        let result = registry.execute("echo", &input).unwrap();
        assert!(!result.is_error);
        assert_eq!(result.output, "hello hello");
    }

    #[test]
    fn test_duplicate_registration_rejected() {
        let registry = ToolRegistry::new();
        registry.register_tool(EchoTool::new()).unwrap();

        let err = registry.register_tool(EchoTool::new()).unwrap_err();
        match err {
            CortexError::Validation(msg) => {
                assert!(msg.contains("already registered"));
            }
            _ => panic!("expected Validation error"),
        }
    }

    #[test]
    fn test_unregistered_tool_returns_not_found() {
        let registry = ToolRegistry::new();
        let err = registry.execute("missing_tool", &json!({})).unwrap_err();
        match err {
            CortexError::NotFound(msg) => {
                assert!(msg.contains("tool 'missing_tool' not found"));
            }
            _ => panic!("expected NotFound error"),
        }
    }

    #[test]
    fn test_missing_required_parameter() {
        let registry = ToolRegistry::new();
        registry.register_tool(EchoTool::new()).unwrap();

        // Missing required "message" parameter
        let input = json!({ "count": 3 });
        let err = registry.execute("echo", &input).unwrap_err();
        match err {
            CortexError::Validation(msg) => {
                assert!(msg.contains("missing required parameter: 'message'"));
            }
            _ => panic!("expected Validation error"),
        }
    }

    #[test]
    fn test_parameter_type_mismatch() {
        let registry = ToolRegistry::new();
        registry.register_tool(EchoTool::new()).unwrap();

        // "count" expected integer, passed string
        let input = json!({
            "message": "test",
            "count": "three"
        });
        let err = registry.execute("echo", &input).unwrap_err();
        match err {
            CortexError::Validation(msg) => {
                assert!(msg.contains("parameter 'count' expected type 'integer', found string"));
            }
            _ => panic!("expected Validation error"),
        }
    }

    #[test]
    fn test_failing_tool_result() {
        let registry = ToolRegistry::new();
        registry.register_tool(FailingTool::new()).unwrap();

        let result = registry.execute("failing", &json!({})).unwrap();
        assert!(result.is_error);
        assert_eq!(result.output, "execution failed deliberately");
    }

    #[test]
    fn test_tool_definitions_list() {
        let registry = ToolRegistry::new();
        registry.register_tool(EchoTool::new()).unwrap();
        registry.register_tool(FailingTool::new()).unwrap();

        let list = registry.list();
        assert_eq!(list.len(), 2);
        let names: Vec<String> = list.into_iter().map(|d| d.name).collect();
        assert!(names.contains(&"echo".to_string()));
        assert!(names.contains(&"failing".to_string()));
    }

    #[test]
    fn test_thread_safe_registry_access() {
        let registry = Arc::new(ToolRegistry::new());
        registry.register_tool(EchoTool::new()).unwrap();

        let success_count = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();

        for i in 0..8 {
            let reg = Arc::clone(&registry);
            let counter = Arc::clone(&success_count);
            let handle = thread::spawn(move || {
                let input = json!({ "message": format!("thread-{}", i) });
                let res = reg.execute("echo", &input).unwrap();
                if !res.is_error && res.output == format!("thread-{}", i) {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(success_count.load(Ordering::SeqCst), 8);
    }

    #[test]
    fn test_tool_registry_permissions_enforcement() {
        let registry = ToolRegistry::new().with_permissions(AgentPermissions::read_only());
        registry.register_tool(EchoTool::new()).unwrap();

        let mut dangerous_def = ToolDefinition::new("rm_tool", "dangerous deletion", json!({}));
        dangerous_def.permission_level = PermissionLevel::WorkspaceWrite;

        struct DeleteTool {
            def: ToolDefinition,
        }
        impl Tool for DeleteTool {
            fn definition(&self) -> &ToolDefinition {
                &self.def
            }
            fn execute(&self, _input: &serde_json::Value) -> Result<ToolResult> {
                Ok(ToolResult::success("deleted"))
            }
        }

        registry
            .register_tool(DeleteTool { def: dangerous_def })
            .unwrap();

        // Echo is ReadOnly and permitted
        let res = registry
            .execute("echo", &json!({ "message": "hello" }))
            .unwrap();
        assert_eq!(res.output, "hello");

        // rm_tool is WorkspaceWrite, but permissions are read_only => PermissionDenied!
        let err = registry.execute("rm_tool", &json!({})).unwrap_err();
        match err {
            CortexError::PermissionDenied(msg) => {
                assert!(msg.contains("workspace write operations are not permitted"));
            }
            other => panic!("expected PermissionDenied, got {:?}", other),
        }
    }
}
