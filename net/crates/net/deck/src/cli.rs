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
//!
//! BOTH streams are checked, because either one alone produces the hang. A
//! stdout-only check reads as sufficient and is not: `echo | net-deck` has a
//! perfectly good terminal to draw on and no keyboard behind it, so the
//! interface comes up and waits for keystrokes that cannot arrive. That is
//! also the likelier shape in practice — a cron entry or a container
//! healthcheck keeps a terminal on stdout far more often than on stdin.

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
         Deck needs an interactive terminal on BOTH stdin and stdout. If \
         either is redirected it refuses to start rather than waiting forever \
         for keystrokes nobody can send; set {ALLOW_NON_TTY_ENV}=1 to \
         override.\n\
         \n\
         Docs: https://ai2070.net/docs/reference/deck\n",
        version = env!("CARGO_PKG_VERSION"),
    )
}

/// What the process's standard streams look like.
///
/// Passed in rather than probed so the decision is testable without a
/// terminal.
#[derive(Debug, Clone, Copy)]
pub struct Streams {
    /// Where keystrokes come from.
    pub stdin_is_tty: bool,
    /// Where the interface is drawn.
    pub stdout_is_tty: bool,
}

impl Streams {
    /// Probe the real process. Kept out of [`parse`] so tests never need one.
    pub fn probe() -> Self {
        use std::io::IsTerminal;
        Self {
            stdin_is_tty: std::io::stdin().is_terminal(),
            stdout_is_tty: std::io::stdout().is_terminal(),
        }
    }
}

/// Decide what to do with the arguments after the program name.
///
/// `streams` and `allow_non_tty` are passed in rather than probed so the
/// decision is testable without a terminal.
pub fn parse<I, S>(args: I, streams: Streams, allow_non_tty: bool) -> Startup
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

    // BOTH streams, because the hang needs only one of them. Redirected
    // stdout writes control sequences into a pipe; redirected stdin means the
    // keystrokes the interface is waiting for can never arrive. `echo |
    // net-deck` has a perfectly good terminal on stdout and still hangs
    // forever, which is exactly the failure this refusal exists to prevent —
    // checking stdout alone closed one door and left the other open.
    if !allow_non_tty {
        let redirected = match (streams.stdin_is_tty, streams.stdout_is_tty) {
            (true, true) => None,
            (true, false) => Some(("stdout", "it would emit terminal control sequences into a pipe and then wait for keystrokes nobody is there to send")),
            (false, true) => Some(("stdin", "the interface would draw correctly and then wait for keystrokes that cannot arrive, because stdin is not a keyboard")),
            (false, false) => Some(("stdin and stdout", "it would emit terminal control sequences into a pipe and wait for keystrokes that cannot arrive")),
        };

        if let Some((stream, consequence)) = redirected {
            return Startup::FailWith {
                text: format!(
                    "net-deck: {stream} is not a terminal, refusing to start.\n\
                     \n\
                     Deck is an interactive TUI. Started like this, \
                     {consequence} — an unattended hang, not an error.\n\
                     \n\
                     Run it in a terminal, or set {ALLOW_NON_TTY_ENV}=1 if you \
                     really do mean it (a recording harness, say).\n"
                ),
                code: 1,
            };
        }
    }

    Startup::Run
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An interactive terminal: both streams are a TTY.
    const TTY: Streams = Streams {
        stdin_is_tty: true,
        stdout_is_tty: true,
    };
    /// `net-deck > file` — the interface draws into a pipe.
    const STDOUT_PIPED: Streams = Streams {
        stdin_is_tty: true,
        stdout_is_tty: false,
    };
    /// `echo | net-deck` — a real terminal to draw on, no keyboard behind it.
    const STDIN_PIPED: Streams = Streams {
        stdin_is_tty: false,
        stdout_is_tty: true,
    };
    /// Fully detached: a cron job, a CI step, a container healthcheck.
    const DETACHED: Streams = Streams {
        stdin_is_tty: false,
        stdout_is_tty: false,
    };

    /// `--help` and `-h` must print usage and exit 0. They used to enter the
    /// TUI and run forever, which is how a `net-deck --help` in a CI step
    /// became a timeout.
    #[test]
    fn help_prints_usage_and_exits_zero() {
        for flag in ["--help", "-h"] {
            match parse([flag], TTY, false) {
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
            match parse([flag], TTY, false) {
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
        match parse(["--bogus"], TTY, false) {
            Startup::FailWith { text, code } => {
                assert_eq!(code, 2, "usage errors exit 2");
                assert!(text.contains("--bogus"), "the message must name it: {text}");
                assert!(text.contains("--help"), "and point somewhere: {text}");
            }
            other => panic!("--bogus produced {other:?}"),
        }
    }

    /// Help wins over the TTY check: `net-deck --help | less` has to work,
    /// and so does `net-deck --help` from a script with nothing attached.
    #[test]
    fn help_still_works_when_the_streams_are_redirected() {
        for streams in [STDOUT_PIPED, STDIN_PIPED, DETACHED] {
            assert!(
                matches!(
                    parse(["--help"], streams, false),
                    Startup::PrintAndExit { code: 0, .. }
                ),
                "--help must answer regardless of the streams: {streams:?}"
            );
        }
    }

    #[test]
    fn no_args_on_a_terminal_starts_the_interface() {
        assert_eq!(parse(Vec::<String>::new(), TTY, false), Startup::Run);
    }

    /// The hang, closed. EITHER stream being redirected is enough to produce
    /// it, so each one has to refuse on its own:
    ///
    ///   - `net-deck > log` draws into a pipe and waits for keystrokes;
    ///   - `echo | net-deck` draws perfectly and waits for keystrokes that
    ///     cannot arrive, because stdin is not a keyboard.
    ///
    /// The second is the one a stdout-only check let through, and it is the
    /// likelier shape in practice — a cron entry or a container healthcheck
    /// keeps a terminal on stdout far more often than it keeps one on stdin.
    #[test]
    fn either_stream_redirected_refuses_instead_of_hanging() {
        for (streams, expected) in [
            (STDOUT_PIPED, "stdout"),
            (STDIN_PIPED, "stdin"),
            (DETACHED, "stdin and stdout"),
        ] {
            match parse(Vec::<String>::new(), streams, false) {
                Startup::FailWith { text, code } => {
                    assert_eq!(code, 1, "an environment problem, not a usage error");
                    assert!(
                        text.contains(&format!("{expected} is not a terminal")),
                        "the refusal must name which stream is redirected; \
                         expected {expected:?} for {streams:?}:\n{text}"
                    );
                    assert!(
                        text.contains(ALLOW_NON_TTY_ENV),
                        "the refusal must name its override: {text}"
                    );
                }
                other => panic!("{streams:?} produced {other:?}"),
            }
        }
    }

    #[test]
    fn the_override_lets_a_non_tty_caller_through() {
        for streams in [STDOUT_PIPED, STDIN_PIPED, DETACHED] {
            assert_eq!(
                parse(Vec::<String>::new(), streams, true),
                Startup::Run,
                "the override must cover {streams:?} too"
            );
        }
    }

    /// The first argument decides; a valid flag after an invalid one must not
    /// rescue the invocation.
    #[test]
    fn an_unknown_flag_is_not_rescued_by_a_later_valid_one() {
        match parse(["--bogus", "--help"], TTY, false) {
            Startup::FailWith { code, .. } => assert_eq!(code, 2),
            other => panic!("produced {other:?}"),
        }
    }
}
