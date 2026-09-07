//! Acceptance for the shared dialog primitives: size classes, region layout,
//! the backdrop scrim and the input tone (docs/dialog-system.md §3, §5, §6).

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App,
    dialog_layout::{DialogClass, DialogContent, dialog_rect, is_compact, pane, regions, scrim},
    fixture::FixtureProvider,
    theme::{Theme, ThemeId, contrast},
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect, style::Color};

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

/// The four terminal sizes the spec is written against.
const SIZES: [(u16, u16); 4] = [(140, 40), (100, 30), (80, 24), (54, 16)];

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
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

/// `selection_modal` is the dialog interior; the border and title sit one cell
/// outside it and are drawn after the scrim, so backdrop checks use this.
fn dialog_popup(app: &App) -> Rect {
    let interior = app
        .hit_regions
        .selection_modal
        .expect("an open dialog publishes its surface");
    Rect::new(
        interior.x.saturating_sub(1),
        interior.y.saturating_sub(1),
        interior.width + 2,
        interior.height + 2,
    )
}

fn draw(
    provider: &FixtureProvider,
    app: &mut App,
    width: u16,
    height: u16,
    theme: Theme,
) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, theme, None))
        .expect("render");
    terminal.backend().buffer().clone()
}

#[test]
fn size_classes_reproduce_the_specified_width_and_height_table() {
    // dialog-system.md §5.3. Widths are exact; heights are maxima.
    /// Terminal width, terminal height, then (width, max height) per class.
    type Expectation = (u16, u16, [(u16, u16); 5]);
    let expected: [Expectation; 4] = [
        (
            140,
            40,
            [(72, 12), (96, 36), (120, 38), (138, 38), (90, 37)],
        ),
        (100, 30, [(60, 12), (72, 26), (86, 28), (98, 28), (64, 27)]),
        (80, 24, [(48, 12), (60, 20), (72, 22), (78, 22), (51, 21)]),
        (54, 16, [(52, 14), (52, 14), (52, 16), (52, 16), (52, 16)]),
    ];
    let classes = [
        DialogClass::S,
        DialogClass::M,
        DialogClass::L,
        DialogClass::XL,
        DialogClass::P,
    ];
    for (width, height, rows) in expected {
        let area = Rect::new(0, 0, width, height);
        for (class, (want_width, want_height)) in classes.into_iter().zip(rows) {
            assert_eq!(
                class.width(area),
                want_width,
                "{class:?} width at {width}x{height}"
            );
            assert_eq!(
                class.max_height(area),
                want_height,
                "{class:?} max height at {width}x{height}"
            );
        }
    }
    assert!(is_compact(Rect::new(0, 0, 54, 16)));
    assert!(!is_compact(Rect::new(0, 0, 80, 24)));
}

#[test]
fn dialog_height_follows_content_and_stops_at_the_class_maximum() {
    let area = Rect::new(0, 0, 100, 30);
    let small = DialogContent {
        body: 2,
        message: 1,
        actions: 1,
        ..DialogContent::default()
    };
    let rect = dialog_rect(area, DialogClass::M, &small);
    assert_eq!(
        rect.height,
        small.interior_rows() + 2,
        "a two-row body must not be padded out to a fixed height"
    );
    assert!(
        rect.height < DialogClass::M.max_height(area),
        "{rect:?} should be well short of the class maximum"
    );

    let huge = DialogContent {
        body: 400,
        message: 1,
        help: 2,
        actions: 1,
        ..DialogContent::default()
    };
    let rect = dialog_rect(area, DialogClass::L, &huge);
    assert_eq!(
        rect.height,
        DialogClass::L.max_height(area),
        "a long body stops at the class maximum and scrolls"
    );
    let laid_out = regions(rect, &huge);
    assert!(
        laid_out.body_overflow > 0,
        "the body must report the rows it could not show"
    );

    for (width, height) in SIZES {
        let area = Rect::new(0, 0, width, height);
        for class in [
            DialogClass::S,
            DialogClass::M,
            DialogClass::L,
            DialogClass::XL,
            DialogClass::P,
        ] {
            let rect = dialog_rect(area, class, &huge);
            assert!(
                rect.right() <= area.right() && rect.bottom() <= area.bottom(),
                "{class:?} {rect:?} escapes {width}x{height}"
            );
            assert!(rect.height <= class.max_height(area));
        }
    }
}

