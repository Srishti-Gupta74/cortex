//! Comprehensive penetration testing and sandbox escape integration tests.
//!
//! Validates workspace containment, symlink escape prevention, null-byte rejection,
//! environment credential scrubbing, container network policy enforcement,
//! and adversarial process cancellation.

use cortex_core::CortexError;
use cortex_runtime::agent::CancellationToken;
use cortex_runtime::sandbox::{
    DockerSandbox, DockerSandboxConfig, HostSandbox, NetworkIsolationPolicy, Sandbox,
};
use cortex_runtime::tool::Tool;
use cortex_runtime::tools::ShellTool;
use cortex_runtime::Workspace;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn create_test_dirs(name: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!(
        "cortex_sec_test_{}_{}_{}",
        std::process::id(),
        name,
        chrono::Utc::now().timestamp_micros()
    ));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).expect("create test workspace directory");

    let outside = std::env::temp_dir().join(format!(
        "cortex_sec_outside_{}_{}_{}",
        std::process::id(),
        name,
        chrono::Utc::now().timestamp_micros()
    ));
    let _ = fs::remove_dir_all(&outside);
    fs::create_dir_all(&outside).expect("create test outside directory");

    (base, outside)
}

#[test]
fn test_penetration_relative_directory_traversal() {
    let (ws_path, outside_path) = create_test_dirs("rel_traversal");
    let ws = Workspace::new(&ws_path).expect("initialize workspace");
    let host_sb = HostSandbox::new(&ws_path);
    let docker_cfg = DockerSandboxConfig::new(&ws_path);
    let docker_sb = DockerSandbox::new(docker_cfg);

    let traversal_payloads = [
        "../../../../../../etc/passwd",
        "../../../../../../windows/system32/cmd.exe",
        "../outside",
        "src/../../outside/secret.txt",
        "nested/deep/../../../outside_dir",
        "nested/../../../../etc/shadow",
    ];

    for payload in traversal_payloads {
        // 1. Workspace resolve_path boundary check
        let ws_res = ws.resolve_path(payload);
        assert!(
            ws_res.is_err(),
            "Workspace must reject traversal payload '{payload}'"
        );
        assert!(
            matches!(ws_res.unwrap_err(), CortexError::PermissionDenied(_)),
            "Workspace traversal must return PermissionDenied for '{payload}'"
        );

        // 2. HostSandbox validate_path boundary check
        let host_res = host_sb.validate_path(Path::new(payload));
        assert!(
            host_res.is_err(),
            "HostSandbox must reject traversal payload '{payload}'"
        );
        assert!(
            matches!(host_res.unwrap_err(), CortexError::PermissionDenied(_)),
            "HostSandbox traversal must return PermissionDenied for '{payload}'"
        );

        // 3. DockerSandbox working directory boundary check
        let docker_res = docker_sb.build_docker_args("echo 1", Some(Path::new(payload)));
        assert!(
            docker_res.is_err(),
            "DockerSandbox must reject traversal working_dir '{payload}'"
        );
        assert!(
            matches!(docker_res.unwrap_err(), CortexError::PermissionDenied(_)),
            "DockerSandbox traversal must return PermissionDenied for '{payload}'"
        );
    }

    let _ = fs::remove_dir_all(&ws_path);
    let _ = fs::remove_dir_all(&outside_path);
}

#[test]
fn test_penetration_absolute_path_escape() {
    let (ws_path, outside_path) = create_test_dirs("abs_escape");
    let ws = Workspace::new(&ws_path).expect("initialize workspace");
    let host_sb = HostSandbox::new(&ws_path);
    let docker_cfg = DockerSandboxConfig::new(&ws_path);
    let docker_sb = DockerSandbox::new(docker_cfg);

    let outside_file = outside_path.join("sensitive_data.env");
    fs::write(&outside_file, "SECRET=compromised").expect("write outside file");

    // 1. Workspace resolve_path
    let ws_res = ws.resolve_path(&outside_file);
    assert!(
        ws_res.is_err(),
        "Workspace must reject absolute outside path"
    );
    assert!(matches!(
        ws_res.unwrap_err(),
        CortexError::PermissionDenied(_)
    ));

    // 2. HostSandbox validate_path
    let host_res = host_sb.validate_path(&outside_file);
    assert!(
        host_res.is_err(),
        "HostSandbox must reject absolute outside path"
    );
    assert!(matches!(
        host_res.unwrap_err(),
        CortexError::PermissionDenied(_)
    ));

    // 3. DockerSandbox working directory
    let docker_res = docker_sb.build_docker_args("cat secret", Some(&outside_file));
    assert!(
        docker_res.is_err(),
        "DockerSandbox must reject absolute outside workdir"
    );
    assert!(matches!(
        docker_res.unwrap_err(),
        CortexError::PermissionDenied(_)
    ));

    let _ = fs::remove_dir_all(&ws_path);
    let _ = fs::remove_dir_all(&outside_path);
}

