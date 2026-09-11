//! Acceptance for the shared dialog primitives: size classes, region layout,
//! the backdrop scrim and the input tone (docs/dialog-system.md §3, §5, §6).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::app::AskTask;
use lvu::components::ask::AskOpen;
use lvu::{
    Action, App, RowProvider, SettingsContext, SettingsValues,
    app::RecipeDialogMode,
    component::{Component, Open, RawEvent},
    components::time::TimeControl,
    dialog_layout::{
        DialogClass, DialogContent, dialog_rect, fitted_rows, is_compact, pane, regions, scrim,
    },
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

fn settings_context() -> SettingsContext {
    SettingsContext {
        saved: SettingsValues {
            provider: "fixture/provider".into(),
            mode: "full-access".into(),
            thinking: "medium".into(),
            theme: ThemeId::LoveDark,
            display_zone: "Z".into(),
            delight_enabled: true,
            reduced_motion: false,
            ascii: false,
            rows_mib: "4".into(),
            membership_mib: "256".into(),
            disk_total_mib: "5120".into(),
            index_per_source_mib: "256".into(),
        },
        effective_provider: "fixture/provider".into(),
        effective_mode: "full-access".into(),
        effective_thinking: "medium".into(),
        effective_theme: ThemeId::LoveDark,
        effective_display_zone: "Z".into(),
        display_zone_source: "default",
        effective_delight_enabled: true,
        effective_reduced_motion: false,
        effective_ascii: false,
        provider_source: "settings.toml".into(),
        mode_source: "settings.toml".into(),
        thinking_source: "settings.toml".into(),
        delight_source: "settings.toml".into(),
        reduced_motion_source: "settings.toml".into(),
        ascii_source: "settings.toml".into(),
        settings_path: "/config/lvu/settings.toml".into(),
        data_path: "/data/lvu".into(),
        cache_path: "/cache/lvu".into(),
        capture_path: "/data/lvu/captures".into(),
        applied_rows_mib: 4,
        applied_membership_mib: 256,
        applied_disk_total_mib: 5120,
        applied_index_per_source_mib: 256,
    }
}

/// The four terminal sizes the spec is written against.
const SIZES: [(u16, u16); 4] = [(140, 40), (100, 30), (80, 24), (54, 16)];

fn raw_alt(code: KeyCode) -> Action {
    Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::ALT)))
}

fn raw_char(character: char) -> Action {
    raw_key(KeyCode::Char(character))
}

fn raw_key(code: KeyCode) -> Action {
    Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

/// The Time layer owns its keymap now, so a test reaches a control the way a
/// user does: Tab until it has focus. This is what `Action::TimeFocus` did.
fn time_focus<P: RowProvider>(app: &mut App, provider: &P, control: TimeControl) {
    if !app.layers.time.is_open() {
        return;
    }
    for _ in 0..64 {
        if app.layers.time.state().focus == control {
            return;
        }
        app.handle(raw_key(KeyCode::Tab), provider);
    }
    panic!("{control:?} never took focus");
}

fn time_activate<P: RowProvider>(app: &mut App, provider: &P, control: TimeControl) {
    time_focus(app, provider, control);
    app.handle(raw_key(KeyCode::Enter), provider);
}

/// Open the dropdown on `control`, pick row `index`, commit it.
fn time_choose<P: RowProvider>(app: &mut App, provider: &P, control: TimeControl, index: usize) {
    time_activate(app, provider, control);
    app.layers.time.highlight(index);
    app.handle(raw_key(KeyCode::Enter), provider);
}

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
    type Expectation = (u16, u16, [(u16, u16); 4]);
    let expected: [Expectation; 4] = [
        (140, 40, [(72, 12), (96, 36), (120, 38), (90, 37)]),
        (100, 30, [(60, 12), (72, 26), (86, 28), (64, 27)]),
        (80, 24, [(48, 12), (60, 20), (72, 22), (51, 21)]),
        (54, 16, [(52, 14), (52, 14), (52, 16), (52, 16)]),
    ];
    let classes = [
        DialogClass::S,
        DialogClass::M,
        DialogClass::L,
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
            app.handle(Action::Open(Open::Time), &provider);
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
    app.handle(Action::Open(Open::Time), &provider);
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
    app.handle(Action::Open(Open::Bookmarks), &provider);
    app.handle(raw_alt(KeyCode::Char('e')), &provider);
    for character in alphabet.chars() {
        app.handle(raw_char(character), &provider);
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

    app.handle(Action::Open(Open::Time), &provider);
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
    app.handle(Action::Open(Open::Time), &provider);
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
    app.handle(Action::Open(Open::Ask(AskOpen::Generic)), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("東京 café é 界 warnings".into())),
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
    app.handle(Action::Open(Open::Grouping), &provider);
    let buffer = draw(&provider, &mut app, 100, 30, Theme::TERMINAL);
    let rendered = screen(&buffer);

    let button = *app
        .layers
        .grouping
        .action_rects()
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

    let grouping = &app.view_state().expect("grouping editor").grouping;
    let applied = grouping.applied.clone();
    let draft = grouping.draft.clone();
    assert_ne!(
        draft, applied,
        "the fixture starts with an uncommitted draft"
    );

    app.handle(
        Action::Raw(RawEvent::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            button.x + 2,
            button.y,
        ))),
        &provider,
    );
    assert!(
        !app.take_query_requests().is_empty(),
        "clicking the drawn Apply button must submit the draft"
    );
}

/// Every dialog surface that has been adopted onto the anatomy, so the size
/// classes can be asserted uniformly as more of them land.
fn adopted_dialogs() -> Vec<(&'static str, Action, DialogClass)> {
    vec![
        ("search", Action::Open(Open::Search), DialogClass::S),
        ("grouping", Action::Open(Open::Grouping), DialogClass::S),
        ("view", Action::Open(Open::View), DialogClass::M),
        ("settings", Action::Open(Open::Settings), DialogClass::L),
        ("source", Action::Open(Open::Source), DialogClass::L),
        ("storage", Action::Open(Open::Storage), DialogClass::L),
        ("time", Action::Open(Open::Time), DialogClass::M),
        (
            "ask",
            Action::Open(Open::Ask(AskOpen::Generic)),
            DialogClass::L,
        ),
        (
            "ask timestamp task",
            Action::Open(Open::Ask(AskOpen::Task(AskTask::TimestampColumn))),
            DialogClass::L,
        ),
    ]
}

/// §12.17 Ask: one anatomy, at every size and in both themes. The reported
/// defect was the action row sitting above the fields it acts on, inside three
/// nested boxes, with an input slab painted over rows the prompt never reached.
#[test]
fn ask_puts_its_actions_after_its_fields_at_every_size_in_both_themes() {
    for id in [ThemeId::LoveDark, ThemeId::LoveLight] {
        let theme = id.theme();
        for (width, height) in SIZES {
            for (name, action, prepared) in [
                ("generic", Action::Open(Open::Ask(AskOpen::Generic)), false),
                (
                    "timestamp task",
                    Action::Open(Open::Ask(AskOpen::Task(AskTask::TimestampColumn))),
                    true,
                ),
            ] {
                let (provider, mut app) = demo();
                app.configure_settings(settings_context());
                app.handle(action, &provider);
                let buffer = draw(&provider, &mut app, width, height, theme);
                let text = screen(&buffer);
                let at = |needle: &str| {
                    text.lines()
                        .position(|line| line.contains(needle))
                        .unwrap_or_else(|| panic!("{name} {width}x{height}: no {needle:?}\n{text}"))
                };
                let request = at("Request");
                let submit = at("[ Submit ]");
                assert!(
                    request < submit,
                    "{name} at {width}x{height}: actions must follow the fields\n{text}"
                );
                assert!(
                    at("Ready") < submit,
                    "{name} at {width}x{height}: the message row precedes the actions\n{text}"
                );
                // §11.1: one border, not three nested boxes.
                for retired in [
                    "┌ Request",
                    "┌ State",
                    "┌ Proposal and activity",
                    "[ Kind:",
                    "[ More ]",
                ] {
                    assert!(
                        !text.contains(retired),
                        "{name} at {width}x{height}: retired {retired:?} is still drawn\n{text}"
                    );
                }
                // §7.5/§11.10: no key-reminder footers anywhere in the dialog.
                for banned in ["Tab ", "Esc ", "PgUp", "PgDn", "↑/↓ scroll"] {
                    assert!(
                        !text.contains(banned),
                        "{name} at {width}x{height}: footer vocabulary {banned:?}\n{text}"
                    );
                }
                // A prepared task states its task and offers no kind choice.
                if prepared {
                    assert!(text.contains("Timestamp column"), "{text}");
                    assert!(text.contains("timestamp_utc"), "{text}");
                    assert!(!text.contains("Kind"), "{name} keeps a kind row\n{text}");
                } else {
                    assert!(text.contains("Kind"), "{name} lost its kind row\n{text}");
                }
            }
        }
    }
}

