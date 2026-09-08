//! Acceptance for docs/dialog-system.md §8.10: where a user learns what they
//! can do. A button shows its accelerator by underlining that letter and
//! nothing else prints it; the palette indexes every base operation with the chord
//! that works in the current focus; Help indexes the base screen and states
//! the conventions once; the base screen prints two doors and no footers.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lvu::{
    Action, App, DisplayRow, GapDirection, GapHit, RowId, RowPage, RowProvider, ViewportRequest,
    app::{Focus, RawContextOrigin, key_to_action},
    command_palette::{CommandId, Palette, PaletteContext},
    component::{Component, Open, RawEvent},
    dialog_controls::{ButtonRole, button_line, button_text, mnemonic, mnemonic_key},
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Modifier};

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

struct GapProvider(FixtureProvider);

impl RowProvider for GapProvider {
    fn page(&self, view_id: &str, request: ViewportRequest) -> RowPage {
        self.0.page(view_id, request)
    }

    fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<DisplayRow> {
        self.0.row_by_id(view_id, id)
    }

    fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize> {
        self.0.index_of_id(view_id, id)
    }

    fn revision(&self, view_id: &str) -> u64 {
        self.0.revision(view_id)
    }

    fn find_gap(
        &self,
        view_id: &str,
        _: Option<&RowId>,
        _: GapDirection,
        _: i64,
        _: lvu::TimeBasis,
    ) -> Option<GapHit> {
        let rows = self
            .0
            .page(view_id, ViewportRequest { start: 0, len: 2 })
            .rows;
        Some(GapHit {
            previous_row: rows[0].id.clone(),
            row: rows[1].id.clone(),
            previous_unix_nanos: 1_700_000_000_000_000_000,
            gap_nanos: 61_000_000_000,
        })
    }
}

fn draw<P: RowProvider>(provider: &P, app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, Theme::LOVE_DARK, None))
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

fn key(app: &mut App, provider: &FixtureProvider, code: KeyCode, modifiers: KeyModifiers) {
    let key = KeyEvent::new(code, modifiers);
    // A layer takes raw keys; the base screen goes through the shell's table,
    // exactly as `terminal.rs` dispatches them.
    let action = if app.focus == Focus::Layer {
        Action::Raw(RawEvent::Key(key))
    } else {
        app.key_to_action(key)
    };
    app.handle(action, provider);
}

/// The cells of the first `[ … ]` run on the row containing `label`, with
/// whether each is underlined.
fn underlined_cells(buffer: &Buffer, label: &str) -> Vec<(String, bool)> {
    for y in 0..buffer.area.height {
        let line: String = (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect();
        if let Some(byte) = line.find(label) {
            let start = line[..byte].chars().count() as u16;
            return (start..start + label.chars().count() as u16)
                .map(|x| {
                    let cell = &buffer[(x, y)];
                    (
                        cell.symbol().to_owned(),
                        cell.modifier.contains(Modifier::UNDERLINED),
                    )
                })
                .collect();
        }
    }
    panic!("{label} not drawn:\n{}", screen(buffer));
}

// ---------------------------------------------------------------------------
// The mnemonic primitive.
// ---------------------------------------------------------------------------

#[test]
fn a_mnemonic_is_one_underlined_letter_and_the_marker_is_never_drawn() {
    assert_eq!(button_text("&Add"), "[ Add ]");
    assert_eq!(button_text("External &command…"), "[ External command… ]");
    assert_eq!(button_text("Tom && Jerry"), "[ Tom & Jerry ]");
    assert_eq!(mnemonic_key("External &command…"), Some('c'));
    assert_eq!(mnemonic_key("Apply"), None);
    assert_eq!(mnemonic("New &blank").key, Some(('b', 4)));

    let line = button_line("Re&move", ratatui::style::Style::default());
    let underlined: Vec<String> = line
        .spans
        .iter()
        .filter(|span| span.style.add_modifier.contains(Modifier::UNDERLINED))
        .map(|span| span.content.to_string())
        .collect();
    assert_eq!(underlined, vec!["m".to_owned()]);
    let plain = button_line("Apply", ratatui::style::Style::default());
    assert!(
        plain
            .spans
            .iter()
            .all(|span| !span.style.add_modifier.contains(Modifier::UNDERLINED))
    );
    let _ = ButtonRole::Default;
}

#[test]
fn enrichment_buttons_underline_exactly_their_alt_letters() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    let buffer = draw(&provider, &mut app, 100, 30);
    for (label, letter) in [
        ("[ Add ]", "A"),
        ("[ Edit ]", "E"),
        ("[ Remove ]", "R"),
        ("[ External command… ]", "c"),
    ] {
        let cells = underlined_cells(&buffer, label);
        let underlined: Vec<&str> = cells
            .iter()
            .filter(|(_, under)| *under)
            .map(|(symbol, _)| symbol.as_str())
            .collect();
        assert_eq!(underlined, vec![letter], "{label}:\n{}", screen(&buffer));
    }
    // The underline survives the fill and the focus ring: Edit is the
    // default on a populated chain, Add gets the focus ring by Tab.
    key(&mut app, &provider, KeyCode::Tab, KeyModifiers::NONE);
    let buffer = draw(&provider, &mut app, 100, 30);
    let add = underlined_cells(&buffer, "[ Add ]");
    assert!(add.iter().any(|(symbol, under)| symbol == "A" && *under));
}

