//! E-2 — downstream type-check of generated TypeScript. Generates modules
//! from a fixture snapshot, writes a `tsc`-strict consumer project that
//! imports them, and runs `tsc --noEmit`. If `tsc` can't be found the test
//! skips (CI installs TypeScript to exercise the check).

use std::process::Command;

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
            "tool_id": "acme/web_search", "name": "Web Search", "version": "1.2.0",
            "description": "Search the web for query terms",
            "input_schema": input, "output_schema": output,
            "requires": [], "estimated_time_ms": 800, "stateless": true,
            "streaming": false, "tags": ["search"], "node_count": 1
        }]
    });
    serde_json::to_vec_pretty(&snapshot).expect("serialize snapshot")
}

/// Discover a working `tsc`: `$TYPEGEN_TSC`, then `tsc` / `tsc.cmd`, then
/// `npx --no-install tsc` (and the Windows `.cmd` variants). Returns the
/// `(program, leading_args)` whose `--version` probe succeeds.
fn find_tsc() -> Option<(String, Vec<String>)> {
    let mut candidates: Vec<(String, Vec<String>)> = Vec::new();
    if let Ok(custom) = std::env::var("TYPEGEN_TSC") {
        candidates.push((custom, vec![]));
    }
    candidates.push(("tsc".into(), vec![]));
    candidates.push(("tsc.cmd".into(), vec![]));
    candidates.push(("npx".into(), vec!["--no-install".into(), "tsc".into()]));
    candidates.push(("npx.cmd".into(), vec!["--no-install".into(), "tsc".into()]));

    for (program, args) in candidates {
        let ok = Command::new(&program)
            .args(&args)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some((program, args));
        }
    }
    None
}

#[test]
fn generated_ts_typechecks_under_tsc_strict() {
    let Some((tsc, tsc_args)) = find_tsc() else {
        eprintln!("skipping E-2: no tsc found (set TYPEGEN_TSC or install typescript)");
        return;
    };

    let home = TempDir::new().expect("home");
    let work = TempDir::new().expect("work");
    let snap = work.path().join("tools.snapshot");
    let out = work.path().join("generated");
    std::fs::write(&snap, snapshot_json()).expect("write snapshot");

    let mut gen = AssertCommand::cargo_bin("net-mesh").expect("cargo_bin");
    gen.env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("USERPROFILE", home.path());
    let status = gen
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
    assert!(
        status.status.success(),
        "generate failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    // Strict tsconfig (kept TS 4.x-compatible: `moduleResolution: node`).
    std::fs::write(
        work.path().join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "strict": true,
    "noEmit": true,
    "target": "es2020",
    "module": "esnext",
    "moduleResolution": "bundler",
    "lib": ["es2020"],
    "skipLibCheck": true,
    "types": []
  },
  "include": ["consumer.ts", "generated/**/*.ts"]
}
"#,
    )
    .expect("write tsconfig");

    // Consumer exercising request (required + optional), response, the
    // nested $def type, and the call helper.
    std::fs::write(
        work.path().join("consumer.ts"),
        r#"import { callAcmeWebSearch } from "./generated/index";
import type {
  AcmeWebSearchRequest,
  AcmeWebSearchResponse,
  AcmeWebSearchResult,
} from "./generated/index";

const req: AcmeWebSearchRequest = { query: "net mesh", max_results: 5 };

declare const mesh: { call: (tool: string, input: unknown) => Promise<unknown> };

export async function run(): Promise<AcmeWebSearchResponse> {
  const res = await callAcmeWebSearch(mesh, req);
  const first: AcmeWebSearchResult | undefined = res.results?.[0];
  void first;
  return res;
}
"#,
    )
    .expect("write consumer");

    let output = Command::new(&tsc)
        .args(&tsc_args)
        .args(["-p", "tsconfig.json"])
        .current_dir(work.path())
        .output()
        .expect("run tsc");
    assert!(
        output.status.success(),
        "tsc --strict rejected the generated TypeScript:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
