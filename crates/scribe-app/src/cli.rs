//! Tiny stdlib-only command-line argument parser for the `scr1b3` binary.
//!
//! Closes F-007 from `docs/audits/overlooked-surfaces-2026-05-29.md`. The
//! pre-audit `main.rs` did `std::env::args().nth(1)` and treated whatever it
//! found as a file path — `scr1b3 --help` opened an empty editor trying to
//! `open file '--help'`, same for `--version`. Every shell user expects
//! `--help` to print help and exit. This module fixes that without adding
//! a third-party arg-parser dep (per
//! `.s4f3/rules/dependency-management.md` "Prefer stdlib over third-party
//! when feasible").
//!
//! ## Grammar
//!
//! ```text
//! scr1b3 [--help|-h] [--version|-V] [PATH[:LINE[:COLUMN]]]
//! ```
//!
//! `PATH:LINE:COLUMN` is the editor-standard "jump to position on open"
//! notation (VSCode / Vim `+N` shorthand / Sublime). On Windows we have to
//! disambiguate a drive-letter colon (`C:\…`) from a line-number colon
//! (`file.rs:42`) — see `split_path_jump`.

use std::path::PathBuf;

/// Parsed CLI invocation.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Launch the editor. Optional file to open and optional `(line, column)`
    /// to jump to on open. `column` is `None` when the user passed only
    /// `path:line`.
    Launch {
        path: Option<PathBuf>,
        jump: Option<(usize, Option<usize>)>,
    },
    /// Print help text and exit 0.
    Help,
    /// Print version text and exit 0.
    Version,
    /// Print an error to stderr and exit 2.
    Error(String),
}

/// Parse `args` (without the program name) into an [`Action`].
pub fn parse<I, S>(args: I) -> Action
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut positional: Vec<String> = Vec::new();
    for raw in args {
        let a: String = raw.into();
        match a.as_str() {
            "--help" | "-h" => return Action::Help,
            "--version" | "-V" => return Action::Version,
            other if other.starts_with("--") || (other.starts_with('-') && other.len() > 1) => {
                return Action::Error(format!("unknown flag: {other}"));
            }
            _ => positional.push(a),
        }
    }
    match positional.len() {
        0 => Action::Launch { path: None, jump: None },
        1 => {
            let (path, jump) = split_path_jump(&positional[0]);
            Action::Launch { path: Some(path), jump }
        }
        n => Action::Error(format!("expected 0 or 1 positional argument, got {n}")),
    }
}

/// Split `arg` into a path and an optional `(line, column)` jump target.
///
/// Recognises the trailing-colon-numeric pattern: `foo.rs:42:10` →
/// `(PathBuf("foo.rs"), Some((42, Some(10))))`. Greedy from the right so
/// Windows drive letters (`C:\path\file.rs`) keep their colons.
pub fn split_path_jump(arg: &str) -> (PathBuf, Option<(usize, Option<usize>)>) {
    // Strategy: split on ':' once or twice from the right. The right-most
    // piece must be a valid `usize`. If two pieces are usize, that's
    // `:line:column`. If one piece is a usize, that's `:line`. Otherwise
    // we treat the whole thing as a path (handles Windows drive letters
    // and any path that legitimately contains `:`).
    let mut parts: Vec<&str> = arg.rsplitn(3, ':').collect();
    // After rsplitn(3): [tail, mid, head] — the right-most piece is parts[0].
    // We only treat the LAST one or two as numeric.
    match parts.len() {
        3 => {
            let (tail, mid, head) = (parts.remove(0), parts.remove(0), parts.remove(0));
            if let (Ok(col), Ok(line)) = (tail.parse::<usize>(), mid.parse::<usize>()) {
                return (PathBuf::from(head), Some((line, Some(col))));
            }
            // Maybe only the last is numeric (path contained a `:`).
            if let Ok(line) = tail.parse::<usize>() {
                let path = format!("{head}:{mid}");
                return (PathBuf::from(path), Some((line, None)));
            }
            (PathBuf::from(arg), None)
        }
        2 => {
            let (tail, head) = (parts.remove(0), parts.remove(0));
            if let Ok(line) = tail.parse::<usize>() {
                return (PathBuf::from(head), Some((line, None)));
            }
            (PathBuf::from(arg), None)
        }
        _ => (PathBuf::from(arg), None),
    }
}