#[test]
fn alt_plus_the_underlined_letter_presses_the_button() {
    // View: `&Clone` is Alt-C; the pre-rule Alt-D keeps working unlisted.
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::View), &provider);
    key(&mut app, &provider, KeyCode::Char('c'), KeyModifiers::ALT);
    assert_eq!(app.layers.view.mode(), lvu::ViewDialogMode::Clone);
    key(&mut app, &provider, KeyCode::Char('s'), KeyModifiers::ALT);
    assert_eq!(app.layers.view.mode(), lvu::ViewDialogMode::Sources);
    key(&mut app, &provider, KeyCode::Char('d'), KeyModifiers::ALT);
    assert_eq!(app.layers.view.mode(), lvu::ViewDialogMode::Clone);
    key(&mut app, &provider, KeyCode::Esc, KeyModifiers::NONE);

    // External command: `Re&move` is Alt-M; the palette says so.
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    let entries = app.layers.external_command.commands(&app.views);
    let remove = entries
        .iter()
        .find(|entry| entry.spec.id == CommandId::CommandEnrichmentRemove)
        .unwrap();
    assert_eq!(remove.spec.shortcut, Some("Alt-M"));
    let labels: Vec<String> = app
        .layers
        .external_command
        .commands(&app.views)
        .iter()
        .filter_map(|entry| entry.spec.shortcut.map(str::to_owned))
        .collect();
    assert_eq!(labels, vec!["Alt-S", "Alt-R", "Alt-M"]);
}

// ---------------------------------------------------------------------------
// The palette is complete and its chords are the ones that work.
// ---------------------------------------------------------------------------

/// Every base-screen key that is an operation (not routine navigation), with
/// the chord the palette must print for it.
const BASE_OPERATIONS: &[(KeyCode, KeyModifiers, &str)] = &[
    (KeyCode::Char('/'), KeyModifiers::NONE, "/"),
    (KeyCode::Char('e'), KeyModifiers::NONE, "e"),
    (KeyCode::Char('m'), KeyModifiers::NONE, "m"),
    (KeyCode::Char('t'), KeyModifiers::NONE, "t"),
    (KeyCode::Char('i'), KeyModifiers::NONE, "i"),
    (KeyCode::Char('c'), KeyModifiers::NONE, "c"),
    (KeyCode::Char('o'), KeyModifiers::NONE, "o"),
    (KeyCode::Char('b'), KeyModifiers::NONE, "b"),
    (KeyCode::Char('B'), KeyModifiers::SHIFT, "B"),
    (KeyCode::Char('v'), KeyModifiers::NONE, "v"),
    (KeyCode::Char('V'), KeyModifiers::SHIFT, "V"),
    (KeyCode::Char('r'), KeyModifiers::NONE, "r"),
    (KeyCode::Char('n'), KeyModifiers::NONE, "n"),
    (KeyCode::Char('S'), KeyModifiers::SHIFT, "S"),
    (KeyCode::Char('A'), KeyModifiers::SHIFT, "A"),
    (KeyCode::Char('I'), KeyModifiers::SHIFT, "I"),
    (KeyCode::Char(','), KeyModifiers::NONE, ","),
    (KeyCode::Char('?'), KeyModifiers::NONE, "?"),
    (KeyCode::Char('f'), KeyModifiers::NONE, "f"),
    (KeyCode::Char('d'), KeyModifiers::NONE, "d"),
    (KeyCode::Char('g'), KeyModifiers::NONE, "g"),
    (KeyCode::Char('G'), KeyModifiers::SHIFT, "G"),
    (KeyCode::Char('{'), KeyModifiers::NONE, "{"),
    (KeyCode::Char('}'), KeyModifiers::NONE, "}"),
    (KeyCode::Char('['), KeyModifiers::NONE, "["),
    (KeyCode::Char(']'), KeyModifiers::NONE, "]"),
    (KeyCode::Char('0'), KeyModifiers::NONE, "0"),
    (KeyCode::Enter, KeyModifiers::NONE, "Enter"),
    // §8.10: no base operation is bound to Alt alone. These two were, and
    // they are the reason the rule now covers the base screen.
    (KeyCode::Char('X'), KeyModifiers::SHIFT, "X"),
    (KeyCode::Char('R'), KeyModifiers::SHIFT, "R"),
];

