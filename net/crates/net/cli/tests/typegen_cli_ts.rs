//! End-to-end integration test for `net-mesh typegen generate --language ts
//! --from-snapshot`. Fully offline: it writes a `TypegenSnapshot` JSON
//! fixture, drives the `net-mesh` binary as a subprocess, and asserts the
//! generated TypeScript module / index / metadata files have the expected
//! shape. The live-discovery path (which needs a running mesh) is covered
//! separately; the snapshot path is the deterministic CI surface.

use assert_cmd::Command as AssertCommand;
use serde_json::json;
use tempfile::TempDir;

/// Build a one-tool snapshot whose descriptor carries request + response
/// JSON Schemas. `input_schema` / `output_schema` are JSON *strings* on the
/// wire, so `json!` embeds them as escaped strings automatically.
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
fn generate_ts_from_snapshot_emits_expected_modules() {
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
            "ts",
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

    // Summary JSON on stdout.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("non-JSON stdout ({e}): {stdout}"));
    assert_eq!(parsed["tool_count"], 1, "stdout={stdout}");
    assert_eq!(parsed["language"], "ts");

    // Per-tool module.
    let module = std::fs::read_to_string(out.join("tools").join("acme_web_search.ts"))
        .expect("read generated module");
    assert!(
        module.contains("export interface AcmeWebSearchRequest {"),
        "{module}"
    );
    assert!(module.contains("query: string;"), "{module}");
    assert!(module.contains("max_results?: number;"), "{module}");
    assert!(
        module.contains("export interface AcmeWebSearchResult {"),
        "{module}"
    );
    assert!(
        module.contains("results?: AcmeWebSearchResult[];"),
        "{module}"
    );
    assert!(
        module.contains("export const AcmeWebSearchMeta = {"),
        "{module}"
    );
    assert!(
        module.contains("export async function callAcmeWebSearch("),
        "{module}"
    );

    // Index re-export + metadata.
    let index = std::fs::read_to_string(out.join("index.ts")).expect("read index");
    assert!(
        index.contains("export * from \"./tools/acme_web_search\";"),
        "{index}"
    );
    let meta = std::fs::read_to_string(out.join("meta.json")).expect("read meta");
    assert!(meta.contains("\"language\": \"ts\""), "{meta}");
    assert!(meta.contains("acme_web_search"), "{meta}");
}

#[test]
fn generate_ts_filters_by_tool_id() {
    // `--tool` narrows a multi-tool snapshot to the requested id.
    let home = TempDir::new().expect("home");
    let work = TempDir::new().expect("work");
    let snap = work.path().join("tools.snapshot");
    let out = work.path().join("generated");

    let schema = r#"{"type":"object","properties":{"q":{"type":"string"}}}"#;
    let snapshot = json!({
        "format_version": 1,
        "captured_at": "2026-06-04T10:00:00Z",
        "source_query": { "tags": [], "tools": [] },
        "descriptors": [
            {
                "tool_id": "acme/one", "name": "One", "version": "1.0.0",
                "description": null, "input_schema": schema, "output_schema": null,
                "requires": [], "estimated_time_ms": 0, "stateless": true,
                "streaming": false, "tags": [], "node_count": 1
            },
            {
                "tool_id": "acme/two", "name": "Two", "version": "1.0.0",
                "description": null, "input_schema": schema, "output_schema": null,
                "requires": [], "estimated_time_ms": 0, "stateless": true,
                "streaming": false, "tags": [], "node_count": 1
            }
        ]
    });
    std::fs::write(&snap, serde_json::to_vec(&snapshot).expect("ser")).expect("write");

    let output = cli(&home)
        .args([
            "typegen",
            "generate",
            "--language",
            "ts",
            "--from-snapshot",
            snap.to_str().expect("snap"),
            "--tool",
            "acme/two",
            "--out",
            out.to_str().expect("out"),
            "--output",
            "json",
        ])
        .output()
        .expect("invoke net-mesh");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(out.join("tools").join("acme_two.ts").exists());
    assert!(
        !out.join("tools").join("acme_one.ts").exists(),
        "filtered-out tool should not be generated"
    );
}

#[test]
fn generate_fails_on_module_basename_collision() {
    // `acme/web-search` and `acme/web_search` both sanitize to
    // `acme_web_search`; generating both would overwrite one file, so the
    // command must refuse rather than silently lose output.
    let home = TempDir::new().expect("home");
    let work = TempDir::new().expect("work");
    let snap = work.path().join("tools.snapshot");
    let out = work.path().join("generated");

    let schema = r#"{"type":"object","properties":{"q":{"type":"string"}}}"#;
    let snapshot = json!({
        "format_version": 1,
        "captured_at": "2026-06-04T10:00:00Z",
        "source_query": { "tags": [], "tools": [] },
        "descriptors": [
            {
                "tool_id": "acme/web-search", "name": "A", "version": "1.0.0",
                "description": null, "input_schema": schema, "output_schema": null,
                "requires": [], "estimated_time_ms": 0, "stateless": true,
                "streaming": false, "tags": [], "node_count": 1
            },
            {
                "tool_id": "acme/web_search", "name": "B", "version": "1.0.0",
                "description": null, "input_schema": schema, "output_schema": null,
                "requires": [], "estimated_time_ms": 0, "stateless": true,
                "streaming": false, "tags": [], "node_count": 1
            }
        ]
    });
    std::fs::write(&snap, serde_json::to_vec(&snapshot).expect("ser")).expect("write");

    let output = cli(&home)
        .args([
            "typegen",
            "generate",
            "--language",
            "ts",
            "--from-snapshot",
            snap.to_str().expect("snap"),
            "--out",
            out.to_str().expect("out"),
        ])
        .output()
        .expect("invoke net-mesh");

    // Exit code 2 (InvalidArgs) and a message naming the clashing module.
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected collision to fail the run"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("collide"), "stderr={stderr}");
    assert!(stderr.contains("acme_web_search"), "stderr={stderr}");
    // No partial output written.
    assert!(
        !out.join("tools").join("acme_web_search.ts").exists(),
        "no module should be written when generation is refused"
    );
}