/// Help text printed for `--help` / `-h`.
pub fn help_text() -> String {
    let name = env!("CARGO_PKG_NAME");
    let bin = "scr1b3";
    format!(
        "{name} — a fast, GPU-rendered, telemetry-free code editor in Rust.\n\
         \n\
         USAGE:\n    \
             {bin} [OPTIONS] [PATH[:LINE[:COLUMN]]]\n\
         \n\
         OPTIONS:\n    \
             -h, --help       Print this help and exit\n    \
             -V, --version    Print version and exit\n\
         \n\
         ARGUMENTS:\n    \
             PATH             File to open. Append :LINE or :LINE:COLUMN to jump\n                     \
                              to a position on open (e.g. src/main.rs:42:10).\n\
         \n\
         CONFIG:\n    \
             ~/.config/scr1b3/config.toml         (Linux)\n    \
             ~/Library/Application Support/scr1b3 (macOS)\n    \
             %APPDATA%\\scr1b3\\config.toml         (Windows)\n\
         \n\
         More: https://github.com/46b-ETYKiAL/Itasha.Corp_S4F3-SCR1B3\n\
         "
    )
}

/// Version text printed for `--version` / `-V`.
pub fn version_text() -> String {
    format!("scr1b3 {}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Action {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_no_args_as_launch_empty() {
        assert_eq!(
            parse_strs(&[]),
            Action::Launch { path: None, jump: None }
        );
    }

    #[test]
    fn parses_help_flags() {
        assert_eq!(parse_strs(&["--help"]), Action::Help);
        assert_eq!(parse_strs(&["-h"]), Action::Help);
        // Help wins even with a path argument behind it.
        assert_eq!(parse_strs(&["--help", "ignored.rs"]), Action::Help);
    }

    #[test]
    fn parses_version_flags() {
        assert_eq!(parse_strs(&["--version"]), Action::Version);
        assert_eq!(parse_strs(&["-V"]), Action::Version);
    }

    #[test]
    fn rejects_unknown_flags() {
        match parse_strs(&["--zap"]) {
            Action::Error(msg) => assert!(msg.contains("--zap"), "{msg}"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parses_plain_path() {
        match parse_strs(&["foo.rs"]) {
            Action::Launch { path: Some(p), jump: None } => {
                assert_eq!(p, PathBuf::from("foo.rs"));
            }
            other => panic!("expected Launch, got {other:?}"),
        }
    }

    #[test]
    fn parses_path_with_line() {
        match parse_strs(&["foo.rs:42"]) {
            Action::Launch { path: Some(p), jump: Some((42, None)) } => {
                assert_eq!(p, PathBuf::from("foo.rs"));
            }
            other => panic!("expected Launch+jump, got {other:?}"),
        }
    }

    #[test]
    fn parses_path_with_line_and_column() {
        match parse_strs(&["src/main.rs:42:10"]) {
            Action::Launch { path: Some(p), jump: Some((42, Some(10))) } => {
                assert_eq!(p, PathBuf::from("src/main.rs"));
            }
            other => panic!("expected Launch+jump, got {other:?}"),
        }
    }

    #[test]
    fn windows_drive_letter_path_is_preserved() {
        // `C:\path\file.rs` must NOT be misparsed as `(C, \path\file.rs)`.
        match parse_strs(&[r"C:\foo\bar.rs"]) {
            Action::Launch { path: Some(p), jump: None } => {
                assert_eq!(p, PathBuf::from(r"C:\foo\bar.rs"));
            }
            other => panic!("expected Launch, got {other:?}"),
        }
    }

    #[test]
    fn windows_drive_letter_with_line_jump() {
        match parse_strs(&[r"C:\foo\bar.rs:42"]) {
            Action::Launch { path: Some(p), jump: Some((42, None)) } => {
                assert_eq!(p, PathBuf::from(r"C:\foo\bar.rs"));
            }
            other => panic!("expected Launch+jump, got {other:?}"),
        }
    }

    #[test]
    fn windows_drive_letter_with_line_and_column() {
        match parse_strs(&[r"C:\foo\bar.rs:42:10"]) {
            Action::Launch { path: Some(p), jump: Some((42, Some(10))) } => {
                assert_eq!(p, PathBuf::from(r"C:\foo\bar.rs"));
            }
            other => panic!("expected Launch+jump, got {other:?}"),
        }
    }

    #[test]
    fn too_many_positional_args_is_error() {
        match parse_strs(&["foo.rs", "bar.rs"]) {
            Action::Error(msg) => assert!(msg.contains("0 or 1"), "{msg}"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn help_text_mentions_path_and_options() {
        let h = help_text();
        assert!(h.contains("USAGE"));
        assert!(h.contains("--help"));
        assert!(h.contains("PATH"));
    }

    #[test]
    fn version_text_starts_with_program_name() {
        let v = version_text();
        assert!(v.starts_with("scr1b3 "));
    }
}
