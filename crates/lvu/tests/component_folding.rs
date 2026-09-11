//! Acceptance for the Folding layer responsive migration.
//!
//! Folding is a SelfContainedForm with immediate semantics: every control
//! writes the view at once, `Collapse expanded runs` is the sole default, and
//! the key-column picker is a live anchored region with reserved rows. What is
//! asserted here is the responsive contract: policy-stable outer frames across
//! pattern/exact states and sizes, shared body projection/hitboxes/cursor,
//! anchored popup/paint/mouse sharing one authority (frame-bounded, may
//! overhang the dialog), focus order, Escape layering, and keyboard/mouse.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, RowProvider,
    component::{Component, LayerId, Open, RawEvent},
    components::folding::FoldingControl,
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

fn key(app: &mut App, provider: &impl RowProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))),
        provider,
    );
}

fn click(app: &mut App, provider: &impl RowProvider, point: (u16, u16)) {
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

fn open_folding(app: &mut App, provider: &impl RowProvider) {
    app.handle(Action::Open(Open::Folding), provider);
    assert_eq!(app.layers.stack, vec![LayerId::Folding]);
}

#[test]
fn immediate_semantics_and_default_are_preserved() {
    let (provider, mut app) = demo();
    open_folding(&mut app, &provider);
    draw(&provider, &mut app, 80, 24);
    // Opens on the key column, not the toggle.
    assert_eq!(
        app.layers.folding.control(),
        Some(FoldingControl::KeyColumn)
    );
    // Enabled toggles the view immediately: no Apply, no outbox.
    let before = app.view_state().unwrap().fold_enabled;
    // Move focus to Enabled (Up from KeyColumn wraps? Tab order: Enabled first).
    key(&mut app, &provider, KeyCode::Up);
    assert_eq!(app.layers.folding.control(), Some(FoldingControl::Enabled));
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(
        app.view_state().unwrap().fold_enabled,
        !before,
        "Enabled must write through immediately"
    );
    // Collapse is the sole default: Enter on it collapses (message) and stays open.
    // Move to Collapse via Tab cycles.
    for _ in 0..6 {
        key(&mut app, &provider, KeyCode::Tab);
        if app.layers.folding.control() == Some(FoldingControl::Collapse) {
            break;
        }
    }
    assert_eq!(app.layers.folding.control(), Some(FoldingControl::Collapse));
    let rendered = screen(&draw(&provider, &mut app, 80, 24));
    assert!(rendered.contains("Collapse expanded runs"), "{rendered}");
    key(&mut app, &provider, KeyCode::Enter);
    assert!(
        app.layers.folding.is_open(),
        "Collapse keeps the dialog open"
    );
}

#[test]
fn responsive_frame_is_policy_stable_across_pattern_exact_and_sizes() {
    use lvu::dialog_layout::{PresentationKind, policy_size};

    for (width, height) in [(240u16, 80u16), (140, 40), (80, 24), (54, 16), (20, 6)] {
        let mut frames = Vec::new();
        for pattern in [true, false] {
            let (provider, mut app) = demo();
            // Pattern key (None) shows Normalisation; exact key hides it. Frame
            // must not move with that row.
            if !pattern {
                app.views.active_mut().unwrap().fold_key_column = Some("service".into());
            }
            open_folding(&mut app, &provider);
            let buffer = draw(&provider, &mut app, width, height);
            let rendered = screen(&buffer);
            let surface = app.layers.folding.surface();
            let (want_w, want_h) = policy_size(
                ratatui::layout::Rect::new(0, 0, width, height),
                PresentationKind::SelfContainedForm,
            );
            assert_eq!(
                (surface.popup.width, surface.popup.height),
                (want_w, want_h),
                "{width}x{height} pattern={pattern} frame must be policy"
            );
            if (width, height) != (20, 6) {
                assert!(
                    surface.popup.width < width || surface.popup.height < height,
                    "{width}x{height} became full frame"
                );
            }
            assert!(rendered.contains("Folding"), "{rendered}");
            // At the 20x6 floor the 26-cell Collapse button clips to its band;
            // the default still survives as "Collapse".
            if (width, height) == (20, 6) {
                assert!(rendered.contains("Collapse"), "{rendered}");
            } else {
                assert!(rendered.contains("Collapse expanded runs"), "{rendered}");
            }
            if pattern {
                assert!(
                    rendered.contains("Normalisation") || width == 20 && height == 6,
                    "{rendered}"
                );
            }
            // Body projection and hitboxes share one authority.
            for (rect, control) in app.layers.folding.control_rects().to_vec() {
                assert_eq!(
                    app.layers.folding.hit((rect.x, rect.y)),
                    Some(lvu::components::folding::FoldingHit::Control(control)),
                    "{width}x{height} pattern={pattern}"
                );
            }
            // Scrollability is derived from actual overflow, not merely open:
            // roomy sizes fit the 4-5 rows with no dropdown, so no wheel; the
            // 20x6 floor overflows the body, so it wants the wheel.
            if (width, height) == (80, 24) {
                assert!(
                    !surface.scrollable,
                    "80x24 fits the form and must not want the wheel"
                );
            } else if (width, height) == (20, 6) {
                assert!(
                    surface.scrollable,
                    "20x6 overflows the body and must want the wheel"
                );
            }
            frames.push(surface.popup);
        }
        assert_eq!(
            frames[0], frames[1],
            "{width}x{height} frame must not move with Normalisation row"
        );
    }

    // Below the floor the tiny fallback owns the frame.
    let (provider, mut app) = demo();
    open_folding(&mut app, &provider);
    let tiny = screen(&draw(&provider, &mut app, 19, 5));
    assert!(tiny.contains("terminal too small"), "{tiny}");
}

#[test]
fn anchored_picker_is_frame_bounded_with_shared_paint_and_mouse() {
    let (provider, mut app) = demo();
    open_folding(&mut app, &provider);
    // Open the KeyColumn picker (focused at open).
    key(&mut app, &provider, KeyCode::Enter);
    let buffer = draw(&provider, &mut app, 80, 24);
    let rendered = screen(&buffer);
    // Picker lists sampled columns plus New; popup may overhang the dialog but
    // stays inside the full frame area.
    assert!(
        rendered.contains("service") || rendered.contains("level"),
        "{rendered}"
    );
    assert!(rendered.contains("New column"), "{rendered}");
    let surface = app.layers.folding.surface();
    assert!(
        surface.popup.width >= 12,
        "anchored popup keeps its minimum width"
    );
    // Same rects for paint and mouse: every drawn choice hit-tests to its index.
    let choices = app.layers.folding.choice_rects().to_vec();
    assert!(!choices.is_empty(), "picker must paint rows");
    for (rect, index) in &choices {
        assert_eq!(
            app.layers.folding.hit((rect.x, rect.y)),
            Some(lvu::components::folding::FoldingHit::Choice(*index)),
            "choice {index} hitbox must match paint"
        );
        // Popup rows stay inside the full 80x24 frame area.
        assert!(
            rect.right() <= 80 && rect.bottom() <= 24,
            "{rect:?} escapes frame"
        );
    }
    // Three choices in eight reserved rows never overflow: no wheel from the
    // picker itself, only ever from the body.
    assert!(
        !app.layers.folding.surface().scrollable,
        "a fitting picker must not want the wheel"
    );
    // Reserved rows are stable: reopening at the same size gives the same popup.
    let first_popup = surface.popup;
    key(&mut app, &provider, KeyCode::Esc);
    key(&mut app, &provider, KeyCode::Enter);
    draw(&provider, &mut app, 80, 24);
    assert_eq!(
        app.layers.folding.surface().popup,
        first_popup,
        "reserved picker rows must keep the popup stable"
    );
    // Escape closes the picker first, then the dialog. Choice hitboxes clear
    // on the next frame after the picker closes.
    key(&mut app, &provider, KeyCode::Esc);
    assert!(
        app.layers.folding.is_open(),
        "first Escape closes the picker, not the dialog"
    );
    draw(&provider, &mut app, 80, 24);
    assert!(app.layers.folding.choice_rects().is_empty());
    key(&mut app, &provider, KeyCode::Esc);
    assert!(!app.layers.folding.is_open());
}

#[test]
fn focus_order_escape_and_mouse_follow_the_dialog() {
    let (provider, mut app) = demo();
    open_folding(&mut app, &provider);
    draw(&provider, &mut app, 80, 24);
    // Tab order: Enabled → KeyColumn → MinimumRun → Scope → Collapse (pattern
    // adds Normalisation before Collapse).
    let mut seen = Vec::new();
    for _ in 0..6 {
        seen.push(app.layers.folding.control());
        key(&mut app, &provider, KeyCode::Tab);
    }
    assert!(seen.contains(&Some(FoldingControl::Enabled)));
    assert!(seen.contains(&Some(FoldingControl::KeyColumn)));
    assert!(seen.contains(&Some(FoldingControl::Collapse)));
    // Click a control focuses it; click a choice picks it.
    open_folding(&mut app, &provider);
    draw(&provider, &mut app, 80, 24);
    let scope_rect = app
        .layers
        .folding
        .control_rects()
        .iter()
        .find(|(_, control)| *control == FoldingControl::Scope)
        .map(|(rect, _)| (rect.x, rect.y))
        .expect("Scope control is drawn");
    click(&mut app, &provider, scope_rect);
    assert_eq!(app.layers.folding.control(), Some(FoldingControl::Scope));
    // Click already opened the dropdown (activate on click); wheel moves the
    // highlight while it is open.
    let before = app.layers.folding.highlighted();
    app.handle(
        Action::Raw(RawEvent::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: scope_rect.0,
            row: scope_rect.1,
            modifiers: KeyModifiers::NONE,
        })),
        &provider,
    );
    assert_ne!(
        app.layers.folding.highlighted(),
        before,
        "wheel must move the open picker"
    );
}