#[test]
fn test_penetration_null_byte_injection() {
    let (ws_path, outside_path) = create_test_dirs("null_byte");
    let ws = Workspace::new(&ws_path).expect("initialize workspace");
    let host_sb = HostSandbox::new(&ws_path);
    let docker_cfg = DockerSandboxConfig::new(&ws_path);
    let docker_sb = DockerSandbox::new(docker_cfg);

    let null_payloads = [
        "sub/file.txt\0/../../etc/passwd",
        "secret.txt\0",
        "\0/root/data",
        "valid_dir\0../../outside",
    ];

    for payload in null_payloads {
        let ws_res = ws.resolve_path(payload);
        assert!(
            ws_res.is_err(),
            "Workspace must reject null byte payload '{payload}'"
        );
        assert!(
            matches!(ws_res.unwrap_err(), CortexError::Validation(_)),
            "Expected Validation error for null byte in Workspace"
        );

        let host_res = host_sb.validate_path(Path::new(payload));
        assert!(
            host_res.is_err(),
            "HostSandbox must reject null byte payload '{payload}'"
        );
        assert!(
            matches!(host_res.unwrap_err(), CortexError::Validation(_)),
            "Expected Validation error for null byte in HostSandbox"
        );

        let docker_res = docker_sb.build_docker_args("echo 1", Some(Path::new(payload)));
        assert!(
            docker_res.is_err(),
            "DockerSandbox must reject null byte payload '{payload}'"
        );
        assert!(
            matches!(docker_res.unwrap_err(), CortexError::Validation(_)),
            "Expected Validation error for null byte in DockerSandbox"
        );
    }

    let _ = fs::remove_dir_all(&ws_path);
    let _ = fs::remove_dir_all(&outside_path);
}

#[test]
fn test_penetration_symlink_breakout() {
    let (ws_path, outside_path) = create_test_dirs("symlink_breakout");
    let ws = Workspace::new(&ws_path).expect("initialize workspace");
    let host_sb = HostSandbox::new(&ws_path);

    let target_outside = outside_path.join("host_credentials.txt");
    fs::write(&target_outside, "HOST_SECRET_TOKEN=xyz123").expect("write target");

    let link_path = ws_path.join("escaped_symlink.txt");

    let mut symlink_created = false;
    #[cfg(unix)]
    {
        if std::os::unix::fs::symlink(&target_outside, &link_path).is_ok() {
            symlink_created = true;
        }
    }
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_file(&target_outside, &link_path).is_ok() {
            symlink_created = true;
        }
    }

    if symlink_created {
        // Resolving through symlink pointing outside must be caught
        let ws_res = ws.resolve_path("escaped_symlink.txt");
        assert!(
            ws_res.is_err(),
            "Workspace must reject symlink escaping root"
        );
        assert!(matches!(
            ws_res.unwrap_err(),
            CortexError::PermissionDenied(_)
        ));

        let host_res = host_sb.validate_path(Path::new("escaped_symlink.txt"));
        assert!(
            host_res.is_err(),
            "HostSandbox must reject symlink escaping root"
        );
        assert!(matches!(
            host_res.unwrap_err(),
            CortexError::PermissionDenied(_)
        ));
    }

    let _ = fs::remove_dir_all(&ws_path);
    let _ = fs::remove_dir_all(&outside_path);
}

