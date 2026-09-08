# Portability audit: macOS and Windows

Status: audit only, 2026-09-07. Evidence is read from the source at main
`bef81ce`. **No macOS or Windows build, test or terminal session was performed.**
The only Rust target installed on this host is `x86_64-unknown-linux-gnu`
(`rustup target list --installed`), so every non-Linux statement below is a
source-level reading, not a verified result. Nothing here may be cited as
platform acceptance for the TODO row "Validate supported installation, terminal
and process behavior on macOS and Windows".

Resolved since this audit (2026-09-08): §6's compile-time helper resolution
is gone, `crates/lvu-app/src/resources.rs` resolves the helper and the bridge
at run time with the precedence in `distribution.md`, and
`.github/workflows/release.yml` builds and runs both Darwin archives on macOS
runners (CI item 1 of §7). Still as read: the Linux-only `cfg`s in discovery
and the live provider, the piped-stdin refusal, and the ungated `os::unix`
imports that block a Windows build. Line references in §6 predate later
edits to `main.rs`. The hands-on counterpart is `mac-test-plan.md`.

Summary of the reading:

| | macOS (arm64/x86_64) | Windows (MSVC/ConPTY) |
| --- | --- | --- |
| Compiles | expected yes, unverified | **no** — two ungated `os::unix` imports |
| Core browsing/follow of files | expected yes | blocked behind the build |
| Piped stdin capture | **fails with a message** | not implemented |
| `/proc` discovery | reports Unsupported (by design) | reports Unsupported |
| Derived-index cleanup | **fails with `Unsupported`** | fails with `Unsupported` |
| Command capture child reaping | works (`SIGKILL` to group) | **silent no-op** |
| Terminal input backend | shared with Linux (`/dev/tty`) | entirely separate, untested |

## 1. Build blockers

Two crates use `std::os::unix` unconditionally. These are hard compile errors on
any non-Unix target, so the workspace cannot even be type-checked for Windows
today.

- `crates/lvu-query/src/host.rs:6` — `os::unix::process::CommandExt` in the
  top-level `use` tree, with `command.pre_exec(...)` and `libc::setpgid(0, 0)` at
  `crates/lvu-query/src/host.rs:309-316` and `libc::kill(-pgid, libc::SIGKILL)`
  at `crates/lvu-query/src/host.rs:361`. There is no `cfg` anywhere in the file.
  This is the Python expression-compiler host.
- `crates/lvu-command-enrich/src/lib.rs:7` — the same import, with
  `pre_exec`/`setpgid` at `crates/lvu-command-enrich/src/lib.rs:938-945` and
  `libc::kill(-pgid, libc::SIGKILL)` at
  `crates/lvu-command-enrich/src/lib.rs:990-992`. No `cfg` in the file either.

Both already have a correct model to copy: `crates/lvu-app/src/agent.rs:504-508`
gates `process_group(0)` behind `#[cfg(unix)]`, and
`crates/lvu-app/src/agent.rs:940-981` provides `#[cfg(unix)]`/`#[cfg(not(unix))]`
pairs for `owned_leader_exited` and `kill_owned_process`.

`libc` is a plain (not target-gated) dependency of `lvu-app`
(`crates/lvu-app/Cargo.toml:21`), `lvu-core`, `lvu-discovery`, `lvu-live`,
`lvu-query` and `lvu-command-enrich`. `libc` itself builds on Windows, so this is
not by itself an error, but it hides the problem: nothing in the manifests
signals that these crates are Unix-only.

macOS has no known build blocker: every Linux-specific construct found is behind
`#[cfg(target_os = "linux")]` with a `#[cfg(not(target_os = "linux"))]` arm. That
is a source reading, not a compile.

## 2. Unix-only and Linux-only constructs

### 2.1 `/proc`

