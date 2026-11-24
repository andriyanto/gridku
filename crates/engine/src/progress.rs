//! How far a long command has got, for the interface to show while it runs on another thread
//! (`ui-egui/src/background.rs`): a phase and a count of a total. Lock-free, so the interface
//! reads it every frame without waiting for the command.
//!
//! Only commands that know their size report (loading a database table: the rows read, then the
//! rows written; saving to one: the rows checked, then the rows saved); others leave it empty and the interface shows its spinner alone.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// What the command is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Connecting to a database server.
    Connecting,
    /// Counting the rows to read.
    CountingRows,
    /// Reading rows from a database.
    ReadingRows,
    /// Putting rows in the sheet.
    WritingRows,
    /// Saving: reading the rows to save again from the database, to find any changed there.
    CheckingRows,
    /// Saving rows to a database.
    SavingRows,
}

impl Phase {
    /// What the interface says it is doing.
    pub fn label(self) -> &'static str {
        match self {
            Phase::Connecting => "Connecting",
            Phase::CountingRows => "Counting rows",
            Phase::ReadingRows => "Reading rows",
            Phase::WritingRows => "Writing rows",
            Phase::CheckingRows => "Checking rows",
            Phase::SavingRows => "Saving rows",
        }
    }

    /// Whether the phase has a count to show (else the bar runs back and forth).
    pub fn counts(self) -> bool {
        matches!(self, Phase::ReadingRows | Phase::WritingRows | Phase::CheckingRows | Phase::SavingRows)
    }

    fn code(self) -> u8 {
        match self {
            Phase::ReadingRows => 1,
            Phase::WritingRows => 2,
            Phase::Connecting => 3,
            Phase::CountingRows => 4,
            Phase::CheckingRows => 5,
            Phase::SavingRows => 6,
        }
    }

    fn from_code(c: u8) -> Option<Phase> {
        match c {
            1 => Some(Phase::ReadingRows),
            2 => Some(Phase::WritingRows),
            3 => Some(Phase::Connecting),
            4 => Some(Phase::CountingRows),
            5 => Some(Phase::CheckingRows),
            6 => Some(Phase::SavingRows),
            _ => None,
        }
    }
}

/// The progress of the running command, shared with the interface ([`crate::Session::progress`]).
#[derive(Debug, Default)]
pub struct Progress {
    phase: AtomicU8,
    done: AtomicU64,
    total: AtomicU64,
}

impl Progress {
    /// `done` of `total` in `phase`.
    pub fn report(&self, phase: Phase, done: u64, total: u64) {
        self.done.store(done, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
        self.phase.store(phase.code(), Ordering::Relaxed);
    }

    /// Nothing to show (the command is over, or reports nothing).
    pub fn clear(&self) {
        self.phase.store(0, Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
        self.total.store(0, Ordering::Relaxed);
    }

    /// The phase, how many done and of how many, while a command reports.
    pub fn get(&self) -> Option<(Phase, u64, u64)> {
        let phase = Phase::from_code(self.phase.load(Ordering::Relaxed))?;
        Some((phase, self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_until_cleared() {
        let p = Progress::default();
        assert_eq!(p.get(), None);
        p.report(Phase::ReadingRows, 10, 100);
        assert_eq!(p.get(), Some((Phase::ReadingRows, 10, 100)));
        p.clear();
        assert_eq!(p.get(), None);
    }
}
