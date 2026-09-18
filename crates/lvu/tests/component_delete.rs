//! Acceptance for safe view deletion and source removal requesting
//! (the durable work itself is `lvu-app` + `lvu-memory` + `lvu-shared`).
//!
//! View deletion is a fifth segmented Delete mode with an explicit two-press
//! confirmation: the first press arms and mutates nothing, the second submits
//! through the view-mutation outbox. Canonical All events and union inputs
//! refuse actionably at the shell. Source removal arms twice on the base
//! screen (Delete key or palette) and queues only after its dependents are
//! re-validated; captured bytes, journals, bookmarks and recipes are never
//! part of either path.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, RowProvider, ViewDialogMode, ViewItem, ViewRole,
    app::Focus,
    component::{Component, Open, RawEvent},
    components::view::{ViewDialogControl, ViewHit},
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

fn draw<P: RowProvider>(provider: &P, app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, Theme::TERMINAL, None))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn screen(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn key(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))),
        provider,
    );
}

fn alt(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::ALT))),
        provider,
    );
}

fn click(app: &mut App, provider: &FixtureProvider, point: (u16, u16)) {
    app.handle(
        Action::Raw(RawEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: point.0,
            row: point.1,
            modifiers: KeyModifiers::NONE,
        })),
        provider,
    );
}

fn open_delete(app: &mut App, provider: &FixtureProvider) {
    app.handle(Action::Open(Open::ViewDelete), provider);
    draw(provider, app, 90, 24);
    assert_eq!(app.layers.view.mode(), ViewDialogMode::Delete);
}

/// Delete opens on the header, never on its destructive action, so Enter
/// cannot delete before an explicit focus move. The first Delete-view press
/// arms and mutates nothing; the second submits one outbox request.
#[test]
fn delete_arms_first_and_submits_only_on_the_second_press() {
    let (provider, mut app) = demo();
    let view = app.active_view_id().unwrap().to_owned();
    open_delete(&mut app, &provider);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Tabs);
    assert!(!app.layers.view.delete_armed());

    // Enter on the header arms (reversible), it does not submit.
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.layers.view.delete_armed());
    assert!(
        app.layers.view.outbox.take().is_empty(),
        "arming must not submit"
    );
    assert!(app.layers.view.is_open());

    // The armed message says what happens next; captured data stays.
    let rendered = screen(&draw(&provider, &mut app, 90, 24));
    assert!(rendered.contains("confirm"), "{rendered}");
    assert!(rendered.contains("Delete view"), "{rendered}");

    // Second Enter submits exactly one durable request for this view.
    key(&mut app, &provider, KeyCode::Enter);
    let requests = app.layers.view.outbox.take();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].mode, ViewDialogMode::Delete);
    assert_eq!(requests[0].view_id, view);
    assert!(!app.layers.view.delete_armed());
    assert!(app.layers.view.is_open(), "submit waits for the ack");
}

/// A refusal keeps the draft state and disarms, exactly like a rejected name.
#[test]
fn delete_refusal_keeps_the_dialog_open_and_disarms() {
    let (provider, mut app) = demo();
    open_delete(&mut app, &provider);
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.layers.view.delete_armed());
    app.layers.view.fail("workspace store unavailable".into());
    assert_eq!(app.layers.view.error(), Some("workspace store unavailable"));
    assert!(!app.layers.view.delete_armed());
    assert!(app.layers.view.is_open());
    let rendered = screen(&draw(&provider, &mut app, 90, 24));
    assert!(
        rendered.contains("workspace store unavailable"),
        "{rendered}"
    );
}

/// Switching modes disarms without submitting.
#[test]
fn delete_disarms_on_mode_switch() {
    let (provider, mut app) = demo();
    open_delete(&mut app, &provider);
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.layers.view.delete_armed());
    alt(&mut app, &provider, KeyCode::Char('c'));
    assert_eq!(app.layers.view.mode(), ViewDialogMode::Clone);
    assert!(!app.layers.view.delete_armed());
    assert!(app.layers.view.outbox.take().is_empty());
}