#[test]
fn regions_are_ordered_disjoint_and_inside_the_dialog() {
    let content = DialogContent {
        header: 1,
        body: 6,
        message: 1,
        help: 2,
        actions: 1,
    };
    for (width, height) in SIZES {
        let area = Rect::new(0, 0, width, height);
        let popup = dialog_rect(area, DialogClass::L, &content);
        let laid_out = regions(popup, &content);

        assert_eq!(
            laid_out.interior,
            popup.inner(ratatui::layout::Margin::new(1, 1))
        );
        assert!(
            laid_out.content.x > laid_out.interior.x
                && laid_out.content.right() < laid_out.interior.right(),
            "content keeps the 2-column side padding at {width}x{height}"
        );

        let ordered = [
            laid_out.header,
            laid_out.body,
            laid_out.message,
            laid_out.help,
            laid_out.actions,
        ];
        let mut previous_bottom = laid_out.interior.y;
        for rect in ordered {
            if rect.height == 0 {
                continue;
            }
            assert!(
                rect.y >= previous_bottom,
                "regions overlap or run backwards at {width}x{height}: {rect:?} after row {previous_bottom}"
            );
            assert!(
                rect.bottom() <= laid_out.interior.bottom(),
                "{rect:?} leaves the interior at {width}x{height}"
            );
            previous_bottom = rect.bottom();
        }
        assert!(
            laid_out.body.height >= 1,
            "the body always keeps at least one row at {width}x{height}"
        );
        assert!(
            laid_out.actions.height >= 1,
            "actions are sticky and never dropped at {width}x{height}"
        );
    }
}

#[test]
fn height_pressure_drops_padding_then_help_and_never_the_actions() {
    let content = DialogContent {
        header: 1,
        body: 8,
        message: 1,
        help: 2,
        actions: 1,
    };
    // Roomy: pads and gaps present, so the first content row is not flush.
    let roomy = regions(Rect::new(0, 0, 90, 22), &content);
    assert!(
        roomy.header.y > roomy.interior.y,
        "an interior of {} rows should afford a pad row",
        roomy.interior.height
    );
    assert_eq!(roomy.help.height, 2, "help survives when there is room");

    // Tight: no pads, no gaps.
    let tight = regions(Rect::new(0, 0, 90, 13), &content);
    assert_eq!(
        tight.header.y, tight.interior.y,
        "pads are the first thing dropped"
    );

    // Tighter still: help goes, actions and message stay.
    let cramped = regions(Rect::new(0, 0, 90, 9), &content);
    assert_eq!(cramped.help.height, 0, "help is dropped before the body");
    assert_eq!(cramped.message.height, 1, "the message row is sticky");
    assert_eq!(cramped.actions.height, 1, "the action row is sticky");
    assert!(cramped.body.height >= 1);
    assert!(cramped.body_overflow > 0, "the body scrolls instead");
}

#[test]
fn panes_show_a_scrollbar_only_when_their_content_overflows() {
    let area = Rect::new(0, 0, 40, 5);
    let fits = pane(area, 8, 3);
    assert!(fits.scrollbar.is_none(), "no affordance when content fits");
    assert_eq!(fits.viewport.width, 38, "no scrollbar column is reserved");
    assert_eq!(fits.viewport.x, area.x + 2, "pane content is indented");

    let overflows = pane(area, 8, 30);
    let scrollbar = overflows.scrollbar.expect("overflowing pane needs a bar");
    assert_eq!(scrollbar.x, area.right() - 1);
    assert_eq!(scrollbar.height, overflows.viewport.height);
    assert_eq!(
        overflows.viewport.right(),
        scrollbar.x,
        "the viewport must not run under the scrollbar"
    );
    assert_eq!(
        overflows.count.right(),
        area.right(),
        "the count is right-aligned in the heading"
    );
}

