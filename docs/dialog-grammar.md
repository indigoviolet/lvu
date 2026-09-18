# Dialog flow grammar

Status: product contract for new work and migration checklist for existing
dialogs. [`dialog-system.md`](dialog-system.md) owns visual anatomy and geometry;
this document owns semantic order.

## Rule

A dialog must identify an existing object before asking the user to change it.
Creation has no object yet, so it starts with the operation or type being
created.

| Flow | Required order |
| --- | --- |
| Existing object | **Object -> operation -> parameters -> review -> submit** |
| New object | **Operation/type -> parameters -> review -> submit** |
| Manager | **Object list -> operation**; parameterized operations open a child or replacement flow |
| Inspector | **Object -> details -> operations** |
| Async result | Pending while no result exists; then **result object -> operation -> parameters -> submit** |

Empty regions disappear, but the remaining regions never reorder. `Apply`,
`Save`, `Create` and `Confirm` submit an already chosen operation; they are not
operation selectors. Selecting a row selects an object; it must not silently
switch into Edit. Operations with materially different parameters do not share
one permanently visible form.

Existing-object titles are subject-first: `View · API errors › Filter`,
`Source · payments › Restart`, `Rule · errors › Edit`. Creation titles lead
with the operation: `Add source`, `Create union`, `New enrichment step`.

## Semantic component model

This model complements `DialogSpec`; it does not replace the existing component
stack, layout system or geometry authority.

```rust
enum FlowGrammar {
    Existing,
    New,
    Manager,
    Inspector,
    Informational,
    AsyncResult,
}

enum Subject {
    Existing(ObjectSummary),
    New {
        kind: &'static str,
        origin: Option<ObjectSummary>,
    },
    None,
}

enum FlowPhase {
    ChooseObject,
    ChooseOperation,
    EditParameters,
    Review,
    InspectDetails,
    Read,
    Pending,
    Result,
}

struct OperationSpec {
    id: OperationId,
    label: Cow<'static, str>,
    parameters: Option<ParameterGroup>,
    submit_label: Cow<'static, str>,
    destructive: bool,
}

struct DialogFlowSpec {
    grammar: FlowGrammar,
    object_kind: Cow<'static, str>,
    subject: Subject,
    operations: Vec<OperationSpec>,
    selected_operation: Option<OperationId>,
    phase: FlowPhase,
}
```

This metadata drives semantic-order and initial-unresolved-phase tests now; each
dialog migration can make it the authority for focus and mode transitions.
Rendering, cursor placement, scroll extents and mouse hitboxes continue to come
from the same `DialogGeometry` and component surface.

The production model lives in `crates/lvu/src/dialog_flow.rs`. Its constructors
establish the first unresolved phase and its checked transitions reject invalid
ordering without changing the last valid state. `Component::flow_spec` is the
incremental adoption seam; `Open::flow_grammars` exhaustively maps every current
component entry point, including multi-phase components. Existing components do
not claim adoption until they return a validated flow spec.

The inventory below remains authoritative for migration status. In particular,
every row marked **Split** is still nonconforming: Source Agent proposal,
shared-key union, Recipes Save, Filter, Grouping, Time, Edit enrichment step,
Existing external command, Colour rules, Folding, Ask Proposal, Investigation
Saved and Investigation Conversation. Rows marked **Clarify** preserve sound
behaviour but still lack explicit subject or phase presentation.

## Current flow inventory

`Keep` means the current order is sound. `Clarify` means the behavior is sound
but the object or phase transition is not explicit. `Split` means distinct
operations currently share controls or retain the previous phase after the
object changes.