/// §8.1/§11.8: the input background is painted over the field rect and nothing
/// else — not four rows and 88 cells for a one-line prompt.
#[test]
fn the_ask_request_field_paints_only_the_rows_its_draft_needs() {
    let theme = ThemeId::LoveDark.theme();
    let (provider, mut app) = demo();
    app.configure_settings(settings_context());
    app.handle(Action::Open(Open::Ask(AskOpen::Generic)), &provider);
    let empty = draw(&provider, &mut app, 140, 40, theme);
    let painted = |buffer: &Buffer| {
        (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .filter(|point| buffer[*point].bg == theme.input_bg)
            .count()
    };
    let one_line = painted(&empty);
    // One row of the field, and the Kind field beside it; nothing more.
    assert!(one_line > 0, "an empty field is still a field");

    app.handle(
        Action::Raw(RawEvent::Paste("wrapping request ".repeat(24))),
        &provider,
    );
    let wrapped = draw(&provider, &mut app, 140, 40, theme);
    let three_lines = painted(&wrapped);
    assert!(
        three_lines > one_line,
        "a wrapped draft uses more rows: {one_line} then {three_lines}"
    );
    // §8.1 caps the field at three visible rows however long the draft is.
    app.handle(
        Action::Raw(RawEvent::Paste("and more text ".repeat(60))),
        &provider,
    );
    let longer = draw(&provider, &mut app, 140, 40, theme);
    assert_eq!(
        painted(&longer),
        three_lines,
        "the Request field is capped at three rows and scrolls internally"
    );
}

#[test]
fn adopted_dialogs_use_their_class_width_and_stay_on_screen_in_both_themes() {
    for id in [ThemeId::LoveDark, ThemeId::LoveLight] {
        let theme = id.theme();
        for (width, height) in SIZES {
            let area = Rect::new(0, 0, width, height);
            for (name, action, class) in adopted_dialogs() {
                let (provider, mut app) = demo();
                app.configure_settings(settings_context());
                draw(&provider, &mut app, width, height, theme);
                app.handle(action, &provider);
                draw(&provider, &mut app, width, height, theme);
                let popup = dialog_popup(&app);
                assert_eq!(
                    popup.width,
                    class.width(area),
                    "{name} at {width}x{height} must use its §5.3 class width"
                );
                assert!(
                    popup.height <= class.max_height(area),
                    "{name} at {width}x{height}: {popup:?} exceeds the class maximum"
                );
                assert!(
                    popup.right() <= width && popup.bottom() <= height,
                    "{name} at {width}x{height}: {popup:?} leaves the terminal"
                );
            }
        }
    }
}

#[test]
fn add_source_keeps_every_mode_reachable_and_its_review_bounded() {
    // §12.7: the three modes are a segmented control, the kinds are radios, and
    // the dialog keeps one primary action. Every control stays clickable.
    use lvu::components::source::SourceControl;

    for (width, height) in SIZES {
        let (provider, mut app) = demo();
        draw(&provider, &mut app, width, height, Theme::TERMINAL);
        app.handle(Action::Open(Open::Source), &provider);
        let buffer = draw(&provider, &mut app, width, height, Theme::TERMINAL);
        let rendered = screen(&buffer);
        let surface = app.hit_regions.selection_modal.expect("source surface");

        for control in [
            SourceControl::Manual,
            SourceControl::Discovery,
            SourceControl::Agent,
            SourceControl::File,
            SourceControl::Command,
            SourceControl::Input,
        ] {
            let rect = app
                .layers
                .source
                .control_rects()
                .iter()
                .find_map(|(rect, candidate)| (*candidate == control).then_some(*rect))
                .unwrap_or_else(|| {
                    panic!("{control:?} has no hitbox at {width}x{height}:\n{rendered}")
                });
            assert!(rect.width > 0 && rect.height > 0, "{control:?} is empty");
            assert!(
                rect.x >= surface.x
                    && rect.right() <= surface.right()
                    && rect.y >= surface.y
                    && rect.bottom() <= surface.bottom(),
                "{control:?} {rect:?} escapes the drawn surface {surface:?} at {width}x{height}"
            );
        }
        assert!(rendered.contains("[ Open ]"), "{rendered}");
        for retired in ["[ Manual ]", "[ Discover ]", "[ File ]", "[ Command ]"] {
            assert!(
                !rendered.contains(retired),
                "{retired} survived:\n{rendered}"
            );
        }
    }
}

#[test]
fn the_source_proposal_review_stays_scrollable_at_every_size() {
    // The bounded review is what makes an irreversible launch safe: every field
    // must be reachable before [ Start reviewed source ], at every size.
    use lvu::app::{SourceAiPreview, SourceAiPreviewItem};

    for (width, height) in SIZES {
        let (provider, mut app) = demo();
        draw(&provider, &mut app, width, height, Theme::TERMINAL);
        app.handle(Action::Open(Open::Source), &provider);
        app.handle(
            Action::Command(
                lvu::component::LayerId::Source,
                lvu::command_palette::CommandId::AskAiSource,
            ),
            &provider,
        );
        // A real submission is what allocates the generation the worker answers.
        app.handle(
            Action::Raw(RawEvent::Paste("follow the api service".into())),
            &provider,
        );
        app.handle(
            Action::Raw(RawEvent::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            &provider,
        );
        let generation = match app.take_source_ai_requests().pop().expect("start request") {
            lvu::SourceAiRequest::Start { generation, .. } => generation,
            other => panic!("unexpected request: {other:?}"),
        };
        assert!(app.finish_source_ai(
            generation,
            Ok(SourceAiPreview {
                sources: vec![SourceAiPreviewItem {
                    name: "reviewed source".into(),
                    kind: "command".into(),
                    launch: "journalctl --follow --unit api.service".into(),
                    effective_path_or_cwd: "/srv/controlled application".into(),
                    restart: "on-failure with bounded delay".into(),
                    environment: (0..10).map(|index| format!("KEY_{index}=value")).collect(),
                }],
                explanation: "selected from bounded local discovery evidence".into(),
            })
        ));

        let mut seen = String::new();
        for _ in 0..40 {
            seen.push_str(&screen(&draw(
                &provider,
                &mut app,
                width,
                height,
                Theme::TERMINAL,
            )));
            let dialog = app.layers.source.state();
            if dialog.ai.preview_scroll >= dialog.ai.preview_scroll_limit {
                break;
            }
            app.handle(
                Action::Raw(RawEvent::Key(KeyEvent::new(
                    KeyCode::Down,
                    KeyModifiers::NONE,
                ))),
                &provider,
            );
        }
        for field in [
            "Launch:",
            "Effective path/cwd:",
            "Restart:",
            "KEY_9=value",
            "Why:",
        ] {
            assert!(
                seen.contains(field),
                "{field} unreachable at {width}x{height}"
            );
        }
        assert!(
            seen.contains("Start reviewed source"),
            "the confirmation action must stay visible at {width}x{height}"
        );
    }
}

#[test]
fn storage_entries_keep_their_columns_and_never_clip_an_identifier_at_the_start() {
    // dialog-system.md §12.13: kind, right-aligned size, name, status. The
    // status column is what width pressure drops; the name is elided in the
    // middle so both ends of a long id survive.
    use lvu::{StorageCategory, StorageEntry, StorageSnapshot};

    for (width, height) in SIZES {
        let (provider, mut app) = demo();
        draw(&provider, &mut app, width, height, Theme::TERMINAL);
        app.handle(Action::Open(Open::Storage), &provider);
        let generation = app.layers.storage.outbox.take()[0].generation;
        assert!(app.layers.storage.complete(
            generation,
            StorageSnapshot {
                entries: vec![StorageEntry {
                    category: StorageCategory::Derived,
                    label: "ed4a0c76-63b7-59e8-bbbd-5167f7c3ec5c.d17625e2.rows.idx".into(),
                    bytes: 2662,
                    reclaimable: 0,
                    status: "unrecognized · kept".into(),
                }],
                total_bytes: 138_000,
                reclaimable_bytes: 0,
                row_cache_bytes: 29_184,
                row_cache_limit: 4_194_304,
                query_index_bytes: 208,
                query_index_limit: 268_435_456,
                derived_index_limit_per_source: 268_435_456,
                derived_index_limit_total: 5_368_709_120,
                truncated: false,
                errors: Vec::new(),
            },
            "scan complete".into(),
            true,
        ));
        let buffer = draw(&provider, &mut app, width, height, Theme::TERMINAL);
        let rendered = screen(&buffer);

        let row = app.layers.storage.row_rects()[0].0;
        let text: String = (row.x..row.right())
            .map(|x| buffer[(x, row.y)].symbol())
            .collect();
        assert!(
            text.contains("derived") && text.contains("2.6 KiB"),
            "kind and size columns at {width}x{height}: {text:?}"
        );
        // Both ends of the identifier survive: §11 bans clipping from the start.
        assert!(
            text.contains("ed4a0c76"),
            "the identifier head must survive at {width}x{height}: {text:?}"
        );
        assert!(
            text.contains("rows.idx"),
            "the identifier tail must survive at {width}x{height}: {text:?}"
        );
        // §12.13 keeps the budget caption with the numbers it qualifies.
        assert!(
            rendered.contains("not a process RSS limit"),
            "at {width}x{height}:\n{rendered}"
        );
        assert!(rendered.contains("[ Refresh ]"), "{rendered}");
        assert!(!rendered.contains("r refresh"), "{rendered}");
    }
}

#[test]
fn a_long_storage_diagnostic_stays_reachable_by_scrolling() {
    use lvu::{StorageCategory, StorageEntry, StorageSnapshot};

    let (provider, mut app) = demo();
    draw(&provider, &mut app, 100, 30, Theme::TERMINAL);
    app.handle(Action::Open(Open::Storage), &provider);
    let generation = app.layers.storage.outbox.take()[0].generation;
    assert!(app.layers.storage.complete(
        generation,
        StorageSnapshot {
            entries: vec![StorageEntry {
                category: StorageCategory::Derived,
                label: "unused.rows.idx".into(),
                bytes: 12,
                reclaimable: 12,
                status: "unused, recomputable".into(),
            }],
            total_bytes: 12,
            reclaimable_bytes: 12,
            row_cache_bytes: 1,
            row_cache_limit: 10,
            query_index_bytes: 2,
            query_index_limit: 20,
            derived_index_limit_per_source: 30,
            derived_index_limit_total: 300,
            truncated: false,
            errors: vec![format!("scan warning {}", "bounded detail ".repeat(30))],
        },
        "complete".into(),
        true,
    ));
    draw(&provider, &mut app, 100, 30, Theme::TERMINAL);
    let limit = app.layers.storage.scroll_limit();
    assert!(
        limit > 0,
        "a long diagnostic must really overflow, not be clipped away"
    );
    // §5.1: the wheel target is the component's own geometry, resolved through
    // `hit()` rather than a shared `HitRegions` entry.
    let popup = dialog_popup(&app);
    let wheel = (0..popup.height)
        .map(|offset| (popup.x + 2, popup.y + offset))
        .find(|point| {
            app.layers.storage.hit(*point)
                == Some(lvu::components::storage::StorageHit::Diagnostics)
        })
        .expect("the diagnostics pane needs a wheel target");
    for _ in 0..limit {
        app.handle(
            Action::Raw(RawEvent::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: wheel.0,
                row: wheel.1,
                modifiers: KeyModifiers::NONE,
            })),
            &provider,
        );
    }
    assert_eq!(app.layers.storage.scroll(), limit);
    let scrolled = screen(&draw(&provider, &mut app, 100, 30, Theme::TERMINAL));
    assert!(scrolled.contains("bounded detail"), "{scrolled}");
}

#[test]
fn the_time_form_keeps_one_label_column_and_no_scroll_pseudo_buttons() {
    // §12.4 and §1: the dropdowns join the label column, the boxed one-line
    // status becomes the message row, and §9 retires the scroll buttons.
    for (width, height) in SIZES {
        let (provider, mut app) = demo();
        draw(&provider, &mut app, width, height, Theme::TERMINAL);
        app.handle(Action::Open(Open::Time), &provider);
        let buffer = draw(&provider, &mut app, width, height, Theme::TERMINAL);
        let rendered = screen(&buffer);

        assert!(
            rendered.contains("Time basis"),
            "at {width}x{height}:\n{rendered}"
        );
        assert!(
            rendered.contains("Window"),
            "at {width}x{height}:\n{rendered}"
        );
        // §8.3 retires the `[ Label: value ▾ ]` button form.
        assert!(
            !rendered.contains("Time basis: "),
            "at {width}x{height}:\n{rendered}"
        );
        // §7.4: one state word, no stutter, and no bordered one-line status.
        assert!(
            rendered.contains("Applied"),
            "at {width}x{height}:\n{rendered}"
        );
        assert!(
            !rendered.contains("Applied:"),
            "at {width}x{height}:\n{rendered}"
        );
        assert!(
            !rendered.contains("Scroll up") && !rendered.contains("Scroll down"),
            "at {width}x{height}:\n{rendered}"
        );
        assert!(
            rendered.contains("[ Apply ]"),
            "at {width}x{height}:\n{rendered}"
        );

        // Both dropdown values sit at the same column as each other. Measured
        // in characters, not bytes: the sidebar behind the dialog carries
        // multi-byte glyphs, so a byte offset says nothing about a column.
        let column =
            |line: &str, needle: &str| line.find(needle).map(|byte| line[..byte].chars().count());
        let basis = rendered
            .lines()
            .find(|line| line.contains("Time basis"))
            .expect("basis row");
        let window = rendered
            .lines()
            .find(|line| line.contains("Window"))
            .expect("window row");
        assert_eq!(
            column(basis, "Capture"),
            column(window, "All time"),
            "dropdown values share the field column at {width}x{height}:\n{rendered}"
        );
    }
}

#[test]
fn a_reflowed_time_bound_keeps_its_compound_label() {
    // §4.2: a group that reflows to one field per row must not leave a bare
    // `time` or `zone` that no longer says which bound it belongs to.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 54, 16, Theme::TERMINAL);
    app.handle(Action::Open(Open::Time), &provider);
    let rendered = screen(&draw(&provider, &mut app, 54, 16, Theme::TERMINAL));
    for label in [
        "Start date",
        "Start time",
        "Start zone",
        "End date",
        "End time",
    ] {
        assert!(rendered.contains(label), "missing {label}:\n{rendered}");
    }

    // The values line up under one column for the whole group.
    let column = |needle: &str| {
        rendered
            .lines()
            .find(|line| line.contains(needle))
            .and_then(|line| line.find(needle))
            .map(|x| x + needle.len())
    };
    let start = column("Start date");
    assert!(start.is_some());
    for label in ["Start time", "Start zone"] {
        assert_eq!(
            column(label).map(|x| x + "Start date".len() - label.len()),
            start,
            "{label} shares the group's label column:\n{rendered}"
        );
    }
}

