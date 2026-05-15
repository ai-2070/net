//! Build script — bakes the current git short SHA into the
//! binary as the `DECK_GIT_SHA` env var. The status bar reads
//! it via `option_env!` and renders it next to the Cargo
//! version; missing or empty falls back to "dev".
//!
//! Failures here are silent on purpose — `git` may not be on
//! `PATH`, the source may be a tarball with no `.git`, or CI
//! may pre-set `DECK_GIT_SHA` via env. In any of those cases
//! the status bar shows "dev" rather than masquerading as a
//! real release.

fn main() {
    // Honor an externally-set value first — CI sets this
    // directly so its release pipeline doesn't depend on `git`.
    if let Ok(s) = std::env::var("DECK_GIT_SHA") {
        let s = s.trim();
        if !s.is_empty() {
            println!("cargo:rustc-env=DECK_GIT_SHA={s}");
            return;
        }
    }

    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "dev".to_string());

    println!("cargo:rustc-env=DECK_GIT_SHA={sha}");
    // No `rerun-if-changed` for the .git tree — the path
    // depends on the active branch (`.git/refs/heads/<branch>`)
    // and is brittle to detached HEAD / packed-refs. Source
    // edits typically accompany commits, so cargo's default
    // rebuild trigger catches the common case; CI builds run
    // build.rs every time anyway.
    println!("cargo:rerun-if-env-changed=DECK_GIT_SHA");
}