- `crates/lvu-live/src/provider.rs:567-588` — `derived_artifact_paths` lists the
  index directory through `/proc/self/fd/{fd}` to keep the listing pinned to the
  directory handle. `crates/lvu-live/src/provider.rs:592-599` returns
  `io::ErrorKind::Unsupported` elsewhere. macOS equivalent: `fdopendir(2)` on a
  `dup`ed directory fd (available on macOS and every BSD).
- `crates/lvu-app/src/main.rs:6025-6047` — redirected-stdin pipes are re-opened
  through `/proc/self/fd/0` to get an isolated open-file description before Tokio
  sets `O_NONBLOCK`. `crates/lvu-app/src/main.rs:6021-6024` returns an error on
  non-Linux Unix, so **`lvu-app < pipe` does not work on macOS**. macOS has no
  `/proc`; the realistic options are `fcntl(F_SETFL)` restore-on-drop, or a
  dedicated blocking reader thread feeding a Tokio channel.
- `crates/lvu-discovery/src/procfs.rs:34-43` — the whole process/`tee`/open-file
  provider returns `ProviderState::Unsupported` off Linux, which is honest and
  correct. `crates/lvu-discovery/src/procfs.rs:288-395` are the Linux-only
  helpers (`/proc/<pid>/cmdline` splitting, `stat` ppid parsing, `fdinfo` flag
  parsing, `pipe:`/`socket:`/`anon_inode:` target classification).

### 2.2 Handle-relative filesystem operations and file identity

- `crates/lvu-live/src/provider.rs:766-786` — `open_artifact` uses
  `openat(dirfd, name, O_RDWR|O_CLOEXEC|O_NOFOLLOW)`; the non-Unix arm at
  `crates/lvu-live/src/provider.rs:788-793` returns `Unsupported`.
- `crates/lvu-live/src/provider.rs:801-865` — `exchange_and_unlink_reviewed` uses
  `renameat2(..., RENAME_EXCHANGE)` and `unlinkat`, gated
  `#[cfg(target_os = "linux")]`; `crates/lvu-live/src/provider.rs:869-879`
  returns `Unsupported`. **On macOS this means reviewed cleanup of unused derived
  indexes — a shipped feature — cannot run at all.** macOS has
  `renamex_np(RENAME_SWAP)` plus `renameatx_np`, which gives the same
  no-clobber-swap primitive; `unlinkat` and `openat` are already portable across
  Unix.
- `crates/lvu-live/src/provider.rs:931-937` — **the non-Unix
  `directory_identity` returns `{device: 0, inode: 0}` for every directory.**
  That value is compared in `artifact_directory_is_current`
  (`crates/lvu-live/src/provider.rs:560-566`), so on a hypothetical Windows build
  the "is this still the same directory I opened?" check would silently succeed
  for *any* directory. This is a `cfg` branch whose non-Unix side is wrong, not
  merely absent; it should return an `Option`/`None` and force callers to treat
  identity as unknown.
- `crates/lvu-live/src/provider.rs:961-971` — the non-Unix
  `file_revision_identity` likewise zeroes device, inode, mtime and ctime, so
  `same_content_revision` (`crates/lvu-live/src/provider.rs:953-959`) degenerates
  to a length comparison. Same defect class.
- `crates/lvu-ingest/src/writer.rs:667-679` shows the correct shape for contrast:
  `file_identity` returns `Option<FileIdentity>` and the non-Unix arm returns
  `None`, which callers must handle. Windows can supply a real identity from
  `GetFileInformationByHandle` (volume serial + file index) or
  `FILE_ID_INFO`.
- `crates/lvu-memory/src/recipe.rs:370-373` — recipe export publishes atomically
  via `fs::hard_link` from a temporary. NTFS and APFS both support hard links, so
  this is portable in principle, but it fails on FAT/exFAT, on many SMB/network
  shares, and it is exactly the kind of export target a user picks. There is no
  fallback path today.
- `crates/lvu-memory/src/recipe.rs:353-357` and
  `crates/lvu-app/src/settings.rs:492-500` correctly gate `mode(0o600)` and
  `fsync` of the parent directory. The Windows `sync_directory` no-op
  (`crates/lvu-app/src/settings.rs:497-500`) is the right call — Windows has no
  directory fsync — but it does mean the rename-durability guarantee that the
  Linux path documents is weaker there.

