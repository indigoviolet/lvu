//! Acceptance for the Fields layer as a component (docs/component-model.md
//! §6.3 step 3).
//!
//! Fields' seam is the provider and the active view: it reads a frozen record
//! identity through `ctx.provider` and writes `ViewState.pinned_columns` /
//! `color_field`, with no outbox of its own. The two things it cannot do yet —
//! the correlation queue and Raw context — it hands back to the shell.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, Focus,
    app::FieldPickerControl,
    component::{Component, LayerId, Open, RawEvent},
    components::fields::FieldsHit,
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};

const SIZES: [(u16, u16); 4] = [(140, 40), (100, 30), (80, 24), (54, 16)];

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

fn draw(provider: &FixtureProvider, app: &mut App, width: u16, height: u16) -> Buffer {
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

fn opened() -> (FixtureProvider, App) {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 10);
    app.handle(Action::Open(Open::Fields), &provider);
    (provider, app)
}

#[test]
fn every_recorded_rect_was_painted_and_answers_the_hit_test() {
    for (width, height) in SIZES {
        let (provider, mut app) = opened();
        let rendered = screen(&draw(&provider, &mut app, width, height));
        assert!(rendered.contains("Fields"), "at {width}x{height}");

        let surface = app.layers.fields.surface();
        assert_eq!(
            app.hit_regions.selection_modal,
            Some(surface.interior),
            "at {width}x{height}"
        );
        let rows: Vec<(Rect, usize)> = app.layers.fields.row_rects().to_vec();
        let controls: Vec<(Rect, FieldPickerControl)> = app.layers.fields.control_rects().to_vec();
        assert!(!rows.is_empty(), "at {width}x{height}");
        assert!(!controls.is_empty(), "at {width}x{height}");
        for (rect, index) in rows {
            assert!(
                surface.interior.union(rect) == surface.interior,
                "row {index} at {rect:?} escapes the surface at {width}x{height}"
            );
            assert_eq!(
                app.layers.fields.hit((rect.x, rect.y)),
                Some(FieldsHit::Row(index)),
                "at {width}x{height}"
            );
        }
        for (rect, control) in controls {
            assert!(
                surface.interior.union(rect) == surface.interior,
                "{control:?} at {rect:?} escapes the surface at {width}x{height}"
            );
            assert_eq!(
                app.layers.fields.hit((rect.x, rect.y)),
                Some(FieldsHit::Control(control)),
                "at {width}x{height}"
            );
        }
    }
}

#[test]
fn pinning_and_colouring_write_the_active_view_and_nothing_else() {
    let (provider, mut app) = opened();
    draw(&provider, &mut app, 100, 30);
    let view = app.active_view_id().unwrap().to_owned();
    let field = lvu::components::fields::anchored_row(&app.views, &provider)
        .unwrap()
        .fields[0]
        .0
        .clone();

    key(&mut app, &provider, KeyCode::Char(' '));
    assert_eq!(
        app.view_state().unwrap().pinned_columns,
        std::slice::from_ref(&field)
    );
    key(&mut app, &provider, KeyCode::Char('c'));
    assert_eq!(
        app.view_state().unwrap().color_field.as_deref(),
        Some(field.as_str())
    );
    // The log re-renders from the view; nothing was queued for it (§4.2).
    assert!(app.take_query_requests().is_empty());

    // Toggling is symmetric.
    key(&mut app, &provider, KeyCode::Char(' '));
    assert!(app.view_state().unwrap().pinned_columns.is_empty());
    key(&mut app, &provider, KeyCode::Char('c'));
    assert!(app.view_state().unwrap().color_field.is_none());
    assert_eq!(app.active_view_id().unwrap(), view);
}

#[test]
fn the_button_row_pins_the_row_the_marker_points_at() {
    let (provider, mut app) = opened();
    draw(&provider, &mut app, 100, 30);
    // Move to the second field, then pin through the button rather than Space.
    key(&mut app, &provider, KeyCode::Down);
    let second = lvu::components::fields::anchored_row(&app.views, &provider)
        .unwrap()
        .fields[1]
        .0
        .clone();
    let pin = app
        .layers
        .fields
        .control_rects()
        .iter()
        .find_map(|(rect, control)| (*control == FieldPickerControl::Pin).then_some(*rect))
        .expect("the Pin button is drawn");
    click(&mut app, &provider, (pin.x + 2, pin.y));
    assert_eq!(app.view_state().unwrap().pinned_columns, [second]);
}

#[test]
fn clicking_a_row_selects_it_and_returns_focus_to_the_list() {
    let (provider, mut app) = opened();
    draw(&provider, &mut app, 100, 30);
    let pin = app
        .layers
        .fields
        .control_rects()
        .iter()
        .find_map(|(rect, control)| (*control == FieldPickerControl::Pin).then_some(*rect))
        .expect("the Pin button is drawn");
    let (row, index) = app.layers.fields.row_rects()[1];
    // Focus a button first, so the click has something to move focus away from.
    key(&mut app, &provider, KeyCode::Tab);
    assert_ne!(
        app.view_state().unwrap().field_picker_control,
        FieldPickerControl::List
    );
    let _ = pin;
    click(&mut app, &provider, (row.x, row.y));
    assert_eq!(app.view_state().unwrap().field_picker_selected, index);
    assert_eq!(
        app.view_state().unwrap().field_picker_control,
        FieldPickerControl::List
    );
}

