//! Acceptance for the Bookmarks layer as a component (docs/component-model.md
//! §6.3 step 5).
//!
//! The store is `Views`': bookmarks belong to the source, are persisted, and
//! the log and `b` reach them without the dialog. What the layer owns is the
//! selection, the focused control, the note draft with its caret, and its
//! geometry. The two things it hands back are the record jump and Raw context.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, Focus,
    app::BookmarkDialogControl,
    component::{Component, LayerId, Open, RawEvent},
    components::bookmarks::BookmarksHit,
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

/// Two bookmarked records, then the dialog.
fn opened() -> (FixtureProvider, App) {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 10);
    app.handle(Action::Top, &provider);
    app.handle(Action::ToggleBookmark, &provider);
    app.handle(Action::MoveLine(1), &provider);
    app.handle(Action::ToggleBookmark, &provider);
    assert_eq!(
        app.bookmarks_for_view(app.active_view_id().unwrap()).len(),
        2,
        "two records are bookmarked before the dialog opens"
    );
    app.handle(Action::Open(Open::Bookmarks), &provider);
    (provider, app)
}

#[test]
fn every_recorded_rect_was_painted_and_answers_the_hit_test() {
    for (width, height) in SIZES {
        let (provider, mut app) = opened();
        let rendered = screen(&draw(&provider, &mut app, width, height));
        assert!(rendered.contains("Bookmarks"), "at {width}x{height}");

        let surface = app.layers.bookmarks.surface();
        assert_eq!(
            app.hit_regions.selection_modal,
            Some(surface.interior),
            "at {width}x{height}"
        );
        let rows: Vec<(Rect, usize)> = app.layers.bookmarks.row_rects().to_vec();
        let controls: Vec<(Rect, BookmarkDialogControl)> =
            app.layers.bookmarks.control_rects().to_vec();
        assert!(!rows.is_empty(), "at {width}x{height}");
        assert!(!controls.is_empty(), "at {width}x{height}");
        for (rect, index) in rows {
            assert!(
                surface.interior.union(rect) == surface.interior,
                "row {index} at {rect:?} escapes the surface at {width}x{height}"
            );
            assert_eq!(
                app.layers.bookmarks.hit((rect.x, rect.y)),
                Some(BookmarksHit::Row(index)),
                "at {width}x{height}"
            );
        }
        for (rect, control) in controls {
            assert!(
                surface.interior.union(rect) == surface.interior,
                "{control:?} at {rect:?} escapes the surface at {width}x{height}"
            );
            assert_eq!(
                app.layers.bookmarks.hit((rect.x, rect.y)),
                Some(BookmarksHit::Control(control)),
                "at {width}x{height}"
            );
        }
    }
}

#[test]
fn the_store_is_the_views_and_the_layer_only_edits_through_it() {
    let (provider, mut app) = opened();
    let view = app.active_view_id().unwrap().to_owned();
    draw(&provider, &mut app, 100, 30);
    assert_eq!(app.bookmarks_for_view(&view).len(), 2);

    // Editing a note goes through `Views`, and marks the view worth saving.
    let fence = app.view_interaction_revision(&view).unwrap();
    alt(&mut app, &provider, KeyCode::Char('e'));
    key(&mut app, &provider, KeyCode::Char('h'));
    key(&mut app, &provider, KeyCode::Char('i'));
    assert_eq!(app.layers.bookmarks.state().draft, "hi");
    // Nothing is committed until Enter: the store still has the old note.
    assert_eq!(app.bookmarks_for_view(&view)[0].note, "");
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.bookmarks_for_view(&view)[0].note, "hi");
    assert!(app.view_interaction_revision(&view).unwrap() > fence);
    assert!(app.take_query_requests().is_empty(), "a note runs no query");
}

#[test]
fn escape_abandons_the_note_before_the_dialog() {
    let (provider, mut app) = opened();
    let view = app.active_view_id().unwrap().to_owned();
    draw(&provider, &mut app, 100, 30);
    alt(&mut app, &provider, KeyCode::Char('e'));
    key(&mut app, &provider, KeyCode::Char('x'));
    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.layers.bookmarks.state().editing.is_none());
    assert!(
        app.layers.bookmarks.is_open(),
        "§5.3: innermost thing first"
    );
    assert_eq!(
        app.bookmarks_for_view(&view)[0].note,
        "",
        "an abandoned note is not written"
    );
    key(&mut app, &provider, KeyCode::Esc);
    assert!(!app.layers.bookmarks.is_open());
    assert_eq!(app.focus, Focus::Logs);
}

