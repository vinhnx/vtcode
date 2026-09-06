#![allow(
    missing_docs,
    clippy::expect_used,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
use serde_json::json;
use std::fs;
use tempfile::TempDir;
use vtcode_config::CommandsConfig;
use vtcode_core::tools::ToolRegistry;

async fn temp_registry() -> (TempDir, ToolRegistry) {
    temp_registry_with_config(None).await
}

async fn temp_registry_with_config(vtcode_toml: Option<&str>) -> (TempDir, ToolRegistry) {
    let temp = TempDir::new().expect("temp workspace");
    fs::write(temp.path().join("Cargo.toml"), "[package]\nname = \"pty-test\"\n").expect("write fixture Cargo.toml");
    if let Some(config) = vtcode_toml {
        fs::write(temp.path().join("vtcode.toml"), config).expect("write fixture vtcode.toml");
    }
    let registry = ToolRegistry::new(temp.path().to_path_buf()).await;
    let _allow_result = registry.allow_all_tools().await.ok();
    (temp, registry)
}

#[cfg(unix)]
fn exec_session_id(response: &serde_json::Value) -> String {
    response
        .get("process_id")
        .and_then(|value| value.as_str())
        .or_else(|| response.get("session_id").and_then(|value| value.as_str()))
        .expect("session id should be present")
        .to_string()
}

#[cfg(unix)]
async fn read_session_until_exit(
    registry: &ToolRegistry,
    session_id: &str,
    attempts: usize,
    yield_time_ms: u64,
) -> (String, serde_json::Value) {
    let mut output = String::new();
    let mut last = json!({});

    for attempt in 0..attempts {
        let read = registry
            .execute_tool(
                "read_pty_session",
                json!({
                    "session_id": session_id,
                    "yield_time_ms": yield_time_ms + attempt as u64,
                }),
            )
            .await
            .expect("read pty session");

        output.push_str(read.get("output").and_then(|value| value.as_str()).unwrap_or_default());
        if read.get("is_exited").and_then(|value| value.as_bool()).unwrap_or(false) || read.get("error").is_some() {
            return (output, read);
        }
        last = read;
    }

    (output, last)
}

#[tokio::test]
async fn test_pty_functionality() {
    let (_temp, registry) = temp_registry().await;

    // Run an allow-listed command and verify output is captured
    let result = registry
        .execute_tool(
            "exec_command",
            json!({
                "cmd": "ls Cargo.toml",
                "tty": true,
            }),
        )
        .await;

    assert!(result.is_ok());
    let response = result.unwrap();

    assert_eq!(response["success"], true);
    let output = response["output"].as_str().unwrap_or_default();
    assert!(output.contains("Cargo.toml"));
    assert!(response["session_id"].as_str().is_some());
    assert!(response["command"].as_str().unwrap_or_default().contains("ls"));
    assert!(response["working_directory"].is_string() || response.get("working_directory").is_none());
    assert!(response["rows"].is_number());
    assert!(response["cols"].is_number());
    assert!(response["is_exited"].is_boolean());
}