### 2.3 `O_NOFOLLOW`/`O_NONBLOCK` probing

- `crates/lvu-discovery/src/relevance.rs:141-147` opens discovery candidates with
  `O_NONBLOCK | O_NOFOLLOW | O_CLOEXEC` specifically so that a candidate replaced
  by a FIFO or a symlink cannot block or redirect the probe — there is a
  regression test for exactly that at
  `crates/lvu-discovery/src/relevance.rs:158-175`.
- `crates/lvu-discovery/src/relevance.rs:150-153` — the non-Linux arm falls back
  to a plain `File::open`, **dropping both flags**. macOS supports
  `O_NONBLOCK`, `O_NOFOLLOW` and `O_CLOEXEC`; the gate is simply too narrow. As
  written, discovery on macOS can block indefinitely opening a FIFO and will
  follow symlinks out of the scanned tree. Widening this to `#[cfg(unix)]` is a
  one-line fix and is the single highest value-per-effort item in this document.

### 2.4 Signals, process groups and parent death

- There is no `prctl`/`PR_SET_PDEATHSIG` anywhere in the tree, and no
  `SIGWINCH`, `SIGINT` or `SIGTERM` handler in the app. Resize arrives as a
  crossterm `Event::Resize` (`crates/lvu/src/terminal.rs:366`,
  `509`, `535`, `552`), which crossterm implements with `signal-hook` on Unix and
  console events on Windows — so the app code is already platform-neutral here.
- Every captured child is put in its own process group and killed with a negative
  `kill`: `crates/lvu-core/src/acquisition.rs:380-386` and `500-509`,
  `crates/lvu-discovery/src/docker.rs:427-444`,
  `crates/lvu-app/src/agent.rs:504-508` and `967-981`,
  `crates/lvu-query/src/host.rs:309-316`/`361`,
  `crates/lvu-command-enrich/src/lib.rs:938-945`/`990-992`.
- **The `#[cfg(not(unix))]` arms of `kill_process_group` are empty no-ops**
  (`crates/lvu-core/src/acquisition.rs:509`,
  `crates/lvu-discovery/src/docker.rs:443`). `terminate_child_tree`
  (`crates/lvu-core/src/acquisition.rs:511-518`) does add
  `child.kill()` under `#[cfg(not(unix))]`, so the immediate child dies — but
  **grandchildren do not**. For `sh -c` capture that is the entire point: the
  shell dies and the pipeline keeps writing. Windows needs a Job Object
  (`CreateJobObject` + `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` +
  `AssignProcessToJobObject`) to reproduce the process-group semantics. Without
  it, "killing captured commands when the owner dies" is silently unimplemented,
  which is worse than an error.
- Because children are placed in a *new* process group and there is no
  `PDEATHSIG`, a `SIGKILL`ed lvu leaves captured commands, the compiler host and
  the bridge running on Linux and macOS as well. This is a pre-existing
  cross-platform gap, not a portability one, but a Windows Job Object would
  actually fix it there for free.
- `crates/lvu-app/src/agent.rs:940-958` uses `waitid(P_PID, ..., WNOWAIT)` to
  observe the bridge leader's exit without releasing the PID. `waitid` with
  `WNOWAIT` is POSIX and present on macOS. The `#[cfg(not(unix))]` arm
  (`crates/lvu-app/src/agent.rs:961-965`) uses `try_wait`, which is a correct but
  weaker approximation — it reaps, so a subsequent group cleanup could target a
  reused PID. On Windows, process handles (not PIDs) make this safe, so the
  Windows implementation should hold the `Child` handle rather than a PID.

### 2.5 Advisory vs mandatory file locking

