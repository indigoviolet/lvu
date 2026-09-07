# Partitioning `crates/lvu` for parallel work

## Why

`crates/lvu/src/app.rs` (11,302 lines) and `ui.rs` (7,026) are the only real
constraint on running implementers in parallel. Machine time is not the limit;
these two files are. Three functions carry almost the whole conflict surface:

| Function | Lines | Why every agent touches it |
| --- | ---: | --- |
| `App::handle` | 3,502 | one match over every `Action` |
| `App::handle_mouse` | 1,904 | one match over every hit region |
| `key_to_action` | 550 | one match per focus |

Adding any feature means editing all three plus a `render_*`, so four agents
adding four unrelated features collide four ways.

## Target layout

`ui.rs` splits cleanly: every dialog is already a self-contained `render_*`.

    ui/mod.rs        render_with_theme, shared helpers, dispatch
    ui/logs.rs       render_logs, render_status
    ui/source.rs     render_source_dialog          (569)
    ui/time.rs       render_time_editor            (438)
    ui/enrichment.rs render_enrichment_step, _list (596)
    ui/storage.rs    render_storage                (333)
    ui/settings.rs   render_settings               (277)
    ui/command.rs    render_command_enrichment     (240)
    ui/recipes.rs    render_recipes                (230)
    ui/ask.rs        render_ask_ai, render_investigation (403)
    ui/views.rs      render_view_dialog            (189)
    ui/inspect.rs    bookmarks, field picker, context, help

`app.rs` splits by feature. Rust allows one `impl App` to span several files in
the same crate, so each feature owns its handlers:

    app/mod.rs       App, ViewState, Action, Focus, shared state
    app/dispatch.rs  handle/handle_mouse/key_to_action, delegating only
    app/source.rs    app/views.rs   app/enrichment.rs  app/time.rs
    app/recipes.rs   app/bookmarks.rs app/storage.rs   app/ask.rs
    app/query.rs     apply_query_completion and the query seam
    app/text.rs      active_text_target, replace_active_text, completion

`dispatch.rs` must contain routing only. A match arm with a body belongs in the
feature module; that is the rule that keeps the file from growing back.

## The mechanical split is not the goal

Splitting `impl App` across files removes merge pain and nothing else. `App` has
**74 fields** and `Action` has **180 variants**; every module would still take
`&mut self` over the whole struct, so `app/time.rs` could still reach into
`source_dialog`. That trades textual conflicts for semantic ones, which are worse
because they do not announce themselves in a diff.

The encouraging part: **14 per-dialog state structs already exist**
(`TimeDialogState`, `SourceDialogState`, `RecipeDialogState`, ...). The data is
already partitioned. What is missing is that the behaviour lives in one
3,500-line match instead of on those structs, so a component model is a
conversion rather than a rewrite.

## Target: owned components with a shared context

    struct TimeDialog { /* private state */ }
    impl TimeDialog {
        fn handle(&mut self, event: Event, ctx: &mut Ctx) -> Outcome;
        fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme);
    }

`App` becomes a router holding components plus shared services. Components take
`&mut Ctx` — row provider, active view, query dispatch — never `&mut App`, so the
boundary is compiler-enforced. Each component owns its message type and the
parent maps it, which lets `Action` shrink instead of growing with every feature.

Genuinely cross-cutting concerns stay in the shell and should NOT be pushed into
components: focus and layered dismissal, hit-region registration, theme,
terminal size, and the query seam.

The crate layering is already sound — `lvu-core` → `ingest` → `live` → `query` →
`view` → `app`, dependencies pointing inward. That is why HTTP sources could be
added without touching the UI. The problem is confined to the one crate that grew
fastest under feature pressure.

## Sequencing

This is a move-only, behaviour-preserving refactor that rewrites both files, so
it conflicts with everything in flight. Do it at a quiescent point, as one
commit, with no behaviour change in the same commit:

1. Let the current wave land and integrate.
2. Split, verifying with `cargo test -p lvu` and `mise run test:pty:matrix`
   before and after — the two runs must be identical.
3. Dispatch the next wave against the new partition, one module per agent.
4. Then convert dialogs to owned components ONE AT A TIME, each its own commit,
   each verified by the matrix. Incremental and reversible; no big-bang rewrite.

Step 4 is the one that pays, and the one that is easiest to defer under feature
pressure. It is a standing commitment, not an opportunistic cleanup.

Do not interleave a behaviour change with the move; a reviewer cannot tell the
two apart in the diff.
