#![allow(
    missing_docs,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
use serde_json::json;
use vtcode_config::constants::tools;
use vtcode_core::tools::ToolRegistry;

#[tokio::test]
async fn delete_file_tool_removes_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let file_path = tmp.path().join("to_delete.txt");
    tokio::fs::write(&file_path, b"hello").await.unwrap();

    let registry = ToolRegistry::new(tmp.path().to_path_buf()).await;
    registry.initialize_async().await.unwrap();
    registry.allow_all_tools().await.unwrap();

    // Ensure file exists
    assert!(file_path.exists());

    let args = json!({
        "input": "*** Begin Patch\n*** Delete File: to_delete.txt\n*** End Patch"
    });
    let val = registry.execute_tool(tools::APPLY_PATCH, args).await.unwrap();
    assert_eq!(val.get("success").and_then(|v| v.as_bool()), Some(true));
    // Verify removal
    assert!(!file_path.exists());
}

#[tokio::test]
async fn recursive_shell_delete_is_blocked_even_with_confirmation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir_path = tmp.path().join("nested");
    let child_path = dir_path.join("file.txt");
    tokio::fs::create_dir_all(&dir_path).await.unwrap();
    tokio::fs::write(&child_path, b"hi").await.unwrap();

    let registry = ToolRegistry::new(tmp.path().to_path_buf()).await;
    registry.initialize_async().await.unwrap();
    registry.allow_all_tools().await.unwrap();

    let result = registry
        .execute_tool(
            tools::EXEC_COMMAND,
            json!({
                "cmd": "rm -rf nested",
                "confirm": true,
            }),
        )
        .await
        .expect_err("recursive shell deletion must remain blocked by command policy");

    assert!(
        result.to_string().contains("not permitted")
            || result.to_string().contains("blocked")
            || result.to_string().contains("Potential dangerous command detected")
    );
    assert!(dir_path.exists(), "blocked recursive shell deletion must not touch the workspace");
}
