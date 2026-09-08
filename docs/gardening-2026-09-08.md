# Repository gardening inventory, 2026-09-08

Every checked-in file or directory outside `crates/`, plus every crate-level
file that is not source. Read against main `14ad91c` (preview 054, `v0.1.0`
tagged). Nothing was deleted by the commit that adds this file; the verdicts
were applied in the commit after it.

Columns: what it is; who or what still references it (grep for the path and
for the title across code, docs, mise tasks, CI and tests); whether its
content is still true against the current code; the verdict with a one-line
reason. Verdicts are **keep**, **merge into X**, **rewrite** or **delete**.
"keep (patch)" means keep with a targeted correction listed under the table.

## Documentation convention applied alongside

The 🧠 glyph appears in docs only when naming the specific control or dialog
that shows it (`Ask 🧠`, `Investigation 🧠`, `Source 🧠`, the `🧠` button, a
quoted screen). Prose says "assistance", "the agent", "the bridge" or "AI".
Other glyphs the app prints (`✖`, `⚙`, `◐`, `★`, `▾`, `›`) stay wherever a
doc quotes a screen; the `✓` ticks in an audit table were decorative and are
replaced with words.

## Root

| Path | What it is | Referenced by | Still true? | Verdict |
| --- | --- | --- | --- | --- |
| `AGENTS.md` | Working rules for agents: invariants, coordination, validation, documentation. | Every assignment; `docs/development.md`, `docs/README.md`. | Yes. Its Documentation section did not yet state the icon convention. | **keep (patch)**: add the icon convention in one sentence. |
| `README.md` | The product README: install, sixty seconds, guarantees, assistance prerequisites. | `packaging/stage.sh` stages it into the archive; `docs/*`. | Yes. Four prose uses of 🧠 (lines 9, 19, 41, 106). | **keep (patch)**: apply the icon convention. |
| `TODO.md` | The single feedback and work list, by area, plus rows shipped since preview 047. | `AGENTS.md`, `docs/development.md`, `docs/work-ledger.md`. | Mostly. Line 3 says preview 054 while the section heading at line 9 says preview 052; the W22 FOLLOW row appears twice (lines 90 and 92, byte-identical); the portability row (line 102) does not point at the macOS plan that discharges it. `docs/columnar-cache.md` (line 91) is a planned file, not a missing one. | **keep (patch)**: fix the heading, drop the duplicate row, link the macOS plan. |
| `Cargo.toml`, `Cargo.lock` | Workspace manifest and lockfile; profile opt-levels recorded in `docs/performance.md`. | Every build; `.github/workflows/release.yml` (`--locked`). | Yes. | **keep** |
| `mise.toml` | Pinned tools, environment and every developer task. | `AGENTS.md`, `docs/development.md`, CI (`jdx/mise-action`). | Yes; every task's script path exists. | **keep** |
| `.gitignore` | `target/`, `node_modules/`, `.venv/`, caches, `*.log`, `/previews/`, `/.lvu-captures/`. | git. | Missing a pattern for the `lvu-journal-test-*` scratch that a core test writes into the working directory (see below). | **keep (patch)**: add `lvu-journal-test-*`. |
| `LICENSE`, `LICENSE-MIT`, `LICENSE-APACHE` | Dual licence text; crates declare `MIT OR Apache-2.0`. | `packaging/stage.sh` refuses to stage without all three; `packaging/README.md`. | Yes. The README does not mention the licence; minor. | **keep** |
| `lvu-journal-test-6ed5ba71-….one`, `.one.lock` | Zero-byte scratch left by `relative_paths_writer_lock_and_collision_safe_sidecars_work` in `crates/lvu-core/tests/journal.rs`, which opens journals by relative path in the working directory and only removes them on success. Committed by accident in `410e369`. | Nothing. | Not content; an artifact. | **delete** and gitignore the pattern. |
| `.github/workflows/release.yml` | The only workflow: per-target archives via `packaging/stage.sh`, draft release with `SHA256SUMS`. | `docs/distribution.md`, `docs/release-runbook.md`, `packaging/README.md`. | Yes; the docs it names exist. | **keep** |

## `docs/`

