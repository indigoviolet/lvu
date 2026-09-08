# Dialog captures (evidence for docs/dialog-system.md)
Generated from the composed `lvu-app` binary (worktree base 625e1de, rebased on main a6813f0) driven through the real PTY harness in `tests/pty/`, theme `love-dark`, `LVU_NO_DELIGHT=1`, fixture of 64 JSON records plus a traceback. Each block is the full pyte screen of an otherwise empty workspace (no saved recipes, no proposals), complementing the populated-state TestBackend captures in `dialog-audit-captures.md`. `cursor=` is the terminal caret position (hidden means no editable focus). Light-theme screens are textually identical; role dumps are summarised in the main document.

## 100x30

### time @ 100x30

`time @ 100x30 theme=love-dark marker=OK cursor=hidden`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  Running: 64 record││02:13:14.343Z DEBUG  {"timestamp": "2026-09-06T12:00:39.039000Z", "level": "│
│ › Ra┌ Time window ─────────────────────────────────────────────────────────────────────────┐l": "│
│     │                                                                                      │l": "│
│     │ [ Time basis: Capture ▾ ]                                                            │l": "│
│     │ [ Window: All time ▾ ]                                                               │l": "│
│     │                                                                                      │l": "│
│     │ Start 2026-09-07 02:12:44.343236213                                   UTC      [ ▾ ] │l": "│
│     │ End   2026-09-07 02:13:44.343236213                                   UTC      [ ▾ ] │l": "│
│     │                                                                                      │l": "│
│     │ [ Apply ] [ Clear ] [ 🧠 Recognize timestamp ]                                       │l": "│
│     │                                                                                      │l": "│
│     │ ┌ Applied ─────────────────────────────────────────────────────────────────────────┐ │l": "│
│     │ │Applied: all times                                                                │ │l": "│
│     │ └──────────────────────────────────────────────────────────────────────────────────┘ │l": "│
│     │                                                                                      │l": "│
│     │ Bounds are half-open. UTC and numeric offsets are normalized to UTC; named zones are │l": "│
│     │  not supported.                                                                      │l": "│
│     │                                                                                      │l": "│
│     │                                                                                      │l": "│
│     │                                                                                      │l": "│
│     │                                                                                      │l": "│
│     │                                                                                      │     │
│     └──────────────────────────────────────────────────────────────────────────────────────┘     │
│                    ││02:13:14.343Z            raise ValueError                                   │
│                    ││02:13:14.343Z        ValueError: boom                                       │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | raw view | 40-64/64 | ? help
```

### time-dropdown @ 100x30

`time-dropdown @ 100x30 theme=love-dark marker=OK cursor=hidden`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  Running: 64 record││02:16:17.098Z DEBUG  {"timestamp": "2026-09-06T12:00:39.039000Z", "level": "│
│ › Ra┌ Time window ─────────────────────────────────────────────────────────────────────────┐l": "│
│     │                                                                                      │l": "│
│     │ [ Time basis: Capture ▾ ]                                                            │l": "│
│     │ [ Window: All time ▾ ]                                                               │l": "│
│     │ ┌─────────────────┐                                                                  │l": "│
│     │ │All time         │:15:47.098608086                                   UTC      [ ▾ ] │l": "│
│     │ │Absolute         │:16:47.098608086                                   UTC      [ ▾ ] │l": "│
│     │ │Last 5m          │                                                                  │l": "│
│     │ │Last 15m         │ [ 🧠 Recognize timestamp ]                                       │l": "│
│     │ │Last 1h          │                                                                  │l": "│
│     │ │Around selected  │────────────────────────────────────────────────────────────────┐ │l": "│
│     │ └─────────────────┘                                                                │ │l": "│
│     │ └──────────────────────────────────────────────────────────────────────────────────┘ │l": "│
│     │                                                                                      │l": "│
│     │ Bounds are half-open. UTC and numeric offsets are normalized to UTC; named zones are │l": "│
│     │  not supported.                                                                      │l": "│
│     │                                                                                      │l": "│
│     │                                                                                      │l": "│
│     │                                                                                      │l": "│
│     │                                                                                      │l": "│
│     │                                                                                      │     │
│     └──────────────────────────────────────────────────────────────────────────────────────┘     │
│                    ││02:16:17.098Z            raise ValueError                                   │
│                    ││02:16:17.098Z        ValueError: boom                                       │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | raw view | 40-64/64 | ? help
```

### search @ 100x30

`search @ 100x30 theme=love-dark marker=OK cursor=(12, 10)`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  Running: 64 record││02:13:14.343Z DEBUG  {"timestamp": "2026-09-06T12:00:39.039000Z", "level": "│
│ › Raw events       ││02:13:14.343Z INFO   {"timestamp": "2026-09-06T12:00:40.040000Z", "level": "│
│                    ││02:13:14.343Z WARN   {"timestamp": "2026-09-06T12:00:41.041000Z", "level": "│
│                    ││02:13:14.343Z ERROR  {"timestamp": "2026-09-06T12:00:42.042000Z", "level": "│
│                    ││02:13:14.343Z DEBUG  {"timestamp": "2026-09-06T12:00:43.043000Z", "level": "│
│                    ││02:13:14.343Z INFO   {"timestamp": "2026-09-06T12:00:44.044000Z", "level": "│
│         ┌ Search ──────────────────────────────────────────────────────────────────────┐level": "│
│         │                                                                              │level": "│
│         │ Applied  No filter applied.                                                  │level": "│
│         │                                                                              │level": "│
│         │                                                                              │level": "│
│         │                                                                              │level": "│
│         │                                                                              │level": "│
│         │ Examples: text · "field name": text · /regex/ims · \/literal                 │level": "│
│         │                                                                              │level": "│
│         │                                                                              │level": "│
│         └──────────────────────────────────────────────────────────────────────────────┘level": "│
│                    ││02:13:14.343Z INFO   {"timestamp": "2026-09-06T12:00:56.056000Z", "level": "│
│                    ││02:13:14.343Z WARN   {"timestamp": "2026-09-06T12:00:57.057000Z", "level": "│
│                    ││02:13:14.343Z ERROR  {"timestamp": "2026-09-06T12:00:58.058000Z", "level": "│
│                    ││02:13:14.343Z DEBUG  {"timestamp": "2026-09-06T12:00:59.059000Z", "level": "│
│                    ││02:13:14.343Z        Traceback (most recent call last):                     │
│                    ││02:13:14.343Z          File "x.py", line 1                                  │
│                    ││02:13:14.343Z            raise ValueError                                   │
│                    ││02:13:14.343Z        ValueError: boom                                       │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | raw view | 40-64/64 | ? help
```

### search-typed @ 100x30

`search-typed @ 100x30 theme=love-dark marker=OK cursor=(21, 10)`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  Running: 64 record││02:13:14.343Z DEBUG  {"timestamp": "2026-09-06T12:00:39.039000Z", "level": "│
│ › Raw events       ││02:13:14.343Z INFO   {"timestamp": "2026-09-06T12:00:40.040000Z", "level": "│
│                    ││02:13:14.343Z WARN   {"timestamp": "2026-09-06T12:00:41.041000Z", "level": "│
│                    ││02:13:14.343Z ERROR  {"timestamp": "2026-09-06T12:00:42.042000Z", "level": "│
│                    ││02:13:14.343Z DEBUG  {"timestamp": "2026-09-06T12:00:43.043000Z", "level": "│
│                    ││02:13:14.343Z INFO   {"timestamp": "2026-09-06T12:00:44.044000Z", "level": "│
│         ┌ Search ──────────────────────────────────────────────────────────────────────┐level": "│
│         │ request 1                                                                    │level": "│
│         │ Applied  No filter applied.                                                  │level": "│
│         │                                                                              │level": "│
│         │                                                                              │level": "│
│         │                                                                              │level": "│
│         │                                                                              │level": "│
│         │ Examples: text · "field name": text · /regex/ims · \/literal                 │level": "│
│         │                                                                              │level": "│
│         │                                                                              │level": "│
│         └──────────────────────────────────────────────────────────────────────────────┘level": "│
│                    ││02:13:14.343Z INFO   {"timestamp": "2026-09-06T12:00:56.056000Z", "level": "│
│                    ││02:13:14.343Z WARN   {"timestamp": "2026-09-06T12:00:57.057000Z", "level": "│
│                    ││02:13:14.343Z ERROR  {"timestamp": "2026-09-06T12:00:58.058000Z", "level": "│
│                    ││02:13:14.343Z DEBUG  {"timestamp": "2026-09-06T12:00:59.059000Z", "level": "│
│                    ││02:13:14.343Z        Traceback (most recent call last):                     │
│                    ││02:13:14.343Z          File "x.py", line 1                                  │
│                    ││02:13:14.343Z            raise ValueError                                   │
│                    ││02:13:14.343Z        ValueError: boom                                       │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | raw view | 40-64/64 | ? help
```

### advanced @ 100x30

`advanced @ 100x30 theme=love-dark marker=OK cursor=(12, 11)`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  Running: 64 record││02:13:14.342Z ERROR  {"timestamp": "2026-09-06T12:00:10.010000Z", "level": "│
│ › Raw events       ││02:13:14.342Z DEBUG  {"timestamp": "2026-09-06T12:00:11.011000Z", "level": "│
│                    ││02:13:14.342Z INFO   {"timestamp": "2026-09-06T12:00:12.012000Z", "level": "│
│                    ││02:13:14.342Z WARN   {"timestamp": "2026-09-06T12:00:13.013000Z", "level": "│
│                    ││02:13:14.342Z ERROR  {"timestamp": "2026-09-06T12:00:14.014000Z", "level": "│
│                    ││02:13:14.342Z DEBUG  {"timestamp": "2026-09-06T12:00:15.015000Z", "level": "│
│         ┌ Advanced filter ─────────────────────────────────────────────────────────────┐level": "│
│         │ FILTER EXPRESSION                                                            │level": "│
│         │                                                                              │level": "│
│         │ Applied  No filter applied.                                                  │level": "│
│         │                                                                              │         │
│         │                                                                              │         │
│         │                                                                              │         │
│         │ Use a Polars expression. Fields and static sampled literals are available as │         │
│         │ completions.                                                                 │         │
│         │                                                                              │         │
│         └──────────────────────────────────────────────────────────────────────────────┘         │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | query ready: matched 10 / scanned 64 | 1-10/10 | search:"request 1"
```

### enrichment @ 100x30

`enrichment @ 100x30 theme=love-dark marker=OK cursor=(13, 9)`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.┌ Enrichment ──────────────────────────────────────────────────────────────────┐         │
│  Running│ [ Steps ] [ Editor ] [ Add ] [ Edit ] [ Remove ] [ External command ]        │level": "│
│ › Raw ev│                                                                              │level": "│
│         │ ┌ Saved steps · kept when you add ─────────────────────────────────────────┐ │level": "│
│         │ │No extracted fields yet. Add an expression below.                         │ │level": "│
│         │ └──────────────────────────────────────────────────────────────────────────┘ │level": "│
│         │ ┌ Add step · name = expression OR /regex with named groups/ ───────────────┐ │level": "│
│         │ │                                                                          │ │level": "│
│         │ │                                                                          │ │level": "│
│         │ │                                                                          │ │level": "│
│         │ │                                                                          │ │level": "│
│         │ └──────────────────────────────────────────────────────────────────────────┘ │         │
│         │ Status                                                                       │         │
│         │ Applied: Accepted steps are active; a new draft changes nothing until it     │         │
│         │ succeeds.                                                                    │         │
│         │ ┌ Raw input before enrichment: ──────┐┌ Accepted output · same record ─────┐ │         │
│         │ │{"timestamp":                       ││No accepted outputs yet.            │ │         │
│         │ │"2026-09-06T12:00:19.019000Z",      ││Apply a valid step to see its values│ │         │
│         │ │"level": "DEBUG", "service":        ││here.                               │ │         │
│         │ │"worker", "request_id": "req-0019", ││                                    │ │         │
│         │ │"message": "fixture request 19      ││                                    │ │         │
│         │ │completed in 57ms", "path":         ││                                    │ │         │
│         │ │"/v1/items/19", "user": {"id": 5,   ││                                    │ │         │
│         │ └────────────────────────────────────┘└────────────────────────────────────┘ │         │
│         │                                                                              │         │
│         └──────────────────────────────────────────────────────────────────────────────┘         │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | query ready: matched 10 / scanned 64 | 1-10/10 | search:"request 1"
```

### external-command @ 100x30

`external-command @ 100x30 theme=love-dark marker=OK cursor=(9, 5)`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  Runn┌ External command · runs only when confirmed ───────────────────────────────────────┐el": "│
│ › Raw│ Program: Executable path; no shell parsing                                         │el": "│
│      │                                                                                    │el": "│
│      │ Arguments: 1 line(s) · One argument per line, e.g. --format then json              │el": "│
│      │                                                                                    │el": "│
│      │ Working directory: Optional; defaults to this workspace directory                  │el": "│
│      │                                                                                    │el": "│
│      │ Environment: 1 line(s) · Optional KEY=value per line, e.g. LANG=C                  │el": "│
│      │                                                                                    │el": "│
│      │ Applied command step: None · enrichment steps still apply                          │el": "│
│      │                                                                                    │      │
│      │ [ New line (Alt-N) ] [ Save ] [ Review ] [ Remove ]                                │      │
│      │                                                                                    │      │
│      │ ┌ Status and review ─────────────────────────────────────────────────────────────┐ │      │
│      │ │Status: Unrun · new records wait for an explicit run                            │ │      │
│      │ │Saving or restoring never starts this command. New records remain pending until │ │      │
│      │ │another explicit run.                                                           │ │      │
│      │ │Results appear in Details as command.<field>; command.status shows Ready or     │ │      │
│      │ │Pending. Filters and field choices use the enrichment steps above.              │ │      │
│      │ │                                                                                │ │      │
│      │ │                                                                                │ │      │
│      │ │                                                                                │ │      │
│      │ └────────────────────────────────────────────────────────────────────────────────┘ │      │
│      └────────────────────────────────────────────────────────────────────────────────────┘      │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | query ready: matched 10 / scanned 64 | 1-10/10 | search:"request 1"
```

### grouping @ 100x30

`grouping @ 100x30 theme=love-dark marker=OK cursor=(29, 10)`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  Running: 64 record││02:13:14.342Z ERROR  {"timestamp": "2026-09-06T12:00:10.010000Z", "level": "│
│ › Raw events       ││02:13:14.342Z DEBUG  {"timestamp": "2026-09-06T12:00:11.011000Z", "level": "│
│                    ││02:13:14.342Z INFO   {"timestamp": "2026-09-06T12:00:12.012000Z", "level": "│
│                    ││02:13:14.342Z WARN   {"timestamp": "2026-09-06T12:00:13.013000Z", "level": "│
│                    ││02:13:14.342Z ERROR  {"timestamp": "2026-09-06T12:00:14.014000Z", "level": "│
│         ┌ Display-only multiline grouping ─────────────────────────────────────────────┐level": "│
│         │ Continuation regex over raw bytes                                            │level": "│
│         │ ^(\s+|Caused by:)                                                            │level": "│
│         │ Applied: Grouping disabled.                                                  │level": "│
│         │                                                                              │level": "│
│         │                                                                              │         │
│         │                                                                              │         │
│         │ Preview (display only):                                                      │         │
│         │ RuntimeException: boom                                                       │         │
│         │   at worker.rs:42  → 2 physical lines                                        │         │
│         │ Empty draft disables grouping                                                │         │
│         │                                                                              │         │
│         └──────────────────────────────────────────────────────────────────────────────┘         │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | query ready: matched 10 / scanned 64 | 1-10/10 | search:"request 1"
```

### source-manual @ 100x30

`source-manual @ 100x30 theme=love-dark marker=OK cursor=(2, 6)`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
┌ Add source ──────────────────────────────────────────────────────────────────────────────────────┐
│ FILE PATH                                                                                        │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│ ┌ State ───────────────────────────────────────────────────────────────────────────────────────┐ │
│ │Ready: provide a file path or command. Capture starts only after submission.                  │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────┘ │
│                                                                                                  │
│ [ Manual ] [ Discover ] [ 🧠 ] [ File ] [ Command ]                                              │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | query ready: matched 10 / scanned 64 | 1-10/10 | search:"request 1"
```

### source-discovery @ 100x30

`source-discovery @ 100x30 theme=love-dark marker=MISSING:Add source cursor=(9, 4)`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
┌ Discover sources — selection never auto-starts ──────────────────────────────────────────────────┐
│ Search                                                                                           │
│ 2/2 matches · selection never starts capture                                                     │
│ > events.log [Remembered Unknown]                                                                │
│   events.log [Project Medium Available]                                                          │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│ ┌ Diagnostics ─────────────────────────────────────────────────────────────────────────────────┐ │
│ │Selected: events.log                                                                          │ │
│ │Evidence/path: /tmp/lvu-design-cap-xu9_qhqq/events.log — remembered source                    │ │
│ │SCAN SUMMARY: 1 candidates, complete; Procfs Limited: file descriptor limit reached; Project  │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────┘ │
│                                                                                                  │
│ [ Manual ] [ Discover ] [ 🧠 ] [ Refresh ]                                                       │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
│                    ││02:16:17.098Z        ValueError: boom                                       │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | raw view | 40-64/64 | ? help
```

### source-ai @ 100x30

`source-ai (clicked) @ (100, 30) theme=love-dark`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
┌ Ask 🧠 for a source — preview never executes ────────────────────────────────────────────────────┐
│ Request                                                                                          │
│                                                                                                  │
│ ┌ State ───────────────────────────────────────────────────────────────────────────────────────┐ │
│ │Ready: Describe the source to follow                                                          │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────┘ │
│ ┌ Preview ─────────────────────────────────────────────────────────────────────────────────────┐ │
│ │                                                                                              │ │
│ │                                                                                              │ │
│ │                                                                                              │ │
│ │                                                                                              │ │
│ │                                                                                              │ │
│ │                                                                                              │ │
│ │                                                                                              │ │
│ │                                                                                              │ │
│ │                                                                                              │ │
│ │                                                                                              │ │
│ │                                                                                              │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────┘ │
│ Describe a source; review is required before capture starts.                                     │
│                                                                                                  │
│ [ Request ] [ Manual ] [ Discover ] [ 🧠 ]                                                       │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
│                    ││02:16:17.098Z        ValueError: boom                                       │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | raw view | 40-64/64 | ? help
```

### views @ 100x30

`views @ 100x30 theme=love-dark marker=OK cursor=(38, 13)`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  Running: 64 record││02:13:14.342Z ERROR  {"timestamp": "2026-09-06T12:00:10.010000Z", "level": "│
│ › Raw events       ││02:13:14.342Z DEBUG  {"timestamp": "2026-09-06T12:00:11.011000Z", "level": "│
│                    ││02:13:14.342Z INFO   {"timestamp": "2026-09-06T12:00:12.012000Z", "level": "│
│                    ││02:13:14.342Z WARN   {"timestamp": "2026-09-06T12:00:13.013000Z", "level": "│
│                    ││02:13:14.342Z ERROR  {"timestamp": "2026-09-06T12:00:14.014000Z", "level": "│
│                    ││02:13:14.342Z DEBUG  {"timestamp": "2026-09-06T12:00:15.015000Z", "level": "│
│                    ││02:13:14.342Z INFO   {"timestamp": "2026-09-06T12:00:16.016000Z", "level": "│
│           ┌ Source view ─────────────────────────────────────────────────────────────┐ "level": "│
│           │ Mode: CLONE SETTINGS                                                     │ "level": "│
│           │                                                                          │ "level": "│
│           │ Name: Copy of Raw events                                                 │           │
│           │                                                                          │           │
│           │ Name the view. Creating, cloning, and renaming preserve the source       │           │
│           │[ New blank ] [ Clone ] [ Rename ] [ Sources ] [ Apply ]                  │           │
│           │                                                                          │           │
│           │                                                                          │           │
│           └──────────────────────────────────────────────────────────────────────────┘           │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | query ready: matched 10 / scanned 64 | 1-10/10 | search:"request 1"
```

### recipes @ 100x30

`recipes @ 100x30 theme=love-dark marker=OK cursor=hidden`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  Running: 64 record││02:13:14.342Z ERROR  {"timestamp": "2026-09-06T12:00:10.010000Z", "level": "│
│ › Raw events       ││02:13:14.342Z DEBUG  {"timestamp": "2026-09-06T12:00:11.011000Z", "level": "│
│       ┌ Named recipes ───────────────────────────────────────────────────────────────────┐vel": "│
│       │ (no saved recipes)                                                               │vel": "│
│       │ Applied: 0 saved recipes                                                         │vel": "│
│       │                                                                                  │vel": "│
│       │                                                                                  │vel": "│
│       │                                                                                  │vel": "│
│       │                                                                                  │vel": "│
│       │                                                                                  │vel": "│
│       │                                                                                  │       │
│       │                                                                                  │       │
│       │                                                                                  │       │
│       │                                                                                  │       │
│       │                                                                                  │       │
│       │                                                                                  │       │
│       │                                                                                  │       │
│       │                                                                                  │       │
│       │[ Browse ] [ Save ] [ Import ] [ Export ] [ History ] [ Update ] [ Refresh ]      │       │
│       │[ Apply revision ]                                                                │       │
│       │                                                                                  │       │
│       └──────────────────────────────────────────────────────────────────────────────────┘       │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | query ready: matched 10 / scanned 64 | 1-10/10 | search:"request 1"
```

### bookmarks @ 100x30

`bookmarks @ 100x30 theme=love-dark marker=OK cursor=hidden`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  Running: 64 record││02:13:14.342Z ERROR  {"timestamp": "2026-09-06T12:00:10.010000Z", "level": "│
┌ Bookmarks / notes · this view ───────────────────────────────────────────────────────────────────┐
│ 1 / 128 bookmarks ·                                                                              │
│ > #19 (no note)                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│ [ Raw context ] [ Edit note ] [ Remove ]                                                         │
│↑/↓ select                                                                                        │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | query ready: matched 10 / scanned 64 | 1-10/10 | search:"request 1"
```

### fields @ 100x30

`fields @ 100x30 theme=love-dark marker=OK cursor=hidden`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  Running: 64 record││02:13:14.342Z ERROR  {"timestamp": "2026-09-06T12:00:10.010000Z", "level": "│
│ › Raw events       ││02:13:14.342Z DEBUG  {"timestamp": "2026-09-06T12:00:11.011000Z", "level": "│
│                    ││02:13:14.342Z INFO   {"timestamp": "2026-09-06T12:00:12.012000Z", "level": "│
│                    ││02:13:14.342Z WARN   {"timestamp": "2026-09-06T12:00:13.013000Z", "level": "│
│              ┌ Event fields ──────────────────────────────────────────────────────┐Z", "level": "│
│              │ > [ ] level = DEBUG                                                │Z", "level": "│
│              │   [ ] message = fixture request 19 completed in 57ms               │Z", "level": "│
│              │   [ ] path = /v1/items/19                                          │Z", "level": "│
│              │   [ ] request_id = req-0019                                        │Z", "level": "│
│              │   [ ] service = worker                                             │00Z", "level":│
│              │   [ ] timestamp = 2026-09-06T12:00:19.019000Z                      │              │
│              │                                                                    │              │
│              │                                                                    │              │
│              │                                                                    │              │
│              │                                                                    │              │
│              │                                                                    │              │
│              │                                                                    │              │
│              │                                                                    │              │
│              │↑/↓ select · Space pin · c Color rows by this field                 │              │
│              └────────────────────────────────────────────────────────────────────┘              │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | query ready: matched 10 / scanned 64 | 1-10/10 | search:"request 1"
```

### details @ 100x30

`details @ 100x30 theme=love-dark marker=OK cursor=hidden`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  Running: 64 record││02:13:14.342Z ERROR  {"timestamp": "2026-09-06T12:00:10.010000Z", "level": "│
│ › Raw events       ││02:13:14.342Z DEBUG  {"timestamp": "2026-09-06T12:00:11.011000Z", "level": "│
│                    ││02:13:14.342Z INFO   {"timestamp": "2026-09-06T12:00:12.012000Z", "level": "│
│                    ││02:13:14.342Z WARN   {"timestamp": "2026-09-06T12:00:13.013000Z", "level": "│
│                    ││02:13:14.342Z ERROR  {"timestamp": "2026-09-06T12:00:14.014000Z", "level": "│
│                    ││02:13:14.342Z DEBUG  {"timestamp": "2026-09-06T12:00:15.015000Z", "level": "│
│                    ││02:13:14.342Z INFO   {"timestamp": "2026-09-06T12:00:16.016000Z", "level": "│
│                    ││02:13:14.342Z WARN   {"timestamp": "2026-09-06T12:00:17.017000Z", "level": "│
│                    ││02:13:14.342Z ERROR  {"timestamp": "2026-09-06T12:00:18.018000Z", "level": "│
│                    ││02:13:14.342Z DEBUG  ★ {"timestamp": "2026-09-06T12:00:19.019000Z", "level":│
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    │└────────────────────────────────────────────────────────────────────────────┘
│                    │┌ Selected event details ────────────────────────────────────────────────────┐
│                    ││stable display id: ed4a0c76-63b7-59e8-bbbd-5167f7c3ec5c:19                  │
│                    ││raw: {"timestamp": "2026-09-06T12:00:19.019000Z", "level": "DEBUG",         │
│                    ││"service": "worker", "request_id": "req-0019", "message": "fixture request  │
│                    ││19 completed in 57ms", "path": "/v1/items/19", "user": {"id": 5, "name":    │
│                    ││"user5"}}                                                                   │
│                    ││level: DEBUG                                                                │
│                    ││message: fixture request 19 completed in 57ms                               │
│                    ││↑/↓ scroll                                                                  │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | query ready: matched 10 / scanned 64 | 1-10/10 | search:"request 1"
```

### context @ 100x30

`context @ 100x30 theme=love-dark marker=OK cursor=hidden`

```text
lvu live sources
┌ Sources / views ───┐
┌ Raw context · filter unchanged ──────────────────────────────────────────────────────────────────┐
│ Anchor: ed4a0c76-63b7-59e8-bbbd-5167f7c3ec5c:19 · physical source records                        │
│ 15–35 / 64 · raw, unfiltered, ungrouped                                                          │
│       14 {"timestamp": "2026-09-06T12:00:14.014000Z", "level": "ERROR", "service": "scheduler",  │
│       15 {"timestamp": "2026-09-06T12:00:15.015000Z", "level": "DEBUG", "service": "api", "reque │
│       16 {"timestamp": "2026-09-06T12:00:16.016000Z", "level": "INFO", "service": "worker", "req │
│       17 {"timestamp": "2026-09-06T12:00:17.017000Z", "level": "WARN", "service": "scheduler", " │
│       18 {"timestamp": "2026-09-06T12:00:18.018000Z", "level": "ERROR", "service": "api", "reque │
│ >     19 {"timestamp": "2026-09-06T12:00:19.019000Z", "level": "DEBUG", "service": "worker", "re │
│       20 {"timestamp": "2026-09-06T12:00:20.020000Z", "level": "INFO", "service": "scheduler", " │
│       21 {"timestamp": "2026-09-06T12:00:21.021000Z", "level": "WARN", "service": "api", "reques │
│       22 {"timestamp": "2026-09-06T12:00:22.022000Z", "level": "ERROR", "service": "worker", "re │
│       23 {"timestamp": "2026-09-06T12:00:23.023000Z", "level": "DEBUG", "service": "scheduler",  │
│       24 {"timestamp": "2026-09-06T12:00:24.024000Z", "level": "INFO", "service": "api", "reques │
│       25 {"timestamp": "2026-09-06T12:00:25.025000Z", "level": "WARN", "service": "worker", "req │
│       26 {"timestamp": "2026-09-06T12:00:26.026000Z", "level": "ERROR", "service": "scheduler",  │
│       27 {"timestamp": "2026-09-06T12:00:27.027000Z", "level": "DEBUG", "service": "api", "reque │
│       28 {"timestamp": "2026-09-06T12:00:28.028000Z", "level": "INFO", "service": "worker", "req │
│       29 {"timestamp": "2026-09-06T12:00:29.029000Z", "level": "WARN", "service": "scheduler", " │
│       30 {"timestamp": "2026-09-06T12:00:30.030000Z", "level": "ERROR", "service": "api", "reque │
│       31 {"timestamp": "2026-09-06T12:00:31.031000Z", "level": "DEBUG", "service": "worker", "re │
│       32 {"timestamp": "2026-09-06T12:00:32.032000Z", "level": "INFO", "service": "scheduler", " │
│       33 {"timestamp": "2026-09-06T12:00:33.033000Z", "level": "WARN", "service": "api", "reques │
│       34 {"timestamp": "2026-09-06T12:00:34.034000Z", "level": "ERROR", "service": "worker", "re │
│↑/↓ scroll · g anchor                                                                             │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
└────────────────────┘
                       FOLLOW | query ready: matched 10 / scanned 64 | 1-10/10 | search:"request 1"
```

### storage @ 100x30

`storage @ 100x30 theme=love-dark marker=OK cursor=hidden`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  Running: 64 record││02:13:14.342Z ERROR  {"timestamp": "2026-09-06T12:00:10.010000Z", "level": "│
│ › Raw events       ││02:13:14.342Z DEBUG  {"timestamp": "2026-09-06T12:00:11.011000Z", "level": "│
│     ┌ Storage usage — total 134.8 KiB / unused derived 0 B ────────────────────────────────┐l": "│
│     │ row cache 28.5 KiB / 4.0 MiB   query membership 208 B / 256.0 MiB                    │l": "│
│     │ derived disk cap/source 256.0 MiB · global 5.0 GiB                                   │l": "│
│     │ managed budgets; not a process RSS limit                                             │l": "│
│     │                                                                                      │l": "│
│     │ > derived        32 B .lvu-index-budget — unrecognized or symlink; preserved         │l": "│
│     │   derived     2.6 KiB ed4a0c76-63b7-59e8-bbbd-5167f7c3ec5c.d17625e2-232f-45e5-affc-1 │l": "│
│     │   derived         0 B .lvu-index-ownership.lock — unrecognized or symlink; preserved │vel":│
│     │   capture    32.2 KiB source ed4a0c76-63b7-59e8-bbbd-5167f7c3ec5c — raw journal/cata │     │
│     │   workspace 100.0 KiB workspace memory + recipes — durable; preserved                │     │
│     │                                                                                      │     │
│     │                                                                                      │     │
│     │                                                                                      │     │
│     │                                                                                      │     │
│     │                                                                                      │     │
│     │ ┌ Status ──────────────────────────────────────────────────────────────────────────┐ │     │
│     │ │Status: scan complete; c previews cleanup, c again confirms                       │ │     │
│     │ └──────────────────────────────────────────────────────────────────────────────────┘ │     │
│     │↑/↓ active pane · r refresh · c preview/confirm cleanup                               │     │
│     └──────────────────────────────────────────────────────────────────────────────────────┘     │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | query ready: matched 10 / scanned 64 | 1-10/10 | search:"request 1"
```

### settings @ 100x30

`settings @ 100x30 theme=love-dark marker=OK cursor=(32, 2)`

```text
┌ Settings ────────────────────────────────────────────────────────────────────────────────────────┐
│ 🧠 configuration                                                                                 │
│ Provider/model: xture/provider  Mode: full-access               Thinking: medium                 │
│                                                                                                  │
│ Appearance                                                                                       │
│ [ Theme: love-dark ▾ ] [ Delight: Off ] [ Reduced motion: On ] [ ASCII: Off ]                    │
│                                                                                                  │
│                                                                                                  │
│ Cache limits (MiB)                                                                               │
│ Rows: 4                                         Membership: 256                                  │
│ Derived total: 5120                             Per source: 256                                  │
│ [ Save ]                                                                                         │
│ ┌ State ───────────────────────────────────────────────────────────────────────────────────────┐ │
│ │Saved: Saved; restart required for cache-limit changes                                        │ │
│ │Saved settings loaded; cache-limit changes apply after restart                                │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────┘ │
│ ┌ Effective values and paths ──────────────────────────────────────────────────────────────────┐ │
│ │Effective 🧠: fixture/provider [settings.toml] · full-access [settings.toml] · medium         │ │
│ │[settings.toml]                                                                               │ │
│ │Effective appearance: theme love-dark · delight false [environment LVU_NO_DELIGHT] · motion   │ │
│ │true [settings.toml] · ASCII false [settings.toml]                                            │ │
│ │Startup-applied MiB: rows 4 · membership 256 · total derived 5120 · index/source 256          │ │
│ │Settings: /tmp/lvu-design-cap-gmhz4r5e/config/lvu/settings.toml                               │ │
│ │Data: /tmp/lvu-design-cap-gmhz4r5e/data/lvu                                                   │ │
│ │Cache: /tmp/lvu-design-cap-gmhz4r5e/cache/lvu                                                 │ │
│ │Capture: /tmp/lvu-design-cap-gmhz4r5e/capture                                                 │ │
│ │Cache-limit changes take effect after restart; appearance previews immediately.               │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────┘ │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
```

### help @ 100x30

`help @ 100x30 theme=love-dark marker=OK cursor=hidden`

```text
lvu live sources
┌ S┌ Help ──────────────────────────────────────────────────────────────────────────────────────┐──┐
│● │ EVERYWHERE                                                                                 │  │
│  │   Ctrl-P               Open the command palette                                            │ "│
│ ›│   ?                    Open or close this help                                             │ "│
│  │   Ctrl-L               Redraw the terminal                                                 │ "│
│  │   ,                    Open settings                                                       │ "│
│  │   q / Ctrl-C           Quit                                                                │ "│
│  │                                                                                            │ "│
│  │ LOGS & VIEWS                                                                               │ "│
│  │   g / G                Jump to first / last record                                         │ "│
│  │   ←/→ · 0              Pan the selected event / reset pan                                  │ "│
│  │   [ / ]                Previous or next view                                               │":│
│  │   f                    Toggle follow / history                                             │  │
│  │   d                    Toggle selected-record details                                      │  │
│  │   o                    Open raw context                                                    │  │
│  │   b                    Toggle a bookmark                                                   │  │
│  │   B                    Open bookmarks and notes                                            │  │
│  │   Alt-S                Stop the selected source                                            │  │
│  │   Alt-R                Restart the selected source                                         │  │
│  │                                                                                            │  │
│  │ FILTER & SHAPE                                                                             │  │
│  │   /                    Literal or field-aware search                                       │  │
│  │   p                    Open the advanced filter                                            │  │
│  │   e                    Open ordered enrichments                                            │  │
│  │   Alt-C in Enrichment  Add, edit, remove, or explicitly run the terminal command step      │  │
│  │   m                    Open display-only grouping                                          │  │
│  │↑/↓ or j/k scroll · ? close                                                                 │  │
└──└────────────────────────────────────────────────────────────────────────────────────────────┘──┘
                       FOLLOW | query ready: matched 10 / scanned 64 | 1-10/10 | search:"request 1"
```

### palette @ 100x30

`palette @ 100x30 theme=love-dark marker=OK cursor=(7, 4)`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  R┌ Command palette · Ctrl-P ────────────────────────────────────────────────────────────────┐: "│
│ › │>                                                                                         │: "│
│   │› Add source                n       Sources                                               │: "│
│   │  Advanced filter           p       Filter                                                │: "│
│   │  Ask agent                 A       agent                                                 │: "│
│   │  Bookmarks and notes       B       View                                                  │: "│
│   │  Enrichment                e       Filter                                                │: "│
│   │  Expand or collapse group          View                                                  │: "│
│   │  Fields                    i       Fields                                                │: "│
│   │  Follow new records        f       View                                                  │l":│
│   │  Grouping                  m       Filter                                                │   │
│   │  Help                      ?       Application                                           │   │
│   │  Investigations            I       agent                                                 │   │
│   │  Literal filter            /       Filter                                                │   │
│   │  Manage views              v       Views                                                 │   │
│   │  Next view                 ]       Views                                                 │   │
│   │  Previous view             [       Views                                                 │   │
│   │  Quit                      Ctrl-C  Application                                           │   │
│   │  Raw record context        o       View                                                  │   │
│   │  Recipes                   r       Recipes                                               │   │
│   │  Recognize timestamp               Agent                                                 │   │
│   │Selected: Add source                                                                      │   │
│   │Open the admitted source dialog                                                           │   │
│   └──────────────────────────────────────────────────────────────────────────────────────────┘   │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | query ready: matched 10 / scanned 64 | 1-10/10 | search:"request 1"
```

### ask @ 100x30

`ask @ 100x30 theme=love-dark marker=OK cursor=(6, 6)`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  ┌ Ask 🧠 ────────────────────────────────────────────────────────────────────────────────────┐ "│
│ ›│ [ Kind: Filter ▾ ] [ Submit ]                                                              │ "│
│  │ ┌ Request ───────────────────────────────────────────────────────────────────────────────┐ │ "│
│  │ │                                                                                        │ │ "│
│  │ │                                                                                        │ │ "│
│  │ │                                                                                        │ │ "│
│  │ │                                                                                        │ │ "│
│  │ └────────────────────────────────────────────────────────────────────────────────────────┘ │ "│
│  │ ┌ State ─────────────────────────────────────────────────────────────────────────────────┐ │ "│
│  │ │Ready: Describe the desired filter                                                      │ │":│
│  │ │                                                                                        │ │  │
│  │ └────────────────────────────────────────────────────────────────────────────────────────┘ │  │
│  │ ┌ Proposal and activity ─────────────────────────────────────────────────────────────────┐ │  │
│  │ │Agent: fixture/provider · mode full-access · thinking medium                            │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ └────────────────────────────────────────────────────────────────────────────────────────┘ │  │
│  │                                                                                            │  │
│  └────────────────────────────────────────────────────────────────────────────────────────────┘  │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | query ready: matched 10 / scanned 64 | 1-10/10 | search:"request 1"
```

### investigation @ 100x30

`investigation @ 100x30 theme=love-dark marker=OK cursor=(6, 7)`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  ┌ Investigation 🧠 ──────────────────────────────────────────────────────────────────────────┐ "│
│ ›│ [ Start ]                                                                                  │ "│
│  │                                                                                            │ "│
│  │ ┌ Question or follow-up ─────────────────────────────────────────────────────────────────┐ │ "│
│  │ │                                                                                        │ │ "│
│  │ │                                                                                        │ │ "│
│  │ │                                                                                        │ │ "│
│  │ └────────────────────────────────────────────────────────────────────────────────────────┘ │ "│
│  │ ┌ State ─────────────────────────────────────────────────────────────────────────────────┐ │ "│
│  │ │Ready: enter a question for a new fixed snapshot                                        │ │":│
│  │ └────────────────────────────────────────────────────────────────────────────────────────┘ │  │
│  │ ┌ Activity and saved investigations ─────────────────────────────────────────────────────┐ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ └────────────────────────────────────────────────────────────────────────────────────────┘ │  │
│  │                                                                                            │  │
│  └────────────────────────────────────────────────────────────────────────────────────────────┘  │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | query ready: matched 10 / scanned 64 | 1-10/10 | search:"request 1"
```

## 54x16

### time @ 54x16

`time @ 54x16 theme=love-dark marker=OK cursor=hidden`

```text
lvu┌ Time window ────────────────────────────────┐
┌ S│ [ ▲ Scroll up ]                             │───┐
│● │ [ Time basis: Capture ▾ ]                   │   │
│  │ [ Window: All time ▾ ]                      │sta│
│ ›│                                             │sta│
│  │ Start date  2026-09-07                      │sta│
│  │ time  02:13:40.896006013                    │sta│
│  │ zone  UTC                             [ ▾ ] │sta│
│  │ End date  2026-09-07                        │sta│
│  │ time  02:14:40.896006013                    │sta│
│  │ zone  UTC                             [ ▾ ] │ack│
│  │                                             │ "x│
│  │ [ Apply ] [ Clear ]                         │ise│
│  │ [ 🧠 Recognize timestamp ]                  │rro│
└──│ [ ▼ Scroll down ]                           │───┘
   └─────────────────────────────────────────────┘4 |
```

### time-dropdown @ 54x16

`time-dropdown @ 54x16 theme=love-dark marker=OK cursor=hidden`

```text
lvu┌ Time window ────────────────────────────────┐
┌ S│ [ ▲ Scroll up ]                             │───┐
│● │ [ Time basis: Capture ▾ ]                   │   │
│  │ [ Window: All time ▾ ]                      │sta│
│ ›│ ┌─────────────────┐                         │sta│
│  │ │All time         │-07                      │sta│
│  │ │Absolute         │11723                    │sta│
│  │ │Last 5m          │                   [ ▾ ] │sta│
│  │ │Last 15m         │7                        │sta│
│  │ │Last 1h          │11723                    │sta│
│  │ │Around selected  │                   [ ▾ ] │ack│
│  │ └─────────────────┘                         │ "x│
│  │ [ Apply ] [ Clear ]                         │ise│
│  │ [ 🧠 Recognize timestamp ]                  │rro│
└──│ [ ▼ Scroll down ]                           │───┘
   └─────────────────────────────────────────────┘4 |
```

### search @ 54x16

`search @ 54x16 theme=love-dark marker=OK cursor=(7, 3)`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ────────────────┐
│● ev┌ Search ─────────────────────────────────┐t    │
│  Ru│                                         │mesta│
│ › R│ Applied  No filter applied.             │mesta│
│    │                                         │mesta│
│    │                                         │mesta│
│    │                                         │mesta│
│    │                                         │mesta│
│    │ Examples: text · "field name": text ·   │mesta│
│    │ /regex/ims · \/literal                  │eback│
│    │                                         │le "x│
│    └─────────────────────────────────────────┘raise│
│                    ││02:14:10.896Z        ValueErro│
└────────────────────┘└──────────────────────────────┘
                       FOLLOW | raw view | 54-64/64 |
```

### search-typed @ 54x16

`search-typed @ 54x16 theme=love-dark marker=OK cursor=(16, 3)`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ────────────────┐
│● ev┌ Search ─────────────────────────────────┐t    │
│  Ru│ request 1                               │mesta│
│ › R│ Applied  request 1                      │mesta│
│    │                                         │mesta│
│    │                                         │mesta│
│    │                                         │mesta│
│    │                                         │mesta│
│    │ Examples: text · "field name": text ·   │mesta│
│    │ /regex/ims · \/literal                  │mesta│
│    │                                         │mesta│
│    └─────────────────────────────────────────┘mesta│
│                    ││                              │
└────────────────────┘└──────────────────────────────┘
                       FOLLOW | query ready: matched 1
```

### advanced @ 54x16

`advanced @ 54x16 theme=love-dark marker=OK cursor=(7, 4)`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ────────────────┐
│● ev┌ Advanced filter ────────────────────────┐t    │
│  Ru│ FILTER EXPRESSION                       │mesta│
│ › R│                                         │mesta│
│    │ Applied  No filter applied.             │mesta│
│    │                                         │mesta│
│    │                                         │mesta│
│    │                                         │mesta│
│    │ Use a Polars expression. Fields and     │mesta│
│    │ static sampled literals are available   │mesta│
│    │                                         │mesta│
│    └─────────────────────────────────────────┘mesta│
│                    ││                              │
└────────────────────┘└──────────────────────────────┘
                       FOLLOW | query ready: matched 1
```

### enrichment @ 54x16

`enrichment @ 54x16 theme=love-dark marker=OK cursor=(7, 5)`

```text
lvu l┌ Enrichment ─────────────────────────────┐
┌ Sou│ Applied steps                           │─────┐
│● ev│   None yet                              │t    │
│  Ru│                                         │mesta│
│ › R│ Add step: name = expression or named-ca │mesta│
│    │                                         │mesta│
│    │ Applied: No enrichment steps yet.       │mesta│
│    │ Raw: {"timestamp": "2026-09-06T12:00:19 │mesta│
│    │ Fields and sampled values remain        │mesta│
│    │ available through completion.           │mesta│
│    │                                         │mesta│
│    │                                         │mesta│
│    │ [ Steps ] [ Editor ] [ Add ] [ Edit ]   │mesta│
│    │ [ Remove ] [ External command ]         │     │
└────│                                         │─────┘
     └─────────────────────────────────────────┘ched 1
```

### external-command @ 54x16

`external-command @ 54x16 theme=love-dark marker=OK cursor=(6, 2)`

```text
lvu ┌ External command · runs only when confirmed┐
┌ So│ Program: Executable path; no shell parsing │───┐
│● e│                                            │   │
│  R│ Arguments: 1 line(s) · One argument per li │sta│
│ › │                                            │sta│
│   │ Working directory: Optional; defaults to t │sta│
│   │                                            │sta│
│   │ Environment: 1 line(s) · Optional KEY=valu │sta│
│   │ Applied command step: None · enrichment st │sta│
│   │                                            │sta│
│   │ [ New line (Alt-N) ] [ Save ] [ Review ]   │sta│
│   │ [ Remove ]                                 │sta│
│   │ ┌ Status and review · ↑/↓ scroll ────────┐ │sta│
│   │ │Status: Unrun · new records wait for an │ │   │
└───│ └────────────────────────────────────────┘ │───┘
    └────────────────────────────────────────────┘ed 1
```

### grouping @ 54x16

`grouping @ 54x16 theme=love-dark marker=OK cursor=(24, 3)`

```text
lvu live sources
┌ Sou┌ Display-only multiline grouping ────────┐─────┐
│● ev│ Continuation regex over raw bytes       │t    │
│  Ru│ ^(\s+|Caused by:)                       │mesta│
│ › R│ Applied: Grouping disabled.             │mesta│
│    │                                         │mesta│
│    │                                         │mesta│
│    │                                         │mesta│
│    │ Preview (display only):                 │mesta│
│    │ RuntimeException: boom                  │mesta│
│    │   at worker.rs:42  → 2 physical lines   │mesta│
│    │ Empty draft disables grouping           │mesta│
│    │                                         │mesta│
│    └─────────────────────────────────────────┘     │
└────────────────────┘└──────────────────────────────┘
                       FOLLOW | query ready: matched 1
```

### source-manual @ 54x16

`source-manual @ 54x16 theme=love-dark marker=OK cursor=(2, 3)`

```text
┌ Add source ────────────────────────────────────────┐
│ FILE PATH                                          │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│ ┌ State ─────────────────────────────────────────┐ │
│ │Ready: provide a file path or command. Capture  │ │
│ └────────────────────────────────────────────────┘ │
│                                                    │
│ [ Manual ] [ Discover ] [ 🧠 ] [ File ]            │
│ [ Command ]                                        │
└────────────────────────────────────────────────────┘
```

### source-discovery @ 54x16

`source-discovery @ 54x16 theme=love-dark marker=MISSING:Add source cursor=(9, 1)`

```text
┌ Discover sources — selection never auto-starts ────┐
│ Search                                             │
│ 2/2 matches · selection never starts capture       │
│ > events.log [Remembered Unknown]                  │
│   events.log [Project Medium Available]            │
│                                                    │
│                                                    │
│ ┌ Diagnostics ───────────────────────────────────┐ │
│ │Selected: events.log                            │ │
│ │Evidence/path:                                  │ │
│ │/tmp/lvu-design-cap-r96qfdux/events.log —       │ │
│ └────────────────────────────────────────────────┘ │
│                                                    │
│ [ Manual ] [ Discover ] [ 🧠 ] [ Refresh ]         │
│                                                    │
└────────────────────────────────────────────────────┘
```

### source-ai @ 54x16

`source-ai (clicked) @ (54, 16) theme=love-dark`

```text
┌ Ask 🧠 for a source — preview never executes ──────┐
│ Request                                            │
│                                                    │
│ ┌ State ─────────────────────────────────────────┐ │
│ │Ready: Describe the source to follow            │ │
│ └────────────────────────────────────────────────┘ │
│ ┌ Preview ───────────────────────────────────────┐ │
│ │                                                │ │
│ │                                                │ │
│ │                                                │ │
│ └────────────────────────────────────────────────┘ │
│ Describe a source; review is required before captu │
│                                                    │
│ [ Request ] [ Manual ] [ Discover ] [ 🧠 ]         │
│                                                    │
└────────────────────────────────────────────────────┘
```

### views @ 54x16

`views @ 54x16 theme=love-dark marker=OK cursor=(32, 6)`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ────────────────┐
│● events.log        ││time          level  event    │
│  Run┌ Source view ──────────────────────────┐imesta│
│ › Ra│ Mode: CLONE SETTINGS                  │imesta│
│     │                                       │imesta│
│     │ Name: Copy of Raw events              │imesta│
│     │                                       │imesta│
│     │ Name the view. Creating, cloning, and │imesta│
│     │[ New blank ]s[ Clone ]s[ Rename ]ure. │imesta│
│     │[ Sources ] [ Apply ]                  │imesta│
│     │                                       │imesta│
│     └───────────────────────────────────────┘imesta│
│                    ││                              │
└────────────────────┘└──────────────────────────────┘
                       FOLLOW | query ready: matched 1
```

### recipes @ 54x16

`recipes @ 54x16 theme=love-dark marker=OK cursor=hidden`

```text
lvu ┌ Named recipes ────────────────────────────┐
┌ So│ (no saved recipes)                        │────┐
│● e│ Applied: 0 saved recipes                  │    │
│  R│                                           │esta│
│ › │                                           │esta│
│   │                                           │esta│
│   │                                           │esta│
│   │                                           │esta│
│   │                                           │esta│
│   │                                           │esta│
│   │                                           │esta│
│   │                                           │esta│
│   │[ Browse ] [ Save ] [ Import ] [ Export ]  │esta│
│   │[ History ] [ Update ] [ Refresh ]         │    │
└───│[ Apply revision ]                         │────┘
    └───────────────────────────────────────────┘hed 1
```

### bookmarks @ 54x16

`bookmarks @ 54x16 theme=love-dark marker=OK cursor=hidden`

```text
┌ Bookmarks / notes · this view ─────────────────────┐
│ 1 / 128 bookmarks ·                                │
│ > #19 (no note)                                    │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│ [ Raw context ] [ Edit note ] [ Remove ]           │
│↑/↓ select                                          │
└────────────────────────────────────────────────────┘
```

### fields @ 54x16

`fields @ 54x16 theme=love-dark marker=OK cursor=hidden`

```text
lvu live┌ Event fields ─────────────────────┐
┌ Source│ > [ ] level = DEBUG               │────────┐
│● event│   [ ] message = fixture request 1 │vent    │
│  Runni│   [ ] path = /v1/items/19         │"timesta│
│ › Raw │   [ ] request_id = req-0019       │"timesta│
│       │   [ ] service = worker            │"timesta│
│       │   [ ] timestamp = 2026-09-06T12:0 │"timesta│
│       │                                   │"timesta│
│       │                                   │"timesta│
│       │                                   │"timesta│
│       │                                   │"timesta│
│       │                                   │"timesta│
│       │                                   │ {"times│
│       │↑/↓ select · Space pin · c Color   │        │
└───────│rows by this field                 │────────┘
        └───────────────────────────────────┘matched 1
```

### details @ 54x16

`details @ 54x16 theme=love-dark marker=OK cursor=hidden`

```text
lvu live sources
┌ Sources / views ───┐┌ Log viewport ────────────────┐
│● events.log        ││time          level  event    │
│  Running: 64 record││02:14:10.895Z ERROR  {"timesta│
│ › Raw events       ││02:14:10.895Z DEBUG  {"timesta│
│                    ││02:14:10.895Z INFO   {"timesta│
│                    ││02:14:10.895Z WARN   {"timesta│
│                    ││02:14:10.895Z ERROR  {"timesta│
│                    ││02:14:10.895Z DEBUG  ★ {"times│
│                    │└──────────────────────────────┘
│                    │┌ Selected event details ──────┐
│                    ││stable display id:            │
│                    ││bb4e8c79-be39-5d6a-bd37-4042f1│
│                    ││↑/↓ scroll                    │
└────────────────────┘└──────────────────────────────┘
                       FOLLOW | query ready: matched 1
```

### context @ 54x16

`context @ 54x16 theme=love-dark marker=OK cursor=hidden`

```text
┌ Raw context · filter unchanged ────────────────────┐
│ Anchor: bb4e8c79-be39-5d6a-bd37-4042f195ab76:19 ·  │
│ 15–25 / 64 · raw, unfiltered, ungrouped            │
│       14 {"timestamp": "2026-09-06T12:00:14.014000 │
│       15 {"timestamp": "2026-09-06T12:00:15.015000 │
│       16 {"timestamp": "2026-09-06T12:00:16.016000 │
│       17 {"timestamp": "2026-09-06T12:00:17.017000 │
│       18 {"timestamp": "2026-09-06T12:00:18.018000 │
│ >     19 {"timestamp": "2026-09-06T12:00:19.019000 │
│       20 {"timestamp": "2026-09-06T12:00:20.020000 │
│       21 {"timestamp": "2026-09-06T12:00:21.021000 │
│       22 {"timestamp": "2026-09-06T12:00:22.022000 │
│       23 {"timestamp": "2026-09-06T12:00:23.023000 │
│       24 {"timestamp": "2026-09-06T12:00:24.024000 │
│↑/↓ scroll · g anchor                               │
└────────────────────────────────────────────────────┘
```

### storage @ 54x16

`storage @ 54x16 theme=love-dark marker=OK cursor=hidden`

```text
lvu┌ Storage usage — total 134.8 KiB / unused der┐
┌ S│ row cache 13.9 KiB / 4.0 MiB   query member │───┐
│● │ derived disk cap/source 256.0 MiB · global  │   │
│  │ managed budgets; not a process RSS limit    │sta│
│ ›│                                             │sta│
│  │ > derived        32 B .lvu-index-budget — u │sta│
│  │   derived         0 B .lvu-index-ownership. │sta│
│  │   derived     2.6 KiB bb4e8c79-be39-5d6a-bd │sta│
│  │   workspace 100.0 KiB workspace memory + re │sta│
│  │   capture    32.2 KiB source bb4e8c79-be39- │sta│
│  │ ┌ Status ─────────────────────────────────┐ │sta│
│  │ │Status: scan complete; c previews        │ │sta│
│  │ └─────────────────────────────────────────┘ │mes│
│  │↑/↓ active pane · r refresh · c              │   │
└──│preview/confirm cleanup                      │───┘
   └─────────────────────────────────────────────┘ed 1
```

### settings @ 54x16

`settings @ 54x16 theme=love-dark marker=OK cursor=(25, 2)`

```text
┌ Settings ──────────────────────────────────────────┐
│ Provider/model                                     │
│ Value: fixture/provider                            │
│ [ Save ] [ More ]                                  │
│ ┌ State ─────────────────────────────────────────┐ │
│ │Saved: Saved; restart required for cache-limit  │ │
│ │changes                                         │ │
│ └────────────────────────────────────────────────┘ │
│ ┌ Effective values and paths ────────────────────┐ │
│ │State detail: Saved settings loaded; cache-limit│ │
│ │changes apply after restart                     │ │
│ │Effective 🧠: fixture/provider [settings.toml] ·│ │
│ │full-access [settings.toml] · medium            │ │
│ └────────────────────────────────────────────────┘ │
│                                                    │
└────────────────────────────────────────────────────┘
```

### help @ 54x16

`help @ 54x16 theme=love-dark marker=OK cursor=hidden`

```text
lvu live sources
┌ ┌ Help ──────────────────────────────────────────┐─┐
│●│ EVERYWHERE                                     │ │
│ │   Ctrl-P               Open the command        │a│
│ │ palette                                        │a│
│ │   ?                    Open or close this help │a│
│ │   Ctrl-L               Redraw the terminal     │a│
│ │   ,                    Open settings           │a│
│ │   q / Ctrl-C           Quit                    │a│
│ │                                                │a│
│ │ LOGS & VIEWS                                   │a│
│ │   g / G                Jump to first / last    │a│
│ │ record                                         │s│
│ │↑/↓ or j/k scroll · ? close                     │ │
└─└────────────────────────────────────────────────┘─┘
                       FOLLOW | query ready: matched 1
```

### palette @ 54x16

`palette @ 54x16 theme=love-dark marker=OK cursor=(3, 1)`

```text
┌ Command palette · Ctrl-P ──────────────────────────┐
│>                                                   │
│› Add source                n  Sources              │
│  Advanced filter           p  Filter               │
│  Ask agent                 A  agent                │
│  Bookmarks and notes       B  View                 │
│  Enrichment                e  Filter               │
│  Expand or collapse group     View                 │
│  Fields                    i  Fields               │
│  Follow new records        f  View                 │
│  Grouping                  m  Filter               │
│  Help                      ?  Application          │
│  Investigations            I  agent                │
│Selected: Add source                                │
│Open the admitted source dialog                     │
└────────────────────────────────────────────────────┘
```

### ask @ 54x16

`ask @ 54x16 theme=love-dark marker=OK cursor=(5, 4)`

```text
lv┌ Ask 🧠 ────────────────────────────────────────┐
┌ │ [ Kind: Filter ▾ ]                             │─┐
│●│ [ Submit ]                                     │ │
│ │ ┌ Request ───────────────────────────────────┐ │a│
│ │ │                                            │ │a│
│ │ └────────────────────────────────────────────┘ │a│
│ │ ┌ State ─────────────────────────────────────┐ │a│
│ │ │Ready: Describe the desired filter          │ │a│
│ │ └────────────────────────────────────────────┘ │a│
│ │ ┌ Proposal and activity ─────────────────────┐ │a│
│ │ │Agent: fixture/provider · mode full-access ·│ │a│
│ │ │thinking medium                             │ │a│
│ │ │                                            │ │s│
│ │ └────────────────────────────────────────────┘ │ │
└─│                                                │─┘
  └────────────────────────────────────────────────┘ 1
```

### investigation @ 54x16

`investigation @ 54x16 theme=love-dark marker=OK cursor=(5, 4)`

```text
lv┌ Investigation 🧠 ──────────────────────────────┐
┌ │ [ Start ]                                      │─┐
│●│                                                │ │
│ │ ┌ Question or follow-up ─────────────────────┐ │a│
│ │ │                                            │ │a│
│ │ │                                            │ │a│
│ │ │                                            │ │a│
│ │ └────────────────────────────────────────────┘ │a│
│ │ ┌ State ─────────────────────────────────────┐ │a│
│ │ │Ready: enter a question for a new fixed     │ │a│
│ │ └────────────────────────────────────────────┘ │a│
│ │ ┌ Activity and saved investigations ─────────┐ │a│
│ │ │                                            │ │s│
│ │ └────────────────────────────────────────────┘ │ │
└─│                                                │─┘
  └────────────────────────────────────────────────┘ 1
```

## 100x30 — regenerated 2026-09-08 on main `ba793af`

Same harness and theme; fixture of two file sources (`events.log`,
`worker`; `api.log`, `api`) of 64 JSON records each — `timestamp`, `level`,
`service`, `request_id`, `message`, and a nested `http` object — plus a
four-line traceback, so the Fields tree and the Correlation dialog have
something to show. Two expression steps were accepted before the Enrichment
capture. The dialog regions of these screens are the "Now" mocks in
`dialog-system.md` §12.5, §12.5a, §12.6, §12.11 and §12.21.

### enrichment @ 100x30

`enrichment @ 100x30 theme=love-dark marker=OK cursor=hidden`

```text
lvu live sources                                                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  Running: 68 record││04:52:48.745Z INFO   {"timestamp": "2026-09-06T12:00:01.001000Z", "level": "│
│   All events       ││04:52:48.745Z WARN   {"timestamp": "2026-09-06T12:00:02.002000Z", "level": "│
│ › Enriched         ││04:52:48.745Z ERROR  {"timestamp": "2026-09-06T12:00:03.003000Z", "level": "│
│● api.log           ││04:52:48.745Z DEBUG  {"timestamp": "2026-09-06T12:00:04.004000Z", "level": "│
│  Running: 68 record││04:52:48.745Z INFO   {"timestamp": "2026-09-06T12:00:05.005000Z", "level": "│
│   All events       ││04:52:48.745Z WARN   {"timestamp": "2026-09-06T12:00:06.006000Z", "level": "│
│                    ││04:52:48.745Z ERROR  {"timestamp": "2026-09-06T12:00:07.007000Z", "level": "│
│      ┌ Enrichment ────────────────────────────────────────────────────────────────────────┐el": "│
│      │ Steps                                                                       1 of 2 │el": "│
│      │   › 1  /completed in (?P<ms>\d+)ms/                                                │el": "│
│      │     2  level_lower = pl.col('level').str.to_lowercase()                            │el": "│
│      │ ● Applied   2 steps active                                                         │el": "│
│      │ Later steps can use fields from earlier steps, command output as <name>.<field> ·  │el": "│
│      │ Alt-Up/Down reorder · commands run only when you confirm                           │el": "│
│      │ [ Add ]  [ Edit ]  [ Remove ]  [ External command… ]                               │el": "│
│      └────────────────────────────────────────────────────────────────────────────────────┘el": "│
│                    ││04:52:48.745Z INFO   {"timestamp": "2026-09-06T12:00:17.017000Z", "level": "│
│                    ││04:52:48.745Z WARN   {"timestamp": "2026-09-06T12:00:18.018000Z", "level": "│
│                    ││04:52:48.745Z ERROR  {"timestamp": "2026-09-06T12:00:19.019000Z", "level": "│
│                    ││04:52:48.745Z DEBUG  {"timestamp": "2026-09-06T12:00:20.020000Z", "level": "│
│                    ││04:52:48.745Z INFO   {"timestamp": "2026-09-06T12:00:21.021000Z", "level": "│
│                    ││04:52:48.745Z WARN   {"timestamp": "2026-09-06T12:00:22.022000Z", "level": "│
│                    ││04:52:48.745Z ERROR  {"timestamp": "2026-09-06T12:00:23.023000Z", "level": "│
│                    ││04:52:48.745Z DEBUG  {"timestamp": "2026-09-06T12:00:24.024000Z", "level": "│
│                    ││04:52:48.745Z INFO   {"timestamp": "2026-09-06T12:00:25.025000Z", "level": "│
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       HISTORY | query ready: matched 68 / scanned 68 | 1-25/68 | enrich:on | ? help
```

### enrichment-step @ 100x30

`enrichment-step @ 100x30 theme=love-dark marker=OK cursor=51,8`

```text
lvu live sources                                                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  Running: 68 record││04:52:48.745Z INFO   {"timestamp": "2026-09-06T12:00:01.001000Z", "level": "│
│   All events       ││04:52:48.745Z WARN   {"timestamp": "2026-09-06T12:00:02.002000Z", "level": "│
│ › Enriched         ││04:52:48.745Z ERROR  {"timestamp": "2026-09-06T12:00:03.003000Z", "level": "│
│● api.lo┌ Enrichment › Edit step ────────────────────────────────────────────────────────┐evel": "│
│  Runnin│                                                                                │evel": "│
│   All e│ Expression  /completed in (?P<ms>\d+)ms/                                       │evel": "│
│        │                                                                                │evel": "│
│      ┌ │                                                                                │─┐el": "│
│      │ │                                                                                │ │el": "│
│      │ │ Input record                   1 of 68  Accepted output                        │ │el": "│
│      │ │   {"timestamp": "2026-09-06T12:00:01.…    ms  41                               │ │el": "│
│      │ │   Fields  timestamp, level, service, …    level_lower  info                    │ │el": "│
│      │ │                                                                                │ │el": "│
│      │ │                                                                                │ │el": "│
│      │ │ ○ Ready     saving replaces this accepted step                                 │ │el": "│
│      └─│ name = expression  or  /regex with (?P<name>…) groups/                         │─┘el": "│
│        │                                                                                │evel": "│
│        │ [ Save ]  [ Remove ]                                                           │evel": "│
│        │                                                                                │evel": "│
│        └────────────────────────────────────────────────────────────────────────────────┘evel": "│
│                    ││04:52:48.745Z INFO   {"timestamp": "2026-09-06T12:00:21.021000Z", "level": "│
│                    ││04:52:48.745Z WARN   {"timestamp": "2026-09-06T12:00:22.022000Z", "level": "│
│                    ││04:52:48.745Z ERROR  {"timestamp": "2026-09-06T12:00:23.023000Z", "level": "│
│                    ││04:52:48.745Z DEBUG  {"timestamp": "2026-09-06T12:00:24.024000Z", "level": "│
│                    ││04:52:48.745Z INFO   {"timestamp": "2026-09-06T12:00:25.025000Z", "level": "│
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       HISTORY | query ready: matched 68 / scanned 68 | 1-25/68 | enrich:on | ? help
```

### external-command @ 100x30

`external-command @ 100x30 theme=love-dark marker=OK cursor=34,7`

```text
lvu live sources                                                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  Running: 68 record││04:52:48.745Z INFO   {"timestamp": "2026-09-06T12:00:01.001000Z", "level": "│
│   All┌ Enrichment › External command ─────────────────────────────────────────────────────┐el": "│
│ › Enr│                                                                                    │el": "│
│● api.│ Name          command                                                              │el": "│
│  Runn│ Program       /usr/bin/jq                                                          │el": "│
│   All│ Arguments     (none)                                                               │el": "│
│      │ Directory     (workspace directory)                                                │el": "│
│      │ Environment   (inherited)                                                          │el": "│
│      │                                                                                    │el": "│
│      │ Results and review                                                          5 of 5 │el": "│
│      │   Applied command step: none · will be inserted as step 2 of 3                     │el": "│
│      │   Saving or restoring never starts this command.                                   │el": "│
│      │   New records stay pending until you run it again.                                 │el": "│
│      │   Results appear in Details and to later steps as command.<field>; command.status  │el": "│
│      │   shows Ready or Pending.                                                          │el": "│
│      │                                                                                    │el": "│
│      │ ○ Unrun     Draft changed · save before reviewing a run · runs only when you       │el": "│
│      │             confirm                                                                │el": "│
│      │ Program is an executable path; no shell parsing. One argument per line.            │el": "│
│      │                                                                                    │el": "│
│      │ [ Save ]  [ Review and run ]  [ Remove ]  [ New line ]                             │el": "│
│      │                                                                                    │el": "│
│      └────────────────────────────────────────────────────────────────────────────────────┘el": "│
│                    ││04:52:48.745Z DEBUG  {"timestamp": "2026-09-06T12:00:24.024000Z", "level": "│
│                    ││04:52:48.745Z INFO   {"timestamp": "2026-09-06T12:00:25.025000Z", "level": "│
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       HISTORY | query ready: matched 68 / scanned 68 | 1-25/68 | enrich:on | ? help
```

### fields @ 100x30

`fields @ 100x30 theme=love-dark marker=OK cursor=hidden`

```text
lvu live sources                                                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  Running: 68 record││04:52:48.745Z INFO   {"timestamp": "2026-09-06T12:00:01.001000Z", "level": "│
│   All events       ││04:52:48.745Z WARN   {"timestamp": "2026-09-06T12:00:02.002000Z", "level": "│
│ › Enriched         ││04:52:48.745Z ERROR  {"timestamp": "2026-09-06T12:00:03.003000Z", "level": "│
│● api.┌ Fields · record 0 ─────────────────────────────────────────────────────────────────┐el": "│
│  Runn│                                                                                    │el": "│
│   All│ Field                   Value   9 fields  Value · http.status  first 2,048 records │el": "│
│      │     [ ] timestamp       "2026-09-06T12:…    Type      integer · 100% of present va │el": "│
│      │     [ ] level           "INFO"              Sample    200 · record 0               │el": "│
│      │     [ ] service         "worker"            Present   64 of 68 sampled records     │el": "│
│      │     [ ] request_id      "req-0001"          Distinct  2 values                     │el": "│
│      │     [ ] message         "fixture reques…    Range     200 … 503                    │el": "│
│      │      ▾  http            {3 keys}            Top           58  200                  │el": "│
│      │   ›       status        200                                6  503                  │el": "│
│      │           path          "/v1/items/1"                                              │el": "│
│      │      ▸    tags          [2]                                                        │el": "│
│      │                                                                                    │el": "│
│      │ Pinned fields become log columns; a nested value acts through its top-level field. │el": "│
│      │                                                                                    │el": "│
│      │ [ Pin ]  [ Filter ]  [ Exclude ]  [ Color ]  [ Fold ]  [ Correlate ]               │el": "│
│      │                                                                                    │el": "│
│      └────────────────────────────────────────────────────────────────────────────────────┘el": "│
│                    ││04:52:48.745Z WARN   {"timestamp": "2026-09-06T12:00:22.022000Z", "level": "│
│                    ││04:52:48.745Z ERROR  {"timestamp": "2026-09-06T12:00:23.023000Z", "level": "│
│                    ││04:52:48.745Z DEBUG  {"timestamp": "2026-09-06T12:00:24.024000Z", "level": "│
│                    ││04:52:48.745Z INFO   {"timestamp": "2026-09-06T12:00:25.025000Z", "level": "│
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       HISTORY | query ready: matched 68 / scanned 68 | 1-25/68 | enrich:on | ? help
```

### correlation @ 100x30

`correlation @ 100x30 theme=love-dark marker=OK cursor=hidden`

```text
lvu live sources                                                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● events.log        ││time          level  event                                                  │
│  Running: 68 record││04:52:48.745Z INFO   {"timestamp": "2026-09-06T12:00:01.001000Z", "level": "│
│   All events       ││04:52:48.745Z WARN   {"timestamp": "2026-09-06T12:00:02.002000Z", "level": "│
│ › Enriched         ││04:52:48.745Z ERROR  {"timestamp": "2026-09-06T12:00:03.003000Z", "level": "│
│● api.log           ││04:52:48.745Z DEBUG  {"timestamp": "2026-09-06T12:00:04.004000Z", "level": "│
│  Running: 68 record││04:52:48.745Z INFO   {"timestamp": "2026-09-06T12:00:05.005000Z", "level": "│
│   All events       ││04:52:48.745Z WARN   {"timestamp": "2026-09-06T12:00:06.006000Z", "level": "│
│                    ││04:52:48.745Z ERROR  {"timestamp": "2026-09-06T12:00:07.007000Z", "level": "│
│             ┌ Correlate across sources ────────────────────────────────────────────┐", "level": "│
│             │ request_id = "req-0001" · from the selected record                   │", "level": "│
│             │ Source                                                        2 of 2 │", "level": "│
│             │   › events.log                                  request_id ▾         │", "level": "│
│             │     api.log                                     request_id ▾         │", "level": "│
│             │ ● Applied   2 of 2 sources mapped                                    │", "level": "│
│             │ Sources name the same identity differently; unmapped sources         │", "level": "│
│             │ contribute no records.                                               │", "level": "│
│             │ [ Correlate ]  [ Cancel ]                                            │", "level": "│
│             └──────────────────────────────────────────────────────────────────────┘", "level": "│
│                    ││04:52:48.745Z WARN   {"timestamp": "2026-09-06T12:00:18.018000Z", "level": "│
│                    ││04:52:48.745Z ERROR  {"timestamp": "2026-09-06T12:00:19.019000Z", "level": "│
│                    ││04:52:48.745Z DEBUG  {"timestamp": "2026-09-06T12:00:20.020000Z", "level": "│
│                    ││04:52:48.745Z INFO   {"timestamp": "2026-09-06T12:00:21.021000Z", "level": "│
│                    ││04:52:48.745Z WARN   {"timestamp": "2026-09-06T12:00:22.022000Z", "level": "│
│                    ││04:52:48.745Z ERROR  {"timestamp": "2026-09-06T12:00:23.023000Z", "level": "│
│                    ││04:52:48.745Z DEBUG  {"timestamp": "2026-09-06T12:00:24.024000Z", "level": "│
│                    ││04:52:48.745Z INFO   {"timestamp": "2026-09-06T12:00:25.025000Z", "level": "│
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       HISTORY | query ready: matched 68 / scanned 68 | 1-25/68 | enrich:on | ? help
```
