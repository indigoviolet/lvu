//! Integration-test harness for the worker child entry: runs exactly the
//! [`run_child_blocking`](lvu_shared::child::run_child_blocking) code path
//! the application binary will invoke for `--worker-child`, so the
//! two-window integration test exercises real processes, real election,
//! and the real socket — never a reimplementation. The application's own
//! wiring calls the same entry with its own executable; the argv contract
//! is pinned once by `SpawnSpec`.

use std::ffi::OsString;

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let child = match lvu_shared::child::parse_child_args(&args) {
        Ok(Some(child)) => child,
        Ok(None) => {
            eprintln!("lvu-shared-worker: this binary only runs --worker-child");
            std::process::exit(lvu_shared::spawn::exit::STARTUP);
        }
        Err(error) => {
            eprintln!("lvu-shared-worker: {error}");
            std::process::exit(lvu_shared::spawn::exit::STARTUP);
        }
    };
    std::process::exit(lvu_shared::child::run_child_blocking(child));
}
