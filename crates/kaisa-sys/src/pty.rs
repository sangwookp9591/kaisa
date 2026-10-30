//! Pseudo-terminals an agent runs in (architecture §5.1 "PTY layer").
//!
//! macOS and Linux use `openpty`; Windows uses ConPTY. Every backend implements
//! [`PseudoTerminal`], so one conformance suite (§16) checks them all, including mature
//! wrappers Kaisa is compared against.

use std::fmt;
use std::io::{self, Read, Write};

/// The visible size of a terminal, in character cells. Neither dimension may be zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    rows: u16,
    columns: u16,
}

/// Why a [`TerminalSize`] was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InvalidTerminalSize {
    /// A terminal needs at least one row.
    ZeroRows,
    /// A terminal needs at least one column.
    ZeroColumns,
}

impl fmt::Display for InvalidTerminalSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroRows => f.write_str("a terminal needs at least one row"),
            Self::ZeroColumns => f.write_str("a terminal needs at least one column"),
        }
    }
}

impl std::error::Error for InvalidTerminalSize {}

impl TerminalSize {
    /// A terminal of `rows` by `columns` cells.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidTerminalSize`] if either dimension is zero.
    pub fn new(rows: u16, columns: u16) -> Result<Self, InvalidTerminalSize> {
        if rows == 0 {
            return Err(InvalidTerminalSize::ZeroRows);
        }
        if columns == 0 {
            return Err(InvalidTerminalSize::ZeroColumns);
        }
        Ok(Self { rows, columns })
    }

    /// The number of rows.
    #[must_use]
    pub fn rows(self) -> u16 {
        self.rows
    }

    /// The number of columns.
    #[must_use]
    pub fn columns(self) -> u16 {
        self.columns
    }
}

/// The side of a pseudo-terminal Kaisa holds: what the agent prints comes out of
/// [`reader`](Self::reader), and what is written to [`writer`](Self::writer) reaches the agent
/// as typed input.
pub trait PseudoTerminal {
    /// A new handle that reads the agent's output.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error if the handle cannot be duplicated.
    fn reader(&self) -> io::Result<Box<dyn Read + Send>>;

    /// A new handle that writes input to the agent.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error if the handle cannot be duplicated.
    fn writer(&self) -> io::Result<Box<dyn Write + Send>>;

    /// Changes the size the agent sees.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error if the terminal cannot be resized.
    fn resize(&mut self, size: TerminalSize) -> io::Result<()>;

    /// The size the terminal currently has.
    fn size(&self) -> TerminalSize;

    /// Closes the terminal and returns everything the agent printed that was not read yet.
    ///
    /// Call it once the agent has finished or is finishing. It closes the terminal in the
    /// order that loses nothing: on Windows the remaining output is read while the pseudo
    /// console closes, because closing it may wait until that output has been read. On macOS
    /// an agent cannot even finish exiting while its output is unread, so a keeper must keep
    /// reading for as long as the agent runs.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error if the remaining output cannot be read.
    fn drain(self) -> io::Result<Vec<u8>>
    where
        Self: Sized;
}

#[cfg(test)]
mod tests {
    use super::{InvalidTerminalSize, TerminalSize};

    /// The size of a classic text terminal.
    const CLASSIC_ROWS: u16 = 24;
    /// The width of a classic text terminal.
    const CLASSIC_COLUMNS: u16 = 80;
    /// A dimension with no cells at all.
    const NO_CELLS: u16 = 0;

    #[test]
    fn a_terminal_size_with_zero_rows_is_rejected() {
        assert_eq!(
            TerminalSize::new(NO_CELLS, CLASSIC_COLUMNS),
            Err(InvalidTerminalSize::ZeroRows),
            "an agent cannot draw into a terminal without rows"
        );
    }

    #[test]
    fn a_terminal_size_with_zero_columns_is_rejected() {
        assert_eq!(
            TerminalSize::new(CLASSIC_ROWS, NO_CELLS),
            Err(InvalidTerminalSize::ZeroColumns),
            "an agent cannot draw into a terminal without columns"
        );
    }

    #[test]
    fn a_terminal_size_keeps_its_rows_and_columns() {
        let size: TerminalSize =
            TerminalSize::new(CLASSIC_ROWS, CLASSIC_COLUMNS).expect("a classic size is valid");

        assert_eq!(
            (size.rows(), size.columns()),
            (CLASSIC_ROWS, CLASSIC_COLUMNS),
            "rows and columns are never swapped on the way to the OS"
        );
    }
}