#[test]
fn anchored_picker_paints_themed_background_with_contained_glyphs() {
    // The anchored picker clears through the shared themed path, so every one
    // of its cells carries the dialog surface (or the selection fill on the
    // highlighted row), never the terminal default — in dark, light and
    // Terminal themes alike. Choice rects stay inside the drawn popup, which
    // stays inside the frame even where the popup overhangs the dialog.
    for theme in [Theme::LOVE_DARK, Theme::LOVE_LIGHT, Theme::TERMINAL] {
        let (provider, mut app) = demo();
        open_folding(&mut app, &provider);
        key(&mut app, &provider, KeyCode::Enter);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| ui::render_with_theme(frame, &mut app, &provider, theme, None))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let surface = app.layers.folding.surface();
        assert!(
            surface.popup.right() <= 80 && surface.popup.bottom() <= 24,
            "popup escapes the 80x24 frame: {:?}",
            surface.popup
        );
        let choices = app.layers.folding.choice_rects().to_vec();
        assert!(!choices.is_empty(), "picker must paint rows");
        for (rect, index) in &choices {
            assert!(
                rect.x >= surface.popup.x
                    && rect.right() <= surface.popup.right()
                    && rect.y >= surface.popup.y
                    && rect.bottom() <= surface.popup.bottom(),
                "choice {index} {rect:?} escapes popup {:?}",
                surface.popup
            );
            let bg = buffer[(rect.x, rect.y)].bg;
            assert!(
                bg == theme.dialog_bg || bg == theme.selection_bg,
                "choice {index} has unthemed popup background {bg:?}"
            );
        }
    }
}

