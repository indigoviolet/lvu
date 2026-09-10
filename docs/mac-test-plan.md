# macOS acceptance plan

Status: prepared for published `v0.1.6`; not executed by this documentation refresh.

This checklist covers Apple-silicon macOS only. Intel Macs and Windows are
unsupported; historical Intel archive evidence below is retained for provenance.

This checklist validates the immutable published macOS distribution in a real
terminal emulator. It is not evidence that the checks passed. Record every
result against the exact executable and resources described below; do not
substitute a development build or a moving branch.

The source under test is annotated tag `v0.1.6`, commit
`697865e493b1607b63423f5f2646e3339de73c3e`. The release workflow was
`34440412338`, and the Homebrew formula update was commit
`8fce418c7ff069530942a9e63927c0cdde1f98ec`. Published archive digests are:

| Artifact | SHA-256 |
| --- | --- |
| `lvu-0.1.6-aarch64-apple-darwin.tar.gz` | `19b6ebbdcc8de941aee6a18a8b8b970049bb19a3833f99a7d4590706a252f278` |
| `lvu-0.1.6-x86_64-apple-darwin.tar.gz` | `7a34e2aef62746d46b55fd3ef761d17cce3b541ba692d2c0f63156b1e17f0f23` |
| `SHA256SUMS` | `ec3e0001f948a2acc9a63b8dd8a67189c072139ce9ed59503fc11f72eeb59636` |

Dedicated platform validation at source commit
`9dff245ddcf0c217116b168fe7262eb47c65bb2e` proved the arm64 Darwin kernel PTY
and terminal restoration/cleanup probes; its separate Intel Darwin job proved
compilation only. Release workflow `34440412338` separately executed checks on
all four published release archives, including the Intel Darwin archive. None
of that CI evidence accepts the human Terminal.app, iTerm2, truecolor, mouse,
clipboard, resize, or Option/Meta checks below.

## Scope and prerequisites

Run the checklist on each supported machine/terminal combination being claimed:

- Apple silicon with the native arm64 Homebrew package.
- Terminal.app and iTerm2, separately.

Use a normal interactive terminal with `brew`, `python3`, `shasum`, and `ps`
available. Do not run the long-scale checks during routine acceptance.
Do not use captured user data. The fixture commands below create an isolated,
marked temporary root with deterministic content.

This plan does not accept Windows support, Darwin parent-death cleanup after
`SIGKILL`, or piped-stdin capture. It does not publish, merge, or replace a
release.

## 1. Install and bind exact provenance

Install the published formula, then record the immutable executable and its
adjacent resources. `lvu` does not currently expose a `--version` option, so the
formula version, resolved binary path, binary hash, and resource report together
identify the installed subject.

```sh
if brew help trust >/dev/null 2>&1; then
  brew trust indigoviolet/tap
fi
brew install indigoviolet/tap/lvu

LVU_TEST_ROOT="$(mktemp -d /tmp/lvu-macos-v016.XXXXXX)"
chmod 700 "$LVU_TEST_ROOT"
touch "$LVU_TEST_ROOT/.lvu-macos-acceptance-fixture"
export LVU_TEST_ROOT
printf 'Retain this exact root for the second shell: %s\n' "$LVU_TEST_ROOT"
LVU_CAPTURE="$LVU_TEST_ROOT/capture"
export LVU_CAPTURE

LVU_PREFIX="$(brew --prefix lvu)"
export LVU_PREFIX

brew info --json=v2 indigoviolet/tap/lvu >"$LVU_TEST_ROOT/brew-info.json"
python3 - <<'PY' | tee "$LVU_TEST_ROOT/provenance.txt"
import hashlib
import json
import os
from pathlib import Path

root = Path(os.environ["LVU_TEST_ROOT"])
info = json.loads((root / "brew-info.json").read_text())
formula = info["formulae"][0]
prefix = Path(os.environ["LVU_PREFIX"]).resolve()
binary = (prefix / "bin" / "lvu").resolve()
digest = hashlib.sha256(binary.read_bytes()).hexdigest()
print(f"formula={formula['full_name']}")
print(f"stable={formula['versions']['stable']}")
print(f"prefix={prefix}")
print(f"binary={binary}")
print(f"binary_sha256={digest}")
PY

LVU_BIN="$(cd "$LVU_PREFIX/bin" && pwd -P)/lvu"
export LVU_BIN
"$LVU_BIN" --resources | tee "$LVU_TEST_ROOT/resources.txt"
"$LVU_BIN" --help | tee "$LVU_TEST_ROOT/help.txt"
uname -a | tee "$LVU_TEST_ROOT/uname.txt"
sw_vers | tee "$LVU_TEST_ROOT/sw-vers.txt"
printf 'TERM=%s\nCOLORTERM=%s\n' "${TERM-}" "${COLORTERM-}" \
  | tee "$LVU_TEST_ROOT/terminal-env.txt"
```