`fs2::FileExt` is the cross-process lock everywhere: the per-source runtime lease
(`crates/lvu-ingest/src/manager.rs:311`, mapping `WouldBlock` to
`RuntimeError::AlreadyRunning`), the journal (`crates/lvu-core/src/journal.rs:109`),
settings (`crates/lvu-app/src/settings.rs:430`), recipes
(`crates/lvu-memory/src/recipe.rs:427`) and the derived-index budget
(`crates/lvu-live/src/index.rs:146`, `358`, `401`;
`crates/lvu-live/src/provider.rs:653`, `724`, `798`).

`fs2` is `flock(2)` on Unix and `LockFileEx` on Windows. Three semantic
differences matter and none of them are handled:

1. Windows byte-range locks are **mandatory**. A reader without the lock gets
   `ERROR_LOCK_VIOLATION` rather than reading stale data. Index readers that
   currently tolerate concurrent access would start failing.
2. `flock` locks are per open-file-description; two `File`s for the same path in
   the *same* process both succeed on Linux. `LockFileEx` on separate handles in
   the same process does not. Any place that relies on re-locking within one
   process would behave differently — this needs an explicit audit before a
   Windows build is trusted, not just a compile.
3. Windows will not let a locked or open file be deleted or renamed by default,
   which directly conflicts with the quarantine-swap-unlink cleanup design in
   `crates/lvu-live/src/provider.rs:801-865`.

macOS `flock` matches Linux for these purposes. The known macOS caveat is
network filesystems (`flock` over NFS/SMB), which is out of scope for a local log
viewer.

### 2.6 Path and shell assumptions

- `crates/lvu-app/src/main.rs:6399-6408` — `path_identity_bytes` hashes
  `OsStr::as_bytes` on Unix and `to_string_lossy().into_bytes()` elsewhere. Two
  problems on Windows: lossy conversion means two distinct paths with unpaired
  surrogates collapse to the same identity, and because the comparison is
  byte-exact while NTFS is case-insensitive, `C:\Logs\a.log` and `c:\logs\a.log`
  produce **different** stable `SourceId`s for the same file. That violates the
  stable-identity invariant. Windows should normalise through the canonical
  final path (`GetFinalPathNameByHandle`) before hashing, or use the file index.
- `crates/lvu-core/src/acquisition.rs:356-368` — `-c/--command` runs
  `Command::new("sh").arg("-c")`. No `sh` on stock Windows. The UI label at
  `crates/lvu/src/ui.rs:5022` and the launch preview at
  `crates/lvu-app/src/main.rs:5237` also hardcode `sh -c`. A Windows port needs
  either a `cmd /C`/PowerShell selection or an explicit "shell capture requires a
  POSIX shell" refusal; the `CommandProgram::Exec` path already works anywhere.
- `crates/lvu-app/src/settings.rs:47` and
  `crates/lvu-discovery/src/relevance.rs:74-78` read `HOME` only. On Windows that
  is usually unset, so **every** storage root resolution fails and the
  "HOME is not available" errors at `crates/lvu-app/src/main.rs:4893`, `4898`,
  `4967`, `4972` become the normal path. Windows needs `USERPROFILE` /
  `FOLDERID_LocalAppData`. XDG-with-`HOME`-fallback is fine on macOS; note that
  it deliberately does not use `~/Library/Application Support`, which is a
  documentation decision rather than a defect.
- `crates/lvu-discovery/src/relevance.rs:57-62` excludes lvu's own output by
  looking for a literal `.lvu-captures` component and by `starts_with` on XDG
  roots. `starts_with` is case-sensitive, so on a case-insensitive macOS or
  Windows volume a differently-cased path to the same capture directory would not
  be excluded and lvu could discover its own journals as log candidates.

## 3. Terminal behaviour

lvu builds crossterm as
`crossterm = { version = "=0.29.0", features = ["osc52", "use-dev-tty"] }`
(`crates/lvu/Cargo.toml:9`), with default features left on
(`bracketed-paste`, `events`, `windows`, `derive-more`).

### What `use-dev-tty` actually means