/// A recognizer report whose leading candidate needs a timezone assumption, and
/// which offers a clean override, so the confirmation step has both paths.
fn recognition() -> lvu::app::TimeRecognition {
    lvu::app::TimeRecognition {
        sampled_records: 128,
        candidates: vec![
            lvu::app::TimeFieldCandidate {
                token: "structured:ts|text|reject|-".into(),
                label: "ts".into(),
                reading: "date-time without timezone".into(),
                coverage_percent: Some(97),
                assumptions: vec!["value has no timezone; assumed UTC".into()],
                blocked: None,
                text_format: None,
                alternatives: vec![lvu::app::TimeFieldCandidate {
                    token: "structured:ts|epoch_ms|reject|-".into(),
                    label: "ts".into(),
                    reading: "epoch milliseconds".into(),
                    coverage_percent: Some(41),
                    assumptions: Vec::new(),
                    blocked: None,
                    text_format: None,
                    alternatives: Vec::new(),
                }],
            },
            lvu::app::TimeFieldCandidate {
                token: "structured:when|auto|reject|-".into(),
                label: "when".into(),
                reading: "detected per row".into(),
                coverage_percent: Some(12),
                assumptions: Vec::new(),
                blocked: Some("epoch unit is ambiguous between seconds and milliseconds".into()),
                text_format: None,
                alternatives: Vec::new(),
            },
        ],
        diagnostics: Vec::new(),
        anchored_selected_nanos: None,
        scanning: false,
        probe: None,
    }
}

fn open_time_with_candidates(app: &mut App, provider: &FixtureProvider) -> u64 {
    app.handle(Action::Open(Open::Time), provider);
    let generation = app
        .layers
        .time
        .outbox
        .take()
        .first()
        .expect("opening Time asks for candidates")
        .generation;
    assert!(
        app.layers
            .time
            .complete_recognition(generation, recognition())
    );
    generation
}

/// The basis dropdown offers the built-in bases and then every recognized
/// candidate, each labelled with what accepting it would cost.
#[test]
fn time_basis_dropdown_ranks_candidates_and_states_their_cost() {
    let (provider, mut app) = demo();
    open_time_with_candidates(&mut app, &provider);
    time_focus(&mut app, &provider, TimeControl::Basis);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let rendered = screen(&draw(&provider, &mut app, 100, 30, Theme::TERMINAL));
    for expected in [
        "Capture",
        "Recognized",
        "Extracted",
        "ts · date-time without timezone · needs an assumption",
        "when · detected per row · blocked",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected}:\n{rendered}"
        );
    }
}

/// A candidate that rests on a guess is never applied by choosing it. The
/// assumption, the validated coverage and an explicit Accept stand between.
#[test]
fn choosing_an_assuming_candidate_requires_explicit_acceptance() {
    let (provider, mut app) = demo();
    open_time_with_candidates(&mut app, &provider);
    time_choose(&mut app, &provider, TimeControl::Basis, 3);
    let dialog = app.layers.time.state();
    assert_eq!(dialog.basis, lvu::TimeBasis::Capture, "nothing was applied");
    assert!(dialog.field_token.is_none());
    assert!(dialog.pending_field.is_some());

    let rendered = screen(&draw(&provider, &mut app, 100, 30, Theme::TERMINAL));
    for expected in [
        "Field: ts",
        "Coverage: 97% of 128 sampled records",
        "Assumes: value has no timezone; assumed UTC",
        "Accept assumption",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected}:\n{rendered}"
        );
    }

    time_activate(&mut app, &provider, TimeControl::AcceptField);
    let dialog = app.layers.time.state();
    assert_eq!(dialog.basis, lvu::TimeBasis::Selected);
    assert_eq!(
        dialog.field_token.as_deref(),
        Some("structured:ts|text|reject|-")
    );
    assert!(dialog.pending_field.is_none());
}

/// The override is the alternative reading, and picking one with no assumption
/// leaves nothing to confirm beyond the accept itself.
#[test]
fn overriding_the_reading_switches_the_token_that_would_be_applied() {
    let (provider, mut app) = demo();
    open_time_with_candidates(&mut app, &provider);
    time_choose(&mut app, &provider, TimeControl::Basis, 3);
    time_focus(&mut app, &provider, TimeControl::Reading);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let rendered = screen(&draw(&provider, &mut app, 100, 30, Theme::TERMINAL));
    assert!(
        rendered.contains("epoch milliseconds · no assumption"),
        "the override is on offer:\n{rendered}"
    );
    app.handle(raw_key(KeyCode::Down), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    time_activate(&mut app, &provider, TimeControl::AcceptField);
    assert_eq!(
        app.layers.time.state().field_token.as_deref(),
        Some("structured:ts|epoch_ms|reject|-")
    );
}

/// A blocked candidate stays visible with its reason and applies nothing.
#[test]
fn a_blocked_candidate_reports_why_and_is_not_selectable() {
    let (provider, mut app) = demo();
    open_time_with_candidates(&mut app, &provider);
    time_choose(&mut app, &provider, TimeControl::Basis, 4);
    let dialog = app.layers.time.state();
    assert!(dialog.pending_field.is_none());
    assert!(dialog.field_token.is_none());
    assert_eq!(dialog.basis, lvu::TimeBasis::Capture);
    let rendered = screen(&draw(&provider, &mut app, 100, 30, Theme::TERMINAL));
    assert!(
        rendered.contains("epoch unit is ambiguous"),
        "the reason is shown:\n{rendered}"
    );
}

/// The confirmation step survives the smallest supported terminal: every row it
/// needs stays reachable rather than being clipped away.
#[test]
fn the_confirmation_step_stays_reachable_when_compact() {
    let (provider, mut app) = demo();
    open_time_with_candidates(&mut app, &provider);
    time_choose(&mut app, &provider, TimeControl::Basis, 3);
    let mut seen = String::new();
    for _ in 0..12 {
        seen.push_str(&screen(&draw(&provider, &mut app, 54, 16, Theme::TERMINAL)));
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    for expected in ["Assumes:", "Coverage:", "Accept assumption"] {
        assert!(
            seen.contains(expected),
            "missing {expected} at 54x16:\n{seen}"
        );
    }
}

/// Delivers a recipe list the way `lvu-app` does, matching the fence the dialog
/// recorded when it asked.
fn deliver_recipes(app: &mut App, items: Vec<lvu::app::RecipeItem>) {
    let meta = app
        .take_recipe_requests()
        .into_iter()
        .find_map(|request| match request {
            lvu::app::RecipeRequest::List { meta } => Some(meta),
            _ => None,
        })
        .expect("opening Recipes asks for the list");
    app.set_recipes_with_suggestions(meta, items, Vec::new(), None);
}

/// §12.9: the recipe list is a pane with a name column, a summary that says
/// what applying it would restore, and a revision — not an implementation dump.
#[test]
fn a_recipe_row_says_what_applying_it_would_restore() {
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    deliver_recipes(
        &mut app,
        vec![lvu::app::RecipeItem {
            id: "one".into(),
            revision: "0123456789abcdef".into(),
            saved_at_unix_nanos: None,
            name: "error triage".into(),
            config: lvu::app::RecipeConfig {
                search: "ERROR".into(),
                enrichments: vec![],
                enrichment: "pl.col('raw')".into(),
                grouping: "^\\s".into(),
                ..Default::default()
            },
            incompatibility: None,
        }],
    );
    // Wide enough for the whole summary; the narrow case is covered by
    // `recipes_and_bookmarks_stay_within_the_frame_at_every_size`.
    let rendered = screen(&draw(&provider, &mut app, 140, 40, Theme::TERMINAL));
    for expected in [
        "Saved recipes",
        "error triage",
        "search=\"ERROR\"",
        "1 enrichment",
        "grouping",
        "Apply restores a recipe",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected}:\n{rendered}"
        );
    }
    // §12.9's last column is the date this revision was saved. This recipe was
    // written before the stored document carried one, so it says so rather than
    // guessing; the revision id it replaced is still what the message row and
    // History name.
    let row = rendered
        .lines()
        .find(|line| line.contains("error triage"))
        .unwrap_or_default();
    assert!(row.contains('—'), "{rendered}");
    assert!(!row.contains("01234567"), "{rendered}");
    // §11: the implementation-shaped preview row is gone.
    assert!(!rendered.contains("advanced=false"), "{rendered}");
}

/// §12.9's date column: the day a revision was saved, in the app's display
/// zone, right-aligned in ten cells.
#[test]
fn a_recipe_row_dates_the_revision_it_would_apply() {
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    deliver_recipes(
        &mut app,
        vec![lvu::app::RecipeItem {
            id: "one".into(),
            revision: "0123456789abcdef".into(),
            // 2026-09-06T12:00:00Z
            saved_at_unix_nanos: Some(1_788_696_000_000_000_000),
            name: "error triage".into(),
            config: lvu::app::RecipeConfig::default(),
            incompatibility: None,
        }],
    );
    let rendered = screen(&draw(&provider, &mut app, 140, 40, Theme::TERMINAL));
    let row = rendered
        .lines()
        .find(|line| line.contains("error triage"))
        .unwrap_or_default();
    assert!(row.contains("2026-09-06"), "{rendered}");
    assert!(
        !row.contains('—'),
        "a dated revision must not read as unknown\n{rendered}"
    );
}

/// A row's hitbox is where the row is drawn, so clicking one selects it.
#[test]
fn clicking_a_recipe_row_selects_that_recipe() {
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    deliver_recipes(
        &mut app,
        (0..3)
            .map(|index| lvu::app::RecipeItem {
                id: format!("id-{index}"),
                revision: format!("rev{index}0000000"),
                saved_at_unix_nanos: None,
                name: format!("recipe {index}"),
                config: lvu::app::RecipeConfig::default(),
                incompatibility: None,
            })
            .collect(),
    );
    draw(&provider, &mut app, 100, 30, Theme::TERMINAL);
    let (rect, index) = app
        .layers
        .recipes
        .row_rects()
        .iter()
        .copied()
        .find(|(_, index)| *index == 2)
        .expect("the third row is drawn");
    app.handle(
        Action::Raw(lvu::component::RawEvent::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x + 1,
            rect.y,
        ))),
        &provider,
    );
    assert_eq!(app.layers.recipes.state().selected, index);
}

/// §12.10: a bookmark is two lines — the record it marks, then its note.
#[test]
fn a_bookmark_shows_the_record_it_marks_and_its_note() {
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 100, 30, Theme::TERMINAL);
    app.handle(Action::ToggleBookmark, &provider);
    app.handle(Action::Open(Open::Bookmarks), &provider);
    let rendered = screen(&draw(&provider, &mut app, 100, 30, Theme::TERMINAL));
    assert!(rendered.contains("Bookmarks · "), "{rendered}");
    assert!(
        rendered.contains("of 128"),
        "the cap stays visible:\n{rendered}"
    );
    assert!(rendered.contains("no note"), "{rendered}");
    // The record's own text is what identifies the bookmark, not just its id.
    assert!(rendered.contains("fixture request"), "{rendered}");

    // The note editor is its own named child (§12.10), and says what it caps.
    app.handle(raw_alt(KeyCode::Char('e')), &provider);
    let editing = screen(&draw(&provider, &mut app, 100, 30, Theme::TERMINAL));
    assert!(editing.contains("Note for #"), "{editing}");
    assert!(editing.contains("1024 bytes"), "{editing}");
}

/// Both adopted dialogs stay inside the smallest supported terminal, in both
/// themes, with their actions still on screen.
#[test]
fn recipes_and_bookmarks_stay_within_the_frame_at_every_size() {
    for theme in [Theme::LOVE_DARK, Theme::LOVE_LIGHT] {
        for (width, height) in [(140, 40), (100, 30), (80, 24), (54, 16)] {
            let (provider, mut app) = demo();
            draw(&provider, &mut app, width, height, theme);
            app.handle(Action::ToggleBookmark, &provider);
            app.handle(Action::Open(Open::Bookmarks), &provider);
            let rendered = screen(&draw(&provider, &mut app, width, height, theme));
            assert!(
                rendered.contains("Raw context"),
                "bookmarks actions at {width}x{height}:\n{rendered}"
            );
            app.handle(Action::CancelEditor, &provider);
            app.handle(
                Action::Open(Open::Recipes {
                    mode: RecipeDialogMode::Browse,
                }),
                &provider,
            );
            let rendered = screen(&draw(&provider, &mut app, width, height, theme));
            assert!(
                rendered.contains("Saved recipes"),
                "recipes pane at {width}x{height}:\n{rendered}"
            );
        }
    }
}

