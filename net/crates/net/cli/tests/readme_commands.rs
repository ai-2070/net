//! Every `net-mesh …` command the README publishes must exist.
//!
//! The Quick start told operators to run `net-mesh snapshot show`. There is
//! no `show` subcommand — there never was one under that spelling — so the
//! first read-only operation a new user copies out of the README fails at
//! argument parsing, in every released and candidate build:
//!
//! ```text
//! error: unrecognized subcommand 'show'
//! Usage: net-mesh snapshot [OPTIONS] <COMMAND>
//! [exit=2]
//! ```
//!
//! Nothing connected the README to the clap tree, so renaming or removing a
//! subcommand left the documentation behind silently. This walks the other
//! way: pull the commands out of the README and ask the real binary whether
//! each one resolves.
//!
//! `--help` is the probe because it short-circuits inside clap — it succeeds
//! on any valid subcommand path without needing valid argument VALUES, and it
//! never runs the command, so nothing here touches a config file, a network,
//! or an identity on disk. An unrecognized subcommand still fails first, with
//! exit 2, which is the case that matters.

use assert_cmd::prelude::*;
use std::process::Command;

const README: &str = include_str!("../README.md");

/// The command name the README uses. The crate is `net-cli`; the binary it
/// installs is `net-mesh`.
const BIN: &str = "net-mesh";

/// Pull every `net-mesh …` invocation out of the README's shell fences.
///
/// Returns `(line_number, argv_after_the_binary_name)`. Continuation lines
/// (`\` at end) are skipped rather than joined: no published command uses
/// one today, and silently mis-joining would be worse than not checking it.
fn documented_commands() -> Vec<(usize, Vec<String>)> {
    let mut out = Vec::new();
    let mut in_shell_fence = false;

    for (idx, raw) in README.lines().enumerate() {
        let line = raw.trim();

        if line.starts_with("```") {
            // Any fence toggles; only shell-ish ones are inspected.
            in_shell_fence = if in_shell_fence {
                false
            } else {
                matches!(&line[3..], "sh" | "bash" | "shell" | "console")
            };
            continue;
        }

        if !in_shell_fence || !line.starts_with(BIN) {
            continue;
        }
        if line.ends_with('\\') {
            continue;
        }

        // Stop at anything the SHELL consumes rather than the CLI: a
        // trailing comment, a redirection, or a pipe. Those tokens never
        // reach argv, so feeding them to the binary would manufacture
        // failures that say nothing about the documented command.
        let argv: Vec<String> = line
            .split_whitespace()
            .skip(1) // the binary name itself
            .take_while(|tok| !matches!(*tok, "#" | ">" | ">>" | "|" | "2>" | "&&" | ";"))
            .map(str::to_owned)
            .collect();

        if !argv.is_empty() {
            out.push((idx + 1, argv));
        }
    }

    out
}

#[test]
fn readme_publishes_commands_at_all() {
    let found = documented_commands();
    assert!(
        found.len() >= 5,
        "extracted only {} commands from the README — the fence parser is \
         probably broken, and a checker that finds nothing passes forever",
        found.len()
    );
}

#[test]
fn every_readme_command_resolves_against_the_real_binary() {
    let mut failures = Vec::new();

    for (line, argv) in documented_commands() {
        let output = Command::cargo_bin(BIN)
            .unwrap()
            .args(&argv)
            .arg("--help")
            .output()
            .unwrap();

        if !output.status.success() {
            failures.push(format!(
                "README.md:{line}: `{BIN} {}` does not resolve\n    {}",
                argv.join(" "),
                String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .next()
                    .unwrap_or("(no stderr)"),
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "the README documents {} command(s) the CLI does not accept:\n{}",
        failures.len(),
        failures.join("\n"),
    );
}

/// Guards the guard: the probe must actually reject a bad subcommand.
///
/// If `--help` ever started short-circuiting *before* subcommand resolution,
/// the test above would pass on any string at all.
#[test]
fn the_probe_rejects_a_subcommand_that_does_not_exist() {
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args(["snapshot", "show", "--help"])
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "`{BIN} snapshot show --help` succeeded — `--help` is short-circuiting \
         ahead of subcommand resolution, so the README check above proves nothing"
    );
}
