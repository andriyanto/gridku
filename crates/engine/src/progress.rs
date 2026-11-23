//! How far a long command has got, for the interface to show while it runs on another thread
//! (`ui-egui/src/background.rs`): a phase and a count of a total. Lock-free, so the interface
//! reads it every frame without waiting for the command.
//!
//! Only commands that know their size report (loading a database table: the rows read, then the
//! rows written); others leave it empty and the interface shows its spinner alone.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// What the command is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Reading rows from a database.
    ReadingRows,
    /// Putting rows in the sheet.
    WritingRows,
}

impl Phase {
    /// What the interface says it is doing.
    pub fn label(self) -> &'static str {
        match self {
            Phase::ReadingRows => "Reading rows",
            Phase::WritingRows => "Writing rows",
        }
    }

    fn code(self) -> u8 {
        match self {
            Phase::ReadingRows => 1,
            Phase::WritingRows => 2,
        }
    }

    fn from_code(c: u8) -> Option<Phase> {
        match c {
            1 => Some(Phase::ReadingRows),
            2 => Some(Phase::WritingRows),
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