/// A provider whose rows carry twelve columns, so the key-column picker holds
/// thirteen rows (plus `[ New column… ]`) against eight reserved viewport rows.
struct FatFieldProvider;

impl lvu::RowProvider for FatFieldProvider {
    fn page(&self, _view_id: &str, _request: lvu::ViewportRequest) -> lvu::RowPage {
        lvu::RowPage {
            total: 0,
            rows: Vec::new(),
        }
    }

    fn row_by_id(&self, _view_id: &str, _id: &lvu::RowId) -> Option<lvu::DisplayRow> {
        None
    }

    fn index_of_id(&self, _view_id: &str, _id: &lvu::RowId) -> Option<usize> {
        Some(0)
    }

    fn revision(&self, _view_id: &str) -> u64 {
        0
    }

    fn unfolded_page(&self, _view_id: &str, _request: lvu::ViewportRequest) -> lvu::RowPage {
        lvu::RowPage {
            total: 1,
            rows: vec![lvu::DisplayRow {
                id: lvu::RowId::new("api", 1),
                timestamp: "12:00:01.000Z".into(),
                captured_at_unix_nanos: None,
                level: "INFO".into(),
                text: "fat".into(),
                details: Vec::new(),
                fields: (0..12)
                    .map(|index| (format!("field{index:02}"), "v".into()))
                    .collect(),
            }],
        }
    }
}