/// §5.2: a dialog is as tall as its content, at every size.
///
/// `dialog_rect` used to size from the padded row count while `regions` then
/// shed that padding to fit, so the rows padding gave back landed in the body
/// as blank rows. The two now agree, so the interior a dialog is given is
/// exactly the interior it lays out.
#[test]
fn a_dialog_is_never_taller_than_the_rows_it_lays_out() {
    let contents = [
        DialogContent {
            body: 1,
            message: 1,
            actions: 1,
            ..DialogContent::default()
        },
        DialogContent {
            header: 1,
            body: 4,
            message: 2,
            help: 2,
            actions: 2,
        },
        DialogContent {
            body: 12,
            message: 1,
            help: 1,
            actions: 1,
            ..DialogContent::default()
        },
        DialogContent {
            body: 400,
            message: 2,
            help: 2,
            actions: 2,
            ..DialogContent::default()
        },
    ];
    for (width, height) in SIZES {
        let area = Rect::new(0, 0, width, height);
        for class in [
            DialogClass::S,
            DialogClass::M,
            DialogClass::L,
            DialogClass::P,
        ] {
            for content in &contents {
                let rect = dialog_rect(area, class, content);
                let laid_out = regions(rect, content);
                // The invariant: the interior a dialog is given is the
                // interior its content uses. A dialog taller than that ends up
                // with rows nothing draws into.
                let interior = laid_out.interior.height;
                let used = fitted_rows(interior, content);
                assert!(
                    used >= interior || interior <= 1,
                    "{class:?} at {width}x{height} is {interior} rows tall but \
                     uses {used}: {rect:?} from {content:?}"
                );
                // The body still receives every leftover row, so a dialog that
                // draws more than it declared is shortened, never starved.
                let occupied: u16 = [
                    laid_out.header,
                    laid_out.body,
                    laid_out.message,
                    laid_out.help,
                    laid_out.actions,
                ]
                .iter()
                .map(|rect| rect.height)
                .sum();
                let slack = interior.saturating_sub(occupied);
                assert!(
                    slack <= 5,
                    "{class:?} at {width}x{height} left {slack} rows to nothing: \
                     {rect:?} {laid_out:?} from {content:?}"
                );
            }
        }
    }
}

/// The same invariant through the real dialogs, at the smallest supported size:
/// no adopted dialog ends with blank rows between its last content and its
/// border.
#[test]
fn the_adopted_dialogs_have_no_dead_rows_at_54x16() {
    /// The longest run of empty rows anywhere inside the border. An over-tall
    /// body shows up here rather than at the bottom: its unused rows sit
    /// between the last content row and the message row.
    fn longest_blank_run(buffer: &Buffer, interior: Rect) -> u16 {
        let mut longest = 0;
        let mut run = 0;
        for y in interior.y..interior.bottom() {
            let empty =
                (interior.x..interior.right()).all(|x| buffer[(x, y)].symbol().trim().is_empty());
            run = if empty { run + 1 } else { 0 };
            longest = longest.max(run);
        }
        longest
    }

    // Source is deliberately absent: §5.2.1 reserves rows for its suggestion
    // list, and those rows are blank exactly when the list is empty. The
    // property that replaces this one for Source is that the reservation never
    // changes size — `add_source_keeps_one_rectangle_while_the_completion_list_changes`.
    type Opener = (&'static str, fn(&FixtureProvider, &mut App));
    let openers: [Opener; 4] = [
        ("search", |provider, app| {
            app.handle(Action::Open(Open::Search), provider);
        }),
        ("time", |provider, app| {
            app.handle(Action::Open(Open::Time), provider);
        }),
        ("recipes", |provider, app| {
            app.handle(
                Action::Open(Open::Recipes {
                    mode: RecipeDialogMode::Browse,
                }),
                provider,
            );
        }),
        ("bookmarks", |provider, app| {
            app.handle(Action::ToggleBookmark, provider);
            app.handle(Action::Open(Open::Bookmarks), provider);
        }),
    ];
    for (name, open) in openers {
        let (provider, mut app) = demo();
        draw(&provider, &mut app, 54, 16, Theme::TERMINAL);
        open(&provider, &mut app);
        let buffer = draw(&provider, &mut app, 54, 16, Theme::TERMINAL);
        // The dialog records its own interior for selection and hit testing;
        // that is the box to measure, not whichever border is tallest.
        let interior = app
            .hit_regions
            .selection_modal
            .expect("an open dialog records its interior");
        let blank = longest_blank_run(&buffer, interior);
        // §4.1 allows one gap between region groups. More than that is a
        // region padded out with rows it has no content for.
        assert!(
            blank <= 1,
            "{name} has a {blank}-row blank run inside its border:\n{}",
            screen(&buffer)
        );
    }
}

/// §5.2.1: the Add source suggestion list is a live region, so the dialog it
/// lives in keeps one rectangle from the moment it opens.
///
/// Before this rule the body asked for `2 + 2 + min(candidates, 8)` rows. Each
/// keystroke restarted the debounced scan, which emptied the list, so the popup
/// collapsed and grew back around every character: measured on a real terminal
/// at 80x24 the top edge moved from row 8 to row 4 and the height from 7 to 16,
/// twice per keystroke.
#[test]
fn add_source_keeps_one_rectangle_while_the_completion_list_changes() {
    for (width, height) in [(80u16, 24u16), (54, 16)] {
        let mut rects: Vec<(&str, Rect)> = Vec::new();
        // Every state one keystroke can put the list into: freshly opened, scan
        // pending, no matches, a few matches, and more than the reservation.
        let states: [(&str, usize); 4] = [("pending", 0), ("empty", 0), ("few", 3), ("many", 40)];
        let (provider, mut app) = demo();
        draw(&provider, &mut app, width, height, Theme::TERMINAL);
        app.handle(Action::Open(Open::Source), &provider);
        draw(&provider, &mut app, width, height, Theme::TERMINAL);
        rects.push((
            "opened",
            app.hit_regions.selection_modal.expect("source surface"),
        ));

        for (name, count) in states {
            app.handle(
                Action::Raw(RawEvent::Key(KeyEvent::new(
                    KeyCode::Char('a'),
                    KeyModifiers::NONE,
                ))),
                &provider,
            );
            draw(&provider, &mut app, width, height, Theme::TERMINAL);
            rects.push((
                "scan pending",
                app.hit_regions.selection_modal.expect("source surface"),
            ));
            if name != "pending" {
                std::thread::sleep(std::time::Duration::from_millis(45));
                let requests = app.take_path_completion_requests();
                let generation = requests
                    .last()
                    .map(|request| request.generation)
                    .expect("the draft schedules a completion scan");
                let candidates: Vec<String> =
                    (0..count).map(|index| format!("a{index:02}.log")).collect();
                app.apply_path_completion_result(generation, "a", None, candidates, None);
                draw(&provider, &mut app, width, height, Theme::TERMINAL);
                rects.push((
                    name,
                    app.hit_regions.selection_modal.expect("source surface"),
                ));
            }
            app.handle(
                Action::Raw(RawEvent::Key(KeyEvent::new(
                    KeyCode::Backspace,
                    KeyModifiers::NONE,
                ))),
                &provider,
            );
        }

        let first = rects[0].1;
        assert!(
            rects.iter().all(|(_, rect)| *rect == first),
            "§5.2.1: the Add source surface changed at {width}x{height}: {rects:?}"
        );
    }
}

/// §12.11: the fields list is a two-column pane with a checkbox per row, and
/// the affordances are buttons rather than a printed key list.
#[test]
fn fields_names_its_record_and_offers_its_actions_as_buttons() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 10);
    app.handle(Action::Open(Open::Fields), &provider);
    let rendered = screen(&draw(&provider, &mut app, 100, 30, Theme::TERMINAL));
    for expected in [
        "Fields · record",
        "Field",
        "Value",
        "[ ]",
        "[ Pin ]",
        "[ Color ]",
        "Pinned fields become log columns",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected}:\n{rendered}"
        );
    }
    // §11: no key inventory.
    for banned in ["Space pin", "↑/↓", "Esc", "Enter"] {
        assert!(!rendered.contains(banned), "{banned} leaked:\n{rendered}");
    }
    // Pinning through the button changes what the button then offers.
    let (rect, _) = app
        .layers
        .fields
        .control_rects()
        .iter()
        .copied()
        .find(|(_, control)| *control == lvu::app::FieldPickerControl::Pin)
        .expect("the Pin button is drawn");
    app.handle(
        Action::Raw(RawEvent::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x + 2,
            rect.y,
        ))),
        &provider,
    );
    let pinned = screen(&draw(&provider, &mut app, 100, 30, Theme::TERMINAL));
    assert!(pinned.contains("[ Unpin ]"), "{pinned}");
    assert!(pinned.contains("[x]"), "{pinned}");
}

/// §12.15: Help is two columns once the content is wide enough, a wrapped
/// description continues under the description column, and there is no footer.
#[test]
fn help_reflows_into_columns_and_continues_under_its_description() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Help), &provider);
    let wide = screen(&draw(&provider, &mut app, 140, 40, Theme::TERMINAL));
    // Two columns: a row carries an entry from each group.
    assert!(
        wide.lines()
            .any(|line| line.contains("EVERYWHERE") && line.contains("OPEN")),
        "two columns at 140x40:\n{wide}"
    );

    let narrow = screen(&draw(&provider, &mut app, 100, 30, Theme::TERMINAL));
    assert!(
        !narrow
            .lines()
            .any(|line| line.contains("EVERYWHERE") && line.contains("OPEN")),
        "one column at 100x30:\n{narrow}"
    );
    // The continued half of a wrapped description starts under the description,
    // not back in the key column where it would read as another binding.
    let lines: Vec<&str> = narrow.lines().collect();
    let wrapped = lines
        .iter()
        .position(|line| line.contains("Command palette:"))
        .expect("the longest entry is on screen");
    // Byte offsets would drift by the sidebar's multi-byte glyphs, so
    // measure in terminal columns.
    let column_of = |line: &str, needle: &str| {
        line.find(needle)
            .map(|byte| unicode_width::UnicodeWidthStr::width(&line[..byte]))
    };
    let key_column = column_of(lines[wrapped], "Ctrl-P").expect("key column");
    let description_column =
        column_of(lines[wrapped], "Command palette").expect("description column");
    // The rendered line keeps the dialog border, so measure where the
    // continuation's text sits rather than how much whitespace precedes it.
    let continuation = lines[wrapped + 1];
    let indent =
        column_of(continuation, "shortcut shown").expect("the entry wraps onto the next line");
    assert!(
        indent == description_column && indent > key_column,
        "continuation at {indent} should start at the description column \
         {description_column}:\n{narrow}"
    );
    // §12.15: nothing here is actionable, so there is no footer and no button.
    // Scope the button check to the dialog: the log behind it draws its own.
    let top = lines
        .iter()
        .position(|line| line.contains("┌ Help "))
        .expect("the Help frame");
    let bottom = lines[top..]
        .iter()
        .position(|line| line.contains('└'))
        .map(|offset| top + offset)
        .expect("the Help frame closes");
    for line in &lines[top..=bottom] {
        let inside = line.split('│').nth(1).unwrap_or("");
        assert!(
            !inside.contains("[ "),
            "a button leaked into Help:\n{narrow}"
        );
    }
    for banned in ["↑/↓ or j/k", "? close"] {
        assert!(!narrow.contains(banned), "{banned} leaked:\n{narrow}");
    }
}

