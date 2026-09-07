//! Acceptance for the Storage layer as a component (docs/component-model.md
//! §6.1). What is asserted here is the contract, not the drawing: the geometry
//! `render` recorded is the geometry `hit()` answers with, the shell's
//! selection bound is the surface the component published, input reaches the
//! component as raw events, and a click outside the popup is contained.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use lvu::{
    Action, App, RowProvider, StorageCategory, StorageEntry, StorageRequestKind, StorageSnapshot,
    component::{Component, Open, RawEvent},
    components::storage::StorageHit,
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};

/// dialog-system.md quotes these four terminals; §12.13 drops the entry status
/// column at the narrowest of them.
const SIZES: [(u16, u16); 4] = [(140, 40), (100, 30), (80, 24), (54, 16)];

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

fn snapshot(entries: usize, reclaimable: u64) -> StorageSnapshot {
    StorageSnapshot {
        entries: (0..entries)
            .map(|index| StorageEntry {
                category: StorageCategory::Derived,
                label: format!("ed4a0c76-63b7-59e8-bbbd-5167f7c3ec5c.d17625e2-{index}.rows.idx"),
                bytes: 2662,
                reclaimable: if index == 0 { reclaimable } else { 0 },
                status: "unused, recomputable".into(),
            })
            .collect(),
        total_bytes: 138_000,
        reclaimable_bytes: reclaimable,
        row_cache_bytes: 29_184,
        row_cache_limit: 4_194_304,
        query_index_bytes: 208,
        query_index_limit: 268_435_456,
        derived_index_limit_per_source: 268_435_456,
        derived_index_limit_total: 5_368_709_120,
        truncated: false,
        errors: Vec::new(),
    }
}

/// Opens the layer and lands one completed scan on it.
fn opened(entries: usize, reclaimable: u64) -> (FixtureProvider, App) {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Storage), &provider);
    let generation = app.layers.storage.outbox.take()[0].generation;
    assert!(app.layers.storage.complete(
        generation,
        snapshot(entries, reclaimable),
        "scan complete".into(),
        true,
    ));
    (provider, app)
}

#[test]
fn every_recorded_rect_was_painted_and_answers_the_hit_test() {
    for (width, height) in SIZES {
        let (provider, mut app) = opened(6, 4096);
        let buffer = draw(&provider, &mut app, width, height);
        let rendered = screen(&buffer);
        assert!(rendered.contains("Storage"), "at {width}x{height}");
        assert!(rendered.contains("[ Refresh ]"), "at {width}x{height}");

        let surface = app.layers.storage.surface();
        // §3/§5.1: the shell's selection bound is exactly what the component
        // published; nothing recomputes a modal rect of its own.
        assert_eq!(
            app.hit_regions.selection_modal,
            Some(surface.interior),
            "at {width}x{height}"
        );
        assert!(
            surface.interior.width > 0 && surface.interior.height > 0,
            "at {width}x{height}"
        );

        // §7.9: a rect `hit()` answers for must be inside the surface that was
        // drawn this frame.
        let rects: Vec<Rect> = app
            .layers
            .storage
            .row_rects()
            .iter()
            .map(|(rect, _)| *rect)
            .chain(
                app.layers
                    .storage
                    .action_rects()
                    .iter()
                    .map(|(_, rect)| *rect),
            )
            .collect();
        assert!(!rects.is_empty(), "at {width}x{height}");
        for rect in rects {
            assert!(
                surface.interior.union(rect) == surface.interior,
                "{rect:?} escapes {:?} at {width}x{height}",
                surface.interior
            );
            assert!(
                app.layers.storage.hit((rect.x, rect.y)).is_some(),
                "{rect:?} was painted but is not hit-testable at {width}x{height}"
            );
        }
    }
}

#[test]
fn row_and_button_hitboxes_drive_selection_and_the_two_step_cleanup() {
    for (width, height) in SIZES {
        let (provider, mut app) = opened(6, 4096);
        draw(&provider, &mut app, width, height);

        // A click on the second visible row selects that entry.
        let (rect, index) = app.layers.storage.row_rects()[1];
        assert_eq!(
            app.layers.storage.hit((rect.x, rect.y)),
            Some(StorageHit::Row(index)),
            "at {width}x{height}"
        );
        click(&mut app, &provider, (rect.x, rect.y));
        assert_eq!(app.layers.storage.selected(), index, "at {width}x{height}");

        // `[ Preview cleanup ]` arms the confirmation; nothing is submitted yet.
        let cleanup = app
            .layers
            .storage
            .action_rects()
            .iter()
            .find_map(|(slot, rect)| (*slot == 1).then_some(*rect))
            .expect("the cleanup button is always drawn");
        assert_eq!(
            app.layers.storage.hit((cleanup.x, cleanup.y)),
            Some(StorageHit::Cleanup),
            "at {width}x{height}"
        );
        click(&mut app, &provider, (cleanup.x, cleanup.y));
        assert!(app.layers.storage.confirm_clear(), "at {width}x{height}");
        assert!(
            app.layers.storage.outbox.take().is_empty(),
            "preview must not submit at {width}x{height}"
        );

        // The second click submits, and the label was destructive in between.
        let rendered = screen(&draw(&provider, &mut app, width, height));
        assert!(rendered.contains("[ Confirm cleanup ]"), "{rendered}");
        let cleanup = app
            .layers
            .storage
            .action_rects()
            .iter()
            .find_map(|(slot, rect)| (*slot == 1).then_some(*rect))
            .expect("the cleanup button is always drawn");
        click(&mut app, &provider, (cleanup.x, cleanup.y));
        let requests = app.layers.storage.outbox.take();
        assert!(
            matches!(
                requests.first().map(|request| request.kind),
                Some(StorageRequestKind::ClearUnusedDerived)
            ),
            "at {width}x{height}: {requests:?}"
        );

        // `[ Refresh ]` starts a new fenced scan.
        let refresh = app
            .layers
            .storage
            .action_rects()
            .iter()
            .find_map(|(slot, rect)| (*slot == 0).then_some(*rect))
            .expect("the refresh button is always drawn");
        click(&mut app, &provider, (refresh.x, refresh.y));
        let requests = app.layers.storage.outbox.take();
        assert!(
            requests
                .iter()
                .any(|request| request.kind == StorageRequestKind::Scan),
            "at {width}x{height}: {requests:?}"
        );
    }
}

