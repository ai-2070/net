//! Argument handling for the Deck binary.
//!
//! Deck read no argv at all. `--help`, `-h`, `--version` and `--bogus` were
//! each a silent no-op: every one of them initialized the alternate-screen
//! TUI and ran until killed. With stdout captured that meant ~5 KiB of ANSI
//! control sequences into a pipe and a process that never exits — so the
//! conventional way to discover a new tool was also the way to hang a shell,
//! a CI step, or a container healthcheck.
//!
//! It also made argument acceptance a lie: `net-deck --bogus` behaved exactly
//! like `net-deck`, so a typo in a service definition looked like it worked.
//!
//! The parser here is deliberately tiny — Deck has no operational flags, and
//! adding clap for four strings would be more surface than the problem. What
//! matters is that the three conventional probes answer conventionally, and
//! that an unknown flag fails loudly.
//!
//! The non-TTY refusal is the other half. Deck is an interactive TUI; there
//! is nothing useful it can do writing escape codes into a pipe, and the
//! failure mode is an unattended hang rather than an error. It refuses
//! instead, with an escape hatch for callers who really do mean it.

use std::ffi::OsString;

/// Environment escape hatch for the non-TTY refusal. Set to any non-empty
/// value to run Deck with a redirected stdout anyway — a terminal multiplexer
/// or recording harness may legitimately want that.
pub const ALLOW_NON_TTY_ENV: &str = "NET_DECK_ALLOW_NON_TTY";

/// What `main` should do after argument handling.
#[derive(Debug, PartialEq, Eq)]
pub enum Startup {
    /// Start the TUI.
    Run,
    /// Print `text` to stdout and exit with `code`.
    PrintAndExit { text: String, code: i32 },
    /// Print `text` to stderr and exit with `code`.
    FailWith { text: String, code: i32 },
}

fn usage() -> String {
    format!(
        "net-deck {version} — operator cyberdeck for the Net mesh.\n\
         \n\
         An interactive terminal UI. There are no operational flags: the mesh \
         it attaches to comes from configuration, and everything else is \
         driven from inside the interface.\n\
         \n\
         Usage: net-deck [OPTIONS]\n\
         \n\
         Options:\n\
         \x20 -h, --help       Print this help and exit\n\
         \x20 -V, --version    Print the version and exit\n\
         \n\
         Inside the interface:\n\
         \x20 ?                Keybindings and per-tab help\n\
         \x20 q                Quit\n\
         \n\
         Deck needs an interactive terminal. If stdout is redirected it \
         refuses to start rather than writing terminal control sequences into \
         a pipe and waiting forever; set {ALLOW_NON_TTY_ENV}=1 to override.\n\
         \n\
         Docs: https://ai2070.net/docs/reference/deck\n",
        version = env!("CARGO_PKG_VERSION"),
    )
}