#[test]
fn q_types_into_an_open_note_and_dismisses_from_the_list() {
    let (provider, mut app) = opened();
    draw(&provider, &mut app, 100, 30);
    key(&mut app, &provider, KeyCode::Char('q'));
    assert!(!app.layers.bookmarks.is_open(), "q dismisses the list");

    let (provider, mut app) = opened();
    draw(&provider, &mut app, 100, 30);
    // Clicking `[ Edit note ]` opens the input with no redraw in between, so
    // the dismissal rule has to read live state, not the last `Surface`.
    let edit = app
        .layers
        .bookmarks
        .control_rects()
        .iter()
        .find_map(|(rect, control)| (*control == BookmarkDialogControl::Edit).then_some(*rect))
        .expect("the Edit note button is drawn");
    click(&mut app, &provider, (edit.x, edit.y));
    assert!(app.layers.bookmarks.state().editing.is_some());
    key(&mut app, &provider, KeyCode::Char('q'));
    assert!(app.layers.bookmarks.is_open(), "q is a character in a note");
    assert_eq!(app.layers.bookmarks.state().draft, "q");
}

#[test]
fn removing_a_bookmark_keeps_the_selection_inside_the_list() {
    let (provider, mut app) = opened();
    let view = app.active_view_id().unwrap().to_owned();
    draw(&provider, &mut app, 100, 30);
    key(&mut app, &provider, KeyCode::Down);
    assert_eq!(app.layers.bookmarks.state().selected, 1);
    alt(&mut app, &provider, KeyCode::Char('d'));
    assert_eq!(app.bookmarks_for_view(&view).len(), 1);
    assert_eq!(app.layers.bookmarks.state().selected, 0);
    alt(&mut app, &provider, KeyCode::Char('d'));
    assert!(app.bookmarks_for_view(&view).is_empty());
    // An empty list offers no actions and answers no row hit.
    draw(&provider, &mut app, 100, 30);
    assert!(app.layers.bookmarks.row_rects().is_empty());
    assert!(app.layers.bookmarks.control_rects().is_empty());
}

#[test]
fn go_to_hands_the_record_to_the_shell_and_closes_the_layer() {
    let (provider, mut app) = opened();
    let view = app.active_view_id().unwrap().to_owned();
    draw(&provider, &mut app, 100, 30);
    let id = app.bookmarks_for_view(&view)[0].id.clone();
    // Enter on the list is `Go to`.
    key(&mut app, &provider, KeyCode::Enter);
    assert!(!app.layers.bookmarks.is_open());
    assert_eq!(app.focus, Focus::Logs);
    assert_eq!(app.view_state().unwrap().selected.as_ref(), Some(&id));
    assert!(!app.view_state().unwrap().follow, "a jump stops following");
}

#[test]
fn raw_context_closes_the_layer_and_jumps_and_the_raw_stream_gives_it_back() {
    // raw-context-as-jump.md: the button is a jump. The demo's active view is
    // its source's only view, so there is no raw stream to jump to; the shell
    // says so and re-pushes the dialog that asked.
    let (provider, mut app) = opened();
    let view = app.active_view_id().unwrap().to_owned();
    app.set_view_role(&view, lvu::ViewRole::Canonical);
    draw(&provider, &mut app, 100, 30);
    let context = app
        .layers
        .bookmarks
        .control_rects()
        .iter()
        .find_map(|(rect, control)| (*control == BookmarkDialogControl::Context).then_some(*rect))
        .expect("the Raw context button is drawn");
    click(&mut app, &provider, (context.x, context.y));
    assert_eq!(app.focus, Focus::Layer);
    assert!(
        app.layers.bookmarks.is_open(),
        "the dialog that asked comes back"
    );
    assert_eq!(
        app.action_notice.as_deref(),
        Some("this is the raw stream · o returns nowhere")
    );
    assert!(screen(&draw(&provider, &mut app, 100, 30)).contains("Bookmarks"));
}