`use-dev-tty` pulls in `filedescriptor` and `rustix/process`, both declared under
`[target."cfg(unix)".dependencies]` in crossterm's manifest, and it is only read
by `src/event/source/unix.rs` and `src/event/sys/unix/waker.rs`. So:

- It is **not** a Windows build blocker. On Windows the feature is inert and
  crossterm uses its `ReadConsoleInputW`-based ConPTY backend.
- It **is** the reason the Unix input path works the way lvu depends on: input is
  read from `/dev/tty` rather than stdin, which is what allows `lvu-app < file`
  to keep a live keyboard. `crates/lvu-app/src/main.rs:5950-5957`
  (`ensure_controlling_terminal`, opening `/dev/tty`) exists for the same reason,
  and its `#[cfg(not(unix))]` arm at `crates/lvu-app/src/main.rs:5959-5962`
  returns `Ok(())` unconditionally — i.e. on Windows the precondition is
  unchecked, not satisfied.
- The consequence for Windows is that **not one line of the input path is shared
  with the tested Linux path**. The Windows alternative is the default
  (`use-dev-tty` off) `WindowsKeyboardEvent` source plus a real ConPTY; there is
  no "make it more like Unix" option. This must be treated as a from-scratch
  input surface, with its own PTY-equivalent tests, not as a port.
- macOS uses exactly the same `/dev/tty` source as Linux. `/dev/tty`,
  `tcsetattr` raw mode and the self-pipe waker all behave identically. This is
  the strongest single reason macOS is close to credible and Windows is not.

### Escape-sequence risks by feature

Setup and teardown are `crates/lvu/src/terminal.rs:118-141` (enter) and
`143-166` (restore), driven by `Drop` at `crates/lvu/src/terminal.rs:169-173`.

- **Mouse reporting.** `EnableMouseCapture` emits `?1000h ?1002h ?1003h ?1015h
  ?1006h`. macOS Terminal.app does **not** support SGR (1006) mouse mode and has
  limited any-event (1003) support; iTerm2 supports both. On Terminal.app the app
  will therefore fall back to the legacy X10 encoding, which cannot report
  columns beyond 223 — the sidebar/log hitboxes in a wide window would misfire.
  The PTY suites all drive SGR coordinates (for example
  `tests/pty/test_lvu_pty.py` sends `\x1b[<0;24;4M`), so this failure mode is
  currently untestable here. Windows Terminal supports 1006.
- **Bracketed paste.** `?2004h`. Supported by iTerm2, Windows Terminal and
  Terminal.app. The Source-typing regression suite depends on paste and on
  literal typing behaving differently; both need re-running per terminal.
- **OSC 52 clipboard.** `crates/lvu/src/terminal.rs:402-410` writes
  `CopyToClipboard` and then sets the notice
  "Copy sent to terminal clipboard (OSC 52)". macOS Terminal.app does not
  implement OSC 52 at all; iTerm2 requires it to be enabled in preferences;
  Windows Terminal supports it. The notice text asserts delivery the app cannot
  confirm — AGENTS.md explicitly warns against confusing a clipboard request with
  confirmed delivery, and on Terminal.app this notice would simply be false. The
  wording should say the request was sent, and the copy test must be marked
  terminal-dependent rather than assumed.
- **Truecolor.** The delight artwork emits `38;2;r;g;b` directly (visible in the
  PTY transcript). Terminal.app supports only 256 colours and will approximate;
  iTerm2 and Windows Terminal are fine. Nothing crashes, but any pixel-exact
  colour assertion is Linux/iTerm2-only.
- **Synchronized output.** `?2026h`/`?2026l` is used around every frame
  (`EndSynchronizedUpdate` at `crates/lvu/src/terminal.rs:144`). Unknown private
  modes are ignored by conforming terminals; Terminal.app and older ConPTY do
  ignore them, so this degrades to tearing rather than corruption.