#[test]
fn input_tone_and_scrim_meet_the_specified_contrast_floors() {
    for id in [ThemeId::LoveDark, ThemeId::LoveLight] {
        let theme = id.theme();
        let input = contrast(theme.input_bg, theme.dialog_bg).expect("rgb theme");
        assert!(
            input >= 1.35,
            "{id:?}: input_bg must read as a distinct tone against dialog_bg, got {input:.2}:1"
        );
        let text = contrast(theme.input_fg, theme.input_bg).expect("rgb theme");
        assert!(
            text >= 7.0,
            "{id:?}: input_fg on input_bg must stay well above the text floor, got {text:.2}:1"
        );
        let backdrop = contrast(theme.muted, theme.base_bg).expect("rgb theme");
        assert!(
            backdrop >= 4.5,
            "{id:?}: the scrimmed backdrop must stay legible, got {backdrop:.2}:1"
        );
    }
}

#[test]
fn the_scrim_mutes_the_workspace_and_leaves_the_dialog_active() {
    for id in [ThemeId::LoveDark, ThemeId::LoveLight] {
        let theme = id.theme();
        for (width, height) in SIZES {
            let (provider, mut app) = demo();
            draw(&provider, &mut app, width, height, theme);
            app.handle(Action::OpenTime, &provider);
            let buffer = draw(&provider, &mut app, width, height, theme);
            let popup = dialog_popup(&app);

            let mut coloured_outside = 0usize;
            let mut modified_outside = 0usize;
            for y in 0..height {
                for x in 0..width {
                    if popup.contains(ratatui::layout::Position::new(x, y)) {
                        continue;
                    }
                    let style = buffer[(x, y)].style();
                    if style
                        .fg
                        .is_some_and(|fg| fg != theme.muted && fg != Color::Reset)
                    {
                        coloured_outside += 1;
                    }
                    if !style.add_modifier.is_empty() {
                        modified_outside += 1;
                    }
                }
            }
            assert_eq!(
                coloured_outside, 0,
                "{id:?} {width}x{height}: the backdrop must be entirely muted"
            );
            assert_eq!(
                modified_outside, 0,
                "{id:?} {width}x{height}: the backdrop must lose bold and reverse"
            );
        }
    }
}

#[test]
fn the_scrim_preserves_the_background_so_the_workspace_stays_readable() {
    let theme = ThemeId::LoveDark.theme();
    let (provider, mut app) = demo();
    let before = draw(&provider, &mut app, 100, 30, theme);
    app.handle(Action::OpenTime, &provider);
    let after = draw(&provider, &mut app, 100, 30, theme);
    let popup = dialog_popup(&app);

    let mut compared = 0usize;
    for y in 0..30u16 {
        for x in 0..100u16 {
            if popup.contains(ratatui::layout::Position::new(x, y)) {
                continue;
            }
            assert_eq!(
                before[(x, y)].symbol(),
                after[(x, y)].symbol(),
                "the scrim restyles cells, it never moves or erases them ({x},{y})"
            );
            assert_eq!(
                before[(x, y)].style().bg,
                after[(x, y)].style().bg,
                "backgrounds are untouched ({x},{y})"
            );
            compared += 1;
        }
    }
    assert!(compared > 500, "the comparison must cover real workspace");
}

#[test]
fn an_overflowing_input_renders_its_tail_once_behind_an_ellipsis() {
    // Regression for dialog-design.md §3: the field was painted at full width
    // and then repainted one column narrower, so the final column kept a
    // duplicated glyph from the first pass.
    let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 54, 16, Theme::TERMINAL);
    app.handle(Action::ToggleBookmark, &provider);
    app.handle(Action::OpenBookmarks, &provider);
    app.handle(Action::EditBookmarkNote, &provider);
    for character in alphabet.chars() {
        app.handle(Action::BookmarkInput(character), &provider);
    }
    let buffer = draw(&provider, &mut app, 54, 16, Theme::TERMINAL);
    let rendered = screen(&buffer);
    let field = rendered
        .lines()
        .find(|line| line.contains("0123456789"))
        .unwrap_or_else(|| panic!("the note field must show the tail:\n{rendered}"));

    let value = field.trim_matches('│').trim_end();
    assert!(
        value.ends_with('9'),
        "the field must end at the last typed character, not a stale glyph: {field:?}"
    );
    assert!(
        field.contains('…'),
        "an overflowing value marks its hidden head: {field:?}"
    );
    assert_eq!(
        field.matches("789").count(),
        1,
        "the visible window is painted exactly once: {field:?}"
    );
}