#[tokio::test]
async fn test_pty_functionality_with_exit_code() {
    let (_temp, registry) = temp_registry().await;

    // Run an allow-listed command that exits with a non-zero code
    let result = registry
        .execute_tool(
            "exec_command",
            json!({
                "mode": "pty",
                "command": "ls",
                "args": ["this_file_does_not_exist"],
            }),
        )
        .await;

    assert!(result.is_ok());
    let response = result.unwrap();

    // The command should execute successfully (no error in execution)
    // but the exit code should be non-zero. Different `ls` implementations
    // use different codes for a missing operand/path.
    assert_eq!(response["success"], true);
    // Check for exit_code field (may be "code" or "exit_code" depending on implementation)
    let exit_code = response["exit_code"].as_i64().or_else(|| response["code"].as_i64());
    assert!(
        exit_code.is_some_and(|code| code != 0),
        "expected present non-zero exit code, got {exit_code:?}; response={response:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_pty_run_returns_live_session_after_yield_window() {
    let (_temp, registry) = temp_registry().await;

    let start = registry
        .execute_tool(
            "exec_command",
            json!({
                "mode": "pty",
                "command": ["bash", "-lc", "sleep 0.75; echo done"],
                "yield_time_ms": 0,
            }),
        )
        .await
        .expect("start sleep result");
    let sid = exec_session_id(&start);

    assert_eq!(start["success"], true);
    assert_eq!(start["is_exited"].as_bool(), Some(false));
    assert!(start.get("session_id").and_then(|value| value.as_str()).is_some());
    assert!(start.get("exit_code").is_none());

    let (output, read) = read_session_until_exit(&registry, sid.as_str(), 20, 250).await;

    assert_eq!(read["success"], true);
    let exit_code = read["exit_code"].as_i64().or_else(|| read["code"].as_i64());
    assert_eq!(exit_code, Some(0));
    assert!(read["is_exited"].as_bool().unwrap_or(false));
    assert!(output.contains("done"));
}

#[cfg(unix)]
#[tokio::test]
async fn test_pty_shell_option_runs_through_requested_shell() {
    let (_temp, registry) = temp_registry().await;

    let result = registry
        .execute_tool(
            "exec_command",
            json!({
                "mode": "pty",
                "shell": "sh",
                "command": "echo shell-check",
            }),
        )
        .await
        .expect("shell run result");

    assert_eq!(result["success"], true);
    let output = result["output"].as_str().unwrap_or_default();
    assert!(output.contains("shell-check"));
}

#[cfg(unix)]
#[tokio::test]
async fn test_exec_command_small_pipe_output_stays_inline_without_spool_reference() {
    let (_temp, registry) = temp_registry_with_config(Some(
        r#"[context.dynamic]
enabled = true
tool_output_threshold = 64
"#,
    ))
    .await;

    let result = registry
        .execute_tool(
            "exec_command",
            json!({
                "tty": false,
                "cmd": "printf 'small output\\n'",
            }),
        )
        .await
        .expect("small pipe command result");

    assert_eq!(result["success"], true);
    assert_eq!(result["output"].as_str(), Some("small output\n"));
    assert!(result.get("spool_path").is_none(), "small output leaked spool path: {result:?}");
    assert!(result.get("output_spooled").is_none(), "small output leaked spool marker: {result:?}");
    assert!(result.get("spool_complete").is_none(), "small output leaked spool lifecycle: {result:?}");
    assert!(result.get("spool_pending").is_none(), "small output leaked pending metadata: {result:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn test_create_pty_session_uses_requested_shell() {
    let (_temp, registry) = temp_registry().await;

    let create_result = registry
        .execute_tool(
            "create_pty_session",
            json!({
                "command": "echo $0",
                "shell": "/bin/sh",
            }),
        )
        .await
        .expect("create session result");

    assert_eq!(create_result["success"], true);
    let output = create_result["output"].as_str().unwrap_or_default().to_ascii_lowercase();
    assert!(output.contains("sh"));
}

#[tokio::test]
async fn test_pty_output_has_no_ansi_codes() {
    let (_temp, registry) = temp_registry().await;

    let result = registry
        .execute_tool(
            "exec_command",
            json!({
                "mode": "pty",
                "command": "ls",
                "args": ["-a"],
            }),
        )
        .await
        .expect("ls result");

    assert_eq!(result["success"], true);
    let output = result["output"].as_str().unwrap_or_default();

    // Check that output doesn't contain ANSI escape sequences
    assert!(!output.contains("\x1b["), "Output should not contain ANSI escape codes");
    assert!(!output.contains("\u{001b}["), "Output should not contain ANSI escape codes");

    // Verify we got actual file names
    assert!(
        output.contains("Cargo.toml") || output.contains("cargo") || output.len() > 10,
        "Output should contain actual filenames, not just escape codes"
    );
}

#[tokio::test]
async fn test_pty_command_not_found_handling() {
    let (_temp, registry) = temp_registry().await;
    // Keep the command-not-found assertion meaningful while still exercising
    // the shared command allowlist: explicitly allow this inert fixture name.
    let mut commands_config = CommandsConfig::default();
    commands_config
        .allow_list
        .push("this_command_definitely_does_not_exist_12345".to_string());
    registry.apply_commands_config(&commands_config);

    // Run a command that definitely doesn't exist
    let result = registry
        .execute_tool(
            "exec_command",
            json!({
                "mode": "pty",
                "shell": "/bin/bash",
                "command": "this_command_definitely_does_not_exist_12345",
                // Force a deterministic login shell to test robust extraction logic (shell -l -c ...)
                "login": true
            }),
        )
        .await;

    assert!(result.is_ok());
    let response = result.unwrap();

    assert_eq!(response["success"], true);
    // Check for exit_code field (may be "code" or "exit_code" depending on implementation)
    let exit_code = response["exit_code"].as_i64().or_else(|| response["code"].as_i64());
    assert_eq!(exit_code, Some(127));

    // Check that we have error information in message or output
    let message = response["message"].as_str().unwrap_or_default();
    let output = response["output"].as_str().unwrap_or_default();
    let combined = format!("{message} {output}").to_lowercase();

    // Should indicate command not found in some way
    assert!(
        combined.contains("not found")
            || combined.contains("not exist")
            || combined.contains("127")
            || output.contains("this_command_definitely_does_not_exist_12345"),
        "Should indicate command not found. message='{message}', output='{output}'"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_read_pty_session_includes_command_context_fields() {
    let (_temp, registry) = temp_registry().await;

    let start = registry
        .execute_tool(
            "exec_command",
            json!({
                "cmd": "sleep 1",
                "tty": true,
                "yield_time_ms": 10
            }),
        )
        .await
        .expect("start sleep command");

    let sid = exec_session_id(&start);

    let read = registry
        .execute_tool(
            "write_stdin",
            json!({
                "session_id": sid.as_str(),
                "chars": "",
                "yield_time_ms": 10
            }),
        )
        .await
        .expect("read pty session");

    assert_eq!(read["success"], true);
    assert_eq!(read["session_id"].as_str(), Some(sid.as_str()));
    assert!(read["command"].as_str().unwrap_or_default().contains("sleep 1"));
    assert!(read["working_directory"].is_string() || read.get("working_directory").is_none());
    assert!(read["rows"].is_number());
    assert!(read["cols"].is_number());
    assert!(read["is_exited"].is_boolean());

    let _closed = registry
        .execute_tool(
            "close_pty_session",
            json!({
                "session_id": sid
            }),
        )
        .await;
}

#[cfg(unix)]
#[tokio::test]
async fn test_inspect_does_not_drain_session_output() {
    let (_temp, registry) = temp_registry().await;

    let start = registry
        .execute_tool(
            "exec_command",
            json!({
                "cmd": "bash -lc 'sleep 0.4; printf \"<alpha>\\n\"; sleep 1'",
                "yield_time_ms": 0,
            }),
        )
        .await
        .expect("start delayed output command");

    let sid = exec_session_id(&start);

    let mut read = json!({});
    for attempt in 0..8 {
        read = registry
            .execute_tool(
                "write_stdin",
                json!({
                    "session_id": sid.as_str(),
                    "chars": "",
                    "yield_time_ms": 500 + (attempt * 250),
                }),
            )
            .await
            .expect("poll session output");
        if read["output"].as_str().unwrap_or_default().contains("<alpha>") {
            break;
        }
    }

    assert_eq!(read["success"], true);
    assert!(read["output"].as_str().unwrap_or_default().contains("<alpha>"));

    let _closed = registry
        .execute_tool(
            "close_pty_session",
            json!({
                "session_id": sid.as_str()
            }),
        )
        .await;
}

#[cfg(unix)]
#[tokio::test]
async fn test_exited_sessions_are_pruned_after_final_poll() {
    let (_temp, registry) = temp_registry().await;

    let start = registry
        .execute_tool(
            "exec_command",
            json!({
                "mode": "pty",
                "command": ["bash", "-lc", "sleep 0.4; echo done"],
                "yield_time_ms": 0,
            }),
        )
        .await
        .expect("start delayed exit command");

    let sid = exec_session_id(&start);

    let (_, read) = read_session_until_exit(&registry, sid.as_str(), 20, 200).await;

    assert_eq!(read["success"], true);
    assert_eq!(read["exit_code"].as_i64().or_else(|| read["code"].as_i64()), Some(0));
    assert_eq!(read["is_exited"].as_bool(), Some(true));

    let sessions = registry
        .execute_tool("list_pty_sessions", json!({}))
        .await
        .expect("list sessions");

    let active_sessions = sessions["sessions"].as_array().expect("sessions list should be an array");
    assert!(
        !active_sessions
            .iter()
            .any(|session| { session.get("id").and_then(|value| value.as_str()) == Some(sid.as_str()) })
    );

    let reread = registry
        .execute_tool(
            "read_pty_session",
            json!({
                "session_id": sid.as_str(),
            }),
        )
        .await
        .expect("re-read should return a structured error");
    assert!(reread.get("error").is_some());

    let close = registry
        .execute_tool(
            "close_pty_session",
            json!({
                "session_id": sid.as_str(),
            }),
        )
        .await
        .expect("close after prune should return a structured error");
    assert!(close.get("error").is_some());
}

#[cfg(unix)]
#[tokio::test]
async fn test_exec_command_write_preserves_whitespace() {
    let (_temp, registry) = temp_registry().await;

    let start = registry
        .execute_tool(
            "exec_command",
            json!({
                "mode": "pty",
                "command": ["bash", "-lc", "IFS= read -r line; printf '<%s>' \"$line\""],
                "yield_time_ms": 0,
            }),
        )
        .await
        .expect("start read session");
    let sid = exec_session_id(&start);

    let write = registry
        .execute_tool(
            "send_pty_input",
            json!({
                "session_id": sid.as_str(),
                "chars": "  keep  \n",
                "yield_time_ms": 50,
            }),
        )
        .await
        .expect("write exact bytes");

    assert_eq!(write["success"], true);
    let (tail_output, final_read) = if write["is_exited"].as_bool().unwrap_or(false) {
        (String::new(), write.clone())
    } else {
        read_session_until_exit(&registry, sid.as_str(), 8, 100).await
    };
    assert_eq!(final_read["success"], true);
    let combined_output = format!("{}{}", write["output"].as_str().unwrap_or_default(), tail_output);
    assert!(combined_output.contains("<  keep  >"), "combined output was: {combined_output:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn test_exec_command_write_stdin_continues_session() {
    let (_temp, registry) = temp_registry().await;

    let start = registry
        .execute_tool(
            "exec_command",
            json!({
                "cmd": "cat",
                "yield_time_ms": 0,
            }),
        )
        .await
        .expect("start public exec command session");
    let sid = exec_session_id(&start);

    let write = registry
        .execute_tool(
            "write_stdin",
            json!({
                "session_id": sid.as_str(),
                "chars": "  keep  \n",
                "yield_time_ms": 250,
            }),
        )
        .await
        .expect("continue public exec command session");

    assert_eq!(write["success"], true);
    assert_eq!(write["session_id"].as_str(), Some(sid.as_str()));
    assert_eq!(write["is_exited"].as_bool(), Some(false));
    assert!(
        write["output"].as_str().unwrap_or_default().contains("  keep  "),
        "write_stdin output was: {write:?}"
    );

    let _closed = registry
        .execute_tool(
            "close_pty_session",
            json!({
                "session_id": sid.as_str(),
            }),
        )
        .await;
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_stdin_empty_chars_polls_without_sending_input() {
    let (_temp, registry) = temp_registry().await;

    let start = registry
        .execute_tool(
            "exec_command",
            json!({
                "cmd": "IFS= read -r line; printf '<%s>\\n' \"$line\"",
                "yield_time_ms": 0,
            }),
        )
        .await
        .expect("start command waiting for stdin");
    let sid = exec_session_id(&start);

    let poll = registry
        .execute_tool(
            "write_stdin",
            json!({
                "session_id": sid.as_str(),
                "chars": "",
                "yield_time_ms": 25,
                "max_output_tokens": 4,
            }),
        )
        .await
        .expect("poll public exec command session");

    assert_eq!(poll["success"], true);
    assert_eq!(poll["session_id"].as_str(), Some(sid.as_str()));
    assert_eq!(poll["is_exited"].as_bool(), Some(false));
    assert_eq!(poll["output"].as_str().unwrap_or_default(), "");

    let write = registry
        .execute_tool(
            "write_stdin",
            json!({
                "session_id": sid.as_str(),
                "chars": "  keep  \n",
                "yield_time_ms": 250,
            }),
        )
        .await
        .expect("write exact bytes after public poll");

    assert_eq!(write["success"], true);
    assert_eq!(write["session_id"].as_str(), Some(sid.as_str()));
    assert!(
        write["output"].as_str().unwrap_or_default().contains("<  keep  >"),
        "write_stdin output was: {write:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_stdin_empty_poll_honours_output_cap() {
    let (temp, registry) = temp_registry_with_config(Some(
        r#"[context.dynamic]
enabled = true
tool_output_threshold = 64
max_spooled_files = 7
spool_max_age_secs = 12
"#,
    ))
    .await;

    let start = registry
        .execute_tool(
            "exec_command",
            json!({
                "cmd": "sleep 0.5; printf 'abcdefghijklmnopqrstuvwxyz\\n'; sleep 2",
                "yield_time_ms": 0,
            }),
        )
        .await
        .expect("start delayed output command");
    let sid = exec_session_id(&start);

    let poll = registry
        .execute_tool(
            "write_stdin",
            json!({
                "session_id": sid.as_str(),
                "chars": "",
                "yield_time_ms": 1000,
                "max_output_tokens": 1,
            }),
        )
        .await
        .expect("poll with capped output");

    assert_eq!(poll["success"], true);
    assert_eq!(poll["session_id"].as_str(), Some(sid.as_str()));
    assert_eq!(poll["is_exited"].as_bool(), Some(false));
    assert_eq!(poll["truncated"].as_bool(), Some(true));
    let spool_path = poll["spool_path"].as_str().expect("capped poll output should be spooled");
    assert!(spool_path.contains("write_stdin_"), "spool path should use the public tool name: {spool_path}");
    let spooled = fs::read_to_string(temp.path().join(spool_path)).expect("read spool file");
    assert!(
        spooled.contains("abcdefghijklmnopqrstuvwxyz"),
        "spool file should contain full polled output: {spooled:?}"
    );

    let _closed = registry
        .execute_tool(
            "close_pty_session",
            json!({
                "session_id": sid.as_str(),
            }),
        )
        .await;
}

#[cfg(unix)]
#[tokio::test]
async fn test_repeated_write_stdin_empty_polls_observe_fresh_output_and_exit() {
    let (_temp, registry) = temp_registry().await;

    let start = registry
        .execute_tool(
            "exec_command",
            json!({
                "cmd": "sleep 0.5; printf 'delayed-output\\n'; sleep 1.2",
                "yield_time_ms": 0,
            }),
        )
        .await
        .expect("start delayed command");
    let sid = exec_session_id(&start);
    let poll_args = json!({
        "session_id": sid.as_str(),
        "chars": "",
        "yield_time_ms": 1000,
        "max_output_tokens": 16,
    });

    let first = registry
        .execute_tool("write_stdin", poll_args.clone())
        .await
        .expect("first public poll");

    assert_eq!(first["success"], true);
    assert_eq!(first["session_id"].as_str(), Some(sid.as_str()));
    assert_eq!(first["is_exited"].as_bool(), Some(false));
    assert!(
        first["output"].as_str().unwrap_or_default().contains("delayed-output"),
        "first poll output was: {first:?}"
    );
    assert!(first.get("reused_recent_result").is_none());

    let second = registry
        .execute_tool("write_stdin", poll_args)
        .await
        .expect("second identical public poll");

    assert_eq!(second["success"], true);
    assert_eq!(second["session_id"].as_str(), Some(sid.as_str()));
    assert_eq!(second["is_exited"].as_bool(), Some(true));
    assert_eq!(second["exit_code"].as_i64(), Some(0));
    assert!(second.get("reused_recent_result").is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_stdin_wait_deadline_preserves_reusable_session() {
    let (temp, registry) = temp_registry().await;

    let start = registry
        .execute_tool(
            "exec_command",
            json!({
                "cmd": "sleep 0.2; printf 'wait-output'; i=0; while [ $i -lt 9000 ]; do printf 'x'; i=$((i+1)); done; printf '\\n'; sleep 2",
                "yield_time_ms": 0,
            }),
        )
        .await
        .expect("start long-running command");
    let sid = exec_session_id(&start);

    let in_progress = registry
        .execute_tool(
            "write_stdin",
            json!({
                "action": "wait",
                "session_id": sid.as_str(),
                "wait_timeout_seconds": 1,
                "max_output_tokens": 16,
            }),
        )
        .await
        .expect("wait until the explicit deadline");

    assert_eq!(in_progress["success"], true);
    assert_eq!(in_progress["session_id"].as_str(), Some(sid.as_str()));
    assert_eq!(in_progress["is_exited"].as_bool(), Some(false));

    let completed = registry
        .execute_tool(
            "write_stdin",
            json!({
                "action": "wait",
                "session_id": sid.as_str(),
                "wait_timeout_seconds": 3,
                "max_output_tokens": 16,
            }),
        )
        .await
        .expect("continue waiting on the reusable session");

    assert_eq!(completed["success"], true);
    assert_eq!(completed["session_id"].as_str(), Some(sid.as_str()));
    assert_eq!(completed["is_exited"].as_bool(), Some(true));
    assert_eq!(completed["exit_code"].as_i64(), Some(0));
    let spool_path = completed["spool_path"]
        .as_str()
        .expect("wait should expose the complete output spool");
    let spooled = fs::read_to_string(temp.path().join(spool_path)).expect("read complete waited output");
    assert!(spooled.contains("wait-output"), "spooled wait output was: {spooled:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_stdin_wait_spools_high_volume_pipe_output() {
    let (temp, registry) = temp_registry().await;
    let start = registry
        .execute_tool(
            "exec_command",
            json!({
                "tty": false,
                "cmd": "i=0; while [ $i -lt 20000 ]; do printf 'line-%05d\\n' \"$i\"; i=$((i+1)); done; sleep 1",
                "yield_time_ms": 0,
            }),
        )
        .await
        .expect("start high-volume pipe command");
    let sid = exec_session_id(&start);

    let completed = registry
        .execute_tool(
            "write_stdin",
            json!({
                "action": "wait",
                "session_id": sid.as_str(),
                "wait_timeout_seconds": 10,
                "max_output_tokens": 8,
            }),
        )
        .await
        .expect("wait for high-volume pipe command");

    assert_eq!(completed["success"], true);
    assert_eq!(completed["is_exited"], true);
    assert_eq!(completed["exit_code"].as_i64(), Some(0));
    assert!(completed["output_truncated"].as_bool().is_some(), "wait should expose truncation state");
    assert!(completed["total_output_bytes"].as_u64().unwrap_or_default() > 200_000);

    let spool_path = completed["spool_path"].as_str().expect("wait should expose pipe spool");
    let spooled = fs::read_to_string(temp.path().join(spool_path)).expect("read high-volume pipe spool");
    assert!(spooled.starts_with("line-00000\n"), "first pipe output chunk was lost");
    assert!(spooled.contains("line-19999\n"), "last pipe output chunk was lost");
    assert_eq!(spooled.lines().count(), 20_000);
}

#[cfg(unix)]
#[tokio::test]
async fn test_reading_pipe_spool_file_does_not_create_a_second_spool_reference() {
    let (temp, registry) = temp_registry().await;
    let start = registry
        .execute_tool(
            "exec_command",
            json!({
                "tty": false,
                "cmd": "i=0; while [ $i -lt 20000 ]; do printf 'line-%05d\\n' \"$i\"; i=$((i+1)); done; sleep 1",
                "yield_time_ms": 0,
            }),
        )
        .await
        .expect("start high-volume pipe command");
    let sid = exec_session_id(&start);

    let completed = registry
        .execute_tool(
            "write_stdin",
            json!({
                "action": "wait",
                "session_id": sid.as_str(),
                "wait_timeout_seconds": 10,
                "max_output_tokens": 8,
            }),
        )
        .await
        .expect("wait for high-volume pipe command");
    let spool_path = completed["spool_path"].as_str().expect("large output spool path");
    assert!(temp.path().join(spool_path).is_file());

    let read_result = registry
        .execute_tool(
            "exec_command",
            json!({
                "tty": false,
                "cmd": format!("cat {spool_path}"),
                "max_output_tokens": 8,
            }),
        )
        .await
        .expect("read spool file through exec command");

    assert_eq!(read_result["success"], true);
    assert!(read_result["output"].as_str().unwrap_or_default().contains("line-00000"));
    assert!(read_result.get("spool_path").is_none(), "spool read recursively spooled: {read_result:?}");
    assert!(read_result.get("output_spooled").is_none(), "spool read leaked marker: {read_result:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_stdin_reports_missing_and_closed_session_ids() {
    let (_temp, registry) = temp_registry().await;

    let missing = registry
        .execute_tool(
            "write_stdin",
            json!({
                "chars": "ignored\n",
            }),
        )
        .await;
    assert!(missing.is_err(), "missing session id should fail preflight");

    let start = registry
        .execute_tool(
            "exec_command",
            json!({
                "cmd": "cat",
                "yield_time_ms": 0,
            }),
        )
        .await
        .expect("start public exec command session");
    let sid = exec_session_id(&start);

    let close = registry
        .execute_tool(
            "close_pty_session",
            json!({
                "session_id": sid.as_str(),
            }),
        )
        .await
        .expect("close session");
    assert_eq!(close["success"], true);

    let closed = registry
        .execute_tool(
            "write_stdin",
            json!({
                "session_id": sid.as_str(),
                "chars": "after-close\n",
            }),
        )
        .await
        .expect("closed session should return structured tool error");
    assert!(closed.get("error").is_some(), "closed session response should be a structured error: {closed:?}");
    assert_eq!(closed["error"]["tool_name"], "write_stdin");
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_stdin_output_cap_forces_spool_path() {
    let (temp, registry) = temp_registry_with_config(Some(
        r#"[context.dynamic]
enabled = true
tool_output_threshold = 64
max_spooled_files = 7
spool_max_age_secs = 12
"#,
    ))
    .await;

    let start = registry
        .execute_tool(
            "exec_command",
            json!({
                "cmd": "cat",
                "yield_time_ms": 0,
            }),
        )
        .await
        .expect("start public exec command session");
    let sid = exec_session_id(&start);
    let payload = "abcdefghijklmnopqrstuvwxyz\n";

    let write = registry
        .execute_tool(
            "write_stdin",
            json!({
                "session_id": sid.as_str(),
                "chars": payload,
                "yield_time_ms": 250,
                "max_output_tokens": 1,
            }),
        )
        .await
        .expect("continue with capped output");

    assert_eq!(write["success"], true);
    assert_eq!(write["truncated"].as_bool(), Some(true));
    let spool_path = write["spool_path"]
        .as_str()
        .expect("capped write_stdin output should be spooled");
    assert!(spool_path.contains("write_stdin_"), "spool path should use the public tool name: {spool_path}");
    let spooled = fs::read_to_string(temp.path().join(spool_path)).expect("read spool file");
    assert!(
        spooled.contains(payload.trim_end()),
        "spool file should contain full continuation output: {spooled:?}"
    );

    let _closed = registry
        .execute_tool(
            "close_pty_session",
            json!({
                "session_id": sid.as_str(),
            }),
        )
        .await;
}