/// §12.18: Investigation is a question, a transcript pane and one primary —
/// not three boxes and a `More` button.
#[test]
fn investigation_pairs_a_question_with_a_transcript_pane() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Investigation), &provider);
    let rendered = screen(&draw(&provider, &mut app, 100, 30, Theme::TERMINAL));
    for expected in ["Question", "Transcript", "0 messages", "[ Start ]"] {
        assert!(
            rendered.contains(expected),
            "missing {expected}:\n{rendered}"
        );
    }
    // §11: the boxed sections and the key inventory are gone.
    for banned in [
        "Question or follow-up",
        "Activity and saved",
        "[ More ]",
        "State ",
    ] {
        assert!(!rendered.contains(banned), "{banned} leaked:\n{rendered}");
    }
}

/// §12.16: the palette's trailing columns are fixed and right-aligned, so they
/// stay put whatever the names do, and the detail row names the selected
/// command in full even when its list row is clipped.
#[test]
fn the_palette_keeps_fixed_columns_and_names_the_selected_command() {
    let mut palette = lvu::command_palette::Palette::new();
    palette.open(lvu::command_palette::PaletteContext::new(
        lvu::app::Focus::Logs,
        true,
    ));
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| palette.render_with_theme(frame, frame.area(), Theme::TERMINAL))
        .unwrap();
    let rendered = screen(terminal.backend().buffer());
    assert!(rendered.contains("Command palette"), "{rendered}");
    // §7.1: the binding is documented in Help, not in the title.
    assert!(!rendered.contains("Command palette · Ctrl-P"), "{rendered}");
    assert!(rendered.contains("Type a command…"), "{rendered}");
    // The category column starts at the same screen column on every row: fixed
    // trailing columns are what make the list scannable.
    // Character columns, not byte offsets: the frame is drawn with box glyphs.
    // The category is the trailing occurrence: a command *name* may carry the
    // same word (`Filter › Search` sits in the Filter category).
    let char_column =
        |line: &str, needle: &str| line.rfind(needle).map(|byte| line[..byte].chars().count());
    let columns: std::collections::BTreeSet<usize> = rendered
        .lines()
        .filter_map(|line| char_column(line, "Views").or_else(|| char_column(line, "Filter")))
        .collect();
    assert_eq!(columns.len(), 1, "category column drifted:\n{rendered}");

    // §7.2: one vocabulary, Title case. Read the column, not the whole row.
    let column = *columns.iter().next().expect("a category column");
    for line in rendered.lines() {
        // Index by character, not by byte: the frame is drawn with box glyphs.
        let category: String = line
            .chars()
            .skip(column)
            .collect::<String>()
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_owned();
        let category = category.as_str();
        if category.is_empty() || !category.chars().all(char::is_alphabetic) {
            continue;
        }
        assert!(
            category.starts_with(char::is_uppercase),
            "lower-case category {category:?}:\n{rendered}"
        );
    }
}

/// §12.6: the External command child is a labelled form, a scrollable review
/// pane and one action row — with no `Status:` stutter in the message row.
#[test]
fn external_command_is_a_form_with_a_review_pane() {
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    let rendered = screen(&draw(&provider, &mut app, 100, 30, Theme::TERMINAL));
    for expected in [
        "Enrichment › External command",
        "Program",
        "Arguments",
        "Directory",
        "Environment",
        "Results and review",
        "[ Save ]",
        "[ Review and run ]",
        "[ New line ]",
        "runs only when you confirm",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected}:\n{rendered}"
        );
    }
    for banned in ["Status:", "Status and review", "Alt-N", "line(s)"] {
        assert!(!rendered.contains(banned), "{banned} leaked:\n{rendered}");
    }
}

// --- Responsive shared-core foundation (phase A) ---
//
// Implementation-independent geometry acceptance: exact policy outputs,
// one-cell inset, compact/tiny guardrails, region order, Fill body, stable
// outer frames across async states, action/list/anchored planning, and
// display-width clipping. No component is migrated here; these exercise only
// `dialog_layout` + `dialog_controls` pure geometry.

use lvu::dialog_controls as responsive_actions;
use lvu::dialog_layout as responsive;

fn responsive_viewport(width: u16, height: u16) -> Rect {
    Rect::new(0, 0, width, height)
}

fn responsive_spec(presentation: responsive::PresentationKind) -> responsive::DialogSpec {
    responsive::DialogSpec::new(presentation, 1, 1, 1, 1, 1)
}

#[test]
#[allow(clippy::type_complexity)]
fn responsive_policy_matrix_matches_percentage_clamp_tokens() {
    use responsive::{ContextFootprint, PresentationKind};
    // (viewport, Prompt, Inspector, Form, Long, Palette) from the reviewed
    // tokens: percent rounded to nearest, clamped to min/max, capped to W-2.
    let cases: [((u16, u16), [(u16, u16); 5]); 4] = [
        (
            (240, 80),
            [(96, 16), (144, 36), (120, 36), (160, 64), (96, 30)],
        ),
        (
            (140, 40),
            [(95, 14), (115, 25), (109, 23), (126, 34), (90, 24)],
        ),
        (
            (100, 30),
            [(68, 11), (82, 19), (78, 17), (90, 26), (64, 18)],
        ),
        ((80, 24), [(54, 11), (66, 15), (64, 14), (72, 21), (51, 14)]),
    ];
    let kinds = [
        PresentationKind::Contextual(ContextFootprint::Prompt),
        PresentationKind::Contextual(ContextFootprint::Inspector),
        PresentationKind::SelfContainedForm,
        PresentationKind::LongContent,
        PresentationKind::Palette,
    ];
    for ((width, height), expected) in cases {
        let viewport = responsive_viewport(width, height);
        for (kind, (want_w, want_h)) in kinds.iter().zip(expected) {
            let (got_w, got_h) = responsive::policy_size(viewport, *kind);
            assert_eq!(
                (got_w, got_h),
                (want_w, want_h),
                "{kind:?} at {width}x{height}"
            );
        }
        // FullFrame is reserved and always the viewport itself.
        assert_eq!(
            responsive::policy_size(viewport, PresentationKind::FullFrame),
            (width, height),
            "FullFrame at {width}x{height}"
        );
    }
}

#[test]
fn responsive_roomy_frames_keep_a_once_cell_inset_and_stay_inside() {
    use responsive::{ContextFootprint, PresentationKind};
    let kinds = [
        PresentationKind::Contextual(ContextFootprint::Prompt),
        PresentationKind::Contextual(ContextFootprint::Inspector),
        PresentationKind::SelfContainedForm,
        PresentationKind::LongContent,
        PresentationKind::Palette,
    ];
    for (width, height) in [(240u16, 80u16), (140, 40), (100, 30), (80, 24)] {
        let viewport = responsive_viewport(width, height);
        for kind in kinds {
            let spec = responsive_spec(kind);
            let geometry =
                responsive::resolve_dialog(viewport, &spec, 10, &["Apply"], Some(0), None)
                    .expect("roomy viewport must resolve");
            assert!(
                geometry.frame.width <= width.saturating_sub(2)
                    && geometry.frame.height <= height.saturating_sub(2),
                "{kind:?} {width}x{height}: {:?} exceeds one-cell inset",
                geometry.frame
            );
            assert!(
                geometry.frame.x > viewport.x && geometry.frame.y > viewport.y,
                "{kind:?} {width}x{height}: {:?} touches the viewport edge",
                geometry.frame
            );
            assert!(
                geometry.frame.right() <= viewport.right()
                    && geometry.frame.bottom() <= viewport.bottom(),
                "{kind:?} {width}x{height}: {:?} escapes",
                geometry.frame
            );
        }
    }
}

#[test]
fn responsive_compact_override_collapses_every_kind() {
    use responsive::{ContextFootprint, PresentationKind};
    let viewport = responsive_viewport(54, 16);
    for kind in [
        PresentationKind::Contextual(ContextFootprint::Prompt),
        PresentationKind::Contextual(ContextFootprint::Inspector),
        PresentationKind::SelfContainedForm,
        PresentationKind::LongContent,
        PresentationKind::Palette,
    ] {
        let (width, height) = responsive::policy_size(viewport, kind);
        assert_eq!((width, height), (52, 14), "{kind:?} at 54x16");
        let spec = responsive_spec(kind);
        let geometry = responsive::resolve_dialog(viewport, &spec, 4, &["Apply"], Some(0), None)
            .expect("54x16 must resolve");
        assert_eq!((geometry.frame.width, geometry.frame.height), (52, 14));
        assert!(
            geometry.frame.right() <= viewport.right()
                && geometry.frame.bottom() <= viewport.bottom(),
            "{kind:?}: {:?} escapes 54x16",
            geometry.frame
        );
    }
}

#[test]
fn responsive_twenty_by_six_survives_essentials_and_nineteen_by_five_refuses() {
    use responsive::{ContextFootprint, PresentationKind};
    // 20x6 uses the full frame when W-2 cannot satisfy the safety floor.
    let tiny_ok = responsive_viewport(20, 6);
    for kind in [
        PresentationKind::Contextual(ContextFootprint::Prompt),
        PresentationKind::SelfContainedForm,
        PresentationKind::LongContent,
        PresentationKind::Palette,
    ] {
        let (width, height) = responsive::policy_size(tiny_ok, kind);
        assert_eq!((width, height), (20, 6), "{kind:?} at 20x6");
    }
    let spec = responsive::DialogSpec::new(
        PresentationKind::Contextual(ContextFootprint::Prompt),
        0,
        1,
        1,
        0,
        1,
    );
    let geometry = responsive::resolve_dialog(tiny_ok, &spec, 3, &["Apply"], Some(0), None)
        .expect("20x6 keeps one body row and its default");
    assert!(
        geometry.body.viewport.height >= 1,
        "body survives: {:?}",
        geometry.body.viewport
    );
    assert!(
        geometry.actions.band.height >= 1,
        "actions survive: {:?}",
        geometry.actions.band
    );
    assert!(
        geometry.actions.visible_indices().contains(&0),
        "default survives at 20x6: {:?}",
        geometry.actions
    );
    // Below the floor the tiny fallback owns the frame: explicit refusal.
    let below = responsive_viewport(19, 5);
    assert!(responsive::is_tiny(below));
    assert_eq!(
        responsive::resolve_dialog(below, &spec, 3, &["Apply"], Some(0), None),
        Err(responsive::GeometryError::TooSmall)
    );
    assert_eq!(
        responsive::resolve_dialog(
            responsive_viewport(19, 30),
            &spec,
            0,
            &["Apply"],
            Some(0),
            None
        ),
        Err(responsive::GeometryError::TooSmall)
    );
    assert_eq!(
        responsive::resolve_dialog(
            responsive_viewport(80, 5),
            &spec,
            0,
            &["Apply"],
            Some(0),
            None
        ),
        Err(responsive::GeometryError::TooSmall)
    );
}

#[test]
fn responsive_regions_stay_ordered_disjoint_with_a_fill_body() {
    use responsive::PresentationKind;
    let viewport = responsive_viewport(100, 30);
    let spec = responsive::DialogSpec::new(PresentationKind::LongContent, 1, 2, 1, 1, 1);
    let geometry =
        responsive::resolve_dialog(viewport, &spec, 40, &["Apply", "Clear"], Some(0), None)
            .expect("resolve");
    assert_eq!(
        geometry.interior,
        geometry.frame.inner(ratatui::layout::Margin::new(1, 1))
    );
    // Content keeps the shared side padding (border + pad).
    assert!(
        geometry.content.x > geometry.interior.x
            && geometry.content.right() < geometry.interior.right()
    );
    let ordered = [
        geometry.header,
        geometry.body.viewport,
        geometry.message,
        geometry.help,
        geometry.actions.band,
    ];
    let mut bottom = geometry.content.y;
    for rect in ordered {
        if rect.height == 0 {
            continue;
        }
        assert!(
            rect.y >= bottom,
            "regions overlap or run backwards: {rect:?} after {bottom}"
        );
        assert!(
            rect.bottom() <= geometry.content.bottom(),
            "{rect:?} leaves the content {:?}",
            geometry.content
        );
        bottom = rect.bottom();
    }
    assert!(
        geometry.body.viewport.height >= 1,
        "Fill body never collapses"
    );
    // Body owns the surplus: chrome plus body exactly fills content height
    // when every band is present (spacing 0 in this compact-checked path may
    // vary, so assert containment rather than exact fill arithmetic here; the
    // Fill ownership is asserted by growth below).
    let chrome = geometry.header.height
        + geometry.message.height
        + geometry.help.height
        + geometry.actions.band.height;
    assert!(
        geometry.body.viewport.height + chrome <= geometry.content.height,
        "body + chrome {:?} exceeds content {:?}",
        (geometry.body.viewport.height, chrome),
        geometry.content
    );
    // A larger minimum takes more body rows from the same frame only via the
    // spec, never via live counts (see the stability test).
    let roomier = responsive::DialogSpec::new(PresentationKind::LongContent, 1, 5, 1, 1, 1);
    let other = responsive::resolve_dialog(viewport, &roomier, 40, &["Apply"], Some(0), None)
        .expect("resolve");
    assert_eq!(
        geometry.frame, other.frame,
        "outer size is policy, not body minimum"
    );
}

