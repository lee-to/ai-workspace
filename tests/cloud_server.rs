use std::process::Command;

#[test]
fn cloud_serve_bypasses_local_config_resolution() {
    let output = Command::new(env!("CARGO_BIN_EXE_ai-workspace"))
        .args(["cloud", "serve"])
        .env("AI_WORKSPACE_CONFIG", "")
        .env_remove("AI_WORKSPACE_CLOUD_PUBLIC_MCP_URI")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("AI_WORKSPACE_CLOUD_PUBLIC_MCP_URI is required"));
    assert!(!stderr.contains("AI_WORKSPACE_CONFIG must not be empty"));
}

#[test]
fn cloud_cli_never_accepts_a_token_argument() {
    let output = Command::new(env!("CARGO_BIN_EXE_ai-workspace"))
        .args(["cloud", "push", "--token", "do-not-accept"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument '--token'"));
}
