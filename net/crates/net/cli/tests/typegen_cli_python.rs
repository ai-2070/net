//! End-to-end integration test for `net-mesh typegen generate --language
//! python --from-snapshot`. Offline, like the TS counterpart: writes a
//! `TypegenSnapshot` JSON fixture, drives the `net-mesh` binary, and
//! asserts the generated Pydantic package has the expected shape.

use assert_cmd::Command as AssertCommand;
use serde_json::json;
use tempfile::TempDir;

fn snapshot_json() -> Vec<u8> {
    let input = r#"{"type":"object","properties":{"query":{"type":"string"},"max_results":{"type":"integer"}},"required":["query"]}"#;
    let output = r##"{"type":"object","properties":{"results":{"type":"array","items":{"$ref":"#/$defs/Result"}}},"$defs":{"Result":{"type":"object","properties":{"url":{"type":"string"},"title":{"type":"string"}},"required":["url","title"]}}}"##;
    let snapshot = json!({
        "format_version": 1,
        "captured_at": "2026-06-04T10:00:00Z",
        "source_query": { "tags": [], "tools": [] },
        "descriptors": [{
            "tool_id": "acme/web_search",
            "name": "Web Search",
            "version": "1.2.0",
            "description": "Search the web for query terms",
            "input_schema": input,
            "output_schema": output,
            "requires": [],
            "estimated_time_ms": 800,
            "stateless": true,
            "streaming": false,
            "tags": ["search", "io"],
            "node_count": 1
        }]
    });
    serde_json::to_vec_pretty(&snapshot).expect("serialize snapshot")
}

fn cli(home: &TempDir) -> AssertCommand {
    let mut cmd = AssertCommand::cargo_bin("net-mesh").expect("cargo_bin");
    cmd.env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("USERPROFILE", home.path());
    cmd
}

#[test]
fn generate_python_from_snapshot_emits_expected_package() {
    let home = TempDir::new().expect("home");
    let work = TempDir::new().expect("work");
    let snap = work.path().join("tools.snapshot");
    let out = work.path().join("generated");
    std::fs::write(&snap, snapshot_json()).expect("write snapshot");

    let output = cli(&home)
        .args([
            "typegen",
            "generate",
            "--language",
            "python",
            "--from-snapshot",
            snap.to_str().expect("snap path"),
            "--out",
            out.to_str().expect("out path"),
            "--output",
            "json",
        ])
        .output()
        .expect("invoke net-mesh");
    assert!(
        output.status.success(),
        "typegen generate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("non-JSON stdout ({e}): {stdout}"));
    assert_eq!(parsed["tool_count"], 1, "stdout={stdout}");
    assert_eq!(parsed["language"], "python");

    let pkg = out.join("acme_web_search");
    let models = std::fs::read_to_string(pkg.join("models.py")).expect("read models.py");
    assert!(
        models.contains("from pydantic import BaseModel"),
        "{models}"
    );
    assert!(
        models.contains("class AcmeWebSearchResult(BaseModel):"),
        "{models}"
    );
    assert!(
        models.contains("class AcmeWebSearchRequest(BaseModel):"),
        "{models}"
    );
    assert!(models.contains("query: str"), "{models}");
    assert!(
        models.contains("max_results: int | None = None"),
        "{models}"
    );
    assert!(
        models.contains("results: list[AcmeWebSearchResult] | None = None"),
        "{models}"
    );

    // Stub + call helper + package init.
    assert!(pkg.join("models.pyi").exists(), "missing models.pyi");
    let call = std::fs::read_to_string(pkg.join("call.py")).expect("read call.py");
    assert!(call.contains("async def call_acme_web_search("), "{call}");
    assert!(
        call.contains("AcmeWebSearchResponse.model_validate(raw)"),
        "{call}"
    );
    assert!(
        pkg.join("__init__.py").exists(),
        "missing package __init__.py"
    );

    // Root re-export + metadata.
    let root_init = std::fs::read_to_string(out.join("__init__.py")).expect("read root init");
    assert!(
        root_init.contains("from . import acme_web_search"),
        "{root_init}"
    );
    let meta = std::fs::read_to_string(out.join("_meta.json")).expect("read meta");
    assert!(meta.contains("\"language\": \"python\""), "{meta}");
}