#[test]
fn long_picker_paints_and_hits_first_and_last_without_stealing_a_row() {
    let (_, sources, views) = FixtureProvider::demo();
    let provider = FatFieldProvider;
    let mut app = App::new(sources, views, true);
    open_folding(&mut app, &provider);
    // Thirteen rows against eight reserved: the popup keeps its reserved
    // height and the shared scrollbar — not an ad-hoc `+N more` row —
    // communicates the overflow.
    key(&mut app, &provider, KeyCode::Enter);
    let buffer = draw(&provider, &mut app, 80, 24);
    let rendered = screen(&buffer);
    assert!(rendered.contains("field00"), "{rendered}");
    // ("3 or more" is the Minimum-run value, not the old affordance: thirteen
    // items minus seven visible rows used to steal the last row for this.)
    assert!(!rendered.contains("+6 more"), "{rendered}");
    // The whole reserved viewport paints choices: eight hitboxes, one per row.
    assert_eq!(app.layers.folding.choice_rects().len(), 8);
    for (rect, index) in app.layers.folding.choice_rects().to_vec() {
        assert_eq!(
            app.layers.folding.hit((rect.x, rect.y)),
            Some(lvu::components::folding::FoldingHit::Choice(index))
        );
    }
    // Bottom highlight: the last item is revealed, painted and hittable —
    // the old summary-row reservation used to hide exactly this row.
    for _ in 0..12 {
        key(&mut app, &provider, KeyCode::Down);
    }
    assert_eq!(app.layers.folding.highlighted(), 12);
    let buffer = draw(&provider, &mut app, 80, 24);
    let rendered = screen(&buffer);
    assert!(rendered.contains("New column"), "{rendered}");
    let last = app
        .layers
        .folding
        .choice_rects()
        .last()
        .copied()
        .expect("revealed window paints eight rows");
    assert_eq!(last.1, 12);
    assert_eq!(
        app.layers.folding.hit((last.0.x, last.0.y)),
        Some(lvu::components::folding::FoldingHit::Choice(12))
    );
    // Wheel scrolls the open picker…
    key(&mut app, &provider, KeyCode::Up);
    assert_eq!(app.layers.folding.highlighted(), 11);
    app.handle(
        Action::Raw(RawEvent::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 40,
            row: 12,
            modifiers: KeyModifiers::NONE,
        })),
        &provider,
    );
    assert_eq!(app.layers.folding.highlighted(), 12);
    // …and a click on a visible row picks exactly that column and closes the
    // picker.
    let middle = app.layers.folding.choice_rects()[2];
    let want = format!("field{:02}", middle.1);
    click(&mut app, &provider, (middle.0.x, middle.0.y));
    assert_eq!(app.view_state().unwrap().fold_key_column, Some(want));
    draw(&provider, &mut app, 80, 24);
    assert!(
        app.layers.folding.choice_rects().is_empty(),
        "choosing a column closes the picker"
    );
}

#[test]
fn wide_key_column_renders_without_breaking_hitboxes() {
    let (provider, mut app) = demo();
    app.views.active_mut().unwrap().fold_key_column = Some("東京".into());
    open_folding(&mut app, &provider);
    let buffer = draw(&provider, &mut app, 80, 24);
    let rendered = screen(&buffer);
    // Wide glyphs occupy two cells each; the TestBackend buffer stores a blank
    // continuation cell between them, so assert the halves, not contiguity.
    assert!(
        rendered.contains("東") && rendered.contains("京"),
        "{rendered}"
    );
    for (rect, control) in app.layers.folding.control_rects().to_vec() {
        assert_eq!(
            app.layers.folding.hit((rect.x, rect.y)),
            Some(lvu::components::folding::FoldingHit::Control(control))
        );
    }
}
