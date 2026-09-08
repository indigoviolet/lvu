# macOS acceptance test plan

For an agent or a person with no prior knowledge of lvu, on macOS. No lvu
binary has ever been run on a Mac by a human; CI proves only that the archive
starts, resolves its payload and compiles a Polars expression. Everything below
is therefore unverified on this platform, and a failure here is news.

**Build under test: `v0.1.0` from the tap.** Steps marked *(post-0.1.0)* need a
build from `main` and must be skipped otherwise; each says what 0.1.0 does
instead. Do not report a skipped step as a failure.

Record every step as PASS or FAIL. Report format is in [§13](#13-reporting-a-failure).

## 0. Prerequisites

| | |
| --- | --- |
| Homebrew | 6 or newer for `brew trust`. `brew --version` |
| Terminals | Terminal.app, iTerm2, and one truecolor terminal (Ghostty, WezTerm, Kitty or Alacritty) |
| `uv` | optional; required only by §5 and §6 |
| `node` | optional; required only by the assistance rows, which this plan does not cover |

```sh
brew trust indigoviolet/tap        # Homebrew 5 has no `trust`; skip the line
brew install indigoviolet/tap/lvu
brew install uv                    # optional
mkdir -p ~/lvu-test && cd ~/lvu-test
```

Fixtures, used throughout:

```sh
python3 - <<'EOF'
import random, datetime
lv=["INFO","WARN","ERROR","DEBUG"]
with open("big.log","w") as f:
    t=datetime.datetime(2026,9,8,10,0,0)
    for i in range(200000):
        t+=datetime.timedelta(milliseconds=random.randint(1,40))
        f.write(f"{t.isoformat()}Z {random.choice(lv)} status={random.choice([200,201,404,500,503])} "
                f"user=u{random.randint(1,50)} latency={random.randint(1,9000)}ms request handled id={i}\n")
EOF
printf 'plain ascii line\n' > small.log
printf '\xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e wide glyphs \xf0\x9f\x8e\x89 emoji\n' >> small.log
printf 'combining: e\xcc\x81 a\xcc\x80 n\xcc\x83 and ZWJ: \xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x92\xbb\n' >> small.log
printf 'tab\there\tand\ttabs\n' >> small.log
wc -l big.log small.log
```

Run every step in each of the three terminals unless the step says otherwise.
`Escape` closes a dialog; `q` quits from the base screen.

## 1. Command line

### 1.1 `lvu --help`

```sh
lvu --help
```

Must show: first line `lvu — live local log viewer`, second blank, third
`Usage: lvu [OPTIONS] [FILE ...]`. The string `lvu-app` must not appear.

PASS / FAIL: ______

### 1.2 `lvu --resources`

```sh
cd /tmp && lvu --resources
```

Must show, for both `Python expression helper` and `agent bridge`, the word
`found` and the line:

```
  origin: installed beside the executable
```

`origin: development checkout` is a failure. So is `missing` on a Homebrew
install. The `executable:` line must point inside
`/opt/homebrew/Cellar/lvu/0.1.0` (Apple silicon) or
`/usr/local/Cellar/lvu/0.1.0` (Intel), not into a source tree.

PASS / FAIL: ______

### 1.3 An unknown option

```sh
lvu --nonesuch
```

Must exit non-zero with a message naming the argument and pointing at `--help`.
**Known cosmetic defect in 0.1.0:** the message is prefixed `lvu-app:`, not
`lvu:`. Record it, do not report it as new.

PASS / FAIL: ______

## 2. First launch and sources

### 2.1 Startup with no sources

```sh
cd ~/lvu-test && lvu
```

Must open the Add source dialog. Startup art is drawn above or behind it.
Nothing may be garbled, and no escape sequence may appear literally.

PASS / FAIL: ______

Note which terminal, and whether the art rendered as blocks, as colour, or not
at all. All three are acceptable; a corrupted screen is not.

### 2.2 A file source from the dialog

From the Add source dialog: press `Alt-F`, type `big.log`, press `Tab` to
complete, press `Enter`.

Must show: the log pane fills with records, a footer, and a record count that
settles at 200000. Timestamps and levels are visible.

PASS / FAIL: ______

Press `q` to quit.

### 2.3 A file source from the command line

```sh
lvu big.log small.log
```

Must show: both sources open. `]` and `[` switch views. `G` jumps to the end,
`g` to the start.

PASS / FAIL: ______

### 2.4 A command source

```sh
lvu --command 'for i in $(seq 1 200); do echo "tick $i"; sleep 0.05; done'
```

Must show: lines appearing live. `f` toggles following the tail; with follow on,
the view stays pinned to the newest line.

PASS / FAIL: ______

Let it finish, confirm the child is reaped (no `sleep` left):

```sh
pgrep -fl 'seq 1 200' || echo "no orphan"
```

PASS / FAIL: ______

### 2.5 stdin

```sh
seq 1 5000 | lvu
```

Must show: 5000 records, and the terminal is still interactive (lvu reads keys
from `/dev/tty`, not stdin). Press `q`.

PASS / FAIL: ______

*docs/portability.md reads piped stdin capture as a macOS risk. If this fails,
capture the exact message — it is one of the two behaviours most expected to
differ here.*

### 2.6 Resume by default

```sh
cd ~/lvu-test && lvu big.log     # quit with q
cd ~/lvu-test && lvu             # no arguments
```

Must show: the second launch re-acquires `big.log` without being told to, and
does not re-capture from the beginning — the record count does not double.

PASS / FAIL: ______

A restored **command** source must not relaunch on its own. Repeat with §2.4's
command, quit, relaunch bare, and confirm the command is listed but not running.

PASS / FAIL: ______

### 2.7 `--fresh` *(post-0.1.0)*

`--fresh` and `--resume` are not in 0.1.0. On the released build:

```sh
lvu --fresh
```

must fail with `unknown argument "--fresh"`. That is the correct 0.1.0
behaviour. On a build from `main`, `lvu --fresh` must instead start with no
restored sources.

PASS / FAIL / SKIPPED: ______

## 3. Search

Open `lvu big.log` for §3-§9.

### 3.1 Literal

Press `/`, type `ERROR`, press `Enter`.

Must show: the view narrows as you type; only matching records remain; the
match count is shown. Press `Escape` to clear.

PASS / FAIL: ______

### 3.2 Field-scoped

Press `/`, type `status: 500`, `Enter`. Must narrow to those records.

PASS / FAIL: ______

### 3.3 Regex

Press `/`, type `/status=5\d\d/`, `Enter`. Must match 500 and 503 and nothing
else.

PASS / FAIL: ______

### 3.4 An invalid regex leaves the view usable

Press `/`, type `/[unclosed/`, `Enter`.

Must show: a message naming the problem, and **the previous valid view still on
screen**. lvu must not clear the pane, must not exit, and must not show an
empty result as if it were a real answer.

PASS / FAIL: ______

## 4. Advanced filter (needs `uv`)

Press `p`. Type:

```
pl.col('status') >= 500
```

Press `Enter`.

Must show: the view narrows to 500 and 503 records. First use may pause while
`uv` provisions CPython 3.12 and Polars — that is expected, once.

PASS / FAIL: ______

Without `uv` installed, `p` must report the helper unavailable with an
actionable message and leave everything else working. If you have `uv`, verify
this too:

```sh
env PATH=/usr/bin:/bin lvu big.log     # uv not on PATH
```

PASS / FAIL: ______

Now an invalid expression: press `p`, type `pl.col('nope' >= `, `Enter`. Must
report the error and keep the last accepted view.

PASS / FAIL: ______

## 5. Enrichment on a large file (needs `uv`)

Press `e`. Add a step:

```
/latency=(?P<ms>\d+)ms/
```

Must show: a new `ms` column, populated across the file, with the record count
unchanged. Confirm on a record near the end (`G`) as well as the start.

PASS / FAIL: ______

Edit the step to a Polars expression:

```
slow = pl.col('ms').cast(pl.Int64) > 5000
```

Must show: a `slow` column of booleans consistent with `ms`. Values must line up
with the right records — spot-check three rows against their raw text in
Details (`d`).

PASS / FAIL: ______

Delete the step. The columns must disappear and the record count must not
change.

PASS / FAIL: ______

## 6. Folding

Press `z`.

Must show: repeated patterns collapse; a gutter marks folded groups with a
count. The record count a filter reports must **not** change — folding is
presentation.

PASS / FAIL: ______

Press `z` again. Every record returns and the gutter clears.

PASS / FAIL: ______

Press `m` for multiline grouping and confirm it toggles the same way.

PASS / FAIL: ______

## 7. Time

### 7.1 Time dialog

Press `t`.

Must show: a window over capture time, event time or an extracted timestamp.
Set a rolling window (for example the last 5 minutes of event time) and apply.
The view must narrow and say which basis it used.

PASS / FAIL: ______

### 7.2 Gap jumps

Press `}` then `{`.

Must show: the selection jumps forward to the next gap in time, and backward to
the previous one. At the last gap, `}` must not wrap silently or move the
selection off-screen.

PASS / FAIL: ______

## 8. Fields, Details, Bookmarks

### 8.1 Fields with the Value pane

Press `i`.

Must show: the record's fields with types and sample values, and a Value pane
for the selected field.

PASS / FAIL: ______

Bare-letter mnemonics inside Fields — press each and confirm it acts on the
selected field:

| Key | Must |
| --- | --- |
| `j` / `k` | move the selection |
| `c` | colour by the field |
| `r` | correlate the field |
| `o` | open raw context for the record |
| `Alt-p` | pin the field |
| `Alt-f` | filter to the selected value |
| `Alt-x` | filter excluding the value |
| `Alt-d` | fold by the field |

PASS / FAIL: ______

*On macOS, `Alt` must be sent as Meta. In Terminal.app enable
Settings → Profiles → Keyboard → **Use Option as Meta key**; in iTerm2 set
Profiles → Keys → Left Option → **Esc+**. If the Alt rows fail, confirm this
setting before reporting.*

PASS / FAIL of the Alt rows after enabling Meta: ______

### 8.2 Details

Select a record, press `d`.

Must show: the record as a tree beside the log, including its raw line
unchanged. Close with `Escape`.

PASS / FAIL: ______

### 8.3 Bookmarks and Go to

Select a record, press `b`. Then press `B`.

Must show: the bookmarks list containing that record, with room for a note. Add
a note. Select it and choose **Go to**.

Must show: the record selected in its source's All events view.

PASS / FAIL: ______

Quit and relaunch; the bookmark and its note must still be there.

PASS / FAIL: ______

### 8.4 `o` — raw context

Press `o` on a selected record.

In 0.1.0 this opens raw context. `docs/raw-context-as-jump.md` changes it to a
jump into All events; on a build from `main` it may do that instead. Record
which behaviour you saw rather than judging it.

Observed: ______

## 9. Palette and Help

Press `Ctrl-P`.

Must show: a searchable list of every operation with its key. Type `fold` and
confirm folding is listed with `z`.

PASS / FAIL: ______

Press `?`.

Must show: Help. Every key it lists must match §8 and the README table.

PASS / FAIL: ______

## 10. Colour

### 10.1 Truecolor

In the truecolor terminal, default environment:

```sh
lvu big.log
```

Must show: level and field colours are distinct and readable. Note the terminal.

PASS / FAIL: ______

### 10.2 Sixteen colours

```sh
TERM=xterm lvu big.log
```

Must show: chrome and text remain **readable** — no dark-on-dark, no invisible
selection, no unreadable footer. Colours will be approximated; that is expected.
Illegible output is a failure.

PASS / FAIL: ______

Repeat 10.1 in Terminal.app, which supports only 256 colours and will
approximate. Approximation is expected; illegibility is not.

PASS / FAIL: ______

## 11. Unicode, mouse, resize

### 11.1 Wide glyphs and combining characters

```sh
lvu small.log
```

Must show: CJK and emoji occupy two cells without overlapping the next column;
combining marks stay attached to their base letter; the ZWJ sequence does not
split a column boundary; tabs do not break alignment. Move the selection over
each line and confirm the highlight covers exactly the line.

PASS / FAIL: ______

Note the terminal — width handling differs most between Terminal.app and the
others.

### 11.2 Mouse selection and copy

With `lvu big.log` open, drag across several rows with the mouse, then press
`Ctrl-C`.

Must show: the drag selects the rows under the pointer, and `Ctrl-C` copies
them. Paste elsewhere to confirm the text arrived.

PASS / FAIL: ______

*`docs/portability.md` names SGR mouse coordinates and OSC 52 clipboard as the
assertions most likely to differ. Terminal.app does not support OSC 52; if the
paste is empty there, check iTerm2 (Preferences → General → Selection →
**Applications in terminal may access clipboard**) before reporting. A failure
in Terminal.app only is expected; a failure everywhere is not.*

Terminal.app: ______ iTerm2: ______ truecolor terminal: ______

### 11.3 Resize during a filter

Apply `/ERROR`, and while it is filtering drag the window narrower and wider,
including down to about 40 columns.

Must show: the layout reflows, the footer stays separate, the selection stays
visible, and no panel draws outside its area. The filter must complete.

PASS / FAIL: ______

## 12. Exit and terminal restoration

### 12.1 `q`

Press `q`.

Must show: the shell prompt, a normal cursor, no leftover colour, and the
scrollback intact. Confirm the terminal is sane:

```sh
stty -a | grep -o 'icanon\|-icanon'; echo "type here and press enter"; read x; echo "got: $x"
```

Must echo what you type and show `icanon`.

PASS / FAIL: ______

### 12.2 Ctrl-C

Relaunch and press `Ctrl-C`. Same checks as 12.1.

PASS / FAIL: ______

### 12.3 SIGKILL

```sh
lvu big.log &            # then, from another window:
kill -9 %1
```

The terminal may be left in raw mode; `reset` must recover it. lvu cannot
restore on `SIGKILL` and is not expected to.

Recovered with `reset`: PASS / FAIL: ______

## 13. Reporting a failure

For every FAIL, attach all four:

1. A screenshot of the terminal showing the failure.
2. The output of `lvu --resources`.
3. The terminal name and version, plus `echo "$TERM"` and `sw_vers`.
4. The step number and what you expected versus what you saw.

```sh
{ echo "step: "; sw_vers; echo "TERM=$TERM"; echo "shell=$SHELL"; lvu --resources; } > ~/lvu-failure.txt
```

Attach `~/lvu-failure.txt` with the screenshot.

## 14. Expected macOS differences

`docs/portability.md` is a source-level reading, not a result. It names these as
the places macOS is expected to differ. Confirm each rather than assuming it.

| Area | Expectation | Step |
| --- | --- | --- |
| Piped stdin capture | Read as failing on macOS with a message. If §2.5 passes, that reading was wrong and the doc needs correcting. | §2.5 |
| Derived-index cleanup | Read as failing with `Unsupported`. Only reachable under storage pressure; not exercised here. | — |
| `/proc` discovery | Reports Unsupported by design. `Ctrl-D` discovery in the Add source dialog offers nothing. | §2.1 |
| SGR mouse coordinates | The PTY suites assert them; unverified on macOS. | §11.2 |
| OSC 52 clipboard | Terminal.app does not support it; iTerm2 needs it enabled. | §11.2 |
| Truecolor | Terminal.app is 256-colour and approximates. | §10 |
| Terminal restoration | Asserted on Unix for `q` and `Ctrl-C`; never checked on macOS. | §12 |
| Command child reaping | Uses `SIGKILL` to the process group; expected to work. | §2.4 |
| Signing | The archives are unsigned and unnotarized. Homebrew installs from its own download, so no quarantine attribute is set; a manually downloaded archive would need `xattr -d com.apple.quarantine`. | §0 |

The PTY suites themselves have never been run on macOS. Running them is not
part of this plan; `docs/portability.md` expects their terminal-capability
assertions to fail under Terminal.app, so a red run there is not a product
failure and must not be reported as one.