/// Decide what to do with the arguments after the program name.
///
/// `stdout_is_tty` and `allow_non_tty` are passed in rather than probed so
/// the decision is testable without a terminal.
pub fn parse<I, S>(args: I, stdout_is_tty: bool, allow_non_tty: bool) -> Startup
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    // The first argument decides. None of the recognized flags combine with
    // anything — each one prints and exits — so there is nothing a later
    // argument could add, and an unknown first flag must not be rescued by a
    // valid one after it.
    if let Some(arg) = args.into_iter().next() {
        let arg = arg.into();
        let arg = arg.to_string_lossy().into_owned();

        match arg.as_str() {
            "-h" | "--help" => {
                return Startup::PrintAndExit {
                    text: usage(),
                    code: 0,
                }
            }
            "-V" | "--version" => {
                return Startup::PrintAndExit {
                    text: format!("net-deck {}\n", env!("CARGO_PKG_VERSION")),
                    code: 0,
                }
            }
            other => {
                // Exit 2 for a usage error, matching net-mesh and the
                // long-standing convention that separates "you asked wrong"
                // from "it went wrong".
                return Startup::FailWith {
                    text: format!(
                        "net-deck: unrecognized argument `{other}`\n\
                         \n\
                         net-deck takes no operational flags. Run \
                         `net-deck --help` for what it does accept.\n"
                    ),
                    code: 2,
                };
            }
        }
    }

    if !stdout_is_tty && !allow_non_tty {
        return Startup::FailWith {
            text: format!(
                "net-deck: stdout is not a terminal, refusing to start.\n\
                 \n\
                 Deck is an interactive TUI. With output redirected it would \
                 emit terminal control sequences into a pipe and then wait for \
                 keystrokes that cannot arrive — an unattended hang, not an \
                 error.\n\
                 \n\
                 Run it in a terminal, or set {ALLOW_NON_TTY_ENV}=1 if you \
                 really do want the raw output (a recording harness, say).\n"
            ),
            code: 1,
        };
    }

    Startup::Run
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--help` and `-h` must print usage and exit 0. They used to enter the
    /// TUI and run forever, which is how a `net-deck --help` in a CI step
    /// became a timeout.
    #[test]
    fn help_prints_usage_and_exits_zero() {
        for flag in ["--help", "-h"] {
            match parse([flag], true, false) {
                Startup::PrintAndExit { text, code } => {
                    assert_eq!(code, 0, "{flag} should exit 0");
                    assert!(text.contains("Usage: net-deck"), "{flag}: {text}");
                    assert!(text.contains("--version"), "{flag} should list --version");
                }
                other => panic!("{flag} produced {other:?}"),
            }
        }
    }

    #[test]
    fn version_prints_the_crate_version_and_exits_zero() {
        for flag in ["--version", "-V"] {
            match parse([flag], true, false) {
                Startup::PrintAndExit { text, code } => {
                    assert_eq!(code, 0);
                    assert!(
                        text.contains(env!("CARGO_PKG_VERSION")),
                        "{flag} printed {text:?}, which does not contain {}",
                        env!("CARGO_PKG_VERSION"),
                    );
                }
                other => panic!("{flag} produced {other:?}"),
            }
        }
    }

    /// An unknown flag was previously indistinguishable from no flag at all,
    /// so a typo in a service definition looked like it worked.
    #[test]
    fn an_unknown_flag_fails_with_exit_two() {
        match parse(["--bogus"], true, false) {
            Startup::FailWith { text, code } => {
                assert_eq!(code, 2, "usage errors exit 2");
                assert!(text.contains("--bogus"), "the message must name it: {text}");
                assert!(text.contains("--help"), "and point somewhere: {text}");
            }
            other => panic!("--bogus produced {other:?}"),
        }
    }

    /// Help wins over the TTY check: `net-deck --help | less` has to work.
    #[test]
    fn help_still_works_when_stdout_is_redirected() {
        assert!(matches!(
            parse(["--help"], false, false),
            Startup::PrintAndExit { code: 0, .. }
        ));
    }

    #[test]
    fn no_args_on_a_terminal_starts_the_interface() {
        assert_eq!(parse(Vec::<String>::new(), true, false), Startup::Run);
    }

    /// The hang, closed: no TTY and no override means refuse.
    #[test]
    fn no_args_without_a_terminal_refuses_instead_of_hanging() {
        match parse(Vec::<String>::new(), false, false) {
            Startup::FailWith { text, code } => {
                assert_eq!(code, 1, "an environment problem, not a usage error");
                assert!(text.contains("not a terminal"), "{text}");
                assert!(
                    text.contains(ALLOW_NON_TTY_ENV),
                    "the refusal must name its override: {text}"
                );
            }
            other => panic!("non-TTY start produced {other:?}"),
        }
    }

    #[test]
    fn the_override_lets_a_non_tty_caller_through() {
        assert_eq!(parse(Vec::<String>::new(), false, true), Startup::Run);
    }

    /// The first argument decides; a valid flag after an invalid one must not
    /// rescue the invocation.
    #[test]
    fn an_unknown_flag_is_not_rescued_by_a_later_valid_one() {
        match parse(["--bogus", "--help"], true, false) {
            Startup::FailWith { code, .. } => assert_eq!(code, 2),
            other => panic!("produced {other:?}"),
        }
    }
}