Accept only when:

- `stable=0.1.6` is recorded.
- The resolved executable is below the installed formula prefix.
- `--resources` reports every packaged resource as found, with origin
  `installed beside the executable`.
- The resource paths resolve beside that same immutable installation, rather
  than a checkout or an older Cellar version.
- The executable SHA-256 is retained with the result. Compare it only with a
  digest calculated from the same installed binary; the archive SHA values
  above are hashes of compressed archives, not of the executable inside them.

Known published inconsistency: the final sentence of `lvu --help` in `v0.1.6`
still mentions `p` for advanced Polars filtering. The executable no longer maps
`p`; the supported path is `/`, then `Alt-A`. Record the help discrepancy, but
do not use `p` in acceptance.

## 2. Create deterministic fixtures

```sh
python3 - <<'PY'
import os
from pathlib import Path

root = Path(os.environ["LVU_TEST_ROOT"])
levels = ("INFO", "WARN", "ERROR", "DEBUG")
statuses = (200, 201, 404, 503)
users = ("alice", "bob", "céline", "李雷")
with (root / "events.log").open("w", encoding="utf-8", newline="\n") as out:
    for i in range(240):
        second = i % 60
        level = levels[i % len(levels)]
        status = statuses[(i // 3) % len(statuses)]
        user = users[(i // 5) % len(users)]
        latency = (i * 137) % 8000
        out.write(
            f"stamp<2026-09-10T05:{i // 60:02d}:{second:02d}Z> "
            f"{level} request id={i:04d} status={status} "
            f"user={user} latency={latency}ms\n"
        )

(root / "unicode.log").write_text(
    "plain ascii\n"
    "combining: cafe\u0301 nai\u0308ve\n"
    "wide: 東京 李雷\n"
    "emoji: investigation 🧠 complete ✅\n"
    "control-looking text: \\x1b[31m is literal\n",
    encoding="utf-8",
    newline="\n",
)

(root / "multiline.log").write_text(
    "2026-09-10T05:00:00Z ERROR request failed\n"
    "  at parser.rs:42\n"
    "  caused by invalid field\n"
    "2026-09-10T05:00:01Z INFO recovered\n",
    encoding="utf-8",
    newline="\n",
)
PY

shasum -a 256 "$LVU_TEST_ROOT"/*.log | tee "$LVU_TEST_ROOT/fixture-sha256.txt"
wc -l "$LVU_TEST_ROOT"/*.log | tee "$LVU_TEST_ROOT/fixture-lines.txt"
```

The expected counts are 240 lines for `events.log`, 5 for `unicode.log`, and 4
for `multiline.log`. Retain the hashes; regenerating the fixtures must produce
the same bytes.

## 3. Startup, file acquisition, and restart

Run in both Terminal.app and iTerm2 unless a narrower platform claim is being
made.

```sh
"$LVU_BIN" --capture-dir "$LVU_CAPTURE" --fresh "$LVU_TEST_ROOT/events.log"
```

Verify interactively:

1. The TUI appears without shell escape sequences or diagnostic output mixed
   into the record list.
2. The 240 records are available and navigation remains responsive.
3. `q` exits and restores the shell prompt, cursor, echo, and canonical input.
4. `stty -a | grep -Eo '(-?echo|-?icanon)'` after exit shows normal `echo` and
   `icanon` for the invoking terminal.

