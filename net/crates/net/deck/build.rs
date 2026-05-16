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
    // Re-run when HEAD itself moves (any commit on the
    // currently checked-out branch). On detached HEAD or a
    // freshly-cloned repo the file is still present, so this
    // is robust against typical worktree shapes. Walking
    // `.git/refs/heads/*` would also catch packed-refs
    // explicitly but the additional surface mostly fires on
    // unrelated branch updates.
    let head_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join(".git")
        .join("HEAD");
    if head_path.exists() {
        println!("cargo:rerun-if-changed={}", head_path.display());
    }
    println!("cargo:rerun-if-env-changed=DECK_GIT_SHA");
}