/// §8.10: the base screen takes no text, so every letter there is a key and no
/// base operation may be reachable only through Alt. Stopping and restarting a
/// source were bound to Alt-S and Alt-R alone, which on an xterm's defaults are
/// the letters `ó` and `ò` and never arrive as a chord at all.
///
/// This walks the whole base table rather than the two that were wrong, so the
/// next Alt-only binding fails here instead of on someone's terminal.
#[test]
fn no_base_operation_is_reachable_only_through_alt() {
    for focus in [Focus::Logs, Focus::Selector, Focus::Details] {
        for (_code, modifiers, label) in BASE_OPERATIONS {
            if !modifiers.contains(KeyModifiers::ALT) {
                continue;
            }
            panic!("{label} is an Alt-only base binding in {focus:?}; give it a bare key");
        }
        // And the two that were: their bare keys reach the same actions their
        // Alt aliases still do.
        for (bare, alt, action) in [
            (
                KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT),
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT),
                Action::StopCapture,
            ),
            (
                KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT),
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT),
                Action::RestartCapture,
            ),
        ] {
            if focus == Focus::Details {
                // Source operations belong to the sidebar and the log; the
                // Details pane is not one of their focuses, and was not before.
                continue;
            }
            assert_eq!(key_to_action(bare, focus), action, "{focus:?}: bare key");
            assert_eq!(
                key_to_action(alt, focus),
                action,
                "{focus:?}: the Alt alias keeps working"
            );
        }
    }
}

#[test]
fn every_base_operation_is_in_the_palette_with_the_chord_that_works() {
    for focus in [Focus::Logs, Focus::Selector] {
        let mut palette = Palette::new();
        palette.open(PaletteContext::new(focus, true));
        for (code, modifiers, label) in BASE_OPERATIONS {
            // Enter is the log's group toggle and deliberately inert in the
            // sidebar (§8.10), where a view row has nothing to expand.
            if focus == Focus::Selector && *code == KeyCode::Enter {
                continue;
            }
            let action = key_to_action(KeyEvent::new(*code, *modifiers), focus);
            assert_ne!(action, Action::None, "{label} is unbound in {focus:?}");
            let command = palette
                .commands()
                .iter()
                .find(|command| command.action == action)
                .unwrap_or_else(|| {
                    panic!("no palette entry for {label} ({action:?}) in {focus:?}")
                });
            assert_eq!(
                command.shortcut,
                Some(*label),
                "{focus:?}: {} shows the wrong chord",
                command.name
            );
        }
    }
}

