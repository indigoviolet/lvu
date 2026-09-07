//! The descriptor crossterm reads terminal input from.
//!
//! crossterm's Unix event source reads the terminal by looping `read(2)` until
//! its parser yields a complete event, and it leaves that loop only when a read
//! reports `WouldBlock` or returns zero. On a *blocking* descriptor neither ever
//! happens: a read that delivers only the beginning of an escape sequence — the
//! `ESC [` of an arrow key, a truncated `ESC [ < .. M` mouse report, a partial
//! UTF-8 character — produces no event and sends crossterm straight back into
//! `read(2)`, where it sleeps until the rest of the sequence arrives. If it
//! never arrives the process is wedged inside `event::poll`: the 25 ms poll
//! timeout never expires, the SIGWINCH pipe is never drained, no frame is ever
//! emitted, and the event loop never returns far enough to restore the terminal.
//! The user is left with a live process and an alternate screen they must
//! `reset` out of.
//!
//! Partial reads are ordinary and every terminal parser must tolerate them;
//! crossterm's parser does, but its read loop does not. So lvu gives crossterm a
//! descriptor on which the `WouldBlock` branch it already implements can
//! actually be taken.
//!
//! crossterm picks its input descriptor with `isatty(0)`: standard input when
//! that is a terminal, otherwise a `/dev/tty` it opens itself. Only the first
//! case is reachable from here, so lvu replaces standard input with a *private*
//! open file description on the same terminal, opened non-blocking. It must be
//! private: a shell hands the same description to descriptors 0, 1 and 2, and
//! setting `O_NONBLOCK` on that shared description would make every write of a
//! frame able to fail with `EAGAIN`. The original standard input is kept and put
//! back by [`NonBlockingInput::restore`], which the terminal guard runs on
//! startup failure, normal exit and panic.

use std::io;

/// Standard input replaced by a private non-blocking view of the same terminal.
pub(crate) struct NonBlockingInput {
    /// The caller's original standard input, restored on the way out.
    saved: Option<SavedStdin>,
}

impl NonBlockingInput {
    /// Installs the private descriptor, or leaves standard input alone when it
    /// is not a terminal. Not being a terminal is not an error: lvu reads log
    /// data from a piped standard input, and there crossterm opens its own
    /// `/dev/tty` that this module cannot reach.
    pub(crate) fn install() -> io::Result<Self> {
        Ok(Self {
            saved: install_private_stdin()?,
        })
    }

    /// Puts the caller's standard input back. Idempotent.
    pub(crate) fn restore(&mut self) {
        if let Some(saved) = self.saved.take() {
            saved.restore();
        }
    }
}

impl Drop for NonBlockingInput {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(unix)]
mod imp {
    use std::io;

    use rustix::{
        fs::{Mode, OFlags},
        stdio,
        termios::isatty,
    };

    pub(super) struct SavedStdin(rustix::fd::OwnedFd);

    impl SavedStdin {
        pub(super) fn restore(self) {
            // Failing here would leave the caller's standard input pointing at
            // our private descriptor. There is nothing further to fall back to,
            // and reporting it would mean writing to a terminal we are in the
            // middle of handing back, so the descriptor is simply closed.
            let _ = stdio::dup2_stdin(&self.0);
        }
    }

    pub(super) fn install_private_stdin() -> io::Result<Option<SavedStdin>> {
        if !isatty(stdio::stdin()) {
            return Ok(None);
        }
        // `NOCTTY` because this is a descriptor onto the terminal we already
        // have, never a request to acquire one.
        let tty = rustix::fs::open(
            "/dev/tty",
            OFlags::RDWR | OFlags::NONBLOCK | OFlags::NOCTTY,
            Mode::empty(),
        )?;
        let saved = rustix::io::dup(stdio::stdin())?;
        // `dup2` shares the open file description, so the private descriptor
        // keeps its own `O_NONBLOCK` and the caller's does not acquire one.
        stdio::dup2_stdin(&tty)?;
        Ok(Some(SavedStdin(saved)))
    }
}

#[cfg(not(unix))]
mod imp {
    use std::io;

    pub(super) struct SavedStdin;

    impl SavedStdin {
        pub(super) fn restore(self) {}
    }

    pub(super) fn install_private_stdin() -> io::Result<Option<SavedStdin>> {
        Ok(None)
    }
}

use imp::{SavedStdin, install_private_stdin};
