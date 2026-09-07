//! Acceptance for the shared dialog primitives: size classes, region layout,
//! the backdrop scrim and the input tone (docs/dialog-system.md §3, §5, §6).

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, SettingsContext, SettingsValues,
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

/// Every dialog surface that has been adopted onto the anatomy, so the size
/// classes can be asserted uniformly as more of them land.
fn adopted_dialogs() -> Vec<(&'static str, Action, DialogClass)> {
    vec![
        ("search", Action::OpenSearch, DialogClass::S),
        ("grouping", Action::OpenGrouping, DialogClass::S),
        ("view", Action::OpenViewDialog, DialogClass::M),
        ("settings", Action::OpenSettings, DialogClass::L),
        ("source", Action::OpenSource, DialogClass::L),
        ("storage", Action::OpenStorage, DialogClass::L),
        ("time", Action::OpenTime, DialogClass::M),
    ]
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
    use lvu::app::SourceControl;

    for (width, height) in SIZES {
        let (provider, mut app) = demo();
        draw(&provider, &mut app, width, height, Theme::TERMINAL);
        app.handle(Action::OpenSource, &provider);
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
                .hit_regions
                .source_controls
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
        app.handle(Action::OpenSource, &provider);
        app.handle(Action::ToggleSourceAi, &provider);
        let generation = app.source_dialog.as_ref().expect("dialog").ai.generation;
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
            let dialog = app.source_dialog.as_ref().expect("dialog");
            if dialog.ai.preview_scroll >= dialog.ai.preview_scroll_limit {
                break;
            }
            app.handle(Action::ModalVertical(1), &provider);
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
        app.handle(Action::OpenStorage, &provider);
        let generation = app.take_storage_requests()[0].generation;
        assert!(app.update_storage(
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

        let row = app.hit_regions.storage_rows[0].0;
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
    app.handle(Action::OpenStorage, &provider);
    let generation = app.take_storage_requests()[0].generation;
    assert!(app.update_storage(
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
    assert!(
        app.dialog_scroll_limit > 0,
        "a long diagnostic must really overflow, not be clipped away"
    );
    assert!(
        app.hit_regions.dialog_scroll.is_some(),
        "the diagnostics pane needs a wheel target"
    );
    app.handle(Action::ScrollDialog(i32::MAX), &provider);
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
        app.handle(Action::OpenTime, &provider);
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
    app.handle(Action::OpenTime, &provider);
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