/// §8.10 with availability: in every focus and state a base operation is
/// either in the default list, runnable, with its chord, or — when it cannot
/// run — absent from the default list yet found by its name under `Not
/// available now` with a reason. Nothing is merely missing.
#[test]
fn every_base_operation_is_listed_when_it_can_run_and_explained_when_it_cannot() {
    for (focus, has_view) in [
        (Focus::Logs, true),
        (Focus::Selector, true),
        (Focus::Logs, false),
        (Focus::Selector, false),
    ] {
        let mut palette = Palette::new();
        palette.open(PaletteContext::new(focus, has_view));
        let default: Vec<_> = palette.results().cloned().collect();
        assert!(
            default.iter().all(|command| command.is_enabled()),
            "{focus:?} view={has_view}: the default list holds only what can run"
        );
        for (code, modifiers, label) in BASE_OPERATIONS {
            if focus == Focus::Selector && *code == KeyCode::Enter {
                continue;
            }
            let action = key_to_action(KeyEvent::new(*code, *modifiers), focus);
            let command = palette
                .commands()
                .iter()
                .find(|command| command.action == action)
                .cloned()
                .unwrap_or_else(|| panic!("no palette entry for {label} in {focus:?}"));
            if command.is_enabled() {
                assert!(
                    default.iter().any(|listed| listed.id == command.id),
                    "{focus:?} view={has_view}: {} can run but is not in the default list",
                    command.name
                );
                continue;
            }
            assert!(
                !default.iter().any(|listed| listed.id == command.id),
                "{focus:?} view={has_view}: {} cannot run but is in the default list",
                command.name
            );
            let mut searched = Palette::new();
            searched.open(PaletteContext::new(focus, has_view));
            for character in command.name.chars() {
                searched.handle_key(
                    KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
                    PaletteContext::new(focus, has_view),
                );
            }
            let found = searched
                .unavailable_results()
                .find(|listed| listed.id == command.id)
                .unwrap_or_else(|| {
                    panic!(
                        "{focus:?} view={has_view}: {} cannot run and is not under Not available now",
                        command.name
                    )
                });
            assert!(
                found
                    .unavailable_reason
                    .is_some_and(|reason| !reason.trim().is_empty()),
                "{focus:?} view={has_view}: {} has no reason",
                command.name
            );
        }
    }
}

#[test]
fn the_quit_row_never_shows_a_chord_that_does_something_else() {
    // In the Details pane `q` hides the pane (the shell's dismissal rule), so
    // the palette prints the chord that quits everywhere, and the key table
    // agrees with the shell about what `q` does there.
    for focus in [Focus::Logs, Focus::Selector, Focus::Details] {
        let mut palette = Palette::new();
        palette.open(PaletteContext::new(focus, true));
        let quit = palette
            .commands()
            .iter()
            .find(|command| command.id == CommandId::Quit)
            .unwrap();
        assert_eq!(quit.shortcut, Some("Ctrl-C"), "{focus:?}");
    }
    assert_eq!(
        key_to_action(
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            Focus::Details
        ),
        Action::ToggleDetails
    );
}

#[test]
fn the_sidebar_binds_only_its_own_keys_and_the_rest_mean_what_the_log_means() {
    let (provider, mut app) = demo();
    app.focus = Focus::Selector;
    key(&mut app, &provider, KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(
        app.layers.filter.is_open(),
        "Search opens from the sidebar too"
    );
    key(&mut app, &provider, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.layers.filter.is_open());
    app.focus = Focus::Selector;
    let before = app.view_state().unwrap().selected.clone();
    key(&mut app, &provider, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        app.view_state().unwrap().selected,
        before,
        "Enter in the sidebar is not the log's group toggle"
    );
}

// ---------------------------------------------------------------------------
// Help and the base screen.
// ---------------------------------------------------------------------------

#[test]
fn help_indexes_the_base_screen_and_never_a_dialogs_own_buttons() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Help), &provider);
    let text = screen(&draw(&provider, &mut app, 160, 70));
    for present in [
        "CONVENTIONS",
        // §8.10: the letter is the accelerator and Alt is the fallback for a
        // focused text field, and Conventions says both, in that order.
        // A single word: the description wraps, so a phrase would not survive
        // the two-column layout at 160 columns.
        "underlines",
        "Alt + letter",
        "Ctrl-P",
        "g / G",
        "{ / }",
        // The two source keys, asserted by their rows rather than by the
        // bare letters, which would match anywhere on the screen.
        "Stop the selected source",
        "Restart the selected source",
    ] {
        assert!(text.contains(present), "{present}\n{text}");
    }
    for retired in [
        "Alt-C in Enrichment",
        "Ctrl-P Fold",
        "Alt-F / Alt-C",
        "Ctrl-D",
        "Ctrl-A",
        "Alt-M",
        "Alt-B",
        "Alt-D",
        "Alt-N",
        "Space pins",
        "PgUp",
    ] {
        assert!(
            !text.contains(retired),
            "{retired} is a dialog's own operation\n{text}"
        );
    }
    // §8.10: Help documents the keys of the running app. The command line is
    // `lvu --help` and the README, and a flag here is a key nobody can press.
    for option in [
        "STARTING LVU",
        "lvu --resume",
        "lvu --fresh",
        "--capture-dir",
        "FILE / -c CMD",
    ] {
        assert!(
            !text.contains(option),
            "{option} is a command-line option, not a key\n{text}"
        );
    }
}

