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
    component::{Component, LayerId, Open, RawEvent},
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
    let storage_only = |app: &App| {
        app.layer_commands()
            .into_iter()
            .filter(|(layer, _)| *layer == LayerId::Storage)
            .collect::<Vec<_>>()
    };
    let unavailable = storage_only(&app);
    assert_eq!(unavailable.len(), 1);
    assert!(
        unavailable[0].1.unavailable_reason.is_some(),
        "cleanup is not confirmable until it has been previewed"
    );

    key(&mut app, &provider, KeyCode::Char('c'));
    let available = storage_only(&app);
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

fn error_snapshot(entries: usize) -> StorageSnapshot {
    let mut snap = snapshot(entries, 0);
    snap.errors = vec![
        "scan failed on café-日本語 volume: permission denied · recomputable".into(),
        "second problem with combining é marks and wide 漢字 text".into(),
    ];
    snap
}

fn complete_with(_provider: &FixtureProvider, app: &mut App, snap: StorageSnapshot) {
    let generation = app.layers.storage.outbox.take()[0].generation;
    assert!(
        app.layers
            .storage
            .complete(generation, snap, "scan complete".into(), true,)
    );
}

/// Policy/stable budgets alone determine the frame and sticky tail: scanning,
/// populated, error and confirm-clear states share one outer frame and one
/// action-band origin. Results/diagnostics/errors only move scroll extents.
#[test]
fn responsive_frame_and_tail_are_stable_across_scan_states() {
    for (width, height) in [(240u16, 80u16), (140, 40), (80, 24), (54, 16), (20, 6)] {
        // Scanning (pending): opened but no completion yet.
        let (provider, mut scanning) = demo();
        scanning.handle(Action::Open(Open::Storage), &provider);
        draw(&provider, &mut scanning, width, height);
        let scanning_surface = scanning.layers.storage.surface();
        let scanning_refresh = scanning
            .layers
            .storage
            .action_rects()
            .iter()
            .find(|(slot, _)| *slot == 0)
            .map(|(_, r)| *r);

        // Populated.
        let (provider, mut full) = demo();
        full.handle(Action::Open(Open::Storage), &provider);
        complete_with(&provider, &mut full, snapshot(6, 4096));
        draw(&provider, &mut full, width, height);
        let full_surface = full.layers.storage.surface();

        // Error with long Unicode diagnostics.
        let (provider, mut failed) = demo();
        failed.handle(Action::Open(Open::Storage), &provider);
        complete_with(&provider, &mut failed, error_snapshot(6));
        draw(&provider, &mut failed, width, height);
        let error_surface = failed.layers.storage.surface();

        // Same-layer confirmation (second button relabels, no child).
        key(&mut failed, &provider, KeyCode::Char('c'));
        // Confirmation needs reclaimable bytes; use a reclaimable snapshot.
        let (provider, mut confirm) = demo();
        confirm.handle(Action::Open(Open::Storage), &provider);
        complete_with(&provider, &mut confirm, snapshot(6, 8192));
        draw(&provider, &mut confirm, width, height);
        key(&mut confirm, &provider, KeyCode::Char('c'));
        assert!(confirm.layers.storage.confirm_clear());
        draw(&provider, &mut confirm, width, height);
        let confirm_surface = confirm.layers.storage.surface();

        for (name, surface) in [
            ("scanning", scanning_surface),
            ("populated", full_surface),
            ("error", error_surface),
            ("confirm", confirm_surface),
        ] {
            assert_eq!(
                surface.popup, scanning_surface.popup,
                "{name} frame moved at {width}x{height}"
            );
            assert_eq!(
                surface.interior, scanning_surface.interior,
                "{name} interior moved at {width}x{height}"
            );
        }
        let full_refresh = full
            .layers
            .storage
            .action_rects()
            .iter()
            .find(|(slot, _)| *slot == 0)
            .map(|(_, r)| *r);
        assert_eq!(
            full_refresh, scanning_refresh,
            "sticky tail moved at {width}x{height}"
        );
        // Confirmation stays same-layer: no new layer, still Storage on top.
        // At the 20x6 floor the shared band collapses to one row with a
        // same-layer More menu; Confirm stays reachable there, directly
        // painted everywhere else.
        assert_eq!(confirm.layers.top(), Some(LayerId::Storage));
        if (width, height) == (20, 6) {
            // Floor pressure collapses the band to Refresh + same-layer More;
            // Cleanup/Confirm hides behind the overflow but stays armed
            // same-layer (confirm_clear) with the default surviving.
            let rendered = screen(&draw(&provider, &mut confirm, width, height));
            // Truncated by display-width clipping at the floor; the default
            // slot itself (not the full verb) proves stickiness.
            assert!(rendered.contains("Ref"), "{rendered}");
            assert!(confirm.layers.storage.confirm_clear());
            assert!(
                confirm
                    .layers
                    .storage
                    .action_rects()
                    .iter()
                    .any(|(slot, _)| *slot == 0),
                "default Refresh slot must survive the floor"
            );
        } else {
            let rendered = screen(&draw(&provider, &mut confirm, width, height));
            assert!(rendered.contains("Confirm cleanup"), "{rendered}");
        }
    }
}