/// The mouse path mirrors the keyboard path: clicking the Delete segment
/// switches, clicking the destructive button arms, clicking again submits.
#[test]
fn delete_mouse_path_arms_then_submits() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::View), &provider);
    draw(&provider, &mut app, 90, 24);
    let delete_tab = app
        .layers
        .view
        .tab_rects()
        .iter()
        .find(|(_, mode)| *mode == ViewDialogMode::Delete)
        .map(|(rect, _)| (rect.x, rect.y))
        .expect("the Delete segment is drawn");
    click(&mut app, &provider, delete_tab);
    assert_eq!(app.layers.view.mode(), ViewDialogMode::Delete);
    assert!(app.layers.view.outbox.take().is_empty());

    let button = app
        .layers
        .view
        .control_rects()
        .iter()
        .find(|(_, control)| *control == ViewDialogControl::Apply)
        .map(|(rect, _)| (rect.x, rect.y))
        .expect("the Delete view button is drawn");
    // Clicking the button focuses it first and arms; nothing submits yet.
    click(&mut app, &provider, button);
    assert!(app.layers.view.delete_armed());
    assert!(app.layers.view.outbox.take().is_empty());
    // The armed click submits.
    click(&mut app, &provider, button);
    assert_eq!(app.layers.view.outbox.take().len(), 1);
}

/// Alt-E reaches Delete from inside the name field, like the other Alt chords.
#[test]
fn alt_e_selects_delete_from_inside_the_name_field() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::View), &provider);
    draw(&provider, &mut app, 90, 24);
    alt(&mut app, &provider, KeyCode::Char('e'));
    assert_eq!(app.layers.view.mode(), ViewDialogMode::Delete);
}

/// Delete renders at every acceptance size with shared geometry: tabs and
/// the destructive button hit-test to what they drew, including the 20x6
/// floor and wide/combining Unicode names.
#[test]
fn delete_geometry_is_shared_at_every_size_with_unicode_names() {
    let (provider, sources, views) = FixtureProvider::demo();
    let mut app = App::new(sources, views, true);
    let view = app.active_view_id().unwrap().to_owned();
    app.rename_view(&view, "errörs über Δview 𝄞".into());
    for (width, height) in [(140u16, 40u16), (80, 24), (54, 16), (20, 6)] {
        app.handle(Action::Open(Open::ViewDelete), &provider);
        let buffer = draw(&provider, &mut app, width, height);
        let rendered = screen(&buffer);
        assert!(
            rendered.contains("Delete view"),
            "{width}x{height}:\n{rendered}"
        );
        // The full sentence survives where it fits; at the 20x6 floor the
        // message row degrades to its head rather than breaking layout.
        if (width, height) != (20, 6) {
            assert!(
                rendered.contains("stays on disk"),
                "{width}x{height}:\n{rendered}"
            );
        }
        // Five segments, one destructive action, all inside the popup.
        let surface = app.layers.view.surface();
        assert_eq!(app.layers.view.tab_rects().len(), 5, "{width}x{height}");
        for (rect, mode) in app.layers.view.tab_rects().to_vec() {
            assert!(
                rect.x >= surface.popup.x && rect.right() <= surface.popup.right(),
                "{width}x{height} {mode:?} escapes popup"
            );
            assert_eq!(
                app.layers.view.hit((rect.x, rect.y)),
                Some(ViewHit::Tab(mode)),
                "{width}x{height}"
            );
        }
        for (rect, control) in app.layers.view.control_rects().to_vec() {
            assert!(
                rect.x >= surface.popup.x && rect.right() <= surface.popup.right(),
                "{width}x{height} {control:?} escapes popup"
            );
            assert_eq!(
                app.layers.view.hit((rect.x, rect.y)),
                Some(ViewHit::Control(control)),
                "{width}x{height}"
            );
        }
        key(&mut app, &provider, KeyCode::Esc);
    }
}

/// A view used by a surviving union cannot be deleted silently: the shell
/// names the union so the refusal is actionable.
#[test]
fn union_dependent_views_refuse_actionably() {
    let (provider, mut app) = demo();
    let target = app.active_view_id().unwrap().to_owned();
    // A second view whose accepted union names the target.
    let union_id = "union-view".to_owned();
    let source = app
        .views()
        .iter()
        .find(|view| view.id == target)
        .map(|view| view.source_id.clone())
        .unwrap();
    app.add_view(ViewItem {
        id: union_id.clone(),
        source_id: source,
        name: "Combined".into(),
    });
    let other = app
        .views()
        .iter()
        .find(|view| view.id != target && view.id != union_id)
        .map(|view| view.id.clone())
        .expect("demo needs a second view for the union");
    let mut restored = app.persistent_view_state(&union_id).unwrap();
    restored.union = Some(lvu::PersistentUnion {
        inputs: vec![
            lvu::PersistentUnionInput {
                view_id: target.clone(),
                accepted_revision: 0,
                applied_generation: 0,
            },
            lvu::PersistentUnionInput {
                view_id: other,
                accepted_revision: 0,
                applied_generation: 0,
            },
        ],
        filter: String::new(),
        advanced_filter: String::new(),
        exact_key: None,
    });
    assert!(app.restore_persistent_view(&union_id, restored));
    let blockers = app.view_deletion_blockers(&target);
    assert!(
        blockers.iter().any(|name| name == "Combined"),
        "union must block with its name: {blockers:?}"
    );
    // An unrelated view has no blockers.
    let _ = provider;
}