Then restart without a path:

```sh
"$LVU_BIN" --capture-dir "$LVU_CAPTURE" --resume
```

Verify that the remembered file source is acquired again without duplicate
records. In `v0.1.6`, bare `lvu` and `--resume` also re-acquire remembered
command sources; older checklist language saying commands must not rerun is no
longer valid. `--fresh` starts without remembered sources.

## 4. Command acquisition and owned-process cleanup

Use only the recorded process identities from this fixture. Never use a blanket
`pkill` or kill unrelated processes.

```sh
rm -f "$LVU_TEST_ROOT/parent.pid" "$LVU_TEST_ROOT/child.pid"
"$LVU_BIN" --capture-dir "$LVU_CAPTURE" --fresh --command '
  echo $$ >"$LVU_TEST_ROOT/parent.pid"
  sleep 600 & child=$!
  echo "$child" >"$LVU_TEST_ROOT/child.pid"
  i=0
  while :; do
    printf "command-line-%04d\n" "$i"
    i=$((i + 1))
    sleep 1
  done
'
```

After several lines appear, use a second shell to record the identities while
they are still live. Enter the exact root printed by the first shell; do not run
`mktemp` or create a second fixture root:

```sh
printf 'Exact retained LVU_TEST_ROOT from the first shell: '
IFS= read -r LVU_TEST_ROOT
export LVU_TEST_ROOT
test -f "$LVU_TEST_ROOT/.lvu-macos-acceptance-fixture"
LVU_CAPTURE="$LVU_TEST_ROOT/capture"
export LVU_CAPTURE

parent_pid="$(cat "$LVU_TEST_ROOT/parent.pid")"
ps -p "$parent_pid" -o pgid= | tr -d ' ' >"$LVU_TEST_ROOT/command.pgid"
owned_pgid="$(cat "$LVU_TEST_ROOT/command.pgid")"
for pidfile in "$LVU_TEST_ROOT/parent.pid" "$LVU_TEST_ROOT/child.pid"; do
  pid="$(cat "$pidfile")"
  ps -p "$pid" -o pid=,ppid=,pgid=,command=
done >"$LVU_TEST_ROOT/command-owned-before.txt"
ps -axo pid=,ppid=,pgid=,command= \
  | awk -v group="$owned_pgid" '$3 == group' \
  >>"$LVU_TEST_ROOT/command-owned-before.txt"
cat "$LVU_TEST_ROOT/command-owned-before.txt"
```

Exit normally with `q`, then check only those recorded PIDs:

```sh
cleanup_status=0
owned_pgid="$(cat "$LVU_TEST_ROOT/command.pgid")"
{
for pidfile in "$LVU_TEST_ROOT/parent.pid" "$LVU_TEST_ROOT/child.pid"; do
  pid="$(cat "$pidfile")"
  if kill -0 "$pid" 2>/dev/null; then
    ps -p "$pid" -o pid=,ppid=,pgid=,command=
    printf 'FAIL: owned process still alive: %s\n' "$pid"
    cleanup_status=1
  else
    printf 'cleaned: %s\n' "$pid"
  fi
done
remaining="$(ps -axo pid=,ppid=,pgid=,command= | awk -v group="$owned_pgid" '$3 == group')"
if test -n "$remaining"; then
  printf 'FAIL: owned process group %s still has members:\n%s\n' "$owned_pgid" "$remaining"
  cleanup_status=1
else
  printf 'cleaned process group: %s\n' "$owned_pgid"
fi
} >"$LVU_TEST_ROOT/command-cleanup.txt" 2>&1
cat "$LVU_TEST_ROOT/command-cleanup.txt"
test "$cleanup_status" -eq 0
```

Copy those PID files before the next run, invoke
`"$LVU_BIN" --capture-dir "$LVU_CAPTURE" --resume`, and
verify that a new command process identity is created because resume re-acquires
the command. Exit normally and repeat the identity-scoped cleanup check for the
new pair. Finally invoke
`"$LVU_BIN" --capture-dir "$LVU_CAPTURE" --fresh` and verify that neither the
file nor command source is restored automatically.