#[test]
fn a_dialog_in_a_compact_terminal_reclaims_the_sidebar_columns() {
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 54, 16, Theme::TERMINAL);
    let workspace_log = app.hit_regions.log.expect("log region");
    assert!(
        app.hit_regions.sidebar.is_some(),
        "the sidebar is present without a dialog"
    );

    app.handle(Action::OpenTime, &provider);
    draw(&provider, &mut app, 54, 16, Theme::TERMINAL);
    assert!(
        app.hit_regions.sidebar.is_none(),
        "§5.5: a compact terminal drops the sidebar behind a dialog"
    );
    let dialog_log = app.hit_regions.log.expect("log region");
    assert!(
        dialog_log.width > workspace_log.width,
        "the log reclaims the sidebar columns: {dialog_log:?} vs {workspace_log:?}"
    );

    // 80x24 is not compact, so the backdrop is unchanged.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 80, 24, Theme::TERMINAL);
    app.handle(Action::OpenTime, &provider);
    draw(&provider, &mut app, 80, 24, Theme::TERMINAL);
    assert!(
        app.hit_regions.sidebar.is_some(),
        "a roomy terminal keeps its sidebar behind a dialog"
    );
}

#[test]
fn scrim_and_layout_survive_wide_and_combining_characters() {
    let theme = ThemeId::LoveDark.theme();
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 80, 24, theme);
    app.handle(Action::OpenAskAi, &provider);
    app.handle(
        Action::EditorPaste("東京 café é 界 warnings".into()),
        &provider,
    );
    let buffer = draw(&provider, &mut app, 80, 24, theme);
    let popup = app.hit_regions.selection_modal.expect("ask surface");
    assert!(popup.right() <= 80 && popup.bottom() <= 24);
    // A wide glyph owns two cells and the second is a blank spacer, so compare
    // with the spacers removed rather than asserting on raw cell text.
    let dense = screen(&buffer).replace(' ', "");
    for fragment in ["東", "京", "café", "界"] {
        assert!(
            dense.contains(fragment),
            "{fragment} must survive the dialog's column arithmetic:\n{}",
            screen(&buffer)
        );
    }

    // A second scrim pass over an already-scrimmed buffer is idempotent.
    let mut once = buffer.clone();
    scrim(&mut once, Rect::new(0, 0, 80, 24), theme);
    let mut twice = once.clone();
    scrim(&mut twice, Rect::new(0, 0, 80, 24), theme);
    assert_eq!(once, twice, "the scrim must be idempotent");
}

#[test]
fn the_grouping_apply_button_is_clickable_where_it_is_drawn() {
    // dialog-system.md §3: the grouping dialog had no actions region at all, so
    // there was nothing to click. The hitbox must be the drawn rect.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 100, 30, Theme::TERMINAL);
    app.handle(Action::OpenGrouping, &provider);
    let buffer = draw(&provider, &mut app, 100, 30, Theme::TERMINAL);
    let rendered = screen(&buffer);

    let button = *app
        .hit_regions
        .editor_actions
        .first()
        .expect("the action row publishes a hitbox");
    let row: String = (button.x..button.right())
        .map(|x| buffer[(x, button.y)].symbol())
        .collect();
    assert_eq!(
        row, "[ Apply ]",
        "the hitbox covers exactly the drawn button"
    );
    assert!(rendered.contains("[ Apply ]"), "{rendered}");

    let applied = app
        .active_editor_state()
        .expect("grouping editor")
        .applied
        .clone();
    let draft = app
        .active_editor_state()
        .expect("grouping editor")
        .draft
        .clone();
    assert_ne!(
        draft, applied,
        "the fixture starts with an uncommitted draft"
    );

    app.handle(
        Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            button.x + 2,
            button.y,
        )),
        &provider,
    );
    assert!(
        !app.take_query_requests().is_empty(),
        "clicking the drawn Apply button must submit the draft"
    );
}