| Path | What it is | Referenced by | Still true? | Verdict |
| --- | --- | --- | --- | --- |
| `docs/README.md` | One-line index of the design documents. | Root README. | Incomplete: omits `release-runbook.md`, `mac-test-plan.md` and `settings.example.toml`; lists files this pass removes. | **rewrite** to match the outcome. |
| `docs/architecture.md` | The implementation map: crates, data flow, persistence, expression boundary, terminal rules. | `AGENTS.md`, `README.md`, `docs/README.md`, `docs/development.md`, `docs/merged-view-ordering.md`, `crates/lvu-app/README.md`. | Body is largely accurate (the app-crate file names it lists exist). Header says "checked 2026-09-06, preview043 is the published baseline" against preview 054 and a tagged `v0.1.0`; the component map omits `crates/lvu/src/components/`; "schema-v4" at line 225 (code is v6); "mise tasks listed in the README" at line 276 (the README lists one). | **keep (patch)**: header, components row, schema version, task pointer. |
| `docs/command-chain.md` | §8.14 change note: the old single-slot model, the new chain model, an invariant audit and five decisions for the user. | `docs/README.md`; `docs/dialog-system.md:878`. | The model table and decisions are true; its quote of the old `command-enrichment.md` wording no longer exists there. Everything except the decisions is now restated in `command-enrichment.md`. | **merge into `command-enrichment.md`**: carry the decisions and the "what was not built" list; delete the file. |
| `docs/command-enrichment.md` | The external command step contract: boundary, protocol, limits, durability, recipes, storage. | Eleven source and test files (`command_controller.rs`, `command_columns.rs`, `recipe.rs`, `export.rs`, …), `docs/architecture.md`, `docs/README.md`. | Already rewritten to the chain model. "Workspace schema v4" in Storage is stale (v6). | **keep (patch)**: absorb the decisions, fix the schema version. |
| `docs/component-model.md` | The component contract, `Ctx`, messages, hit regions, layering, conversion order and recorded deviations. | The most-cited doc: `component.rs`, `components/mod.rs`, `app.rs`, twelve `component_*` test suites, `README.md`, four docs. | The contract is what the code implements. §6.3's status column lags: steps 3, 5, 10, 12, 13 have components on main (`fields.rs`, `bookmarks.rs`, `settings.rs`, `ask.rs`/`investigation.rs`, `enrichment*.rs`); only step 4 (Raw context) is outstanding, held for the `o` decision. | **keep (patch)**: mark the done steps, take the "why" paragraph from `module-partition.md`. |
| `docs/contracts.md` | Shared semantic boundaries: identities, records, helper and command protocols, manifests, cache classes. | `AGENTS.md`, `docs/README.md`, `docs/development.md`, `docs/architecture.md`, `docs/command-enrichment.md` (anchor resolves). | Yes. | **keep** |
| `docs/development.md` | Building, validation, preferences and storage reference, what to read first. | `README.md`, `docs/README.md`, `docs/portability.md`, `docs/work-ledger.md`. | Three stale claims: "the app resolves helpers from its build checkout; a copied binary is not portable" (resources resolution and relocatable archives exist since `v0.1.0`); "workspace schema v4" (v6); the pointer to preview notes for binary acceptance (previews now carry their own `manifest.json`). | **keep (patch)** |
| `docs/dialog-audit-captures.md` | 3,216 lines of raw `TestBackend` screen dumps from main `1d0c44f`, the evidence for the audit section of `dialog-design.md`. | `docs/README.md`, `docs/dialog-design.md`, `docs/dialog-system.md:17`, `docs/dialog-default-actions.md:6`, `docs/dialog-system-captures.md`. | No. Add source is captured as a button row (now a segmented control), the `Ask 🧠 for a source — preview never executes` title no longer exists in the code, the Ask kind dropdown is now conditional, and the 🧠 width fix post-dates them. No script regenerates it. | **delete**: superseded evidence for dialogs that no longer exist; `dialog-system.md` restates every finding. |
| `docs/dialog-system-captures.md` | 1,581 lines of pyte screens from the real PTY harness at `625e1de`/`a6813f0`, plus 2026-09-08 re-captures on `ba793af`. | `docs/README.md`, `docs/dialog-system.md:14`, `docs/dialog-design.md:8`, `docs/dialog-default-actions.md:7`. | Partly. The Time row is current; the Source mode row and the Ask-for-a-source title are stale. Hand-made, with no `mise` task to refresh it, so it drifts by construction. | **delete**: the PTY suites under `tests/pty/` are the living evidence; `dialog-system.md` keeps its own "before/after" screens. |
| `docs/dialog-design.md` | The earlier presentation note: meaning-per-treatment table, footer and validation rules, keyboard policy, and a 250-line rendered-evidence audit at `1d0c44f`. | `docs/README.md`, `docs/architecture.md`, `docs/dialog-system.md` (eight cites), and three code comments (`dialog_layout.rs:470`, `details_colouring.rs:338`, `components/external_command.rs:4`). | The layout guidance is superseded by `dialog-system.md`, which says so. Three things exist only here: the keyboard-binding policy (no PgUp/PgDn/Home/End; a dropdown owns arrows), the meaning table, and the colour-test rule (clear inherited `NO_COLOR`, request truecolor). The audit is findings against a three-revision-old commit. | **merge into `dialog-system.md` §8**; leave a short stub because code comments cite the file by name and code is not touched here. |
| `docs/dialog-default-actions.md` | Audit of every dialog against §8.9 (one default action). | `TODO.md:131`, `docs/README.md`, `docs/architecture.md`, `docs/dialog-discoverability.md`, `docs/field-exploration.md`, `docs/dialog-system.md`; tests `default_actions.rs`, `test_default_actions_pty.py`. | The audit is complete and correct as a record. Its status line names a landed branch; rows 17 to 19 call Bookmarks, Ask and Investigation "legacy" with conversion obligations that have since been met; `✓` ticks are decorative. | **keep (patch)**: status line, the three converted rows, ticks to words, drop the captures pointer. |
| `docs/dialog-discoverability.md` | Audit against §8.10 (where a user learns what they can do). | `TODO.md:121`, `docs/README.md`, `docs/architecture.md`, `docs/field-exploration.md`; tests `discoverability.rs`, `test_discoverability_pty.py`. | Complete and correct; status line names a landed branch; rows 18 and 19 say "legacy". | **keep (patch)**: status line and the two rows. |
| `docs/dialog-system.md` | The dialog specification: classes, sizing, message row, controls, shared rules §8, one section per dialog §12. | Sixty-odd source, test and PTY files cite it by section; every dialog doc. | Yes; consolidated against the code on 2026-09-08. The evidence paragraph names the two capture files and `dialog-design.md`'s audit. One prose use of 🧠 at line 321. | **keep (patch)**: absorb the three `dialog-design.md` rules, rewrite the evidence paragraph, retarget the `command-chain.md` cite, icon convention. |
| `docs/distribution.md` | What the archives contain, the musl baseline, resource resolution, end-user commands. | `README.md`, CI, `packaging/*`, `crates/lvu-app/Cargo.toml`, five docs. | The body is true. The status paragraph says "there is no `v0.1.0` tag, no release and no formula in the tap", which contradicts the runbook, the tag and the tap. Four prose uses of 🧠. | **keep (patch)** |
| `docs/field-exploration.md` | Audit and design for §8.11 to §8.13: JSON tree, Value pane, path picker. | `docs/README.md`, `docs/architecture.md`, `docs/text-time-basis.md`; tests `field_exploration.rs`, `test_field_exploration_pty.py`. | Yes. Status line names a landed branch. | **keep (patch)**: status line. |
| `docs/implementation-plan.md` | The original full requested scope, milestones M1 to M7 with exit gates, delegation policy. | `AGENTS.md`, `docs/README.md`, `docs/architecture.md`, `docs/development.md`. | §1 exclusions and §2 invariants are duplicated in `AGENTS.md`; §11's exit gates are recorded nowhere else. §3's proposed crate layout names `lvu-state` (does not exist) and omits six real crates; §12's initial worktrees are long gone. | **keep (patch)**: stamp §3 and §12 as superseded rather than rewriting 367 lines. |
| `docs/mac-test-plan.md` | A macOS acceptance checklist an agent can execute against `v0.1.0` from the tap. | **Nothing.** | Yes, and it discharges the TODO portability row. One prose use of 🧠. | **keep (patch)**: link it from `docs/README.md` and `TODO.md`; icon convention. |
| `docs/merged-view-ordering.md` | Design for interleaving a merged view by time, with the invariants and the ignored tests that pin them. | `TODO.md`, `crates/lvu-view/tests/merged_ordering.rs:1`, `docs/README.md`, `docs/raw-context-as-jump.md`. | Design and tests match; the status line says it waits for W24, which is Done. | **keep (patch)**: status line. |
| `docs/module-partition.md` | Why `crates/lvu` had to be split for parallel work, a proposed file split, and a revised sequencing that abandons the split. | `docs/README.md`, `docs/component-model.md:4`. | Its numbers are obsolete (`app.rs` 11,302 lines, now 8,158; `Action` 180 variants, now 58); neither proposed layout exists; the file concedes at line 89 that the split is no longer planned, and its last section is labelled superseded. | **merge into `component-model.md`**: one "why" paragraph; delete the file. |
| `docs/performance.md` | The capture and query baseline, the read-ahead follow-up, the two-minute run, and the 2026-09-08 filter latency analysis. | `TODO.md:89`, `docs/README.md`, `docs/work-ledger.md`. | Yes; the numbers match TODO and `scan_throughput.rs`; `bench:live` tasks exist. The 2026-09-05/06 tables predate the opt-level change and say so implicitly. | **keep** |
| `docs/portability.md` | Source-only audit of macOS and Windows readiness at `bef81ce`, with a prioritised plan. | `TODO.md:102`, `docs/README.md`, `docs/release-runbook.md`, `docs/mac-test-plan.md`. | Mostly. Resolved since: relocatable resource resolution (§6) and Darwin CI targets (its CI item 1). Still open: the Linux-only `cfg`s in discovery and live, the piped-stdin refusal, the ungated `os::unix` imports. Line refs in §6 are off by hundreds of lines. | **keep (patch)**: add a "resolved since" note at the top. |
| `docs/previews.md` | Narrative history of previews 001 to 043 with source, checksum and acceptance, and the only record of which preview introduced which workspace schema. | `docs/README.md`, `docs/architecture.md`, `docs/development.md`, `docs/implementation-plan.md`, `assets/startup/love-you-log-time/README.md`. | Stops eleven previews short; the header's retained set and `/tmp` inventory are unverifiable; previews now carry `previews/<n>/manifest.json` locally and TODO lists shipped rows per preview. Its schema facts stop at v4; the code is at v6 and v5/v6 are documented nowhere. | **rewrite**: a short note on where previews are recorded plus the schema compatibility table; the narrative stays in git history at `14ad91c`. |
| `docs/raw-context-as-jump.md` | Design, pending decision: replace the Raw context dialog with a jump to All events and a way back. | `TODO.md:17` (W21, Working), `docs/README.md`, `docs/dialog-system.md` (four cites), `docs/mac-test-plan.md`. | Yes; nothing built yet, consistent with `ui.rs` still rendering `[ Back to anchor ]`. | **keep**: W21 is building it; this becomes the record. |
| `docs/release-runbook.md` | The exact commands that publish a release and cut the next one. | `docs/distribution.md`, `packaging/README.md`, CI release notes. Not in `docs/README.md`. | Yes. | **keep (patch)**: list it in the index. |
| `docs/settings.example.toml` | Annotated example of the global settings file. | `docs/architecture.md`, `docs/development.md`. Not in `docs/README.md`. | Keys match `settings.rs` except `appearance.display_zone` (added with the Time display work) is missing. | **keep (patch)**: add `display_zone`, list it in the index. |
| `docs/storage.md` | Durability classes, ownership pins, the deletion ledger, retention, cache pressure. | `crates/lvu-app/src/storage.rs:19`, `docs/README.md`. | Yes; the five modules it names exist and the caveat about the older dialog scan is still true. | **keep** |
| `docs/supervisor-handoff.md` | A 2026-09-06 operational snapshot for a replacement supervisor: agent UUIDs, worktree paths, `/tmp` build slots, preview-041-era observations. | `docs/README.md`; a historical mention in the ledger. | No. Predates the W-number scheme; cites main at `31b5b04` and "latest 041"; depends on `/tmp` files; its build-slot rules (`/tmp/lvu-discovery-ui-target`) contradict `AGENTS.md`. Its one durable output became `dialog-system.md`. | **delete** |
| `docs/text-time-basis.md` | Landed-change note: a text enrichment column as the event-time basis with an inferred format. | `docs/README.md`. | Yes; `text_format` exists in `time_basis.rs` and `time.rs`, with the tests named. | **keep** |
| `docs/work-ledger.md` | The primary agent's validation evidence, ~140 sections; undated M1-era entries through line 1278, dated entries from 2026-09-05. | `AGENTS.md` ("the current entries"), `TODO.md:7,108` (links a section by title), `docs/development.md`, `docs/README.md`. | Evidence is evidence; it does not go stale. But 2,600 of 2,860 lines precede 2026-09-07, so "current entries" is a judgement over the whole file. | **keep**, split: entries before 2026-09-07 move verbatim to `docs/work-ledger-archive.md`; the three current sections stay, including the one TODO links. |