Do not substitute `SIGKILL` for normal cleanup acceptance. Darwin does not
currently provide the Linux parent-death signal behavior; killing the app with
`SIGKILL` can leave its command process group alive and is an unresolved
boundary, not a passing graceful-cleanup test.

## 5. Piped stdin boundary

`v0.1.6` intentionally does not claim piped-stdin capture on macOS. Verify only
the explicit refusal, outside a TUI:

```sh
printf 'one\ntwo\n' | "$LVU_BIN" \
  --capture-dir "$LVU_TEST_ROOT/stdin-capture" --fresh \
  >"$LVU_TEST_ROOT/stdin.out" 2>"$LVU_TEST_ROOT/stdin.err"; status=$?
printf 'status=%s\n' "$status" | tee "$LVU_TEST_ROOT/stdin-status.txt"
sed -n '1,20p' "$LVU_TEST_ROOT/stdin.err"
```

Accept this boundary only when the status is nonzero and stderr says that stdin
pipe capture requires isolated nonblocking descriptors. Do not reinterpret the
refusal as stdin support.

## 6. Search and last-good behavior

Open the deterministic event fixture with `--fresh`.

1. Press `/`. Confirm the unified Filter dialog opens on its Search tab.
2. Search for `ERROR`; apply it and confirm only matching visible rows remain.
3. Reopen `/`, enter an invalid regular expression such as `[`, and attempt to
   apply it. Confirm the error is actionable and the last valid `ERROR` view
   remains usable.
4. Correct the draft, apply it, then use the dialog's `Clear` control. Confirm
   all records return.
5. Press `Esc` while editing and confirm the dialog closes without applying the
   draft.

## 7. Advanced filter and enrichment

The supported advanced-filter path is `/`, then `Alt-A`; `p` is retired.

1. Open `/`, press `Alt-A`, and confirm the Advanced tab is selected.
2. Apply a valid Polars expression such as:

   ```python
   pl.col("raw_text").str.contains("status=503", literal=True)
   ```

3. Enter an invalid expression and verify the last valid applied view remains
   available.
4. Switch back with `Alt-S`, then close with `Esc`.

Open Enrich and add these deterministic extraction steps in order:

```text
/^stamp<(?P<timestamp_utc>[^>]+)>/
/^stamp<[^>]+> (?P<level>INFO|WARN|ERROR|DEBUG) /
/latency=(?P<latency_ms>\d+)ms/
/(?P<error_start>ERROR)/
```

Add a derived expression named `slow`:

```python
pl.col("latency_ms").cast(pl.Int64) > 5000
```

Verify extracted fields align with the selected raw record, derived values do
not shift across records, and invalid edits preserve the last valid enrichment
chain and view.

## 8. Multiline grouping

`m` and `z` both open the unified **Multiline grouping** dialog; they are not
separate direct toggles.

1. Keep `events.log` open after accepting the enrichment chain, then press `m`.
2. Select Run and choose the accepted `level` output. Apply it and verify only
   consecutive equal, non-null values form runs; grouping does not reorder or
   delete physical records.
3. Reopen with `z`, select Filter and choose the nullable `error_start` output.
   Apply it and verify each non-null ERROR value starts a group whose following
   null-valued records continue until the next start.
4. Select Off, apply, and confirm the ungrouped physical records return.
5. Enter an invalid or unavailable column candidate and verify the last applied
   grouping remains usable with an actionable error.
6. Legacy is compatibility-only. If it is inspected, open `multiline.log`,
   select Legacy Custom, and use `^(\\s+)` to group the two indented continuation
   lines with the first record. Do not confuse this restored-settings path with
   normal Run or Filter behavior.

## 9. Fields, color, severity, time, folding, and correlation

Open the Fields surface and validate the bare-key controls shown by the running
`v0.1.6` executable:

| Key | Expected action |
| --- | --- |
| `Space` | pin or unpin field |
| `f` | include/filter by field value |
| `x` | exclude field value |
| `c` | toggle row coloring by the selected field |
| `s` | assign severity role |
| `t` | assign timestamp role |
| `d` | fold by field |
| `r` | correlate by field |
| `o` | open raw context for the selected value/record |