- **Alternate screen and restoration.** Restoration is `Drop`-based and the
  workspace uses the default `panic = "unwind"` (`Cargo.toml` sets only `debug`
  and `incremental`), so the panic path restores on every platform. It does *not*
  restore on `abort` or `SIGKILL`. On Unix the PTY suite asserts the exact
  sequences (`tests/pty/test_lvu_pty.py`, `assert_restored`: `?1049l`, `?2004l`,
  `?1000l`). ConPTY historically did not restore console modes for a killed
  child, so an aborted lvu can leave a Windows console in raw + alternate-screen
  state; that needs an explicit `SetConsoleMode` restore at process exit.
- **Resize.** Handled purely as `Event::Resize` with a forced full redraw
  (`crates/lvu/src/terminal.rs:365-372`). ConPTY delivers resize as a console
  event, so no code change is needed — but ConPTY reflows and re-emits content on
  resize, and the redraw-corruption behaviour that
  `tests/pty/test_redraw_pty.py` guards is exactly the kind of thing that differs.

## 4. Process and capture lifecycle

- Child stdout/stderr are always owned pipes
  (`crates/lvu-core/src/acquisition.rs:369-371`,
  `crates/lvu-query/src/host.rs:304-307`,
  `crates/lvu-command-enrich/src/lib.rs:934-938`,
  `crates/lvu-app/src/agent.rs:501-503`), with `kill_on_drop(true)` on capture.
  Owned pipes are portable; the reaping around them is not (§2.4).
- The multi-window **background capture worker is not on `main` at `bef81ce`**.
  The current cross-process mechanism is the advisory lease file
  `runtime.lock` per source directory
  (`crates/lvu-ingest/src/manager.rs:301-319`), whose `WouldBlock` becomes
  `RuntimeError::AlreadyRunning`. Whatever local IPC transport the in-flight
  worker adopts, the portability requirement is fixed: if it is a Unix domain
  socket at a filesystem path, Windows 10 1803+ does support `AF_UNIX` sockets
  but Rust's `std::os::unix::net` is not available there, so it needs either the
  `uds_windows` crate or a named pipe (`\\.\pipe\lvu-...`) behind a transport
  trait. Named pipes also give a natural per-user ACL, whereas the Unix socket
  needs directory permissions to do the same job. **Recommendation: define the
  worker transport behind a trait with two implementations from the start**, and
  do not let a `sun_path`-shaped address leak into the protocol or the on-disk
  state.
- Peak-RSS reporting reads `/proc/self/status`
  (`crates/lvu-view/tests/live_view.rs:3000`, test-only). macOS needs
  `task_info(TASK_BASIC_INFO)`; Windows needs
  `GetProcessMemoryInfo`. Test-only today, so it is not a shipping blocker, but
  the memory-budget test would need porting.

## 5. Discovery providers, per platform

| Provider | Linux today | macOS needs | Windows needs |
| --- | --- | --- | --- |
| Process / `tee` / open files (`crates/lvu-discovery/src/procfs.rs`) | `/proc/<pid>/{cmdline,stat,fd,fdinfo}` | `libproc` (`proc_listpids`, `proc_pidinfo` with `PROC_PIDLISTFDS`/`PROC_PIDFDVNODEPATHINFO`) for a bounded in-process scan, or shelling out to `lsof -nP -F`. `libproc` FD listing needs the same uid or root, so results are legitimately partial and must be reported as `ProviderState::Limited`, not `Complete`. | `NtQuerySystemInformation(SystemHandleInformation)` or a driver-free `RestartManager` query. Both are awkward and privileged; the honest first step is to keep `Unsupported`. |
| Docker (`crates/lvu-discovery/src/docker.rs`) | spawns the `docker` CLI | works as-is — only `configure_process_group`/`kill_process_group` (`crates/lvu-discovery/src/docker.rs:427-444`) are Unix-specific, and their non-Unix arms are the no-ops flagged in §2.4 | works once the child-kill no-op is replaced by a Job Object |
| Project files (`crates/lvu-discovery/src/project.rs`) | directory walk | works as-is; `crates/lvu-discovery/src/project.rs:277-289` already has both `cfg` arms | works, but the case-sensitivity exclusion issue in §2.6 applies |
| Relevance probing (`crates/lvu-discovery/src/relevance.rs`) | `O_NONBLOCK\|O_NOFOLLOW` | **fix the too-narrow `cfg` (§2.3)** | needs `FILE_FLAG_OPEN_REPARSE_POINT` + overlapped/`FILE_FLAG_BACKUP_SEMANTICS` equivalents, or an explicit refusal to probe non-regular files |

