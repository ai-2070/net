//! End-to-end integration test for `net-mesh typegen diff`. Writes two
//! snapshot fixtures and asserts the diff surfaces additions and BREAKING
//! schema changes in both JSON and text output.

use assert_cmd::Command as AssertCommand;
use serde_json::json;
use tempfile::TempDir;

fn snapshot(descriptors: serde_json::Value) -> Vec<u8> {
    let snap = json!({
        "format_version": 1,
        "captured_at": "2026-06-04T10:00:00Z",
        "source_query": { "tags": [], "tools": [] },
        "descriptors": descriptors,
    });
    serde_json::to_vec_pretty(&snap).expect("serialize")
}

fn tool(id: &str, version: &str, input: &str) -> serde_json::Value {
    json!({
        "tool_id": id, "name": id, "version": version,
        "description": null, "input_schema": input, "output_schema": null,
        "requires": [], "estimated_time_ms": 0, "stateless": true,
        "streaming": false, "tags": [], "node_count": 1
    })
}

fn cli(home: &TempDir) -> AssertCommand {
    let mut cmd = AssertCommand::cargo_bin("net-mesh").expect("cargo_bin");
    cmd.env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("USERPROFILE", home.path());
    cmd
}

#[test]
fn diff_reports_additions_and_breaking_changes() {
    let home = TempDir::new().expect("home");
    let work = TempDir::new().expect("work");
    let from = work.path().join("old.snapshot");
    let to = work.path().join("new.snapshot");

    let old_input = r#"{"type":"object","properties":{"q":{"type":"string"}},"required":["q"]}"#;
    // `filter` added as required (BREAKING); plus a brand-new tool.
    let new_input = r#"{"type":"object","properties":{"q":{"type":"string"},"filter":{"type":"string"}},"required":["q","filter"]}"#;

    std::fs::write(
        &from,
        snapshot(json!([tool("acme/search", "1.0.0", old_input)])),
    )
    .expect("write old");
    std::fs::write(
        &to,
        snapshot(json!([
            tool("acme/search", "1.1.0", new_input),
            tool("acme/brand_new", "0.1.0", old_input)
        ])),
    )
    .expect("write new");

    // JSON output.
    let output = cli(&home)
        .args([
            "typegen",
            "diff",
            "--from",
            from.to_str().expect("from"),
            "--to",
            to.to_str().expect("to"),
            "--output",
            "json",
        ])
        .output()
        .expect("invoke");
    assert!(
        output.status.success(),
        "diff failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("non-JSON ({e}): {stdout}"));
    assert_eq!(parsed["added"].as_array().expect("added").len(), 1);
    assert_eq!(parsed["added"][0]["tool_id"], "acme/brand_new");
    assert_eq!(parsed["breaking_count"], 1, "stdout={stdout}");

    // Text output marks the breaking change.
    let text = cli(&home)
        .args([
            "typegen",
            "diff",
            "--from",
            from.to_str().expect("from"),
            "--to",
            to.to_str().expect("to"),
            "--output",
            "text",
        ])
        .output()
        .expect("invoke");
    assert!(text.status.success());
    let rendered = String::from_utf8_lossy(&text.stdout);
    assert!(rendered.contains("Added tools (1):"), "{rendered}");
    assert!(rendered.contains("[BREAKING]"), "{rendered}");

    // --exit-code: breaking changes present → exit 14, report still printed.
    let gated = cli(&home)
        .args([
            "typegen",
            "diff",
            "--from",
            from.to_str().expect("from"),
            "--to",
            to.to_str().expect("to"),
            "--output",
            "text",
            "--exit-code",
        ])
        .output()
        .expect("invoke");
    assert_eq!(gated.status.code(), Some(14), "expected exit 14 on breaking change");
    assert!(
        String::from_utf8_lossy(&gated.stdout).contains("[BREAKING]"),
        "report should still print under --exit-code"
    );
}

#[test]
fn diff_exit_code_zero_when_no_breaking_change() {
    let home = TempDir::new().expect("home");
    let work = TempDir::new().expect("work");
    let from = work.path().join("old.snapshot");
    let to = work.path().join("new.snapshot");

    let old_input = r#"{"type":"object","properties":{"q":{"type":"string"}},"required":["q"]}"#;
    // Only an optional field added — non-breaking.
    let new_input = r#"{"type":"object","properties":{"q":{"type":"string"},"hint":{"type":"string"}},"required":["q"]}"#;
    std::fs::write(&from, snapshot(json!([tool("acme/search", "1.0.0", old_input)]))).expect("old");
    std::fs::write(&to, snapshot(json!([tool("acme/search", "1.1.0", new_input)]))).expect("new");

    let out = cli(&home)
        .args([
            "typegen",
            "diff",
            "--from",
            from.to_str().expect("from"),
            "--to",
            to.to_str().expect("to"),
            "--exit-code",
        ])
        .output()
        .expect("invoke");
    assert!(out.status.success(), "non-breaking diff should exit 0 even with --exit-code");
}