#[test]
fn the_anchored_record_is_frozen_at_open_and_reread_every_frame() {
    let (provider, mut app) = opened();
    let anchor = lvu::components::fields::anchor_id(&app.views)
        .cloned()
        .unwrap();
    assert_eq!(
        app.view_state().unwrap().selected.as_ref(),
        Some(&anchor),
        "Fields opens on the selected record"
    );
    // Rows arriving underneath do not move the dialog off its record.
    app.sync_provider(&provider, 24);
    draw(&provider, &mut app, 100, 30);
    assert_eq!(
        lvu::components::fields::anchor_id(&app.views),
        Some(&anchor)
    );
}

#[test]
fn a_click_outside_the_popup_is_contained_by_the_shell() {
    let (provider, mut app) = opened();
    draw(&provider, &mut app, 100, 30);
    let before = app.view_state().unwrap().field_picker_selected;
    assert!(app.layers.fields.hit((0, 0)).is_none());
    click(&mut app, &provider, (0, 0));
    assert_eq!(app.view_state().unwrap().field_picker_selected, before);
    assert_eq!(app.focus, Focus::Layer);
}

#[test]
fn the_palette_entries_are_the_components_and_reach_it_as_a_command() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 10);
    let fields_entries = |app: &App| {
        app.layer_commands()
            .into_iter()
            .filter(|(layer, _)| *layer == LayerId::Fields)
            .collect::<Vec<_>>()
    };
    let closed = fields_entries(&app);
    assert_eq!(closed.len(), 6);
    assert!(
        closed
            .iter()
            .all(|(_, entry)| entry.unavailable_reason == Some("open Fields first")),
        "listed but muted from the base focus, as before"
    );

    app.handle(Action::Open(Open::Fields), &provider);
    draw(&provider, &mut app, 100, 30);
    let open = fields_entries(&app);
    let pin = open
        .iter()
        .find(|(_, entry)| entry.spec.id == lvu::command_palette::CommandId::PinField)
        .expect("Pin is Fields'");
    assert!(pin.1.unavailable_reason.is_none());
    assert_eq!(pin.1.spec.shortcut, Some("Space"));
    app.handle(Action::Command(pin.0, pin.1.spec.id), &provider);
    assert_eq!(app.view_state().unwrap().pinned_columns.len(), 1);
}

#[test]
fn raw_context_closes_the_layer_and_jumps_and_the_raw_stream_gives_it_back() {
    // raw-context-as-jump.md: `o` in Fields is the jump, with Fields as the
    // dialog to re-push on return. On the raw stream itself there is nothing
    // to jump to, so the shell says so and re-pushes Fields at once.
    let (provider, mut app) = opened();
    let view = app.active_view_id().unwrap().to_owned();
    app.set_view_role(&view, lvu::ViewRole::Canonical);
    draw(&provider, &mut app, 100, 30);
    key(&mut app, &provider, KeyCode::Char('o'));
    assert_eq!(app.focus, Focus::Layer);
    assert!(
        app.layers.fields.is_open(),
        "the dialog that asked comes back"
    );
    assert_eq!(
        app.action_notice.as_deref(),
        Some("this is the raw stream · o returns nowhere")
    );
    assert!(screen(&draw(&provider, &mut app, 100, 30)).contains("Fields"));
}

#[test]
fn correlate_hands_the_frozen_record_to_the_shell_and_freezes_the_dialog() {
    let (provider, mut app) = opened();
    draw(&provider, &mut app, 100, 30);
    let anchor = lvu::components::fields::anchor_id(&app.views)
        .cloned()
        .unwrap();
    key(&mut app, &provider, KeyCode::Char('r'));
    assert!(app.field_correlation_pending());
    let requests = app.take_correlation_requests();
    assert_eq!(requests.len(), 1);
    assert!(
        format!("{requests:?}").contains(&anchor.sequence.to_string()),
        "the request names the record Fields froze: {requests:?}"
    );

    // While it runs the dialog is read-only, and its rows stop being clickable.
    let pending = screen(&draw(&provider, &mut app, 100, 30));
    assert!(pending.contains("finding records that share this value"));
    assert!(app.layers.fields.row_rects().is_empty());
    let before = app.view_state().unwrap().pinned_columns.clone();
    key(&mut app, &provider, KeyCode::Char(' '));
    assert_eq!(app.view_state().unwrap().pinned_columns, before);

    // Escape abandons both the lookup and the layer.
    key(&mut app, &provider, KeyCode::Esc);
    assert!(!app.field_correlation_pending());
    assert!(!app.layers.fields.is_open());
    assert_eq!(app.focus, Focus::Logs);
}

#[test]
fn the_list_scrolls_only_far_enough_to_keep_the_selection_visible() {
    let (provider, mut app) = opened();
    draw(&provider, &mut app, 80, 12);
    assert_eq!(app.layers.fields.top(), 0);
    let count = lvu::components::fields::anchored_row(&app.views, &provider)
        .unwrap()
        .fields
        .len();
    // Wrapping to the last field pulls the window down; wrapping back resets it.
    key(&mut app, &provider, KeyCode::Up);
    draw(&provider, &mut app, 80, 12);
    assert_eq!(app.view_state().unwrap().field_picker_selected, count - 1);
    key(&mut app, &provider, KeyCode::Down);
    draw(&provider, &mut app, 80, 12);
    assert_eq!(app.view_state().unwrap().field_picker_selected, 0);
    assert_eq!(app.layers.fields.top(), 0);
}
