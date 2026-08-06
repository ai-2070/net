//! CI guard: dangling file references in the crate's docs and source.
//!
//! Two failure classes, both of which have shipped before:
//!
//!   1. A relative markdown link in `docs/**` pointing at a file that
//!      isn't there. These render as dead links on GitHub and in the
//!      published crate.
//!   2. A `docs/…` path mentioned in a doc comment that no longer
//!      resolves. When the plans moved to `docs/internal/`, 43
//!      references across 24 source files were left pointing at the old
//!      location, and nothing caught it — the plans had been under
//!      `docs/plans/` for months before that with the same result.
//!
//! Unlike the docs site, these files are read on GitHub and inside the
//! crate, so **relative** links are the correct form here and a
//! site-absolute `/docs/…` path would be wrong. That's why this is a
//! separate guard from `web/scripts/check-doc-links.mjs` rather than an
//! extension of it.
//!
//! Run: `cargo test --test doc_link_guard`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The crate root (`net/crates/net`), which is CWD for `cargo test`.
fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_root() -> PathBuf {
    // net/crates/net -> net/crates -> net -> <repo>
    crate_root()
        .ancestors()
        .nth(3)
        .expect("crate is nested three deep under the repo root")
        .to_path_buf()
}

fn walk(dir: &Path, ext: &[&str], out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, ext, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| ext.contains(&e))
        {
            out.push(path);
        }
    }
}

/// Strip fenced blocks and inline code spans. Both contain paths that
/// are illustrative rather than links, and inline code additionally
/// contains generic-call syntax — `TypedCall[Req, Resp](ctx, t, …)` —
/// that is indistinguishable from a markdown link to a naive scan.
fn strip_code(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut in_fence = false;
    for line in src.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        // Drop `…` spans, keeping the surrounding prose.
        let mut in_code = false;
        for part in line.split('`') {
            if !in_code {
                out.push_str(part);
            }
            in_code = !in_code;
        }
        out.push('\n');
    }
    out
}

/// Every `](target)` in `src`, excluding URLs and pure anchors.
fn relative_link_targets(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b'(' {
            let start = i + 2;
            let Some(len) = src[start..].find(')') else {
                break;
            };
            let raw = &src[start..start + len];
            let target = raw.split(['#', ' ']).next().unwrap_or("").trim();
            if !target.is_empty()
                && !target.starts_with("http://")
                && !target.starts_with("https://")
                && !target.starts_with("mailto:")
            {
                out.push(target.to_string());
            }
            i = start + len;
        }
        i += 1;
    }
    out
}

#[test]
fn crate_doc_links_resolve() {
    let docs = crate_root().join("docs");
    let mut files = Vec::new();
    walk(&docs, &["md"], &mut files);
    assert!(!files.is_empty(), "no docs found under {}", docs.display());

    let mut broken = BTreeSet::new();
    for file in &files {
        let Ok(src) = std::fs::read_to_string(file) else {
            continue;
        };
        let dir = file.parent().expect("file has a parent");
        for target in relative_link_targets(&strip_code(&src)) {
            if !dir.join(&target).exists() {
                broken.insert(format!(
                    "{} -> {target}",
                    file.strip_prefix(crate_root()).unwrap_or(file).display()
                ));
            }
        }
    }

    // The site-absolute case gets its own hint. `/docs/concepts/subnets` is not
    // a missing file, it is a real docs-site route written in a form that
    // resolves only inside Astro — which is exactly why it reads as correct to
    // the author and renders dead everywhere this file is actually read.
    // `RELEASE_v0.34_HOTEL_CALIFORNIA.md` landed on master with fourteen of
    // them, and "dangling link" alone did not say what to write instead.
    let site_absolute: Vec<&String> = broken.iter().filter(|b| b.contains("-> /docs/")).collect();
    let hint = if site_absolute.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n{} of these are site-absolute `/docs/…` routes. These files are \
             read on GitHub and inside the published crate, where that path does \
             not resolve — write the full URL instead \
             (`https://ai2070.net/docs/…`), or a relative path for a target \
             inside this crate's `docs/`.",
            site_absolute.len(),
        )
    };
    assert!(
        broken.is_empty(),
        "{} dangling link(s) in the crate docs:\n{}{hint}",
        broken.len(),
        broken.iter().cloned().collect::<Vec<_>>().join("\n"),
    );
}

#[test]
fn doc_paths_mentioned_in_source_resolve() {
    let repo = repo_root();
    let mut files = Vec::new();
    for sub in ["src", "sdk/src", "cli/src", "deck/src"] {
        walk(&crate_root().join(sub), &["rs"], &mut files);
    }
    assert!(!files.is_empty(), "no sources found");

    // `docs/…` or `crates/net/docs/…` mentions, whether in a markdown
    // link or bare in prose. Anchored on the `docs/` segment so a
    // `net/crates/net/docs/x.md` prefix matches too.
    let mut broken = BTreeSet::new();
    for file in &files {
        let Ok(src) = std::fs::read_to_string(file) else {
            continue;
        };
        for token in src
            .split(|c: char| !(c.is_alphanumeric() || c == '/' || c == '_' || c == '.' || c == '-'))
        {
            let Some(idx) = token.find("docs/") else {
                continue;
            };
            if !token.ends_with(".md") {
                continue;
            }
            let rel = &token[idx..];
            // Resolve against the crate root and the repo root — both
            // spellings appear in doc comments.
            if crate_root().join(rel).exists() || repo.join(rel).exists() {
                continue;
            }
            broken.insert(format!(
                "{} -> {rel}",
                file.strip_prefix(&repo).unwrap_or(file).display()
            ));
        }
    }

    assert!(
        broken.is_empty(),
        "{} source comment(s) reference a doc that isn't there:\n{}\n\n\
         Plans live under `docs/internal/plans/` at the repo root; the \
         crate's protocol docs stay in `net/crates/net/docs/`.",
        broken.len(),
        broken.into_iter().collect::<Vec<_>>().join("\n"),
    );
}