#[test]
fn responsive_outer_frame_ignores_async_item_counts() {
    use responsive::PresentationKind;
    let viewport = responsive_viewport(140, 40);
    let spec = responsive::DialogSpec::new(PresentationKind::LongContent, 1, 1, 1, 1, 1);
    let labels = ["Apply", "Clear"];
    // Pending (0 rows), empty, populated and error/transcript states share one
    // stable budget: only the scroll extent moves.
    let empty =
        responsive::resolve_dialog(viewport, &spec, 0, &labels, Some(0), None).expect("empty");
    let populated =
        responsive::resolve_dialog(viewport, &spec, 87, &labels, Some(0), None).expect("populated");
    let transcript = responsive::resolve_dialog(viewport, &spec, 1200, &labels, Some(0), None)
        .expect("transcript");
    assert_eq!(empty.frame, populated.frame);
    assert_eq!(populated.frame, transcript.frame);
    assert_eq!(
        empty.message.y, populated.message.y,
        "sticky tail origin moves with async state"
    );
    assert_eq!(empty.actions.band.y, transcript.actions.band.y);
    assert_eq!(empty.body.overflow(), 0);
    assert!(
        populated.body.overflow() > 0 && transcript.body.overflow() > populated.body.overflow()
    );
    assert_eq!(
        empty.body.viewport, populated.body.viewport,
        "viewport is policy, extent is content"
    );
}

#[test]
fn responsive_no_ordinary_kind_becomes_full_frame() {
    use responsive::{ContextFootprint, PresentationKind};
    for (width, height) in [
        (240u16, 80u16),
        (140, 40),
        (100, 30),
        (80, 24),
        (54, 16),
        (20, 6),
    ] {
        let viewport = responsive_viewport(width, height);
        for kind in [
            PresentationKind::Contextual(ContextFootprint::Prompt),
            PresentationKind::Contextual(ContextFootprint::Inspector),
            PresentationKind::SelfContainedForm,
            PresentationKind::LongContent,
            PresentationKind::Palette,
        ] {
            assert!(kind.is_ordinary(), "{kind:?} must read as ordinary");
            let (w, h) = responsive::policy_size(viewport, kind);
            // At 20x6 the safety floor legitimately is the full frame; that is
            // the tiny guardrail, not an ordinary full-frame workspace.
            if (width, height) != (20, 6) {
                assert!(
                    w < width || h < height,
                    "{kind:?} at {width}x{height} became full frame ({w}x{h})"
                );
            }
        }
    }
    assert!(!PresentationKind::FullFrame.is_ordinary());
}

#[test]
fn responsive_action_default_survives_and_overflow_keeps_indices() {
    // One row band, narrow: trailing verbs move into More ▾.
    let band = Rect::new(0, 0, 40, 1);
    let labels = [
        "Apply",
        "Clear",
        "Edit note",
        "Raw context",
        "Remove",
        "Help",
    ];
    assert_eq!(responsive_actions::stable_action_rows(100, &["Apply"]), 1);
    assert_eq!(responsive_actions::stable_action_rows(40, &labels), 2);
    let plan = responsive_actions::plan_actions(band, &labels, Some(0), None);
    assert!(plan.needs_more(), "narrow band must overflow: {plan:?}");
    assert!(
        plan.visible_indices().contains(&0),
        "default survives: {plan:?}"
    );
    assert!(plan.more.is_some());
    let mut round_trip = plan.visible_indices();
    round_trip.extend(plan.overflow.iter().copied());
    round_trip.sort_unstable();
    assert_eq!(
        round_trip,
        (0..labels.len()).collect::<Vec<_>>(),
        "indices preserved: {plan:?}"
    );
    // Overflow stays in original order for the anchored More ▾ menu.
    let mut sorted = plan.overflow.clone();
    sorted.sort_unstable();
    assert_eq!(plan.overflow, sorted);
    // Roomy band fits everything with no More ▾.
    let roomy = Rect::new(0, 0, 100, 2);
    let fits = responsive_actions::plan_actions(roomy, &labels, Some(0), None);
    assert!(!fits.needs_more());
    assert!(fits.more.is_none());
    assert_eq!(
        fits.visible_indices(),
        (0..labels.len()).collect::<Vec<_>>()
    );
    // Two-row band shows the default first and More ▾ last.
    let two = Rect::new(0, 0, 30, 2);
    let split = responsive_actions::plan_actions(two, &labels, Some(0), None);
    assert!(split.visible_indices().contains(&0));
    assert!(split.more.is_some());
}

#[test]
fn responsive_list_projection_reveals_the_selected_row() {
    // Heading + count + viewport + scrollbar + row rects from one call.
    let area = Rect::new(10, 5, 50, 8);
    let full = responsive::plan_list(area, 8, 30, Some(0), 0);
    assert_eq!(full.heading.height, 1);
    assert_eq!(full.count.right(), area.right(), "count right-aligned");
    assert!(full.scrollbar.is_some(), "overflow needs a bar");
    assert_eq!(full.first_row, 0);
    assert!(!full.row_rects.is_empty());
    for window in full.row_rects.windows(2) {
        assert_eq!(window[1].y, window[0].y + 1, "rows stack: {window:?}");
        assert_eq!(window[1].x, window[0].x);
    }
    for rect in &full.row_rects {
        assert!(rect.y >= full.viewport.y && rect.bottom() <= full.viewport.bottom());
    }
    // Selecting the last row scrolls it into view.
    let scrolled = responsive::plan_list(area, 8, 30, Some(29), 0);
    assert!(scrolled.first_row + scrolled.row_rects.len() > 29 - scrolled.row_rects.len());
    assert!(
        scrolled.visible_range().contains(&29),
        "selected row revealed: {:?}",
        scrolled.visible_range()
    );
    // A fitting list reserves no scrollbar column.
    let fits = responsive::plan_list(area, 8, 3, Some(0), 0);
    assert!(fits.scrollbar.is_none());
    // Pure reveal helper keeps the window when it already contains the row.
    assert_eq!(responsive::reveal_selection(30, 7, 3, 0), 0);
    assert_eq!(responsive::reveal_selection(30, 7, 29, 0), 23);
}

#[test]
fn responsive_inspector_avoids_the_frozen_row() {
    use responsive::{ContextAnchor, ContextFootprint, PresentationKind};
    let viewport = responsive_viewport(100, 30);
    let spec = responsive::DialogSpec::new(
        PresentationKind::Contextual(ContextFootprint::Inspector),
        1,
        1,
        1,
        1,
        1,
    );
    // Anchor near the top: the larger band is below, with a one-row gap.
    let top_anchor = ContextAnchor::new(Rect::new(10, 5, 50, 1), Rect::new(0, 0, 100, 30));
    let below =
        responsive::resolve_dialog(viewport, &spec, 10, &["Apply"], Some(0), Some(top_anchor))
            .expect("resolve");
    assert!(
        below.frame.y > top_anchor.row.bottom(),
        "inspector must clear the frozen row: frame {:?} anchor {:?}",
        below.frame,
        top_anchor.row
    );
    // Anchor near the bottom: the larger band is above.
    let bottom_anchor = ContextAnchor::new(Rect::new(10, 24, 50, 1), Rect::new(0, 0, 100, 30));
    let above = responsive::resolve_dialog(
        viewport,
        &spec,
        10,
        &["Apply"],
        Some(0),
        Some(bottom_anchor),
    )
    .expect("resolve");
    assert!(
        above.frame.bottom() < bottom_anchor.row.y,
        "inspector must sit above the frozen row: frame {:?} anchor {:?}",
        above.frame,
        bottom_anchor.row
    );
    // No anchor behaves like a centered form (no panic, inside viewport).
    let plain = responsive::resolve_dialog(viewport, &spec, 10, &["Apply"], Some(0), None)
        .expect("resolve");
    assert!(plain.frame.right() <= viewport.right() && plain.frame.bottom() <= viewport.bottom());
}

#[test]
fn responsive_anchored_prefers_below_then_clamps_and_scrolls() {
    let area = responsive_viewport(100, 30);
    // Room below: one blank row between the field and the popup.
    let field = Rect::new(10, 5, 20, 1);
    let spec = responsive::AnchoredSpec::new(10, None, 24, 0);
    let below = responsive::anchored_geometry(area, field, &spec, 0, 0);
    assert!(below.placed_below);
    assert_eq!(below.popup.y, field.bottom() + 1);
    assert!(below.popup.right() <= area.right() && below.popup.bottom() <= area.bottom());
    // No room below: above the field, one blank row between popup and field.
    let low_field = Rect::new(10, 26, 20, 1);
    let above = responsive::anchored_geometry(area, low_field, &spec, 0, 0);
    assert!(!above.placed_below);
    assert_eq!(above.popup.bottom() + 1, low_field.y);
    // Right edge clamps inside the frame.
    let edge_field = Rect::new(90, 5, 8, 1);
    let edge = responsive::anchored_geometry(area, edge_field, &spec, 0, 0);
    assert!(edge.popup.right() <= area.right());
    assert!(
        edge.popup.x < edge_field.x,
        "clamped left: {:?}",
        edge.popup
    );
    // Reserved rows are stable: 2 live items and 20 share one frame height.
    let few = responsive::AnchoredSpec::new(2, Some(8), 24, 0);
    let many = responsive::AnchoredSpec::new(20, Some(8), 24, 0);
    let few_geometry = responsive::anchored_geometry(area, field, &few, 0, 0);
    let many_geometry = responsive::anchored_geometry(area, field, &many, 0, 0);
    assert_eq!(few_geometry.popup.height, many_geometry.popup.height);
    assert_eq!(few_geometry.popup.y, many_geometry.popup.y);
    assert!(many_geometry.scrollbar.is_some());
    // Footer occupies its own band below the viewport.
    let with_footer = responsive::AnchoredSpec::new(10, None, 24, 1);
    let footed = responsive::anchored_geometry(area, field, &with_footer, 0, 0);
    assert_eq!(footed.footer.height, 1);
    assert_eq!(footed.footer.y, footed.viewport.bottom());
}