## `packaging/`

| Path | What it is | Referenced by | Still true? | Verdict |
| --- | --- | --- | --- | --- |
| `packaging/README.md` | Archive layout, local build and verification steps, contents, known gaps. | `docs/distribution.md`, `docs/release-runbook.md`. | Yes. Duplicates the archive-layout block from `distribution.md` verbatim; the rest is unique. | **keep** |
| `packaging/stage.sh` | Builds, stages and verifies one target's archive by running it. | `mise` (via `mise exec`), CI, both release docs. | Yes. | **keep** |
| `packaging/homebrew/lvu.rb.in`, `render-formula.sh` | Formula template and the renderer that takes checksums from `SHA256SUMS`. | Runbook, CI publish step, `distribution.md`. | Yes. | **keep** |

## `scripts/`

| Path | What it is | Referenced by | Still true? | Verdict |
| --- | --- | --- | --- | --- |
| `scripts/janitor.py` | Reclaims stale cargo artifacts, orphaned PTY scratch and command-source orphans; never touches previews or captures. | `mise janitor`, `janitor:dry`, `matrix:preflight`; `AGENTS.md`, `TODO.md`, `Cargo.toml`. | Yes. | **keep** |
| `scripts/pty-matrix.py` | Runs every `tests/pty/test_*_pty.py` concurrently, longest first, with load-aware starts; skips `test_ssh_pty.py`. | `mise test:pty:matrix`. | Yes. | **keep** |
| `scripts/preview.sh` | `mise run preview`: execs `previews/latest/lvu` with the app's usage flags. | `mise preview`. | Yes; reads no manifest, which is fine. | **keep** |
| `scripts/play-ansi-animation.py` | Plays an `animation.toml` frame set in the terminal. | `mise art:preview`, `art:preview:large`; the title README. | Yes. | **keep** |
| `scripts/convert-ansi-animation.py` | Chafa conversion of a GIF to timed ANSI frames. | Both asset READMEs (regeneration recipe). | Yes; the output layout matches the checked-in `animation.toml`. | **keep**: the only way to regenerate the art. |
| `scripts/convert-heartbeat-sheet.py` | Crops the four-frame heartbeat sprite sheet into a GIF for conversion. | `assets/indicator/heartbeat/README.md`. | Yes (hard-codes the supplied sheet geometry, by design). | **keep** |
| `scripts/sharpen-ansi-lettering.py` | Sharpens the lower title lettering of a converted frame set. | The title README. | Yes; the embedded variants are the sharpened ones. | **keep** |
| `matrix:preflight` (in `mise.toml`, not a script) | Disk check plus a janitor dry run before the matrix. | `test:pty:matrix` depends on it. | Yes. | **keep** |

