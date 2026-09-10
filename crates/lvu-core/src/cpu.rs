//! What a thread has actually spent, as opposed to how long it waited.
//!
//! Capture's wall clock on a shared build volume measures the neighbours: the
//! same commit took 39 s and 221 s to settle the same source six hours apart.
//! CPU time is the part that belongs to this code, so the measurements that
//! carry a conclusion are normalised by it.
//!
//! Per *thread*, not per process. The writer owns a thread of its own, although
//! that thread also serves bounded journal-page requests; ingest measures
//! capture-sized intervals around writer messages and deliberately excludes
//! those page reads. A process figure would fold capture, live indexing and
//! query work together and answer none of them.

use std::io;

/// This thread's CPU time so far, in nanoseconds, or `None` where the platform
/// does not offer a per-thread clock.
///
/// Cheap enough to call around a unit of work — on Linux it is a vDSO call —
/// but not free, so it belongs at the ends of a loop rather than inside one.
#[cfg(unix)]
pub fn thread_cpu_nanos() -> Option<u64> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `clock_gettime` writes a whole `timespec` through this pointer
    // and reports failure through its return value; nothing else aliases it.
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut value) } != 0 {
        return None;
    }
    let seconds = u64::try_from(value.tv_sec).ok()?;
    let nanos = u64::try_from(value.tv_nsec).ok()?;
    seconds.checked_mul(1_000_000_000)?.checked_add(nanos)
}

#[cfg(not(unix))]
pub fn thread_cpu_nanos() -> Option<u64> {
    None
}

/// The CPU a stretch of work on one thread cost.
///
/// Returns zero rather than an error when the clock is unavailable or runs
/// backwards: an absent measurement must not be reported as work done, and it
/// must not stop capture either.
#[derive(Clone, Copy, Debug, Default)]
pub struct ThreadCpu {
    started: Option<u64>,
}

impl ThreadCpu {
    pub fn start() -> Self {
        Self {
            started: thread_cpu_nanos(),
        }
    }

    pub fn elapsed_nanos(&self) -> u64 {
        match (self.started, thread_cpu_nanos()) {
            (Some(started), Some(now)) => now.saturating_sub(started),
            _ => 0,
        }
    }
}

/// Process CPU consumed after this measurement began.
///
/// This includes every thread, including dependency-owned worker pools. It is
/// therefore suitable only when the caller has established that unrelated
/// owned workers are stopped. Unlike [`ThreadCpu`], an unavailable clock is an
/// error: performance tests must not turn missing accounting into zero cost.
#[derive(Clone, Copy, Debug)]
pub struct ProcessCpu {
    started: u64,
}

impl ProcessCpu {
    pub fn start() -> io::Result<Self> {
        Ok(Self {
            started: process_cpu_nanos()?,
        })
    }

    pub fn elapsed_nanos(&self) -> io::Result<u64> {
        process_cpu_nanos()?
            .checked_sub(self.started)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "process CPU clock moved backwards",
                )
            })
    }
}

/// This process's user and system CPU time in nanoseconds.
#[cfg(unix)]
fn process_cpu_nanos() -> io::Result<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: `getrusage` writes a complete `rusage` on success and reports
    // failure through its return value; nothing else aliases this pointer.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful call above initialized the value.
    let usage = unsafe { usage.assume_init() };
    timeval_nanos(usage.ru_utime)?
        .checked_add(timeval_nanos(usage.ru_stime)?)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "process CPU time overflow"))
}

#[cfg(unix)]
fn timeval_nanos(value: libc::timeval) -> io::Result<u64> {
    let seconds = u64::try_from(value.tv_sec)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "negative process CPU time"))?;
    let micros = u64::try_from(value.tv_usec)
        .ok()
        .filter(|micros| *micros < 1_000_000)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid process CPU microseconds",
            )
        })?;
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|nanos| nanos.checked_add(micros * 1_000))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "process CPU time overflow"))
}

#[cfg(not(unix))]
fn process_cpu_nanos() -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process CPU accounting is unavailable on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clock must move for work and not for waiting, which is the whole
    /// reason capture is measured against it.
    #[test]
    fn thread_cpu_counts_work_and_not_sleep() {
        let idle = ThreadCpu::start();
        std::thread::sleep(std::time::Duration::from_millis(120));
        let slept = idle.elapsed_nanos();

        let busy = ThreadCpu::start();
        let mut total = 0_u64;
        for value in 0..8_000_000_u64 {
            // Without this the optimiser folds the loop away and the test
            // measures nothing: it read 351 ns for eight million iterations.
            total = std::hint::black_box(total.wrapping_add(value.wrapping_mul(2_654_435_761)));
        }
        let worked = busy.elapsed_nanos();
        std::hint::black_box(total);

        if thread_cpu_nanos().is_none() {
            return;
        }
        assert!(
            slept < 40_000_000,
            "sleeping charged {slept} ns of CPU to the thread"
        );
        assert!(worked > slept, "work charged {worked} ns, sleep {slept} ns");
    }

    #[test]
    fn process_cpu_reports_work_or_an_explicit_unsupported_error() {
        let started = match ProcessCpu::start() {
            Ok(started) => started,
            Err(error) => {
                assert_eq!(error.kind(), io::ErrorKind::Unsupported);
                return;
            }
        };
        let mut total = 0_u64;
        for value in 0..8_000_000_u64 {
            total = std::hint::black_box(total.wrapping_add(value.wrapping_mul(2_654_435_761)));
        }
        std::hint::black_box(total);
        assert!(
            started.elapsed_nanos().expect("process CPU clock") > 0,
            "busy work must consume measurable process CPU"
        );
    }
}