#[test]
fn test_penetration_unicode_and_normalization_containment() {
    let (ws_path, outside_path) = create_test_dirs("unicode_containment");
    let ws = Workspace::new(&ws_path).expect("initialize workspace");

    // Legitimate Unicode paths inside workspace resolve correctly
    let sub = ws_path.join("📁_директория").join("日本語_ノート");
    fs::create_dir_all(&sub).expect("create unicode nested dir");

    let resolved = ws
        .resolve_path("📁_директория/日本語_ノート")
        .expect("valid unicode inside workspace should resolve");
    assert!(resolved.starts_with(ws.root()));

    // Unicode path traversal attempt using fullwidth dots/slashes or dot-combinations
    let res = ws.resolve_path("📁_директория/日本語_ノート/../../../../outside");
    assert!(
        res.is_err(),
        "Traversing out via nested unicode folder must fail"
    );
    assert!(matches!(res.unwrap_err(), CortexError::PermissionDenied(_)));

    let _ = fs::remove_dir_all(&ws_path);
    let _ = fs::remove_dir_all(&outside_path);
}

#[test]
fn test_penetration_env_scrubbing_shell_tool() {
    let (ws_path, _) = create_test_dirs("env_scrub_shell");
    let ws = Arc::new(Workspace::new(&ws_path).expect("initialize workspace"));
    let tool = ShellTool::new(ws);

    // Set sensitive host environment variables
    let canary_aws = "CANARY_AWS_SECRET_KEY_98765";
    let canary_aws_session = "CANARY_AWS_SESSION_TOKEN_98765";
    let canary_openai = "CANARY_OPENAI_KEY_98765";
    let canary_anthropic = "CANARY_ANTHROPIC_KEY_98765";
    let canary_github = "CANARY_GITHUB_TOKEN_98765";
    let canary_gh = "CANARY_GH_TOKEN_98765";
    let canary_ssh_sock = "/tmp/canary_ssh_auth_sock_98765";
    let canary_ssh_pid = "CANARY_SSH_AGENT_PID_98765";
    let canary_cortex = "CANARY_CORTEX_API_KEY_98765";

    unsafe {
        std::env::set_var("AWS_SECRET_ACCESS_KEY", canary_aws);
        std::env::set_var("AWS_SESSION_TOKEN", canary_aws_session);
        std::env::set_var("OPENAI_API_KEY", canary_openai);
        std::env::set_var("ANTHROPIC_API_KEY", canary_anthropic);
        std::env::set_var("GITHUB_TOKEN", canary_github);
        std::env::set_var("GH_TOKEN", canary_gh);
        std::env::set_var("SSH_AUTH_SOCK", canary_ssh_sock);
        std::env::set_var("SSH_AGENT_PID", canary_ssh_pid);
        std::env::set_var("CORTEX_API_KEY", canary_cortex);
    }

    #[cfg(windows)]
    let cmd_str = "echo AWS=%AWS_SECRET_ACCESS_KEY% AWS_SESS=%AWS_SESSION_TOKEN% OPENAI=%OPENAI_API_KEY% ANTHROPIC=%ANTHROPIC_API_KEY% GITHUB=%GITHUB_TOKEN% GH=%GH_TOKEN% SSH_SOCK=%SSH_AUTH_SOCK% SSH_PID=%SSH_AGENT_PID% CORTEX=%CORTEX_API_KEY%";

    #[cfg(not(windows))]
    let cmd_str = "echo AWS=$AWS_SECRET_ACCESS_KEY AWS_SESS=$AWS_SESSION_TOKEN OPENAI=$OPENAI_API_KEY ANTHROPIC=$ANTHROPIC_API_KEY GITHUB=$GITHUB_TOKEN GH=$GH_TOKEN SSH_SOCK=$SSH_AUTH_SOCK SSH_PID=$SSH_AGENT_PID CORTEX=$CORTEX_API_KEY";

    let input = json!({ "command": cmd_str });
    let result = tool.execute(&input).expect("execute shell tool");
    let output = result.output.as_str();

    // Verify none of the canary secret tokens leaked into shell output
    assert!(
        !output.contains(canary_aws),
        "AWS_SECRET_ACCESS_KEY must not leak into shell"
    );
    assert!(
        !output.contains(canary_aws_session),
        "AWS_SESSION_TOKEN must not leak into shell"
    );
    assert!(
        !output.contains(canary_openai),
        "OPENAI_API_KEY must not leak into shell"
    );
    assert!(
        !output.contains(canary_anthropic),
        "ANTHROPIC_API_KEY must not leak into shell"
    );
    assert!(
        !output.contains(canary_github),
        "GITHUB_TOKEN must not leak into shell"
    );
    assert!(
        !output.contains(canary_gh),
        "GH_TOKEN must not leak into shell"
    );
    assert!(
        !output.contains(canary_ssh_sock),
        "SSH_AUTH_SOCK must not leak into shell"
    );
    assert!(
        !output.contains(canary_ssh_pid),
        "SSH_AGENT_PID must not leak into shell"
    );
    assert!(
        !output.contains(canary_cortex),
        "CORTEX_API_KEY must not leak into shell"
    );

    let _ = fs::remove_dir_all(&ws_path);
}