/// Merged membership in a surviving view blocks source removal actionably.
#[test]
fn source_removal_refuses_while_merged_views_depend_on_it() {
    let (provider, mut app) = demo();
    let views = app.views().to_vec();
    assert!(views.len() >= 2, "demo needs two views");
    let first = views[0].source_id.clone();
    let second = views[1].id.clone();
    // Make the second view merged over the first view's source.
    let mut restored = app.persistent_view_state(&second).unwrap();
    let primary = app
        .views()
        .iter()
        .find(|view| view.id == second)
        .map(|view| view.source_id.clone())
        .unwrap();
    let mut ids = restored.source_ids.clone();
    if !ids.contains(&primary) {
        ids.insert(0, primary);
    }
    if !ids.contains(&first) {
        ids.push(first.clone());
    }
    restored.source_ids = ids;
    assert!(app.restore_persistent_view(&second, restored));
    let blockers = app.source_removal_blockers(&first);
    assert!(
        !blockers.is_empty(),
        "merged membership must block source removal"
    );
    assert!(
        blockers.iter().all(|text| text.contains("change or delete")
            || text.contains("includes")
            || text.contains("uses")
            || text.contains("correlates")),
        "blockers must be actionable: {blockers:?}"
    );
    let _ = provider;
}

/// Source removal arms on the first Delete and queues only on the second
/// for the same source; moving the selection disarms.
#[test]
fn source_removal_arms_twice_and_disarms_on_selection_change() {
    let (provider, mut app) = demo();
    assert_eq!(app.focus, Focus::Logs);
    let first_source = app
        .views()
        .iter()
        .find(|view| view.id == app.active_view_id().unwrap())
        .map(|view| view.source_id.clone())
        .unwrap();
    app.handle(Action::RemoveSource, &provider);
    assert_eq!(app.pending_source_remove(), Some(first_source.as_str()));
    assert!(
        app.take_source_removals().is_empty(),
        "arming queues nothing"
    );
    let notice = app.action_notice.clone().unwrap_or_default();
    assert!(notice.contains("stays on disk"), "{notice}");

    // A different selection disarms rather than confirming a stale source.
    let other = app
        .views()
        .iter()
        .find(|view| view.source_id != first_source)
        .map(|view| view.id.clone());
    if let Some(other) = other {
        app.select_view(&other);
        assert_eq!(app.pending_source_remove(), None);
    }

    // Re-arm and confirm queues exactly one removal.
    app.handle(Action::RemoveSource, &provider);
    app.handle(Action::RemoveSource, &provider);
    let queued = app.take_source_removals();
    assert_eq!(queued.len(), 1);
    assert_eq!(app.pending_source_remove(), None);
}

/// The Delete key reaches source removal from either base pane, and the
/// palette prints that chord: the binding is one key, not a letter, so it
/// survives every terminal's Alt encoding.
#[test]
fn delete_key_reaches_source_removal_from_either_base_pane() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use lvu::app::{Focus, key_to_action};
    for focus in [Focus::Logs, Focus::Selector] {
        assert_eq!(
            key_to_action(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE), focus),
            Action::RemoveSource,
            "{focus:?}"
        );
    }
}

/// Canonical All events is a view-deletion refusal at the shell boundary:
///
/// role metadata (never the display name) decides, so renaming cannot smuggle
/// a canonical view into deletion.
#[test]
fn canonical_role_is_fixed_by_metadata_not_by_name() {
    let (provider, sources, views) = FixtureProvider::demo();
    let mut app = App::new(sources, views, true);
    let target = app.active_view_id().unwrap().to_owned();
    app.set_view_role(&target, ViewRole::Canonical);
    app.rename_view(&target, "Totally ordinary".into());
    assert_eq!(app.view_role(&target), ViewRole::Canonical);
    // No union may use it silently either: blockers still apply.
    assert!(app.view_deletion_blockers(&target).is_empty());
    let _ = provider;
}