## `tests/`

| Path | What it is | Referenced by | Still true? | Verdict |
| --- | --- | --- | --- | --- |
| `tests/pty/test_*_pty.py` (61 suites) | Real-terminal workflow suites over the demo `lvu` or the real `lvu-app`. | `pty-matrix.py` globs all but `test_ssh_pty.py`; 31 have their own `mise` task; `test_ssh_pty.py` runs through `test:pty:ssh` only. | Yes. | **keep** |
| `tests/pty/test_screen_text.py` | A `unittest` regression for the pinned pyte emulator's wide-cell stub, exercising the harness's `PtyApp.text()`. | Nothing: the matrix glob is `test_*_pty.py` and no task names it. | Yes as a test; it is simply not run by anything. | **keep**; follow-up: run it from the matrix or a task (not done here, tests are out of scope). |
| `tests/pty/test_lvu_pty.py` | Also the harness module (`PtyApp`) every other suite imports. | Every suite; `mise test:pty`. | Yes. | **keep** |
| `tests/pty/pyproject.toml`, `uv.lock`, `.gitignore` | Locked `pyte==0.8.2` environment. | Every `uv run --project tests/pty` task. | Yes. | **keep** |
| `tests/soak/soak.py`, `generate.py`, `slow_terminal.py` | The mixed-workload soak, its deterministic generator, and the slow-link relay. | `mise soak`, `soak:long`, `soak:slow-terminal`; `soak.py` imports `generate.py`. | Yes; TODO marks the harness Ready. | **keep** |

