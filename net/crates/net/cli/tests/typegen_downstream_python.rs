//! E-3 — downstream type-check of generated Python. Generates a Pydantic
//! package from a fixture snapshot, writes a consumer that imports the
//! models + call helper, and verifies:
//!
//! 1. every generated `.py` / `.pyi` parses (always, when Python is present);
//! 2. `mypy --strict` (with the Pydantic plugin) accepts the consumer, when
//!    mypy + pydantic are available.
//!
//! Skips cleanly when a toolchain is absent so the suite still passes where
//! Python / mypy aren't installed; CI installs them to exercise the check.

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::Command as AssertCommand;
use serde_json::json;
use tempfile::TempDir;

fn snapshot_json() -> Vec<u8> {
    // `sort-order` is a non-identifier property → sanitized attr `sort_order`
    // + `Field(alias="sort-order")`; exercises the populate_by_name path.
    let input = r#"{"type":"object","properties":{"query":{"type":"string"},"max_results":{"type":"integer"},"sort-order":{"type":"string"}},"required":["query"]}"#;
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

/// First working Python interpreter (`python`, `python3`, then the Windows
/// `py` launcher), or `None`.
fn python() -> Option<String> {
    for cand in ["python", "python3", "py"] {
        let ok = Command::new(cand)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some(cand.to_string());
        }
    }
    None
}

fn tool_available(py: &str, args: &[&str]) -> bool {
    Command::new(py)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Every generated `.py` / `.pyi` under `root`.
fn python_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("py") | Some("pyi")
            ) {
                out.push(path);
            }
        }
    }
    out
}

#[test]
fn generated_python_compiles_and_typechecks() {
    let Some(py) = python() else {
        eprintln!("skipping E-3: no python interpreter found");
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
            "python",
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

    // Consumer that exercises the request constructor + response access.
    std::fs::write(
        work.path().join("consumer.py"),
        "from generated.acme_web_search import (\n\
        \x20   AcmeWebSearchRequest,\n\
        \x20   AcmeWebSearchResponse,\n\
        \x20   call_acme_web_search,\n\
        )\n\
        \n\
        \n\
        def build() -> AcmeWebSearchRequest:\n\
        \x20   # Omits the optional max_results / sort-order: optional fields\n\
        \x20   # must be omittable under mypy --strict (the .pyi default fix).\n\
        \x20   return AcmeWebSearchRequest(query=\"net mesh\")\n\
        \n\
        \n\
        def count(res: AcmeWebSearchResponse) -> int:\n\
        \x20   return len(res.results or [])\n\
        \n\
        \n\
        __all__ = [\"build\", \"count\", \"call_acme_web_search\"]\n",
    )
    .expect("write consumer");

    // (1) Syntax gate — always runs when Python is present.
    let files = python_files(&out);
    assert!(!files.is_empty(), "no generated python files found");
    let mut args = vec![
        "-c".to_string(),
        "import ast, sys\nfor f in sys.argv[1:]:\n    ast.parse(open(f, encoding='utf-8').read(), f)\n".to_string(),
    ];
    args.extend(files.iter().map(|f| f.display().to_string()));
    let parse = Command::new(&py)
        .args(&args)
        .output()
        .expect("run python ast parse");
    assert!(
        parse.status.success(),
        "generated python failed to parse:\n{}",
        String::from_utf8_lossy(&parse.stderr)
    );

    let have_mypy = tool_available(&py, &["-m", "mypy", "--version"]);
    let have_pydantic = tool_available(&py, &["-c", "import pydantic"]);

    // (2) Runtime gate — construct the model by the *safe attr name* of an
    // aliased field and round-trip through `model_dump(by_alias=True)`. This
    // only works when `populate_by_name=True` is emitted; without it Pydantic
    // rejects construction by attr name, so this is the regression guard.
    if have_pydantic {
        let probe = Command::new(&py)
            .args([
                "-c",
                "from generated.acme_web_search import AcmeWebSearchRequest\n\
                 r = AcmeWebSearchRequest(query='q', sort_order='asc')\n\
                 d = r.model_dump(exclude_none=True, by_alias=True)\n\
                 assert d['sort-order'] == 'asc', d\n\
                 assert d['query'] == 'q', d\n",
            ])
            .current_dir(work.path())
            .output()
            .expect("run pydantic runtime probe");
        assert!(
            probe.status.success(),
            "pydantic rejected construction by attr name (populate_by_name missing?):\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&probe.stdout),
            String::from_utf8_lossy(&probe.stderr)
        );
    }

    // (3) mypy --strict — only when mypy + pydantic are available.
    if !have_mypy || !have_pydantic {
        eprintln!(
            "skipping E-3 mypy portion (mypy={have_mypy}, pydantic={have_pydantic}); syntax gate passed"
        );
        return;
    }

    std::fs::write(
        work.path().join("mypy.ini"),
        "[mypy]\nstrict = True\nplugins = pydantic.mypy\n",
    )
    .expect("write mypy.ini");

    let mypy = Command::new(&py)
        .args(["-m", "mypy", "--config-file", "mypy.ini", "consumer.py"])
        .current_dir(work.path())
        .output()
        .expect("run mypy");
    assert!(
        mypy.status.success(),
        "mypy --strict rejected the generated python:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&mypy.stdout),
        String::from_utf8_lossy(&mypy.stderr)
    );
}