| Flow | Object | Operations and parameters | Required presentation | State |
| --- | --- | --- | --- | --- |
| Sources | Existing source | Add, Restart, Remove/confirm | source list -> operation -> confirmation | Keep |
| Add source · Manual | New source | File/Command; path or argv | type -> parameters -> Open | Keep |
| Add source · Discover | New source | scan/filter/candidate | Discover -> filter/candidate -> Open | Keep |
| Add source · Agent request | New source | description | Agent -> description -> Request | Keep |
| Add source · Agent proposal | Existing proposal; new source target | Start, request again | proposal summary -> operation -> parameters -> submit | Split: proposal arrival must replace request phase |
| View · New blank | New view | name | New blank view -> name -> Create | Clarify: do not frame it as an edit of the current view |
| View · Clone | New view with origin | origin, name | Clone view -> origin summary -> name -> Create | Clarify |
| View · Rename | Existing view | name | view -> Rename -> name -> Apply | Keep; make title subject-first |
| View · Sources | Existing view | source membership | view -> Sources -> choices -> Apply | Keep; make title subject-first |
| View · Delete | Existing view | confirmation | view -> Delete -> Confirm | Keep |
| Union views | New union | selected views | Create union -> views -> Create | Keep |
| Shared-key union | Existing field/value origin; new union | selected views | frozen origin -> Create shared-key union -> views -> Create | Split: origin is currently indirect |
| Recipes · Browse | Existing recipe | Apply, inspect, History, Update, Export | recipe list -> operation | Clarify: keep Browse a pure manager |
| Recipes · Save | New recipe/revision | name | Save recipe -> name -> Save revision | Split: hide name until Save is chosen |
| Recipes · Update | Existing recipe | current-view input, optional name | recipe/revision -> Update -> parameters -> Save revision | Clarify |
| Recipes · Import | New recipe | path | Import recipe -> path -> Review -> Import | Keep |
| Recipes · Export | Existing recipe/revision | path | recipe/revision -> Export -> path -> Export | Clarify |
| Recipe history | Existing recipe/revision | Apply revision | recipe -> revisions -> Apply | Keep |
| Bookmarks | Existing bookmark | Open, edit note, inspect context, remove | bookmark list -> operation | Keep; title owning view first |
| Bookmark note | Existing bookmark | note text | bookmark -> Edit note -> text -> Save | Keep |
| Filter | Existing view | Search/Advanced/Clear; query | view -> operation -> query -> Apply; Clear has no parameters | Split: target view is absent and Clear is mixed into editing |
| Grouping | Existing view | Run/Filter/Off; key | view -> mode -> mode parameters -> Apply | Split: target view is absent |
| Time | Existing view | Configure/Clear/Recognize; basis/range/gap | view -> operation -> operation parameters -> submit | Split: three operations share one form |
| Enrichment | Existing view and stages | Add, Edit, Remove, external command | view -> stage list -> operation | Clarify: name the view first |
| New enrichment step | New stage | expression/output | New step -> parameters/preview -> Save | Keep |
| Edit enrichment step | Existing stage | expression/output | stage summary -> Edit -> parameters/preview -> Save | Split: current editor begins with generic Edit |
| New external command | New stage | program/args/cwd/env | Add command -> parameters -> Save | Keep |
| Existing external command | Existing stage | Edit, Review and run, Remove | stage summary -> operation -> operation controls | Split: definition fields are always visible |
| Colour rules | Existing view/rules and new rules | Add, Edit, Remove, Apply; column/value/colour | manager: view -> rules -> operation; child: Add/Edit -> parameters -> Save | Split: manager and implicit editor are fused |
| Folding | Existing view | Configure, Collapse all; key/minimum/scope | view -> operation -> Configure parameters only | Split: unrelated operation shares the form |
| Fields | Existing record and field/value | Pin, Filter, Exclude, Colour, Fold, Correlate | frozen record -> field/value -> operation | Clarify with a sticky subject summary |
| Correlation | Existing field/value; new result | per-source field mapping | frozen origin -> Correlate -> mappings -> Create | Clarify |
| Storage | Existing workspace storage | Refresh, preview cleanup, confirm | metrics/items -> operation -> confirmation | Keep |
| Settings | Existing application settings | Edit/save | Settings -> values -> Save | Keep: one-operation editor |
| Ask · Request | New proposal | Filter/Enrichment; request text | proposal type -> request -> Submit | Keep; prepared tasks must name target view |
| Ask · Proposal | Existing proposal | Apply, Widen, Dismiss | proposal -> operation -> operation parameters -> submit | Split: request form must not remain above the new object |
| Investigation · New | New investigation | question | New investigation -> question -> Start | Keep |
| Investigation · Saved | Existing investigation | Resume/Open | investigation list -> Resume/Open | Split: hide new-question editor |
| Investigation · Conversation | Existing session/snapshot | Follow up, New snapshot; question | session/transcript -> operation -> question -> Send | Split |
| View summary | Existing view | Open owning editor | view -> applied operations -> Open | Keep; make title subject-first |
| Help | None | Read | content | Keep; inspector exemption |
| Details pane | Existing record | Inspect/expand/scroll | record -> details -> operation | Keep; inspector exemption |
| Inspect context | Existing record | Jump to raw context | record -> Inspect context | Keep |
| Command palette | Existing command row | Search and execute | commands -> query -> Execute | Keep: destinations still establish their own grammar |

## Automatic log setup

Automatic setup attaches to a source only after source creation has completed.
It never adds fields to Add source and never delays raw display.

1. The source and canonical `All events` view exist and raw records are usable.
2. The existing object is shown first: `View · All events` plus its source.
3. The operation is `Automatic setup` / `Analyze`.
4. Non-modal state advances through Waiting for sample, Sampling, Analyzing,
   Validating and Applying.
5. A successful locally validated proposal atomically creates an ordinary
   derived `Enhanced` view. `All events` remains raw.
6. The generated setup receipt becomes an existing child object of that view.

Manual operations follow the same grammar:

- `View · All events -> Analyze again -> optional parameters -> Submit`
- `View · Enhanced -> Automatic setup receipt -> Revert -> exact removal
  preview -> Confirm`

Revert is available only while the accepted configuration still matches the
receipt's generated configuration hash. Later manual edits are never inferred to
be agent-owned and are never removed. The existing Enrichment, Colour rules,
Pins and Grouping editors remain the ordinary way to edit individual accepted
settings.

## Test contract

Every converted flow needs TestBackend coverage for:

- semantic bands appearing in the required order;
- initial focus matching the first unresolved semantic step;
- selecting an object not silently entering Edit;
- operation changes showing only their own parameters;
- proposal/result arrival replacing the request/pending phase;
- Escape from a child restoring its parent object selection;
- keyboard and mouse traversal following the same order;
- narrow layouts preserving the object summary and primary operation.