#[test]
fn a_click_outside_the_popup_is_contained_by_the_shell() {
    let (provider, mut app) = opened();
    draw(&provider, &mut app, 100, 30);
    let before = app.layers.bookmarks.state().selected;
    assert!(app.layers.bookmarks.hit((0, 0)).is_none());
    click(&mut app, &provider, (0, 0));
    assert_eq!(app.layers.bookmarks.state().selected, before);
    assert_eq!(app.focus, Focus::Layer);
}

#[test]
fn the_palette_entry_is_the_components_and_reaches_it_as_a_command() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 10);
    app.handle(Action::Top, &provider);
    app.handle(Action::ToggleBookmark, &provider);
    let entries = |app: &App| {
        app.layer_commands()
            .into_iter()
            .filter(|(layer, _)| *layer == LayerId::Bookmarks)
            .collect::<Vec<_>>()
    };
    let closed = entries(&app);
    assert_eq!(closed.len(), 1);
    assert_eq!(
        closed[0].1.unavailable_reason,
        Some("open Bookmarks first"),
        "listed but muted from the base focus, as before"
    );

    app.handle(Action::Open(Open::Bookmarks), &provider);
    draw(&provider, &mut app, 100, 30);
    let open = entries(&app);
    assert!(open[0].1.unavailable_reason.is_none());
    // §8.10: the palette prints the letter `&Edit note` underlines. Bookmarks
    // takes text only while the Note child is up, so `e` is live in the list.
    assert_eq!(open[0].1.spec.shortcut, Some("e"));
    app.handle(Action::Command(open[0].0, open[0].1.spec.id), &provider);
    assert!(app.layers.bookmarks.state().editing.is_some());
}

#[test]
fn a_note_longer_than_the_cap_is_refused_and_says_so() {
    let (provider, mut app) = opened();
    draw(&provider, &mut app, 100, 30);
    alt(&mut app, &provider, KeyCode::Char('e'));
    app.handle(Action::Raw(RawEvent::Paste("x".repeat(2000))), &provider);
    assert!(app.layers.bookmarks.state().draft.is_empty());
    assert!(
        app.layers
            .bookmarks
            .state()
            .status
            .contains("at most 1024 bytes")
    );
    // A control character is refused for the same reason.
    app.handle(Action::Raw(RawEvent::Paste("a\nb".into())), &provider);
    assert!(app.layers.bookmarks.state().draft.is_empty());
}

#[test]
fn responsive_frame_is_policy_stable_for_longcontent() {
    use lvu::dialog_layout::{PresentationKind, policy_size};

    // 80x24 and 54x16 share one LongContent frame per size; 2 bookmarks and
    // 12 bookmarks share it. The list scrolls behind its count via the shared
    // plan; paint/mouse/scrollbar share its rects.
    for (width, height) in [(80u16, 24u16), (54, 16)] {
        for extra in [0usize, 10] {
            let (provider, mut app) = demo();
            app.sync_provider(&provider, 30);
            app.handle(Action::Top, &provider);
            // 2 bookmarks short, 12 long; bookmark before opening.
            for _ in 0..(2 + extra) {
                app.handle(Action::ToggleBookmark, &provider);
                app.handle(Action::MoveLine(1), &provider);
            }
            app.handle(Action::Open(Open::Bookmarks), &provider);
            let buffer = draw(&provider, &mut app, width, height);
            let rendered = screen(&buffer);
            assert!(rendered.contains("Bookmarks"), "{rendered}");
            assert!(rendered.contains("Open in All events"), "{rendered}");
            let surface = app.layers.bookmarks.surface();
            let (want_w, want_h) = policy_size(
                ratatui::layout::Rect::new(0, 0, width, height),
                PresentationKind::LongContent,
            );
            assert_eq!(
                (surface.popup.width, surface.popup.height),
                (want_w, want_h),
                "{width}x{height} extra={extra} frame must be policy"
            );
        }
    }

    // Below the floor the tiny fallback owns the frame.
    let (provider, mut app) = opened();
    let tiny = screen(&draw(&provider, &mut app, 19, 5));
    assert!(tiny.contains("terminal too small"), "{tiny}");
}