Docker and project discovery alone give macOS a genuinely useful Discovery
dialog. The dialog must show the `/proc` provider's `Unsupported` status rather
than an empty list — the status plumbing for this already exists
(`crates/lvu-discovery/src/procfs.rs:36-42`).

## 6. Packaging and runtime helpers

- `compiler_config()` (`crates/lvu-app/src/main.rs:6100-6116`) resolves
  `env!("CARGO_MANIFEST_DIR")/../../python` at **compile time** and launches
  `mise exec -- uv run --project <that path> --locked python -m lvu_expr_helper`.
- `agent_config()` (`crates/lvu-app/src/main.rs:6118-6134`) resolves
  `env!("CARGO_MANIFEST_DIR")/../..` and calls
  `AgentBridgeConfig::mise_bridge` (`crates/lvu-app/src/agent.rs:40-58`), which
  runs `mise exec node@26.8.1 -- node dist/cli.js` with `cwd = <repo>/bridge`.

`docs/distribution.md` already records that a copied binary is not a portable
installation. The portability-specific additions are:

- Both helpers assume `mise` and `uv` are on `PATH` and that a *build checkout*
  exists at a path baked in at compile time. A macOS or Windows user installing a
  release archive has neither. This is the same relocatability work
  `docs/distribution.md` describes, but it is a hard prerequisite for any
  cross-platform release, not an optimisation.
- Both program names are bare (`"mise"`, `"python3"` in test config at
  `crates/lvu-app/src/main.rs:7354`). Windows `Command` appends `.exe` when
  searching `PATH`, so `mise` resolves; `python3` does not exist on Windows (it
  is `python.exe`, or a Store alias that opens the Microsoft Store).
- `bridge/dist/cli.js` and the Python helper are both interpreted and themselves
  portable. The pinned Node 26.8.1 and Python 3.12/Polars 1.44.1 pair
  (`docs/development.md`) must be provisionable per platform; that is a
  Homebrew-formula / mise-backend question, not a source question.
- Case sensitivity: the workspace database, capture directories and derived index
  names are all generated (UUID-shaped, see
  `crates/lvu-live/src/provider.rs:974-988`), so they are safe on
  case-insensitive volumes. The risk is confined to *user-supplied* paths, which
  is §2.6.

## 7. Prioritised plan

### Blocking vs degraded

**macOS — degraded but usable.** Expected to build and run. Broken/absent:
piped stdin capture (§2.1), derived-index cleanup (§2.2), `/proc` discovery
(by design), and a real hang/symlink-follow risk in discovery probing (§2.3).
Everything else — file capture, command capture and reaping, views, filters,
enrichment, recipes, terminal input via `/dev/tty` — reads as portable.

**Windows — blocking.** It does not compile (§1). Behind that, three
independent redesigns are required: the input backend (§3), child-process
lifetime via Job Objects (§2.4), and file identity + locking semantics
(§2.2, §2.5). Plus `HOME`, `sh -c` and path-case identity (§2.6).

### Minimum work for a credible macOS build

In order. Items 1–4 are small and independently mergeable.

1. **Widen `open_probe` to `#[cfg(unix)]`**
   (`crates/lvu-discovery/src/relevance.rs:141-153`). One line. Removes a real
   hang and a symlink-escape on macOS. Do this first regardless of the rest.
2. **Add a macOS `derived_artifact_paths`** using `fdopendir` on a `dup`ed
   directory fd (`crates/lvu-live/src/provider.rs:567-599`).