#[test]
fn the_base_screen_prints_two_doors_and_no_footers() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 8);
    let text = screen(&draw(&provider, &mut app, 100, 30));
    assert!(text.contains("? help · Ctrl-P commands"), "{text}");
    assert!(!text.contains("↑/↓"), "{text}");
    app.handle(Action::ToggleDetails, &provider);
    let text = screen(&draw(&provider, &mut app, 100, 30));
    assert!(text.contains("Selected event details"), "{text}");
    assert!(!text.contains("↑/↓") && !text.contains("scroll"), "{text}");
}

/// §8.10: the doors are the base screen's only two printed chords, so the
/// status line gives up whole segments to keep them rather than letting the
/// terminal cut wherever the line happens to reach the edge. It used to be
/// formatted whole and clipped from the right, which is exactly where the doors
/// are: at a hundred columns with a transient `query pending` on the line the
/// footer read `? help · Ctrl-P comman`.
#[test]
fn the_status_line_keeps_its_doors_whole_at_every_width() {
    for width in [60u16, 80, 100, 120] {
        let (provider, mut app) = demo();
        app.sync_provider(&provider, 8);
        let text = screen(&draw(&provider, &mut app, width, 30));
        let status = text
            .lines()
            .last()
            .unwrap_or_default()
            .trim_end()
            .to_owned();
        assert!(
            status.contains("? help · Ctrl-P commands"),
            "{width}: the doors were clipped\n{status}"
        );
        // Nothing is cut mid-word: every ` | ` separated piece that survived is
        // whole, so the line never trails off in the middle of a count or a
        // label. A dropped segment is absent, never half-present.
        for piece in status.split('|') {
            let piece = piece.trim();
            assert!(
                !piece.is_empty() || status.starts_with(' '),
                "{width}: an empty segment was left behind\n{status}"
            );
        }
        assert!(
            !status.ends_with("comman") && !status.ends_with("command"),
            "{width}: the doors were cut mid-word\n{status}"
        );
        assert!(
            status.chars().count() <= width as usize,
            "{width}: the line overflowed its area\n{status}"
        );
    }
}

#[test]
fn successful_query_counts_stay_compact_at_a_realistic_width() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 8);
    let view_id = app.active_view_id().unwrap().to_owned();
    app.update_view_runtime_status(&view_id, "query ready: matched 4 / scanned 41".to_owned());
    let text = screen(&draw(&provider, &mut app, 100, 30));
    let status = text.lines().last().unwrap_or_default().trim_end();
    assert!(status.contains("matched 4/41"), "{status}");
    assert!(!status.contains("query ready:"), "{status}");
    assert!(status.contains("? help · Ctrl-P commands"), "{status}");
}

#[test]
fn event_time_health_diagnostics_survive_at_the_real_story_width() {
    for separator in [":", "; prior diagnostic;"] {
        let (provider, mut app) = demo();
        app.sync_provider(&provider, 8);
        let view_id = app.active_view_id().unwrap().to_owned();
        app.update_view_runtime_status(
            &view_id,
            format!("query ready: matched 2 / scanned 5{separator} event time: 1 missing, 1 invalid/ambiguous; unmatched (no capture-time fallback; basis Event)"),
        );
        let text = screen(&draw(&provider, &mut app, 150, 30));
        let status = text.lines().last().unwrap_or_default();
        assert!(status.contains("? help · Ctrl-P commands"), "{status}");
        assert!(status.contains("invalid/ambiguous"), "{status}");
    }
}