The former `Alt-P`, `Alt-F`, `Alt-X`, and `Alt-D` checklist shortcuts are
retired. Confirm the footer and behavior agree with the table rather than using
those old paths.

Using the extracted `level`, `timestamp_utc`, and `latency_ms` fields:

1. Assign severity to `level`; verify INFO/WARN/ERROR/DEBUG presentation is
   consistent and raw bytes remain unchanged.
2. Assign time to `timestamp_utc`; verify ordering and displayed timestamps
   correspond to the fixture.
3. Within Fields, press `c` on `level`; verify rows are colored by the selected
   field, then press it again to clear that field coloring.
4. Back on the base screen, press `c` to open **Colour rules**. Create a rule on
   the accepted `level` enrichment output with exact value `ERROR`; verify it
   affects ERROR only and can be removed.
5. Pin a field, fold by an appropriate field, and correlate on a deterministic
   value. Clear each action and confirm the full raw view remains available.
6. Apply include and exclude from Fields, and verify each produces the expected
   membership without losing the prior last-good view on an invalid edit.

## 10. Details and raw-context round trip

1. Select a filtered record in a non-source view and press `o`.
2. Confirm the app jumps to the same stable record in the source's **All events**
   view and retains enough context to identify it.
3. Press `o` again. Confirm the prior view, record, and dialog context are
   restored.
4. Open record details and confirm raw text/bytes remain available alongside
   derived fields.

This round trip replaces the older checklist's ambiguous instruction to “record
which `o` behavior occurs.”

## 11. Bookmarks, palette, and help

1. Bookmark several deterministic records, navigate away, and return to each.
2. Open the command palette, run representative navigation and view actions,
   and verify disabled/unavailable actions explain their state.
3. Open help and compare the displayed shortcuts with actual behavior in this
   plan. Record the known stale `p` mention from command-line help separately.
4. Confirm escape paths return to the prior usable surface without losing an
   applied filter or enrichment.

## 12. Human terminal-emulator matrix — explicitly unaccepted

These checks require a person observing the actual terminal emulator. They were
not run or accepted during this documentation refresh. Record Terminal.app and
iTerm2 independently; a pass in one does not imply a pass in the other.

### Color and truecolor

- Run the exact-value color-rule steps with each emulator's normal profile.
- Record `TERM`, `COLORTERM`, emulator version, and whether colors are distinct,
  readable, and restored after closing dialogs.
- Change no global profile settings merely to force a pass. A hosted kernel PTY
  cannot prove truecolor rendering.

### Unicode and cell geometry

- Open `unicode.log` and inspect combining marks, wide CJK characters, and emoji.
- Move selection/cursor across each line and verify columns, clipping, details,
  mouse hitboxes, and footer geometry remain aligned at narrow and wide sizes.
- Verify the literal text `\\x1b[31m` is data, not interpreted control output.

### Mouse and clipboard

- Exercise scrolling, row selection, tabs, and dialog controls with the mouse.
- Exercise the app's copy request and separately verify whether content reaches
  the system clipboard.
- Terminal.app does not provide OSC 52 clipboard delivery. A displayed copy
  request is not proof of delivery; record request and confirmed delivery as
  separate outcomes.

### Resize and suspend/resume

- Resize repeatedly across narrow and wide layouts while a dialog is open and
  while a record is selected.
- Suspend/resume only through documented shell/job-control behavior available in
  the tested build. Verify the cursor, echo, canonical mode, and alternate screen
  are correct before, during, and after the cycle.

### Option/Meta keys

- Record the emulator profile's Option-key setting.
- Use `"$LVU_BIN" --keys` in that same profile to capture the bytes generated by
  `Alt-A`, `Alt-S`, and other Option/Meta combinations, then exit the diagnostic.
- Verify `/` plus `Alt-A`/`Alt-S` works in the app. Treat an emulator mapping
  difference as environment evidence, not as an invented key equivalent.

## 13. Human exit and restoration — explicitly unaccepted

These observations were not run during the documentation refresh:

1. Normal `q` exit from the file fixture.
2. Normal exit while a command source and its child are alive.
3. A safe startup failure, for example a capture-root path whose parent is a
   regular fixture file, with the exact invocation and error retained.