#[test]
fn test_penetration_env_scrubbing_host_sandbox() {
    let (ws_path, _) = create_test_dirs("env_scrub_host_sb");
    let host_sb = HostSandbox::new(&ws_path);

    let canary_key = "CANARY_SANDBOX_OPENAI_KEY_112233";
    let canary_ssh = "/tmp/canary_sandbox_ssh_112233";

    unsafe {
        std::env::set_var("OPENAI_API_KEY", canary_key);
        std::env::set_var("SSH_AUTH_SOCK", canary_ssh);
    }

    #[cfg(windows)]
    let cmd_str = "echo OPENAI=%OPENAI_API_KEY% SSH=%SSH_AUTH_SOCK%";

    #[cfg(not(windows))]
    let cmd_str = "echo OPENAI=$OPENAI_API_KEY SSH=$SSH_AUTH_SOCK";

    let res = host_sb
        .execute(cmd_str, None)
        .expect("execute host sandbox command");

    assert!(
        !res.stdout.contains(canary_key),
        "HostSandbox must scrub OPENAI_API_KEY"
    );
    assert!(
        !res.stdout.contains(canary_ssh),
        "HostSandbox must scrub SSH_AUTH_SOCK"
    );

    let _ = fs::remove_dir_all(&ws_path);
}

#[test]
fn test_penetration_docker_sandbox_network_isolation() {
    let (ws_path, _) = create_test_dirs("docker_net_isolation");

    // 1. Offline Mode: verify --network none is passed to docker run
    let cfg_offline =
        DockerSandboxConfig::new(&ws_path).with_network_policy(NetworkIsolationPolicy::Offline);
    let sb_offline = DockerSandbox::new(cfg_offline);
    let args_off = sb_offline
        .build_docker_args("curl -s https://example.com", None)
        .expect("build docker args offline");

    let net_idx = args_off
        .iter()
        .position(|a| a == "--network")
        .expect("--network arg present");
    assert_eq!(
        args_off[net_idx + 1],
        "none",
        "Offline policy must pass --network none"
    );

    // 2. Intranet-Only Mode: verify --network internal is passed
    let cfg_intra = DockerSandboxConfig::new(&ws_path)
        .with_network_policy(NetworkIsolationPolicy::IntranetOnly);
    let sb_intra = DockerSandbox::new(cfg_intra);
    let args_intra = sb_intra
        .build_docker_args("curl -s http://internal-service", None)
        .expect("build docker args intranet");

    let net_idx_intra = args_intra
        .iter()
        .position(|a| a == "--network")
        .expect("--network arg present");
    assert_eq!(
        args_intra[net_idx_intra + 1],
        "internal",
        "Intranet policy must pass --network internal"
    );

    // 3. Full Internet: verify --network bridge is passed
    let cfg_full = DockerSandboxConfig::new(&ws_path)
        .with_network_policy(NetworkIsolationPolicy::FullInternet);
    let sb_full = DockerSandbox::new(cfg_full);
    let args_full = sb_full
        .build_docker_args("curl -s https://api.openai.com", None)
        .expect("build docker args full internet");

    let net_idx_full = args_full
        .iter()
        .position(|a| a == "--network")
        .expect("--network arg present");
    assert_eq!(
        args_full[net_idx_full + 1],
        "bridge",
        "Full internet policy must pass --network bridge"
    );

    let _ = fs::remove_dir_all(&ws_path);
}