## `bridge/`, `python/`, `spikes/`

| Path | What it is | Referenced by | Still true? | Verdict |
| --- | --- | --- | --- | --- |
| `bridge/src/*.ts`, `test/*.test.ts`, `test/fake-backend.ts`, configs, `package*.json` | The TypeScript Paseo adapter and its vitest suite. | `mise install:bridge`, `test:bridge`, `build:bridge`, `check:bridge`; CI; `packaging/stage.sh`. | Yes. | **keep** |
| `bridge/README.md` | Bridge protocol, environment limits, entry point. | `docs/development.md`. | Yes (spot-checked the method list against `protocol.ts`). | **keep** |
| `bridge/VALIDATION.md` | The M1C assignment's completion report: commands run, test counts at the time. | Nothing. | Historical; the ledger holds the acceptance. | **delete** |
| `bridge/test/fixtures/live-investigation/events.jsonl`, `manifest.json` | A recorded investigation fixture added with the first bridge commit. | **Nothing** loads it: no test, script or crate names the directory. | Not loaded. | **delete** |
| `python/lvu_expr_helper/*`, `python/tests/*`, `pyproject.toml`, `uv.lock`, `.gitignore` | The pinned Polars expression helper and its tests. | `mise test:expr:python`, `check:expr`; `crates/lvu-app/src/resources.rs`, `packaging/stage.sh`, four Rust test files. | Yes. | **keep** |
| `spikes/polars-interop/` (README, Cargo files, `src/main.rs`, `differential.py`, `fixtures/cases.json`) | The Python-to-Rust expression compatibility runner and differential check. | `mise build:expr`, `test:expr:rust`, `test:expr:interop`; `differential.py` loads `fixtures/cases.json`. | Yes; still the compatibility gate. | **keep** |