#[test]
fn completed_gap_navigation_survives_competing_status_at_a_narrow_width() {
    let (provider, sources, views) = FixtureProvider::demo();
    let provider = GapProvider(provider);
    let mut app = App::new(sources, views, true);
    app.sync_provider(&provider, 8);
    let view_id = app.active_view_id().unwrap().to_owned();
    app.update_view_runtime_status(
        &view_id,
        "query ready: matched 16 / scanned 16: event time: 2 missing, 1 invalid/ambiguous; unmatched"
            .into(),
    );
    app.handle(Action::JumpToGap(GapDirection::Forward), &provider);
    let text = screen(&draw(&provider, &mut app, 110, 30));
    let status = text.lines().last().unwrap_or_default();
    assert!(status.contains("gap 61s"), "{status}");
    assert!(status.contains("quiet from"), "{status}");
    assert!(status.contains("/16"), "{status}");
    assert!(status.contains("? help · Ctrl-P commands"), "{status}");
}

#[test]
fn wide_and_combining_action_text_never_clips_the_status_doors() {
    for width in [60u16, 80, 100] {
        let (provider, mut app) = demo();
        app.sync_provider(&provider, 8);
        app.action_notice = Some("失敗 e\u{301} ".repeat(80));
        let view_id = app.active_view_id().unwrap().to_owned();
        app.raw_context_origin = Some(RawContextOrigin {
            view_id: "filtered-origin".into(),
            raw_view_id: view_id,
            anchor: RowId::new("api", 5),
            layer: None,
        });
        let text = screen(&draw(&provider, &mut app, width, 30));
        let status = text.lines().last().unwrap_or_default();
        assert!(
            status.contains("? help · Ctrl-P commands"),
            "{width}: {status}"
        );
        assert!(status.contains("o back"), "{width}: {status}");
    }
}

#[test]
fn no_dialog_help_sentence_names_a_routine_key_or_an_alt_chord() {
    let opens = [
        Open::Search,
        Open::Advanced,
        Open::Grouping,
        Open::Time,
        Open::View,
        Open::ViewSummary,
        Open::Fields,
        Open::Storage,
        Open::Settings,
        Open::Enrichment,
        Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        },
        Open::Recipes {
            mode: lvu::RecipeDialogMode::Browse,
        },
    ];
    for open in opens {
        let (provider, mut app) = demo();
        app.sync_provider(&provider, 8);
        app.handle(Action::Open(open.clone()), &provider);
        // The dialog only: the status line behind it prints the two doors.
        let full = screen(&draw(&provider, &mut app, 140, 40));
        let text: String = full
            .lines()
            .take(full.lines().count().saturating_sub(1))
            .collect::<Vec<_>>()
            .join("\n");
        for banned in [
            "Enter ", "Esc", "Escape", "Tab ", "↑/↓", "PgUp", "PgDn", "Alt-", "Ctrl-",
        ] {
            // `Tab` may be named only by the one non-routine completion hint.
            if banned == "Tab " && text.contains("complete with Tab") {
                continue;
            }
            assert!(
                !text.contains(banned),
                "{open:?} prints {banned:?}:\n{text}"
            );
        }
    }
}
/// The width rule has to actually bite: at sixty columns there is not room for
/// the range and the doors both, so the line must be visibly shorter than the
/// one a wide terminal draws rather than the test passing because everything
/// happened to fit.
#[test]
fn narrow_status_lines_give_up_segments_rather_than_overflow() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 8);
    let status_at = |width: u16, app: &mut App, provider: &FixtureProvider| {
        screen(&draw(provider, app, width, 30))
            .lines()
            .last()
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let wide = status_at(120, &mut app, &provider);
    let narrow = status_at(60, &mut app, &provider);
    assert!(
        narrow.len() < wide.len(),
        "sixty columns kept as much as a hundred and twenty:\nwide:   {wide}\nnarrow: {narrow}"
    );
    assert!(narrow.contains("? help · Ctrl-P commands"), "{narrow}");
}