#[test]
fn test_penetration_docker_sandbox_resource_quotas_and_privileges() {
    let (ws_path, _) = create_test_dirs("docker_quotas");

    let cfg = DockerSandboxConfig::new(&ws_path)
        .with_image("debian:bookworm-slim")
        .with_memory_mb(256)
        .with_cpu_quota(0.5)
        .with_max_pids(30);

    let sb = DockerSandbox::new(cfg);
    let args = sb
        .build_docker_args("make build", None)
        .expect("build docker args");

    // 1. Fork bomb resistance via pids limit
    let pids_idx = args
        .iter()
        .position(|a| a == "--pids-limit")
        .expect("--pids-limit present");
    assert_eq!(
        args[pids_idx + 1],
        "30",
        "pids-limit must match configured max_pids"
    );

    // 2. Memory resource quota
    let mem_idx = args
        .iter()
        .position(|a| a == "--memory")
        .expect("--memory present");
    assert_eq!(
        args[mem_idx + 1],
        format!("{}b", 256 * 1024 * 1024),
        "memory limit must match configured bytes"
    );

    // 3. CPU quota
    let cpu_idx = args
        .iter()
        .position(|a| a == "--cpus")
        .expect("--cpus present");
    assert_eq!(
        args[cpu_idx + 1],
        "0.50",
        "cpu quota must match configured quota"
    );

    // 4. Capability dropping: drop all capabilities
    let cap_idx = args
        .iter()
        .position(|a| a == "--cap-drop")
        .expect("--cap-drop present");
    assert_eq!(args[cap_idx + 1], "ALL", "--cap-drop ALL must be set");

    // 5. Privilege escalation block: no-new-privileges
    let priv_idx = args
        .iter()
        .position(|a| a == "--security-opt")
        .expect("--security-opt present");
    assert_eq!(
        args[priv_idx + 1],
        "no-new-privileges:true",
        "no-new-privileges:true must be set"
    );

    // 6. Read-only root filesystem with ephemeral /tmp
    assert!(
        args.contains(&"--read-only".to_string()),
        "root filesystem must be mounted read-only"
    );
    assert!(
        args.contains(&"--tmpfs".to_string()),
        "tmpfs must be configured"
    );

    // 7. Strictly mounts host workspace into /workspace
    let mount_idx = args
        .iter()
        .position(|a| a == "-v")
        .expect("-v mount present");
    assert!(
        args[mount_idx + 1].ends_with(":/workspace:rw"),
        "workspace mount must be pinned to /workspace:rw"
    );

    let _ = fs::remove_dir_all(&ws_path);
}

#[test]
fn test_penetration_adversarial_process_cancellation_and_timeout() {
    let (ws_path, _) = create_test_dirs("adversarial_timeout");
    let ws = Arc::new(Workspace::new(&ws_path).expect("initialize workspace"));
    let tool = ShellTool::new(ws);

    let token = CancellationToken::new();

    // Launch a spinning / long-running command
    #[cfg(windows)]
    let command_str = "ping -n 30 127.0.0.1 > nul";

    #[cfg(not(windows))]
    let command_str = "sleep 30";

    let token_clone = token.clone();
    std::thread::spawn(move || {
        // Cancel the process after 60ms
        std::thread::sleep(std::time::Duration::from_millis(60));
        token_clone.cancel();
    });

    let input = json!({ "command": command_str });
    let start = std::time::Instant::now();
    let result = tool.execute_with_cancellation(&input, Some(&token));
    let elapsed = start.elapsed();

    assert!(
        result.is_err(),
        "Spinning process must be aborted upon cancellation"
    );
    let err = result.unwrap_err();
    assert!(
        matches!(err, CortexError::Cancelled(_)),
        "Expected CortexError::Cancelled, got: {:?}",
        err
    );

    // Process must have been killed promptly, well before the 30-second target
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "Cancelled command took too long to terminate ({:?})",
        elapsed
    );

    let _ = fs::remove_dir_all(&ws_path);
}