4. A panic/failure path only if an existing safe diagnostic can induce it; do not
   modify or corrupt the installation to manufacture one.

The safe invalid-capture-root case in row 3 can be invoked without touching
anything outside the fixture:

```sh
printf 'not a directory\n' >"$LVU_TEST_ROOT/not-a-directory"
stty -a >"$LVU_TEST_ROOT/stty-before-startup-failure.txt"
"$LVU_BIN" --capture-dir "$LVU_TEST_ROOT/not-a-directory/capture" \
  --fresh "$LVU_TEST_ROOT/events.log" \
  >"$LVU_TEST_ROOT/startup-failure.out" \
  2>"$LVU_TEST_ROOT/startup-failure.err"; status=$?
stty -a >"$LVU_TEST_ROOT/stty-after-startup-failure.txt"
printf 'status=%s\n' "$status" >"$LVU_TEST_ROOT/startup-failure.status"
test "$status" -ne 0
```

For each applicable path, retain before/after `stty -a`, the invoking terminal,
the resolved binary hash, and identity-scoped process evidence. Accept only when
echo, canonical input, cursor visibility, alternate screen, and owned child
cleanup are restored. Do not kill unknown PIDs and do not count `SIGKILL` as a
graceful restoration path.

## 14. Long-scale checks — paused and unrun

The following existing acceptance work remains intentionally paused. This
documentation refresh does not activate it and supplies no passing evidence:

- the historical 200,000-line interactive fixture;
- a 512 MB capture/import exercise;
- cold-query latency measurement;
- slow-storage/autosave behavior;
- soak duration, sustained append, or RSS/resource ceilings;
- long command-process lifecycle tests.

Run these only under a separately approved performance/soak plan with explicit
time, storage, cleanup, and evidence bounds. Do not extrapolate from the small
deterministic fixtures in this checklist.

## 15. Result record

For every executed row, retain:

- `PASS`, `FAIL`, `BLOCKED`, or `UNRUN` (never silently omit a row);
- machine architecture and macOS version;
- Terminal.app or iTerm2 version and profile-relevant settings;
- formula version, resolved executable path and SHA-256;
- `lvu --resources` output and resource origins;
- deterministic fixture SHA-256 values;
- exact keystrokes/commands, observed result, and evidence path;
- identity-scoped cleanup results for command tests;
- known-boundary classification rather than a support claim.

Clean up only the root created by this plan after its evidence has been copied
to the approved durable location. Confirm its marker before removal:

```sh
test -f "$LVU_TEST_ROOT/.lvu-macos-acceptance-fixture" && \
  printf 'fixture root ready for reviewed cleanup: %s\n' "$LVU_TEST_ROOT"
```

Do not turn that guarded inspection into automatic deletion in a shared or
unreviewed shell transcript.

## Known macOS boundaries for `v0.1.6`

| Area | Current boundary |
| --- | --- |
| Published package | Immutable `v0.1.6` artifacts exist for arm64 and x86_64 Darwin. |
| Automated native evidence | Dedicated platform validation proved arm64 kernel PTY/runtime cleanup; its separate Intel job was compile-only. Release workflow `34440412338` separately executed all four published archives, including Intel Darwin. |
| Human terminal acceptance | Terminal.app, iTerm2, truecolor, mouse, clipboard, resize, Unicode geometry, and Option/Meta remain unaccepted until this matrix is run. |
| Piped stdin | Explicitly unsupported on macOS; expect the isolated-nonblocking-descriptors refusal. |
| Command resume | Bare launch and `--resume` re-acquire remembered command sources with recorded cwd/environment. |
| Forced death | No Darwin parent-death guarantee; `SIGKILL` can leave command children and is not graceful-cleanup evidence. |
| Clipboard | A copy request is distinct from confirmed delivery; Terminal.app lacks OSC 52 delivery. |
| Long scale | 200k-line, 512 MB, cold-query, slow-storage, soak, and RSS checks remain paused/unrun. |
| CLI help | The published final help sentence still names retired `p`; use `/`, then `Alt-A`. |
