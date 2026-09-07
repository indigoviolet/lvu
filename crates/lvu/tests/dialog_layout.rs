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
            Action::Open(Open::Ask(AskOpen::Task(AskTask::RecognizeTimestamp))),
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
                    Action::Open(Open::Ask(AskOpen::Task(AskTask::RecognizeTimestamp))),
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
                    assert!(text.contains("Recognize timestamp"), "{text}");
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
    use lvu::app::SourceAiPreview;

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
                name: "reviewed source".into(),
                kind: "command".into(),
                launch: "journalctl --follow --unit api.service".into(),
                effective_path_or_cwd: "/srv/controlled application".into(),
                restart: "on-failure with bounded delay".into(),
                environment: (0..10).map(|index| format!("KEY_{index}=value")).collect(),
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

        // Both dropdown values sit at the same column as each other.
        let basis = rendered
            .lines()
            .find(|line| line.contains("Time basis"))
            .expect("basis row");
        let window = rendered
            .lines()
            .find(|line| line.contains("Window"))
            .expect("window row");
        assert_eq!(
            basis.find("Capture"),
            window.find("All time"),
            "dropdown values share the field column at {width}x{height}"
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
            DialogClass::XL,
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

/// §12.12: Raw context keeps its use of space, gains a scrollbar and a button,
/// and never loses the fact that it is unfiltered — even at 54x16.
#[test]
fn raw_context_states_that_it_is_unfiltered_at_every_size() {
    for (width, height) in SIZES {
        let (provider, mut app) = demo();
        app.sync_provider(&provider, 10);
        draw(&provider, &mut app, width, height, Theme::TERMINAL);
        app.handle(Action::OpenContext, &provider);
        let rendered = screen(&draw(&provider, &mut app, width, height, Theme::TERMINAL));
        assert!(
            rendered.contains("Raw context · "),
            "at {width}x{height}:\n{rendered}"
        );
        assert!(
            rendered.contains("· raw"),
            "the header must keep `raw` at {width}x{height}:\n{rendered}"
        );
        assert!(
            rendered.contains("[ Back to anchor ]"),
            "at {width}x{height}:\n{rendered}"
        );
        // §11: `g` stays the accelerator and stays out of the body.
        assert!(!rendered.contains("g anchor"), "at {width}x{height}");
    }
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
    app.handle(Action::OpenInvestigation, &provider);
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
    let char_column =
        |line: &str, needle: &str| line.find(needle).map(|byte| line[..byte].chars().count());
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