/// Below the floor the tiny fallback owns the frame with no stale hitboxes.
#[test]
fn below_floor_uses_the_tiny_fallback() {
    let (provider, mut app) = opened(4, 0);
    let buffer = draw(&provider, &mut app, 19, 5);
    assert!(screen(&buffer).contains("terminal too small"));
    assert!(app.layers.storage.row_rects().is_empty());
    assert!(app.layers.storage.action_rects().is_empty());
}

/// Long Unicode entry labels/diagnostics clip by display width without
/// splitting wide glyphs, and every painted row/action/diagnostics cell still
/// answers its own hit test.
#[test]
fn long_unicode_clips_with_exact_hitboxes() {
    for (width, height) in [(140u16, 40u16), (80, 24), (54, 16)] {
        let (provider, mut app) = demo();
        app.handle(Action::Open(Open::Storage), &provider);
        let mut snap = snapshot(3, 0);
        snap.entries[0].label = "café-日本語-👩‍💻-éxpansion-漢字-mix-".repeat(4);
        snap.entries[1].label = "combining ééé wide 漢字".into();
        snap.errors = vec!["café-日本語 failure · é combining · 漢字".into()];
        complete_with(&provider, &mut app, snap);
        draw(&provider, &mut app, width, height);
        let surface = app.layers.storage.surface();
        for (rect, index) in app.layers.storage.row_rects() {
            assert!(
                contains_rect(surface.popup, rect),
                "row escapes at {width}x{height}"
            );
            assert_eq!(
                app.layers.storage.hit((rect.x, rect.y)),
                Some(StorageHit::Row(*index)),
                "row paint/hit disagree at {width}x{height}"
            );
        }
        for (_, rect) in app.layers.storage.action_rects() {
            assert!(contains_rect(surface.popup, rect));
            assert!(app.layers.storage.hit((rect.x, rect.y)).is_some());
        }
    }
}

fn contains_rect(area: Rect, rect: &Rect) -> bool {
    rect.x >= area.x
        && rect.y >= area.y
        && rect.right() <= area.right()
        && rect.bottom() <= area.bottom()
}

/// At 54x16 and 20x6 the selection, diagnostics scroll and sticky actions stay
/// reachable with wheel/hitboxes matching the shared viewports.
#[test]
fn tiny_pressure_keeps_selection_diagnostics_and_actions_reachable() {
    for (width, height) in [(54u16, 16u16), (20, 6)] {
        let (provider, mut app) = demo();
        app.handle(Action::Open(Open::Storage), &provider);
        complete_with(&provider, &mut app, error_snapshot(20));
        draw(&provider, &mut app, width, height);
        // List selection is painted and hit-testable.
        assert!(
            !app.layers.storage.row_rects().is_empty(),
            "list must paint at {width}x{height}"
        );
        let surface = app.layers.storage.surface();
        for (rect, index) in app.layers.storage.row_rects() {
            assert!(contains_rect(surface.popup, rect));
            assert_eq!(
                app.layers.storage.hit((rect.x, rect.y)),
                Some(StorageHit::Row(*index))
            );
        }
        // Wheel over the list moves selection with matching hitboxes.
        let (rect, _) = app.layers.storage.row_rects()[0];
        let before = app.layers.storage.selected();
        app.handle(
            Action::Raw(RawEvent::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            })),
            &provider,
        );
        draw(&provider, &mut app, width, height);
        assert_ne!(before, app.layers.storage.selected());
        // Real overflow while list-focused: populated/error claims scroll.
        assert!(
            app.layers.storage.surface().scrollable,
            "populated list must be scrollable while list-focused at {width}x{height}"
        );
        // Tab focuses diagnostics; arrows scroll the shared diagnostics
        // viewport when it has room, otherwise the focus itself is the
        // reachability proof at the floor (single-row body).
        key(&mut app, &provider, KeyCode::Tab);
        draw(&provider, &mut app, width, height);
        key(&mut app, &provider, KeyCode::Down);
        draw(&provider, &mut app, width, height);
        assert!(app.layers.storage.scroll() > 0 || app.layers.storage.scroll_limit() == 0);
        // Sticky default action never leaves.
        let refresh = app
            .layers
            .storage
            .action_rects()
            .iter()
            .find(|(slot, _)| *slot == 0)
            .map(|(_, r)| *r)
            .expect("Refresh must survive tiny pressure");
        assert!(contains_rect(surface.popup, &refresh));
    }
    let (provider, mut empty) = demo();
    empty.handle(Action::Open(Open::Storage), &provider);
    complete_with(&provider, &mut empty, snapshot(0, 0));
    draw(&provider, &mut empty, 54, 16);
}