## `assets/`

| Path | What it is | Referenced by | Still true? | Verdict |
| --- | --- | --- | --- | --- |
| `assets/startup/love-you-log-time/80x22-sharp/`, `120x40-sharp/` | The embedded title animation, ten frames each. | `crates/lvu/src/delight/art.rs` (`include_str!`); `mise art:preview*`. | Yes. | **keep** |
| `assets/startup/love-you-log-time/80x22/`, `120x40/` | The unsharpened conversions the sharp variants were derived from. | Only their README. | Not loaded by code; regenerable from `source.gif`. | **keep**: 2.2 MB of the user's artwork pipeline, preserved on purpose per the README; flagged, not removed. |
| `assets/startup/love-you-log-time/source.gif`, `README.md`, `*/still-preview.png`, `*/ansi-preview.gif` | The supplied artwork, the regeneration recipe, and review renders. | README; the `mise` art tasks. | Yes. README's pointer to `docs/previews.md` for publication tracking is stale. | **keep (patch)** the pointer. |
| `assets/indicator/heartbeat/5x3/` | The embedded corner heartbeat. | `crates/lvu/src/delight/art.rs`. | Yes. | **keep** |
| `assets/indicator/heartbeat/7x4/`, `14x7/` | Earlier heartbeat conversions. | Only the README ("previous conversions are preserved") and two ledger entries. | Not loaded. | **keep**: same reasoning as the title intermediates; flagged. |
| `assets/indicator/heartbeat/source.gif`, `source-sheet.png`, `README.md` | The supplied sheet, the extracted GIF, the recipe. | README; `scripts/convert-heartbeat-sheet.py`. | Yes. | **keep** |

