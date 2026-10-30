//! Building the single command-line string `CreateProcessW` takes.
//!
//! Programs built with the Microsoft C runtime split that string back into arguments with
//! these rules (Microsoft Learn, "Parsing C command-line arguments"):
//! - arguments are separated by spaces or tabs; a double quote starts or ends a quoted span;
//! - `2n` backslashes before a quote become `n` backslashes and the quote is a delimiter;
//! - `2n + 1` backslashes before a quote become `n` backslashes and a literal quote;
//! - backslashes anywhere else are literal.
//!
//! The program name (`argv[0]`) is parsed more simply: it ends at the first space unless
//! quoted, and it can never contain a quote.
//!
//! This is a pure function so that it is tested on every OS, not only on Windows.

use std::fmt;

/// The program name cannot be expressed on a Windows command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnquotableProgram;

impl fmt::Display for UnquotableProgram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a Windows program path cannot contain a double quote")
    }
}

impl std::error::Error for UnquotableProgram {}

/// The command line that makes `program` see exactly `args` as its arguments.
pub(crate) fn command_line(program: &str, args: &[String]) -> Result<String, UnquotableProgram> {
    if program.contains('"') {
        return Err(UnquotableProgram);
    }
    let mut line: String = if program.contains([' ', '\t']) {
        format!("\"{program}\"")
    } else {
        program.to_owned()
    };
    for argument in args {
        line.push(' ');
        push_argument(&mut line, argument);
    }
    Ok(line)
}

fn needs_quotes(argument: &str) -> bool {
    argument.is_empty() || argument.contains([' ', '\t', '\n', '\u{b}', '"'])
}

fn push_argument(line: &mut String, argument: &str) {
    if !needs_quotes(argument) {
        line.push_str(argument);
        return;
    }
    line.push('"');
    let mut pending_backslashes: usize = 0;
    for character in argument.chars() {
        match character {
            '\\' => pending_backslashes += 1,
            '"' => {
                // Each pending backslash is doubled, and one more escapes the quote itself.
                push_backslashes(line, pending_backslashes * 2 + 1);
                line.push('"');
                pending_backslashes = 0;
            }
            other => {
                push_backslashes(line, pending_backslashes);
                line.push(other);
                pending_backslashes = 0;
            }
        }
    }
    // Backslashes right before the closing quote are doubled, so the quote still closes.
    push_backslashes(line, pending_backslashes * 2);
    line.push('"');
}

fn push_backslashes(line: &mut String, count: usize) {
    line.extend(std::iter::repeat_n('\\', count));
}

#[cfg(test)]
mod tests {
    use super::{UnquotableProgram, command_line};

    const AGENT: &str = "claude.exe";
    const AGENT_IN_PROGRAM_FILES: &str = r"C:\Program Files\Agent\agent.exe";

    fn line_for(arguments: &[&str]) -> String {
        let arguments: Vec<String> = arguments.iter().map(|&text| text.to_owned()).collect();
        command_line(AGENT, &arguments).expect("the agent path needs no quote")
    }

    #[test]
    fn a_plain_argument_is_passed_without_quotes() {
        assert_eq!(
            line_for(&["--print"]),
            r"claude.exe --print",
            "plain flags stay readable in process listings"
        );
    }

    #[test]
    fn an_argument_with_a_space_is_quoted() {
        assert_eq!(
            line_for(&["fix the login bug"]),
            r#"claude.exe "fix the login bug""#,
            "a goal with spaces must reach the agent as one argument"
        );
    }

    #[test]
    fn an_empty_argument_stays_an_empty_argument() {
        assert_eq!(
            line_for(&["--system-prompt", ""]),
            r#"claude.exe --system-prompt """#,
            "an empty value must not vanish and shift the following arguments"
        );
    }

    #[test]
    fn a_quote_inside_an_argument_is_escaped() {
        assert_eq!(
            line_for(&[r#"rename "old" to "new""#]),
            r#"claude.exe "rename \"old\" to \"new\"""#,
            "quotes in a goal are text, never argument boundaries"
        );
    }

    #[test]
    fn backslashes_before_a_closing_quote_are_doubled() {
        assert_eq!(
            line_for(&[r"C:\work tree\"]),
            r#"claude.exe "C:\work tree\\""#,
            "a directory path ending in a backslash must not swallow the closing quote"
        );
    }

    #[test]
    fn backslashes_not_followed_by_a_quote_are_kept_as_they_are() {
        assert_eq!(
            line_for(&[r"C:\work tree\src"]),
            r#"claude.exe "C:\work tree\src""#,
            "ordinary Windows paths keep single backslashes"
        );
    }

    #[test]
    fn a_program_path_with_a_space_is_quoted() {
        assert_eq!(
            command_line(AGENT_IN_PROGRAM_FILES, &[]),
            Ok(format!("\"{AGENT_IN_PROGRAM_FILES}\"")),
            "an agent installed under Program Files must still be found"
        );
    }

    #[test]
    fn a_program_path_containing_a_quote_is_rejected() {
        assert_eq!(
            command_line(r#"C:\odd"name.exe"#, &[]),
            Err(UnquotableProgram),
            "Windows cannot express such a program name, so starting it must fail loudly"
        );
    }
}