#[test]
fn responsive_anchored_and_lists_measure_display_width() {
    // Wide glyphs count two cells; combining marks count zero and never start
    // a clipped viewport alone.
    assert_eq!(responsive::clip_display_width("東京", 4), "東京");
    assert_eq!(responsive::clip_display_width("東京x", 4), "東京");
    let combined = "e\u{301}cole";
    assert_eq!(responsive::clip_display_width(combined, 4), "e\u{301}col");
    // Truncation shows its work with a trailing ellipsis inside the budget.
    assert_eq!(
        responsive::truncate_cell("ed4a0c76-rows.idx", 10),
        "ed4a0c76-\u{2026}"
    );
    assert_eq!(responsive::truncate_cell("東京界警告", 5), "東京\u{2026}");
    // A wide option widens the popup by display columns, not characters.
    let area = responsive_viewport(100, 30);
    let field = Rect::new(5, 5, 10, 1);
    let narrow = responsive::AnchoredSpec::new(3, None, 14, 0);
    let wide = responsive::AnchoredSpec::new(3, None, 30, 0);
    let narrow_geometry = responsive::anchored_geometry(area, field, &narrow, 0, 0);
    let wide_geometry = responsive::anchored_geometry(area, field, &wide, 0, 0);
    assert!(wide_geometry.popup.width > narrow_geometry.popup.width);
    // Minimum width and frame bounding hold at the edges.
    let tiny_spec = responsive::AnchoredSpec::new(3, None, 4, 0);
    let tiny = responsive::anchored_geometry(area, field, &tiny_spec, 0, 0);
    assert!(tiny.popup.width >= 12);
    let huge = responsive::AnchoredSpec::new(3, None, 200, 0);
    let huge_geometry = responsive::anchored_geometry(area, field, &huge, 0, 0);
    assert!(huge_geometry.popup.right() <= area.right());
}

// --- Pre-review successor acceptance (phase A contract corrections) ---
//
// Discriminating tests for the five hypotheses confirmed against b892d19,
// at the 240/80/54/20 viewport boundaries. Each asserts the corrected
// contract; compatibility callsites (`dialog_rect`, `anchored_rect`,
// `button_layout`, 4-arg `plan_actions`) are unchanged.

#[test]
fn responsive_inspector_centers_over_the_log_pane() {
    use responsive::{ContextAnchor, ContextFootprint, PresentationKind};
    let spec = responsive::DialogSpec::new(
        PresentationKind::Contextual(ContextFootprint::Inspector),
        1,
        1,
        1,
        1,
        1,
    );
    // 240x80 with a 22-column sidebar: viewport center x=48, log center x=59.
    let viewport = responsive_viewport(240, 80);
    let log = Rect::new(22, 0, 218, 80);
    let anchor = ContextAnchor::new(Rect::new(30, 10, 40, 1), log);
    let geometry =
        responsive::resolve_dialog(viewport, &spec, 10, &["Apply"], Some(0), Some(anchor))
            .expect("resolve");
    assert_eq!(geometry.frame.width, 144);
    assert_eq!(
        geometry.frame.x, 59,
        "center over the log, not the viewport: {:?}",
        geometry.frame
    );
    assert!(geometry.frame.right() <= viewport.right());
    // An empty log falls back to viewport centering.
    let no_log = ContextAnchor::new(Rect::new(30, 10, 40, 1), Rect::default());
    let plain = responsive::resolve_dialog(viewport, &spec, 10, &["Apply"], Some(0), Some(no_log))
        .expect("resolve");
    assert_eq!(plain.frame.x, 48);
    // 80x24: popup (66) wider than the log (58) clamps inside the viewport
    // instead of escaping it, and still does not use the viewport center.
    let viewport = responsive_viewport(80, 24);
    let log = Rect::new(22, 0, 58, 24);
    let anchor = ContextAnchor::new(Rect::new(30, 4, 20, 1), log);
    let geometry =
        responsive::resolve_dialog(viewport, &spec, 6, &["Apply"], Some(0), Some(anchor))
            .expect("resolve");
    assert_eq!(geometry.frame.width, 66);
    assert_eq!(
        geometry.frame.x, 14,
        "log-centered then clamped: {:?}",
        geometry.frame
    );
    assert_ne!(
        geometry.frame.x, 7,
        "must not use viewport center (80-66)/2"
    );
    // 54x16 compact hides the sidebar: log == viewport, so both centers agree;
    // the preferred 52x14 fits neither band around row 3 (rooms 11/2), so the
    // frame shrinks into the larger below band with the gap preserved.
    let viewport = responsive_viewport(54, 16);
    let log = responsive_viewport(54, 16);
    let anchor = ContextAnchor::new(Rect::new(5, 3, 20, 1), log);
    let geometry =
        responsive::resolve_dialog(viewport, &spec, 4, &["Apply"], Some(0), Some(anchor))
            .expect("resolve");
    assert_eq!(geometry.frame, Rect::new(1, 5, 52, 11));
    // 20x6 safety floor is full-frame by construction.
    let viewport = responsive_viewport(20, 6);
    let anchor = ContextAnchor::new(Rect::new(2, 2, 10, 1), viewport);
    let geometry =
        responsive::resolve_dialog(viewport, &spec, 2, &["Apply"], Some(0), Some(anchor))
            .expect("20x6 resolves");
    assert_eq!(geometry.frame, viewport);
}

#[test]
fn responsive_inspector_shrinks_into_the_larger_band() {
    use responsive::{ContextAnchor, ContextFootprint, PresentationKind};
    let spec = responsive::DialogSpec::new(
        PresentationKind::Contextual(ContextFootprint::Inspector),
        1,
        1,
        1,
        1,
        1,
    );
    // 240x80 preferred (144x36) fits below a top anchor: no shrink.
    let viewport = responsive_viewport(240, 80);
    let log = Rect::new(22, 0, 218, 80);
    let anchor = ContextAnchor::new(Rect::new(30, 10, 40, 1), log);
    let geometry =
        responsive::resolve_dialog(viewport, &spec, 10, &["Apply"], Some(0), Some(anchor))
            .expect("resolve");
    assert_eq!((geometry.frame.width, geometry.frame.height), (144, 36));
    assert_eq!(geometry.frame.y, 12, "one-row gap below the frozen row");
    // 80x24 preferred (66x15) fits neither band around a middle row (rooms
    // 10/11 < 15): shrink into the larger above band (minimum 6).
    let viewport = responsive_viewport(80, 24);
    let log = Rect::new(22, 0, 58, 24);
    let anchor = ContextAnchor::new(Rect::new(30, 12, 20, 1), log);
    let geometry =
        responsive::resolve_dialog(viewport, &spec, 10, &["Apply"], Some(0), Some(anchor))
            .expect("resolve");
    assert_eq!(
        geometry.frame.height, 11,
        "shrunk to the above band: {:?}",
        geometry.frame
    );
    assert_eq!(
        geometry.frame.bottom() + 1,
        12,
        "gap preserved above: {:?}",
        geometry.frame
    );
    assert!(geometry.frame.bottom() < anchor.row.y);
    // 54x16 compact (52x14) around row 8 (rooms 6/7 < 14): shrink above to 7.
    let viewport = responsive_viewport(54, 16);
    let anchor = ContextAnchor::new(Rect::new(5, 8, 20, 1), viewport);
    let geometry =
        responsive::resolve_dialog(viewport, &spec, 6, &["Apply"], Some(0), Some(anchor))
            .expect("resolve");
    assert_eq!((geometry.frame.width, geometry.frame.height), (52, 7));
    assert_eq!(geometry.frame.bottom() + 1, 8);
    // 20x6 safety floor cannot shrink below the minimum: full frame owns it.
    let viewport = responsive_viewport(20, 6);
    let anchor = ContextAnchor::new(Rect::new(2, 2, 10, 1), viewport);
    let geometry =
        responsive::resolve_dialog(viewport, &spec, 2, &["Apply"], Some(0), Some(anchor))
            .expect("20x6 resolves");
    assert_eq!(geometry.frame, viewport);
}

#[test]
fn responsive_anchored_keeps_one_blank_row_at_boundaries() {
    // 240x80 roomy: gap below.
    let area = responsive_viewport(240, 80);
    let field = Rect::new(20, 10, 20, 1);
    let spec = responsive::AnchoredSpec::new(8, None, 24, 0);
    let below = responsive::anchored_geometry(area, field, &spec, 0, 0);
    assert!(below.placed_below);
    assert_eq!(
        below.popup.y,
        field.bottom() + 1,
        "one blank row: {:?}",
        below.popup
    );
    assert_eq!(below.popup.height, 10);
    // 80x24 roomy: same gap.
    let area = responsive_viewport(80, 24);
    let field = Rect::new(10, 5, 20, 1);
    let below = responsive::anchored_geometry(area, field, &spec, 0, 0);
    assert_eq!(below.popup.y, field.bottom() + 1);
    // Above placement keeps the gap on the other side.
    let low = Rect::new(10, 20, 20, 1);
    let above = responsive::anchored_geometry(area, low, &spec, 0, 0);
    assert!(!above.placed_below);
    assert_eq!(above.popup.bottom() + 1, low.y);
    // 54x16 tight: desired 10 fits neither band (rooms 9/4), shrink below to 9
    // with the gap preserved.
    let area = responsive_viewport(54, 16);
    let field = Rect::new(5, 5, 10, 1);
    let tight = responsive::anchored_geometry(area, field, &spec, 0, 0);
    assert!(tight.placed_below);
    assert_eq!(tight.popup.y, field.bottom() + 1);
    assert_eq!(
        tight.popup.height, 9,
        "shrunk to the below band: {:?}",
        tight.popup
    );
    assert!(tight.popup.bottom() <= area.bottom());
    // 20x6 floor: small count keeps the gap inside the frame.
    let area = responsive_viewport(20, 6);
    let field = Rect::new(2, 1, 10, 1);
    let small = responsive::AnchoredSpec::new(3, None, 14, 0);
    let popup = responsive::anchored_geometry(area, field, &small, 0, 0);
    assert_eq!(popup.popup.y, field.bottom() + 1);
    assert!(popup.popup.right() <= area.right() && popup.popup.bottom() <= area.bottom());
}

#[test]
fn responsive_project_row_rejects_out_of_range_rows() {
    let viewport = Rect::new(0, 0, 20, 5);
    let body = responsive::ScrollViewport::new(viewport, 3, 0);
    assert_eq!(body.project_row(0), Some(Rect::new(0, 0, 20, 1)));
    assert_eq!(body.project_row(2), Some(Rect::new(0, 2, 20, 1)));
    assert_eq!(
        body.project_row(3),
        None,
        "index == content_rows paints nothing"
    );
    assert_eq!(body.project_row(4), None);
    assert_eq!(body.project_row(100), None);
    assert_eq!(
        body.project_row(usize::MAX),
        None,
        "usize edge paints nothing"
    );
    let empty = responsive::ScrollViewport::new(viewport, 0, 0);
    assert_eq!(empty.project_row(0), None);
    // Windowed content still bounds both ends.
    let windowed = responsive::ScrollViewport::new(viewport, 10, 5);
    assert_eq!(windowed.project_row(4), None, "before the window");
    assert!(windowed.project_row(5).is_some());
    assert_eq!(windowed.project_row(10), None, "past the content end");
}

#[test]
fn responsive_action_roles_validate_and_map_focus() {
    let labels = [
        "Apply",
        "Clear",
        "Edit note",
        "Raw context",
        "Remove",
        "Help",
    ];
    // A default naming a destructive index is refused: destructive is never
    // the default, matching ActionRow::role priority.
    let refused = responsive_actions::plan_actions_with_roles(
        Rect::new(0, 0, 100, 2),
        &labels,
        Some(1),
        &[1],
        None,
    );
    assert_eq!(refused.default, None);
    assert_eq!(refused.destructive, vec![1]);
    assert!(!refused.visible_indices().contains(&1) || refused.default != Some(1));
    // A non-destructive default survives with its role recorded.
    let kept = responsive_actions::plan_actions_with_roles(
        Rect::new(0, 0, 100, 2),
        &labels,
        Some(0),
        &[4],
        None,
    );
    assert_eq!(kept.default, Some(0));
    assert_eq!(kept.destructive, vec![4]);
    // Focused overflow maps onto More ▾; visible focus does not.
    let band = Rect::new(0, 0, 40, 1);
    let plan = responsive_actions::plan_actions(band, &labels, Some(0), None);
    assert!(plan.needs_more());
    let hidden = plan.overflow[0];
    let via_more = responsive_actions::plan_actions(band, &labels, Some(0), Some(hidden));
    assert!(
        via_more.more_is_focused(),
        "hidden focus owns More ▾: {via_more:?}"
    );
    assert!(!plan.more_is_focused(), "no focus recorded: {plan:?}");
    let shown = plan.visible_indices()[0];
    let direct = responsive_actions::plan_actions(band, &labels, Some(0), Some(shown));
    assert!(
        !direct.more_is_focused(),
        "visible focus stays on its button"
    );
    // Unreachable overflow is explicit: a trailing default in a single narrow
    // row holds only itself with no room for More ▾, so hidden rows have no
    // hitbox. A leading default squeezes prefix + More ▾ into the row instead.
    let trailing = responsive_actions::plan_actions(Rect::new(0, 0, 16, 1), &labels, Some(5), None);
    assert!(!trailing.overflow.is_empty());
    assert!(
        trailing.more.is_none(),
        "single narrow row holds only the default"
    );
    assert!(trailing.unreachable_overflow());
    assert!(trailing.visible_indices().contains(&5));
    let leading = responsive_actions::plan_actions(Rect::new(0, 0, 16, 1), &labels, Some(0), None);
    assert!(leading.more.is_some());
    assert!(!leading.unreachable_overflow());
    let roomy = responsive_actions::plan_actions(Rect::new(0, 0, 16, 2), &labels, Some(5), None);
    assert!(roomy.more.is_some());
    assert!(!roomy.unreachable_overflow());
}