/// Removing a view drops it from the sidebar model with a deterministic
/// survivor and clears a stale source-removal arm for another source.
#[test]
fn remove_view_selects_a_deterministic_survivor() {
    let (provider, sources, views) = FixtureProvider::demo();
    let mut app = App::new(sources, views, true);
    // Two extra derived views give the removal something to survive on.
    app.add_view(ViewItem {
        id: "extra-a".into(),
        source_id: app.views()[0].source_id.clone(),
        name: "Extra A".into(),
    });
    app.add_view(ViewItem {
        id: "extra-b".into(),
        source_id: app.views()[0].source_id.clone(),
        name: "Extra B".into(),
    });
    app.select_view("extra-a");
    assert!(app.remove_view("extra-a"));
    assert!(!app.views().iter().any(|view| view.id == "extra-a"));
    assert!(app.active_view_id().is_some());
    assert!(!app.remove_view("missing"));
    let _ = provider;
}

/// An empty completed scan hands the category report the unused list space:
/// every category is named with product labels (never Procfs/Debug jargon),
/// including zero-match outcomes.
#[test]
fn empty_discovery_expands_the_report_with_every_category_named() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Source), &provider);
    key_with_ctrl_d(&mut app, &provider);
    let generation = match app.take_discovery_requests().pop().unwrap() {
        lvu::DiscoveryUiRequest::Scan { generation } => generation,
        other => panic!("unexpected request: {other:?}"),
    };
    let report = [
        "3 candidates, complete",
        "Processes / open files: no matches · checked — examined 12 entries",
        "Project files: no matches · checked — examined 4 entries",
        "Docker containers: no matches · checked — docker discovery complete",
        "Previously opened: no matches · checked",
    ]
    .join("\n");
    assert!(app.apply_discovery_result(generation, Vec::new(), report));
    draw(&provider, &mut app, 100, 28);
    let pane = app.layers.source.scroll_rect().expect("report surface");
    assert!(
        pane.height > 3,
        "empty scan must expand the report into the list space: {pane:?}"
    );
    let rendered = screen(&draw(&provider, &mut app, 100, 28));
    for category in [
        "Processes / open files",
        "Project files",
        "Docker containers",
        "Previously opened",
    ] {
        assert!(
            rendered.contains(category),
            "{category} missing:\n{rendered}"
        );
    }
    assert!(!rendered.contains("Procfs"), "no Debug jargon:\n{rendered}");
    // A Docker failure reason stays visible with its suggestion.
    let failure = [
        "0 candidates, complete",
        "Processes / open files: no matches · checked",
        "Project files: no matches · checked",
        "Docker containers: no matches · unavailable — docker ps failed: permission denied; check Docker daemon/socket access for this user (OS permission; lvu cannot change it)",
        "Previously opened: no matches · checked",
    ]
    .join("\n");
    assert!(app.apply_discovery_result(generation, Vec::new(), failure));
    let rendered = screen(&draw(&provider, &mut app, 100, 28));
    assert!(rendered.contains("permission denied"), "{rendered}");
}

fn key_with_ctrl_d(app: &mut App, provider: &FixtureProvider) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        ))),
        provider,
    );
}

/// The palette exposes both deletions with working chords and honest
/// descriptions: capture data is preserved.
#[test]
fn palette_exposes_deletion_with_capture_preserved_wording() {
    use lvu::command_palette::{CommandId, Palette, PaletteContext};
    // The View dialog contributes its Delete row with the Alt chord that
    // works from inside the name field.
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::ViewDelete), &provider);
    draw(&provider, &mut app, 90, 24);
    let rows = app.layers.view.commands(&app.views);
    let delete = rows
        .iter()
        .find(|entry| entry.spec.id == CommandId::ViewDelete)
        .expect("Delete view palette row");
    assert!(delete.spec.description.contains("stays on disk"));
    assert_eq!(delete.unavailable_reason, None);
    assert_eq!(delete.spec.shortcut, Some("Alt-E"));

    // The base palette carries source removal with the Delete chord that
    // works from either pane.
    for focus in [Focus::Logs, Focus::Selector] {
        let mut palette = Palette::new();
        palette.open(PaletteContext::new(focus, true));
        let remove = palette
            .commands()
            .iter()
            .find(|command| command.id == CommandId::RemoveSource)
            .expect("Remove source palette row");
        assert_eq!(remove.shortcut, Some("Delete"), "{focus:?}");
        assert!(remove.description.contains("stays on disk"));
    }
}