#[test]
fn list_scroll_reveals_with_matching_mouse() {
    // Twelve bookmarks (24 logical rows) overflow the LongContent body at
    // 80x24; Down scrolls the shared viewport and a click selects.
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 30);
    app.handle(Action::Top, &provider);
    for _ in 0..12 {
        app.handle(Action::ToggleBookmark, &provider);
        app.handle(Action::MoveLine(1), &provider);
    }
    app.handle(Action::Open(Open::Bookmarks), &provider);
    let buffer = draw(&provider, &mut app, 80, 24);
    assert!(screen(&buffer).contains("Bookmarks"));
    for _ in 0..11 {
        key(&mut app, &provider, KeyCode::Down);
    }
    let scrolled = draw(&provider, &mut app, 80, 24);
    let scrolled_text = screen(&scrolled);
    assert!(scrolled_text.contains("Bookmarks"), "{scrolled_text}");
    // Windowed: the shared plan shows a subset with the selection in it.
    let rows = app.layers.bookmarks.row_rects().to_vec();
    assert!(!rows.is_empty());
    let selected = app.layers.bookmarks.state().selected;
    assert!(rows.iter().any(|(_, index)| *index == selected));
    // Clicking a visible row selects it (same rects for paint and mouse).
    let (rect, index) = rows[0];
    click(&mut app, &provider, (rect.x, rect.y));
    assert_eq!(app.layers.bookmarks.state().selected, index);

    // Same at compact 54x16: frame is policy, list still scrolls.
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 30);
    app.handle(Action::Top, &provider);
    for _ in 0..12 {
        app.handle(Action::ToggleBookmark, &provider);
        app.handle(Action::MoveLine(1), &provider);
    }
    app.handle(Action::Open(Open::Bookmarks), &provider);
    assert!(screen(&draw(&provider, &mut app, 54, 16)).contains("Bookmarks"));
    for _ in 0..11 {
        key(&mut app, &provider, KeyCode::Down);
    }
    draw(&provider, &mut app, 54, 16);
    let rows = app.layers.bookmarks.row_rects().to_vec();
    assert!(!rows.is_empty());
}

#[test]
fn note_child_shows_parent_behind_and_escapes_back() {
    // Noncompact: the parent list stays visible behind the child with a
    // breadcrumb title; Escape returns child→Bookmarks→base with save
    // semantics unchanged.
    let (provider, mut app) = opened();
    let view = app.active_view_id().unwrap().to_owned();
    draw(&provider, &mut app, 100, 30);
    alt(&mut app, &provider, KeyCode::Char('e'));
    let editing = screen(&draw(&provider, &mut app, 100, 30));
    assert!(editing.contains("Bookmarks › Note for"), "{editing}");
    assert!(editing.contains("Save note"), "{editing}");
    // Parent list behind the child: the second bookmark's row survives.
    let second = app.bookmarks_for_view(&view)[1].id.sequence;
    assert!(
        editing.contains(&format!("#{second}")),
        "parent must stay visible behind noncompact child: {editing}"
    );
    key(&mut app, &provider, KeyCode::Char('h'));
    key(&mut app, &provider, KeyCode::Char('i'));
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.bookmarks_for_view(&view)[0].note, "hi");
    // Saved child returns to Bookmarks; Escape returns to base.
    assert!(app.layers.bookmarks.is_open());
    assert!(app.layers.bookmarks.state().editing.is_none());
    key(&mut app, &provider, KeyCode::Esc);
    assert!(!app.layers.bookmarks.is_open());
    assert_eq!(app.focus, Focus::Logs);

    // Compact: the child reuses the parent frame with a breadcrumb; the parent
    // list is not drawn behind it.
    let (provider, mut app) = opened();
    draw(&provider, &mut app, 54, 16);
    alt(&mut app, &provider, KeyCode::Char('e'));
    let compact = screen(&draw(&provider, &mut app, 54, 16));
    assert!(compact.contains("Bookmarks › Note for"), "{compact}");
    assert!(compact.contains("Save note"), "{compact}");
    assert!(
        !compact.contains(&format!("#{second}")),
        "compact child uses the parent frame, parent list hidden: {compact}"
    );
    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.layers.bookmarks.is_open());
    assert!(app.layers.bookmarks.state().editing.is_none());
    key(&mut app, &provider, KeyCode::Esc);
    assert!(!app.layers.bookmarks.is_open());
}
