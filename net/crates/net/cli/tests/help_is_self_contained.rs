//! Help text must stand on its own, and must name the binary that exists.
//!
//! `--help` is the only documentation an installed binary carries. Somebody
//! who ran `pip install net-mesh-cli` has no checkout, so a pointer into the
//! repository is a dead end, and a command spelled with the wrong program
//! name is a copy-and-paste failure.
//!
//! Both had leaked in, because clap renders `///` doc comments as long help
//! and it is easy to forget that a comment written for maintainers is a user
//! interface:
//!
//!   - the root long help ended "See NET_CLI_PLAN.md for the full surface" —
//!     an internal plan that ships in no package;
//!   - `--no-color`'s entry printed its own maintenance history, including a
//!     former bug and what the source does instead;
//!   - 112 places across the command modules wrote `net <verb>` when the
//!     installed executable is `net-mesh`.
//!
//! This walks the real help tree — every subcommand, recursively — and fails
//! on any of those. It runs the binary rather than reading the source, so it
//! sees exactly what an operator sees, including text clap synthesizes.

use assert_cmd::prelude::*;
use std::collections::BTreeSet;
use std::process::Command;
use std::sync::OnceLock;

const BIN: &str = "net-mesh";

/// Repository-internal names that must never reach an installed user. Match
/// is case-insensitive and substring-based.
const INTERNAL_REFERENCES: &[&str] = &[
    "NET_CLI_PLAN",
    "MESHOS_SDK_PLAN",
    "DECK_SDK_PLAN",
    "SUBNET_AUTH_SDK_PLAN",
    "CAPABILITY_SYSTEM_SDK_PLAN",
    "MCP_BRIDGE_SDK_PLAN",
    "SDK_COMPUTE_SURFACE_PLAN",
    "TEST_COVERAGE_PLAN",
    "PERF_AUDIT",
    // Review/plan shorthand that means nothing outside the repo.
    "review-10",
];

/// Top-level verbs. A `net <verb>` in help means the wrong program name:
/// the crate is `net-cli`, the installed executable is `net-mesh`.
const VERBS: &[&str] = &[
    "version",
    "identity",
    "admin",
    "ice",
    "snapshot",
    "audit",
    "log",
    "failures",
    "cap",
    "peer",
    "daemon",
    "netdb",
    "org",
    "db",
    "mcp",
    "wrap",
    "forwarding",
    "node",
    "port",
    "rpc",
    "channel",
    "aggregator",
    "blob",
    "subnet",
    "gateway",
    "transfer",
    "typegen",
    "completion",
    "man",
];

/// Run `net-mesh <path…> --help` and return stdout.
fn help_for(path: &[String]) -> String {
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args(path)
        .arg("--help")
        .output()
        .unwrap_or_else(|e| panic!("failed to run {BIN} {path:?} --help: {e}"));
    assert!(
        output.status.success(),
        "`{BIN} {} --help` failed: {}",
        path.join(" "),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Child subcommand names listed in a help page's `Commands:` block.
///
/// Only the FIRST column counts. clap wraps a long subcommand description
/// onto continuation lines that are indented exactly like a real entry, so
/// taking the first word of every indented line would treat a stray English
/// word as a subcommand — and `help_for` would then panic on
/// `net-mesh <that word> --help` with a message about the CLI rather than
/// about this parser. clap indents names to a consistent column and indents
/// continuations further, so require the name to start at the shallowest
/// indent seen in the block.
fn children_of(help: &str) -> Vec<String> {
    let mut rows: Vec<(usize, String)> = Vec::new();
    let mut in_commands = false;

    for line in help.lines() {
        if line.starts_with("Commands:") {
            in_commands = true;
            continue;
        }
        if !in_commands {
            continue;
        }
        // The block ends at the first non-indented line (clap separates
        // blocks with a blank line, which is not indented either).
        if !line.starts_with(' ') {
            break;
        }
        let indent = line.len() - line.trim_start().len();
        if let Some(name) = line.split_whitespace().next() {
            rows.push((indent, name.to_owned()));
        }
    }

    let Some(name_column) = rows.iter().map(|(indent, _)| *indent).min() else {
        return Vec::new();
    };

    rows.into_iter()
        .filter(|(indent, _)| *indent == name_column)
        // `help` recurses into itself forever and documents clap, not us.
        .filter(|(_, name)| name != "help")
        .map(|(_, name)| name)
        .collect()
}

/// Every reachable `--help` page, keyed by its subcommand path.
///
/// Walked once per test binary, not once per test. The tree is ~35 pages and
/// each page is a process spawn; the three tests below all need the same
/// pages, so walking per-test tripled the cost for nothing. Integration tests
/// share a process and run on separate threads, so this is a `OnceLock`
/// rather than a plain `static`.
fn every_help_page() -> &'static [(String, String)] {
    static PAGES: OnceLock<Vec<(String, String)>> = OnceLock::new();
    PAGES.get_or_init(|| {
        let mut pages = Vec::new();
        let mut queue: Vec<Vec<String>> = vec![Vec::new()];
        let mut seen: BTreeSet<Vec<String>> = BTreeSet::new();

        while let Some(path) = queue.pop() {
            if !seen.insert(path.clone()) {
                continue;
            }
            let help = help_for(&path);
            for child in children_of(&help) {
                let mut next = path.clone();
                next.push(child);
                queue.push(next);
            }
            let label = if path.is_empty() {
                BIN.to_owned()
            } else {
                format!("{BIN} {}", path.join(" "))
            };
            pages.push((label, help));
        }

        pages
    })
}

#[test]
fn the_help_walk_reaches_the_whole_tree() {
    let pages = every_help_page();
    assert!(
        pages.len() > 30,
        "walked only {} help pages — the `Commands:` parser is probably \
         broken, and a checker that visits nothing passes forever",
        pages.len(),
    );
}

#[test]
fn help_never_points_at_a_repository_file() {
    let mut problems = Vec::new();

    for (label, help) in every_help_page() {
        for needle in INTERNAL_REFERENCES {
            if help.to_lowercase().contains(&needle.to_lowercase()) {
                let line = help
                    .lines()
                    .find(|l| l.to_lowercase().contains(&needle.to_lowercase()))
                    .unwrap_or("")
                    .trim();
                problems.push(format!("  `{label} --help` mentions {needle}: {line}"));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "help text points at {} repository-internal name(s). An installed \
         user has no checkout, so this is a dead end:\n{}",
        problems.len(),
        problems.join("\n"),
    );
}

#[test]
fn help_spells_the_binary_the_way_it_is_installed() {
    let mut problems = Vec::new();

    for (label, help) in every_help_page() {
        for line in help.lines() {
            for verb in VERBS {
                // `net <verb>` with no hyphen before `net` — the wrong name.
                // `net-mesh <verb>` does not match, because the character
                // before `net` is checked.
                let needle = format!("net {verb}");
                let mut from = 0;
                while let Some(at) = line[from..].find(&needle) {
                    let start = from + at;
                    let preceded_by_hyphen = start > 0 && line.as_bytes()[start - 1] == b'-';
                    let mid_word = start > 0 && line.as_bytes()[start - 1].is_ascii_alphanumeric();
                    if !preceded_by_hyphen && !mid_word {
                        problems.push(format!(
                            "  `{label} --help` says `{needle}`; the installed \
                             executable is `{BIN}`: {}",
                            line.trim()
                        ));
                    }
                    from = start + needle.len();
                }
            }
        }
    }

    assert!(
        problems.is_empty(),
        "{} help line(s) name a program that is not what gets installed:\n{}",
        problems.len(),
        problems.join("\n"),
    );
}