## Crate-level files that are not source

| Path | What it is | Referenced by | Still true? | Verdict |
| --- | --- | --- | --- | --- |
| `crates/*/README.md` (ten) | Per-crate ownership and seams. | `crates/lvu-app/README.md` cites `docs/architecture.md`; `docs/development.md` cites nothing here. | Headers spot-checked against each crate's modules; accurate at the level they speak. `lvu-app`'s says command enrichment "remains a separate library awaiting application integration", which shipped in preview 034, and has one prose 🧠. | **keep (patch)** `crates/lvu-app/README.md`; keep the rest. |
| `crates/lvu-discovery/IMPLEMENTATION_REPORT.md` | The M3 assignment's delivery report. | Nothing. | Historical. | **delete** |
| `crates/lvu-live/IMPLEMENTATION_REPORT.md` | The live adapter assignment's delivery report. | Nothing. | Historical. | **delete** |
| `crates/lvu-memory/VALIDATION.md` | The M5A assignment's validation report. | Nothing. | Historical. | **delete** |
| `crates/lvu-core/lvu-journal-test-*.one`, `.one.lock` (five pairs) | Zero-byte scratch from the same core test, run from the crate directory; committed in `410e369`. | Nothing. | Artifact. | **delete** and gitignore. |
| `crates/lvu/src/fixture.rs` | Source: the demo fixture the `lvu` executable and UI tests use. | The crate. | Source, listed for completeness. | **keep** |

## Artifacts to gitignore or remove

- `lvu-journal-test-*` at the root and under `crates/lvu-core/`: removed; pattern added to `.gitignore`.
- `bridge/test/fixtures/live-investigation/`: removed; nothing loads it.
- Nothing else checked in is a capture, log or scratch file. `previews/` and `.lvu-captures/` are already ignored; `*.log` is ignored globally.

## Verdict counts

Counted per table row above (a row covering a directory or a set counts once).

| Verdict | Rows |
| --- | ---: |
| keep (including keep with a patch) | 57 |
| merge into another doc | 3 |
| rewrite | 2 |
| delete | 10 |

Merges: `command-chain.md` into `command-enrichment.md`; `module-partition.md` into `component-model.md`; `dialog-design.md` into `dialog-system.md` (stub left for the code comments that cite it).

Rewrites: `docs/README.md`; `docs/previews.md` (short note plus the schema table).

Deletes: `docs/supervisor-handoff.md`; `docs/dialog-audit-captures.md`; `docs/dialog-system-captures.md`; `bridge/VALIDATION.md`; `bridge/test/fixtures/live-investigation/`; `crates/lvu-discovery/IMPLEMENTATION_REPORT.md`; `crates/lvu-live/IMPLEMENTATION_REPORT.md`; `crates/lvu-memory/VALIDATION.md`; the root `lvu-journal-test-*` pair; the five pairs under `crates/lvu-core/`.

## Left alone on purpose

- The unused title and heartbeat conversion variants under `assets/` (about 2.3 MB): the READMEs say they are preserved deliberately and they are part of the user's artwork pipeline. Removing them is a separate decision.
- `tests/pty/test_screen_text.py` is run by nothing. Wiring it into the matrix means editing a script or a task, which this pass does not do.
- `packaging/README.md` repeats the archive-layout block from `distribution.md`. Tolerable; both are short.
- `crates/*/README.md` beyond `lvu-app`: spot-checked headers only, not every claim.
- The `work-ledger.md` split moves text verbatim and changes no evidence.