3. **Add a macOS `exchange_and_unlink_reviewed`** using
   `renamex_np`/`renameatx_np` with `RENAME_SWAP`
   (`crates/lvu-live/src/provider.rs:801-879`). With 2 and 3, reviewed
   derived-index cleanup — a shipped feature — works on macOS.
4. **Give macOS a piped-stdin path** (`crates/lvu-app/src/main.rs:6025-6047`):
   either save/restore `O_NONBLOCK` via `fcntl` around Tokio's registration, or
   use a blocking reader thread. Until then the error message at
   `crates/lvu-app/src/main.rs:6021-6024` is correct but the README's feature
   list is not, for macOS.
5. **Make Discovery's Unsupported status visible** in the dialog so a macOS user
   sees "Linux /proc discovery is unsupported on this platform" instead of a
   short list.
6. **Relocatable helper resolution** (`docs/distribution.md` items) — without it
   there is no macOS *installation*, only a build.
7. **Run the PTY suites on macOS under iTerm2 and Terminal.app separately**, and
   split terminal-dependent assertions (SGR mouse, OSC 52, truecolor) behind a
   capability probe rather than weakening them.

### Larger items for Windows

1. Gate `lvu-query/src/host.rs` and `lvu-command-enrich/src/lib.rs` so the
   workspace type-checks for `x86_64-pc-windows-msvc` (§1). This is a gate, not a
   feature: it lets CI catch every subsequent regression.
2. Job Object–based child lifetime to replace the no-op `kill_process_group`
   arms (§2.4). Until this exists, command capture on Windows should be refused,
   not silently leaky.
3. Real file identity via `GetFileInformationByHandle`, replacing the
   zero-valued `directory_identity`/`file_revision_identity` non-Unix arms
   (§2.2) — these are actively wrong and would be worse than an error.
4. Storage roots from `USERPROFILE`/known folders (§2.6).
5. Path identity normalised for case and for non-UTF-16-clean names (§2.6) — a
   stable-identity invariant, so it must precede any durable Windows capture.
6. Locking-semantics audit against `LockFileEx` (§2.5).
7. ConPTY input, mouse, paste and restoration, as new work with its own test
   harness (§3).
8. `sh -c` policy: `cmd /C` or an explicit refusal (§2.6).

### CI targets, in the order they pay off

1. `cargo check --workspace --all-targets --target aarch64-apple-darwin` on a
   GitHub-hosted `macos-14` runner. Cheap, and it converts every future
   `cfg(target_os = "linux")` omission into a red build. **Add this first.**
2. `cargo test --workspace` on `macos-14`, excluding
   `crates/lvu-discovery/tests/discovery.rs` (already
   `#![cfg(target_os = "linux")]` at line 1) and any `/proc`-dependent test in
   `crates/lvu-query/tests/host_failures.rs:237,258` and
   `crates/lvu-command-enrich/tests/protocol.rs`.
3. The PTY suites on `macos-14`. They are Python + `pty` and should run
   unmodified; the terminal-capability assertions in §3 are the expected
   failures and should be split out before enabling this.
4. `cargo check --target x86_64-pc-windows-msvc` (from a Linux runner via
   `cargo-xwin`, or a `windows-2022` runner) **only after Windows item 1**.
   Adding it before that just pins a permanent red.
5. No Windows test job until items 2–4 of the Windows list exist. A green
   `cargo check` on Windows would otherwise be mistaken for support.

## 8. What this audit does not establish

- No macOS or Windows compile, test, install or terminal session was run.
- The terminal-capability claims in §3 are from published terminal behaviour, not
  from driving those terminals.
- The `fs2`/`LockFileEx` semantics in §2.5 are read from documented behaviour;
  the "same process, two handles" case in particular needs an actual Windows
  experiment before anyone relies on either answer.
- The audit covers `crates/**` at `bef81ce`. The in-flight shared capture worker
  and the shared-dialog visual work are not on main and are not audited; §4 only
  states the requirement their transport will have to meet.