#[test]
fn responsive_action_budgets_and_resolve_fallback_at_boundaries() {
    let labels = [
        "Apply",
        "Clear",
        "Edit note",
        "Raw context",
        "Remove",
        "Help",
    ];
    // Stable budgets at the boundary content widths (frame minus side pads).
    assert_eq!(responsive_actions::stable_action_rows(156, &labels), 1);
    assert_eq!(responsive_actions::stable_action_rows(68, &labels), 2);
    assert_eq!(responsive_actions::stable_action_rows(48, &labels), 2);
    assert_eq!(responsive_actions::stable_action_rows(16, &labels), 2);
    // 20x6 with a one-row budget and a trailing default is unreachable: the
    // single narrow row holds only the default with no room for More ▾, so the
    // dialog fallback owns the frame and the caller grows the budget instead
    // of drawing a dead band. A leading default squeezes into one row.
    use responsive::PresentationKind;
    let viewport = responsive_viewport(20, 6);
    let one_row = responsive::DialogSpec::new(PresentationKind::LongContent, 0, 1, 1, 0, 1);
    assert_eq!(
        responsive::resolve_dialog(viewport, &one_row, 4, &labels, Some(5), None),
        Err(responsive::GeometryError::TooSmall)
    );
    let two_rows = responsive::DialogSpec::new(PresentationKind::LongContent, 0, 1, 1, 0, 2);
    let geometry = responsive::resolve_dialog(viewport, &two_rows, 4, &labels, Some(5), None)
        .expect("two-row budget reaches More ▾ at 20x6");
    assert!(geometry.actions.more.is_some());
    assert!(!geometry.actions.unreachable_overflow());
    assert!(geometry.actions.visible_indices().contains(&5));
}

// --- Phase B shared-core: frozen context anchor lifecycle ---
//
// Capture comes from the same last-rendered base geometry/hit regions when the
// first layer opens, is retained across async frames, child push and Replace,
// never chases live row movement, and clears when the stack returns to base
// (and on resize). An open with no selected/painted row carries the log rect
// with an empty row. Exposed read-only through `RenderCtx::context_anchor`
// for later Contextual resolve calls; no new row rectangle and no widened
// global hit regions.

fn base_geometry(app: &App) -> (Option<Rect>, Vec<(Rect, usize)>) {
    (
        app.hit_regions.log_rows.or(app.hit_regions.log),
        app.hit_regions.log_row_indices.clone(),
    )
}

fn selected_index(provider: &FixtureProvider, app: &App) -> Option<usize> {
    let view_id = app.active_view_id()?;
    let selected = app.view_state()?.selected.clone()?;
    provider.index_of_id(view_id, &selected)
}

#[test]
fn anchor_captures_the_selected_row_and_log_from_one_base_frame() {
    let (provider, mut app) = demo();
    let theme = Theme::TERMINAL;
    // Base frame first so hit regions exist; select the top row so a painted
    // selected rect exists.
    draw(&provider, &mut app, 100, 30, theme);
    app.handle(Action::Top, &provider);
    draw(&provider, &mut app, 100, 30, theme);
    let (log_before, rows_before) = base_geometry(&app);
    let log_before = log_before.expect("base log viewport");
    let selected = selected_index(&provider, &app).expect("selected row");
    let row_before = rows_before
        .iter()
        .find(|(_, index)| *index == selected)
        .map(|(rect, _)| *rect)
        .expect("selected row is painted");
    app.handle(Action::Open(Open::Search), &provider);
    let anchor = app
        .shell
        .context_anchor
        .expect("anchor captured on first open");
    assert_eq!(
        anchor.log, log_before,
        "log viewport must come from the base frame"
    );
    assert_eq!(
        anchor.row, row_before,
        "row must come from the same base hit regions"
    );
    // Retained across async frames (another base+layer render, no new capture).
    draw(&provider, &mut app, 100, 30, theme);
    assert_eq!(
        app.shell.context_anchor,
        Some(anchor),
        "async frames must not recapture"
    );
}

#[test]
fn anchor_falls_back_to_log_with_an_empty_row_when_nothing_is_painted() {
    // Deterministic negative control: the demo "all" view holds 16 rows while
    // a 100x20 log viewport paints only 15, so moving the selection to the
    // last row *without re-rendering* leaves a live selection whose index is
    // absent from the last-rendered hit regions. Capture must reuse those
    // painted regions (empty row) rather than recomputing a fresh rect from
    // the provider (which would find the last row).
    let (provider, mut app) = demo();
    let theme = Theme::TERMINAL;
    draw(&provider, &mut app, 100, 20, theme);
    app.handle(Action::Top, &provider);
    draw(&provider, &mut app, 100, 20, theme);
    let (log_before, rows_before) = base_geometry(&app);
    let log_before = log_before.expect("base log viewport");
    assert!(
        rows_before.len() < app.view_state().map(|state| state.last_total).unwrap_or(0),
        "need more rows than the viewport paints for a real negative control: painted {}, total {}",
        rows_before.len(),
        app.view_state().map(|state| state.last_total).unwrap_or(0),
    );
    // Move the live selection out of the painted page; do NOT render again so
    // the hit regions stay stale by construction.
    app.handle(Action::End, &provider);
    let selected = selected_index(&provider, &app).expect("live selection exists");
    assert!(
        !rows_before.iter().any(|(_, index)| *index == selected),
        "selected index {selected} must be absent from the last-rendered rows {:?}",
        rows_before.iter().map(|(_, i)| *i).collect::<Vec<_>>(),
    );
    // Open the first layer from this state: no painted row matches, so the
    // anchor carries the exact last-rendered log viewport with an empty row.
    app.handle(Action::Open(Open::Help), &provider);
    let anchor = app.shell.context_anchor.expect("anchor captured");
    assert_eq!(
        anchor.log, log_before,
        "log viewport must be the exact last-rendered one"
    );
    assert!(
        anchor.row.is_empty(),
        "unpainted selection must give an empty row, got {:?}",
        anchor.row
    );
}

#[test]
fn anchor_does_not_chase_live_row_movement_and_survives_child_and_replace() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use lvu::component::RawEvent;
    let (provider, mut app) = demo();
    let theme = Theme::TERMINAL;
    draw(&provider, &mut app, 100, 30, theme);
    app.handle(Action::Top, &provider);
    draw(&provider, &mut app, 100, 30, theme);
    app.handle(Action::Open(Open::Search), &provider);
    let held = app.shell.context_anchor.expect("anchor captured");
    // Live movement: move the base selection while the layer is up. The held
    // anchor must not follow it.
    app.handle(Action::End, &provider);
    draw(&provider, &mut app, 100, 30, theme);
    assert_eq!(
        app.shell.context_anchor,
        Some(held),
        "live selection movement must not move the held anchor"
    );
    // Child push retains.
    app.handle(Action::Open(Open::Time), &provider);
    assert_eq!(app.layers.stack.len(), 2, "child pushed");
    assert_eq!(
        app.shell.context_anchor,
        Some(held),
        "child push must retain"
    );
    // Popping the child retains.
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ))),
        &provider,
    );
    assert_eq!(app.layers.stack.len(), 1);
    assert_eq!(
        app.shell.context_anchor,
        Some(held),
        "child pop must retain"
    );
    // Replace retains: ViewSummary Enter replaces with its owning dialog.
    // Stack is [Search] here; pushing ViewSummary makes [Search, ViewSummary],
    // and Replace pops ViewSummary then pushes its owner, staying at two.
    app.handle(Action::Open(Open::ViewSummary), &provider);
    assert_eq!(app.layers.stack.len(), 2);
    let before_replace = app.shell.context_anchor.expect("anchor held");
    assert_eq!(before_replace, held, "second push retains");
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))),
        &provider,
    );
    assert_eq!(app.layers.stack.len(), 2, "Replace keeps the depth");
    assert_eq!(
        app.shell.context_anchor,
        Some(held),
        "Replace must retain the frozen row"
    );
    // Final pop clears.
    for _ in 0..4 {
        app.handle(
            Action::Raw(RawEvent::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            ))),
            &provider,
        );
        if app.layers.stack.is_empty() {
            break;
        }
    }
    assert!(app.layers.stack.is_empty(), "stack returned to base");
    assert_eq!(app.shell.context_anchor, None, "final pop must clear");
}

#[test]
fn anchor_invalidates_on_resize_and_captures_compact_logs() {
    let (provider, mut app) = demo();
    let theme = Theme::TERMINAL;
    draw(&provider, &mut app, 100, 30, theme);
    app.handle(Action::Top, &provider);
    draw(&provider, &mut app, 100, 30, theme);
    app.handle(Action::Open(Open::Search), &provider);
    assert!(app.shell.context_anchor.is_some());
    // Resize invalidates: coordinates from the old viewport misname rows.
    app.handle(Action::Resize(54, 16), &provider);
    assert_eq!(
        app.shell.context_anchor, None,
        "resize must invalidate the frozen anchor"
    );
    // Rendering at a new size without a resize action invalidates too
    // (TestBackend draws change size directly).
    draw(&provider, &mut app, 100, 30, theme);
    app.handle(Action::Open(Open::Time), &provider);
    // Stack is non-empty (Search still up? Actually Search was popped? No:
    // Search still up (we never closed it; Resize cleared anchor but not the
    // stack). Opening Time is a child push retaining None (no recapture while
    // up). Close everything, reopen compact for a fresh capture.
    for _ in 0..4 {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use lvu::component::RawEvent;
        app.handle(
            Action::Raw(RawEvent::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            ))),
            &provider,
        );
        if app.layers.stack.is_empty() {
            break;
        }
    }
    assert!(app.layers.stack.is_empty());
    draw(&provider, &mut app, 54, 16, theme);
    app.handle(Action::Top, &provider);
    draw(&provider, &mut app, 54, 16, theme);
    let log_compact = app
        .hit_regions
        .log_rows
        .or(app.hit_regions.log)
        .expect("compact log viewport");
    // Base at 54x16 still shows the sidebar (no modal yet), so the log is the
    // sidebar-narrowed viewport. The frozen anchor carries exactly that base
    // geometry; after the dialog opens the compact backdrop (§5.5) takes the
    // full width, but the anchor must not chase it.
    app.handle(Action::Open(Open::Search), &provider);
    let anchor = app.shell.context_anchor.expect("compact capture");
    assert_eq!(
        anchor.log, log_compact,
        "compact capture carries its base log"
    );
    draw(&provider, &mut app, 54, 16, theme);
    let log_behind = app
        .hit_regions
        .log_rows
        .or(app.hit_regions.log)
        .expect("log behind the compact dialog");
    assert!(
        log_behind.width >= log_compact.width,
        "compact backdrop gives the log at least its base width: base {log_compact:?}, behind {log_behind:?}"
    );
    assert_eq!(
        app.shell.context_anchor,
        Some(anchor),
        "async frames must not recapture the compact anchor"
    );
}