#[test]
fn a_refusal_to_reclaim_is_reported_and_leaves_the_inventory_intact() {
    // Ownership-aware refusals arrive as the worker's status text; the entries
    // and their statuses are the inventory, and cleanup must not touch them.
    let (provider, mut app) = opened(3, 0);
    draw(&provider, &mut app, 100, 30);
    key(&mut app, &provider, KeyCode::Char('c'));
    assert!(!app.layers.storage.confirm_clear());
    assert!(app.layers.storage.outbox.take().is_empty());
    let rendered = screen(&draw(&provider, &mut app, 100, 30));
    assert!(
        rendered.contains("no unused derived indexes are reclaimable"),
        "{rendered}"
    );
    assert_eq!(app.layers.storage.snapshot().entries.len(), 3);
}

#[test]
fn the_keymap_is_the_components_and_escape_cancels_the_scan_in_flight() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Storage), &provider);
    // Opening starts a scan; the layer is on the stack and owns input.
    assert_eq!(app.focus, lvu::Focus::Layer);
    let opening = app.layers.storage.outbox.take();
    assert_eq!(opening[0].kind, StorageRequestKind::Scan);
    let generation = opening[0].generation;
    assert!(app.layers.storage.complete(
        generation,
        snapshot(4, 4096),
        "scan complete".into(),
        true,
    ));
    draw(&provider, &mut app, 100, 30);

    key(&mut app, &provider, KeyCode::Char('j'));
    assert_eq!(app.layers.storage.selected(), 1);
    key(&mut app, &provider, KeyCode::Char('k'));
    assert_eq!(app.layers.storage.selected(), 0);
    key(&mut app, &provider, KeyCode::Down);
    assert_eq!(app.layers.storage.selected(), 1);

    // `r` re-scans, and Esc while that scan is outstanding cancels it.
    key(&mut app, &provider, KeyCode::Char('r'));
    let scan = app.layers.storage.outbox.take();
    assert_eq!(scan[0].kind, StorageRequestKind::Scan);
    key(&mut app, &provider, KeyCode::Esc);
    let cancel = app.layers.storage.outbox.take();
    assert_eq!(cancel[0].kind, StorageRequestKind::Cancel);
    assert_eq!(cancel[0].generation, scan[0].generation);
    assert_eq!(app.focus, lvu::Focus::Logs);
    assert!(!app.layers.storage.is_open());
}

#[test]
fn a_click_outside_the_popup_is_contained_by_the_shell() {
    let (provider, mut app) = opened(6, 4096);
    draw(&provider, &mut app, 100, 30);
    let before = app.layers.storage.selected();
    let popup = app.layers.storage.surface().popup;
    assert!(popup.y > 0, "the modal never fills the frame at 100x30");
    // The log is behind the scrim at row 0; the shell must not let the layer or
    // the base UI see this click.
    click(&mut app, &provider, (0, 0));
    assert_eq!(app.layers.storage.selected(), before);
    assert_eq!(app.focus, lvu::Focus::Layer);
    assert!(app.layers.storage.hit((0, 0)).is_none());
}

#[test]
fn the_palette_entry_comes_from_the_component_not_a_peek_at_its_state() {
    let (provider, mut app) = opened(4, 4096);
    draw(&provider, &mut app, 100, 30);
    let unavailable = app.layer_commands();
    assert_eq!(unavailable.len(), 1);
    assert!(
        unavailable[0].1.unavailable_reason.is_some(),
        "cleanup is not confirmable until it has been previewed"
    );

    key(&mut app, &provider, KeyCode::Char('c'));
    let available = app.layer_commands();
    assert!(available[0].1.unavailable_reason.is_none());

    // Executing it reaches the component as `Event::Command`.
    let (layer, entry) = available[0];
    app.handle(Action::Command(layer, entry.spec.id), &provider);
    let requests = app.layers.storage.outbox.take();
    assert!(matches!(
        requests.first().map(|request| request.kind),
        Some(StorageRequestKind::ClearUnusedDerived)
    ));
}

#[test]
fn a_completion_for_a_superseded_generation_is_ignored() {
    let (provider, mut app) = opened(2, 0);
    draw(&provider, &mut app, 100, 30);
    key(&mut app, &provider, KeyCode::Char('r'));
    let generation = app.layers.storage.outbox.take()[0].generation;
    assert!(
        !app.layers
            .storage
            .complete(generation - 1, snapshot(9, 0), "stale".into(), true)
    );
    assert_eq!(app.layers.storage.snapshot().entries.len(), 2);
    assert!(
        app.layers
            .storage
            .complete(generation, snapshot(9, 0), "scan complete".into(), true)
    );
    assert_eq!(app.layers.storage.snapshot().entries.len(), 9);
}

#[test]
fn key_kinds_other_than_press_are_ignored() {
    let (provider, mut app) = opened(4, 0);
    draw(&provider, &mut app, 100, 30);
    let before = app.layers.storage.selected();
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent {
            code: KeyCode::Char('j'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: crossterm::event::KeyEventState::NONE,
        })),
        &provider,
    );
    assert_eq!(app.layers.storage.selected(), before);
}
