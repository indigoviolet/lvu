//! Acceptance for docs/dialog-system.md §8.9: every dialog with an action row
//! has one default action, it is the one button that carries the accent fill,
//! Enter executes it from every control that does not consume Enter itself,
//! and a list opens on a real row. The rule lives in one place per component
//! (a `default` function that both `render` and the Enter arm read), and in
//! one place for the look (`dialog_controls::role_style`); what is asserted
//! here is that the two agree on screen.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lvu::{
    Action, App, CaptureTimeRange, QueryCompletion, QueryPurpose, RowProvider, SettingsContext,
    SettingsValues, StorageCategory, StorageEntry, StorageRequestKind, StorageSnapshot,
    app::{
        CommandEnrichmentControl, CommandEnrichmentField, CommandEnrichmentRequest,
        CommandEnrichmentReview, CommandEnrichmentRunState, EnrichmentControl,
        EnrichmentStepControl,
    },
    component::{LayerId, Open, RawEvent},
    components::{
        color_rules::ColorRulesControl,
        settings::{SettingsControl, SettingsField},
        time::TimeControl,
        view::ViewDialogControl,
    },
    dialog_controls::{ButtonRole, role_style},
    fixture::FixtureProvider,
    theme::{Theme, ThemeId},
    ui,
};
use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier},
};

/// §13: the two terminals the acceptance names for the fill, plus the two
/// wider ones every other dialog check uses.
const SIZES: [(u16, u16); 4] = [(140, 40), (100, 30), (80, 24), (54, 16)];

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

fn draw<P: RowProvider>(
    provider: &P,
    app: &mut App,
    width: u16,
    height: u16,
    theme: Theme,
) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, theme, None))
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

fn press(app: &mut App, provider: &FixtureProvider, code: KeyCode, modifiers: KeyModifiers) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, modifiers))),
        provider,
    );
}

fn key(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    press(app, provider, code, KeyModifiers::NONE);
}

fn paste(app: &mut App, provider: &FixtureProvider, text: &str) {
    app.handle(Action::Raw(RawEvent::Paste(text.into())), provider);
}

/// The buttons in `buffer` whose cells carry the accent fill, by label. The
/// fill is the §6.3 marking, so this is "which button is the default" as the
/// user sees it.
fn filled_buttons(buffer: &Buffer, rects: &[(Rect, String)], theme: Theme) -> Vec<String> {
    rects
        .iter()
        .filter(|(rect, _)| (rect.x..rect.right()).all(|x| buffer[(x, rect.y)].bg == theme.accent))
        .map(|(_, label)| label.clone())
        .collect()
}

fn labelled(buffer: &Buffer, rect: Rect) -> String {
    (rect.x..rect.right())
        .map(|x| buffer[(x, rect.y)].symbol())
        .collect::<String>()
        .trim()
        .trim_matches(|c| c == '[' || c == ']')
        .trim()
        .to_owned()
}

fn luminance(color: Color) -> Option<f64> {
    let Color::Rgb(red, green, blue) = color else {
        return None;
    };
    Some(
        [red, green, blue]
            .into_iter()
            .zip([0.2126, 0.7152, 0.0722])
            .map(|(channel, weight)| {
                let channel = f64::from(channel) / 255.0;
                let linear = if channel <= 0.04045 {
                    channel / 12.92
                } else {
                    ((channel + 0.055) / 1.055).powf(2.4)
                };
                linear * weight
            })
            .sum(),
    )
}

fn contrast(foreground: Color, background: Color) -> Option<f64> {
    let (left, right) = (luminance(foreground)?, luminance(background)?);
    let (lighter, darker) = if left > right {
        (left, right)
    } else {
        (right, left)
    };
    Some((lighter + 0.05) / (darker + 0.05))
}

// ---------------------------------------------------------------------------
// The treatment itself, in the one place it is decided.
// ---------------------------------------------------------------------------

#[test]
fn the_default_role_is_an_accent_fill_that_reads_and_differs_from_every_other_state() {
    for theme in ThemeId::ALL.map(Theme::builtin) {
        let default = role_style(theme, ButtonRole::Default, false);
        assert_eq!(default.bg, Some(theme.accent), "{:?}: filled", theme.id);
        assert!(default.add_modifier.contains(Modifier::BOLD));
        if theme.id != ThemeId::Terminal {
            let ratio = contrast(default.fg.unwrap(), theme.accent).unwrap();
            assert!(ratio >= 4.5, "{:?}: fill text reads {ratio:.2}:1", theme.id);
        }
        let normal = role_style(theme, ButtonRole::Normal, false);
        let destructive = role_style(theme, ButtonRole::Destructive, false);
        let focused = role_style(theme, ButtonRole::Normal, true);
        let focused_default = role_style(theme, ButtonRole::Default, true);
        assert_ne!(default.bg, normal.bg, "{:?}", theme.id);
        assert_ne!(default.bg, destructive.bg, "{:?}", theme.id);
        assert_ne!(default, focused, "{:?}", theme.id);
        // One focus ring for every control: a focused default looks like any
        // focused button, and the fill is what returns when focus leaves.
        assert_eq!(focused_default, focused);
    }
}

// ---------------------------------------------------------------------------
// Enrichment — the worked example: the default follows the list.
// ---------------------------------------------------------------------------

fn enrichment_buttons(app: &App, buffer: &Buffer) -> Vec<(Rect, String)> {
    app.layers
        .enrichment
        .control_rects()
        .iter()
        .map(|(rect, _)| (*rect, labelled(buffer, *rect)))
        .collect()
}

fn accept_step(app: &mut App, provider: &FixtureProvider, source: &str) {
    press(app, provider, KeyCode::Char('a'), KeyModifiers::ALT);
    paste(app, provider, source);
    key(app, provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().expect("one chain request");
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
}

#[test]
fn enrichment_defaults_to_add_on_an_empty_chain_and_edit_once_a_step_exists() {
    let theme = Theme::LOVE_DARK;
    for (width, height) in SIZES {
        let (provider, mut app) = demo();
        app.handle(Action::Open(Open::Enrichment), &provider);
        assert_eq!(
            app.view_state().unwrap().enrichment_control,
            EnrichmentControl::Steps,
            "the list is the initial focus"
        );
        let buffer = draw(&provider, &mut app, width, height, theme);
        assert_eq!(
            filled_buttons(&buffer, &enrichment_buttons(&app, &buffer), theme),
            vec!["Add".to_owned()],
            "{width}x{height}: an empty chain fills Add\n{}",
            screen(&buffer)
        );

        // Enter from the initial focus runs the default: the child opens on a
        // *new* step, not on the refusal notice the old Edit-on-empty gave.
        key(&mut app, &provider, KeyCode::Enter);
        assert_eq!(
            app.layers.stack_ids(),
            vec![LayerId::Enrichment, LayerId::EnrichmentStep]
        );
        let child = screen(&draw(&provider, &mut app, width, height, theme));
        assert!(child.contains("New step"), "{width}x{height}:\n{child}");
        key(&mut app, &provider, KeyCode::Esc);

        accept_step(&mut app, &provider, "one = pl.lit(1)");
        let buffer = draw(&provider, &mut app, width, height, theme);
        assert_eq!(
            filled_buttons(&buffer, &enrichment_buttons(&app, &buffer), theme),
            vec!["Edit".to_owned()],
            "{width}x{height}: a selected step fills Edit\n{}",
            screen(&buffer)
        );
        assert!(
            screen(&buffer).contains("› 1"),
            "{width}x{height}: the list opens with its row selected"
        );
        key(&mut app, &provider, KeyCode::Enter);
        let child = screen(&draw(&provider, &mut app, width, height, theme));
        assert!(child.contains("Edit step"), "{width}x{height}:\n{child}");
        assert!(child.contains("one = pl.lit(1)"), "{child}");
    }
}

/// §8.9 again, in Fields: a one-key action follows the state it acts on. Fold
/// only ever turned folding on, so having folded from here the user had to
/// find the Folding dialog to undo it. The button now says what pressing it
/// will do from where the view actually is, and keeps its mnemonic either way.
#[test]
fn fields_fold_reads_unfold_once_the_view_is_folded_by_that_column() {
    let theme = Theme::LOVE_DARK;
    for (width, height) in SIZES {
        let (provider, mut app) = demo();
        // Fields acts on the selected record, so the view has to have one.
        app.sync_provider(&provider, 10);
        app.handle(Action::Top, &provider);
        app.handle(Action::Open(Open::Fields), &provider);
        let buffer = draw(&provider, &mut app, width, height, theme);
        let before = screen(&buffer);
        assert!(before.contains("Fold"), "{width}x{height}:\n{before}");
        assert!(!before.contains("Unfold"), "{width}x{height}:\n{before}");

        // Fold by the selected column.
        press(&mut app, &provider, KeyCode::Char('d'), KeyModifiers::ALT);
        assert!(app.view_state().unwrap().fold_enabled);
        let folded = screen(&draw(&provider, &mut app, width, height, theme));
        assert!(
            folded.contains("Unfold"),
            "{width}x{height}: the button follows the state\n{folded}"
        );

        // The same key from the same place turns it back off.
        press(&mut app, &provider, KeyCode::Char('d'), KeyModifiers::ALT);
        assert!(!app.view_state().unwrap().fold_enabled);
        let unfolded = screen(&draw(&provider, &mut app, width, height, theme));
        assert!(
            !unfolded.contains("Unfold"),
            "{width}x{height}:\n{unfolded}"
        );
        assert!(unfolded.contains("Fold"), "{width}x{height}:\n{unfolded}");
    }
}

#[test]
fn enrichment_reopens_on_a_real_row_after_the_chain_shrinks() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    accept_step(&mut app, &provider, "one = pl.lit(1)");
    accept_step(&mut app, &provider, "two = pl.lit(2)");
    accept_step(&mut app, &provider, "three = pl.lit(3)");
    assert_eq!(app.view_state().unwrap().enrichment_selected, 2);
    // Remove the last two from the list layer; the selection is view-owned and
    // would otherwise still say 2 when only one row is left.
    for _ in 0..2 {
        press(&mut app, &provider, KeyCode::Char('r'), KeyModifiers::ALT);
        let request = app.take_query_requests().pop().expect("a chain request");
        assert!(app.apply_query_completion(QueryCompletion {
            view_id: request.view_id,
            generation: request.generation,
            revision: request.revision,
            purpose: QueryPurpose::Enrichment,
            result: Ok(()),
        }));
    }
    key(&mut app, &provider, KeyCode::Esc);
    app.handle(Action::Open(Open::Enrichment), &provider);
    let state = app.view_state().unwrap();
    assert_eq!(state.enrichments.len(), 1);
    assert_eq!(state.enrichment_selected, 0, "clamped to the chain on open");
    let rendered = screen(&draw(&provider, &mut app, 100, 30, Theme::TERMINAL));
    assert!(rendered.contains("› 1"), "{rendered}");
}

#[test]
fn the_step_editor_hands_enter_from_its_panes_to_save() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    press(&mut app, &provider, KeyCode::Char('a'), KeyModifiers::ALT);
    paste(&mut app, &provider, "one = pl.lit(1)");
    for _ in 0..8 {
        if app.layers.enrichment_step.control() == EnrichmentStepControl::Output {
            break;
        }
        key(&mut app, &provider, KeyCode::Tab);
    }
    assert_eq!(
        app.layers.enrichment_step.control(),
        EnrichmentStepControl::Output
    );
    // A pane has no action of its own, so Enter is the default: Save.
    key(&mut app, &provider, KeyCode::Enter);
    let request = app
        .take_query_requests()
        .pop()
        .expect("Enter on a pane saves");
    assert_eq!(request.constraints.enrichments.len(), 1);
    assert_eq!(request.constraints.enrichments[0].source, "one = pl.lit(1)");
}

// ---------------------------------------------------------------------------
// Storage — a destructive action is never the default.
// ---------------------------------------------------------------------------

fn storage_snapshot() -> StorageSnapshot {
    StorageSnapshot {
        entries: vec![StorageEntry {
            category: StorageCategory::Derived,
            label: "ed4a0c76.rows.idx".into(),
            bytes: 2662,
            reclaimable: 2662,
            status: "unused, recomputable".into(),
        }],
        total_bytes: 138_000,
        reclaimable_bytes: 2662,
        row_cache_bytes: 29_184,
        row_cache_limit: 4 << 20,
        query_index_bytes: 208,
        query_index_limit: 256 << 20,
        derived_index_limit_per_source: 256 << 20,
        derived_index_limit_total: 5 << 30,
        truncated: false,
        errors: Vec::new(),
    }
}

#[test]
fn storage_refreshes_on_enter_and_keeps_refresh_as_the_default_while_cleanup_confirms() {
    let theme = Theme::LOVE_LIGHT;
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Storage), &provider);
    let generation = app.layers.storage.outbox.take()[0].generation;
    assert!(app.layers.storage.complete(
        generation,
        storage_snapshot(),
        "scan complete".into(),
        true
    ));

    // Enter used to fall through to the shell; the list has no row action, so
    // it is the default button.
    key(&mut app, &provider, KeyCode::Enter);
    let requests = app.layers.storage.outbox.take();
    assert!(
        requests
            .iter()
            .any(|request| request.kind == StorageRequestKind::Scan),
        "{requests:?}"
    );
    let generation = requests[0].generation;
    assert!(app.layers.storage.complete(
        generation,
        storage_snapshot(),
        "scan complete".into(),
        true
    ));

    // The first `c` turns the second button into the destructive confirmation;
    // the fill stays on Refresh (§8.9: a destructive action is never default).
    key(&mut app, &provider, KeyCode::Char('c'));
    let buffer = draw(&provider, &mut app, 80, 24, theme);
    let rendered = screen(&buffer);
    assert!(rendered.contains("[ Confirm cleanup ]"), "{rendered}");
    let rects: Vec<(Rect, String)> = app
        .layers
        .storage
        .action_rects()
        .iter()
        .map(|(_, rect)| (*rect, labelled(&buffer, *rect)))
        .collect();
    assert_eq!(
        filled_buttons(&buffer, &rects, theme),
        vec!["Refresh".to_owned()]
    );
}

// ---------------------------------------------------------------------------
// Settings — Enter from a text field saves; Space stays a toggle.
// ---------------------------------------------------------------------------

fn settings_context() -> SettingsContext {
    SettingsContext {
        saved: SettingsValues {
            provider: "codex/old".into(),
            mode: "full-access".into(),
            thinking: "medium".into(),
            theme: ThemeId::Terminal,
            delight_enabled: true,
            reduced_motion: false,
            ascii: false,
            rows_mib: "4".into(),
            membership_mib: "256".into(),
            disk_total_mib: "5120".into(),
            index_per_source_mib: "256".into(),
            display_zone: "Z".into(),
        },
        effective_provider: "codex/env".into(),
        effective_mode: "full-access".into(),
        effective_thinking: "medium".into(),
        effective_theme: ThemeId::Terminal,
        effective_delight_enabled: true,
        effective_reduced_motion: false,
        effective_ascii: false,
        effective_display_zone: "Z".into(),
        display_zone_source: "default",
        provider_source: "environment LVU_AI_PROVIDER".into(),
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

fn settings_focus(app: &mut App, provider: &FixtureProvider, control: SettingsControl) {
    for _ in 0..64 {
        if app
            .layers
            .settings
            .state()
            .is_some_and(|dialog| dialog.focus == control)
        {
            return;
        }
        key(app, provider, KeyCode::Down);
    }
    panic!("{control:?} never took focus");
}

#[test]
fn settings_saves_on_enter_from_the_provider_field_and_space_only_toggles() {
    let (provider, mut app) = demo();
    app.configure_settings(settings_context());
    app.handle(Action::Open(Open::Settings), &provider);
    assert_eq!(
        app.layers.settings.state().unwrap().focus,
        SettingsControl::Field(SettingsField::Provider),
        "the first field is the initial focus"
    );
    key(&mut app, &provider, KeyCode::Char('x'));
    // Enter from the initial focus is the default, Save; it used to do nothing.
    key(&mut app, &provider, KeyCode::Enter);
    let requests = app.layers.settings.outbox.take();
    assert_eq!(requests.len(), 1, "Enter in a single-line field saves");
    assert_eq!(requests[0].values.provider, "codex/oldx");
    assert!(app.layers.settings.state().unwrap().saving);

    // A toggle consumes Enter (§8.9 exception) and Space is never Enter.
    key(&mut app, &provider, KeyCode::Esc);
    app.handle(Action::Open(Open::Settings), &provider);
    settings_focus(
        &mut app,
        &provider,
        SettingsControl::Field(SettingsField::Delight),
    );
    let before = app.layers.settings.state().unwrap().draft.delight_enabled;
    key(&mut app, &provider, KeyCode::Char(' '));
    assert_ne!(
        app.layers.settings.state().unwrap().draft.delight_enabled,
        before
    );
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(
        app.layers.settings.state().unwrap().draft.delight_enabled,
        before,
        "Enter on a checkbox toggles it, exactly as Space does"
    );
    assert!(
        app.layers.settings.outbox.take().is_empty(),
        "neither key saved"
    );

    // The default is the filled button at both acceptance sizes.
    let theme = Theme::LOVE_DARK;
    for (width, height) in [(80, 24), (54, 16)] {
        let buffer = draw(&provider, &mut app, width, height, theme);
        let rects: Vec<(Rect, String)> = app
            .layers
            .settings
            .control_rects()
            .iter()
            .filter(|(_, control)| matches!(control, SettingsControl::Save | SettingsControl::More))
            .map(|(rect, _)| (*rect, labelled(&buffer, *rect)))
            .collect();
        assert_eq!(
            filled_buttons(&buffer, &rects, theme),
            vec!["Save".to_owned()],
            "{width}x{height}\n{}",
            screen(&buffer)
        );
    }
}

// ---------------------------------------------------------------------------
// Time — the text segments hand Enter to Apply; a dropdown keeps it.
// ---------------------------------------------------------------------------

fn time_focus(app: &mut App, provider: &FixtureProvider, control: TimeControl) {
    for _ in 0..64 {
        if app.layers.time.state().focus == control {
            return;
        }
        key(app, provider, KeyCode::Tab);
    }
    panic!("{control:?} never took focus");
}

#[test]
fn time_applies_on_enter_from_a_date_segment_and_opens_a_focused_dropdown() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 8);
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 100, 30, Theme::TERMINAL);
    assert_eq!(app.layers.time.state().focus, TimeControl::Basis);

    // The initial focus is a dropdown: Enter opens it (§8.9 exception) and
    // Escape closes only the popup.
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.layers.time.state().dropdown.is_some());
    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.layers.time.state().dropdown.is_none());
    assert!(app.layers.time.is_open());

    paste(&mut app, &provider, "1970-01-01T00:00:01Z");
    app.layers.time.switch_field();
    paste(&mut app, &provider, "1970-01-01T00:00:03Z");
    time_focus(&mut app, &provider, TimeControl::StartDate);
    // Enter in a text segment used to be a no-op; it is the default now.
    key(&mut app, &provider, KeyCode::Enter);
    let request = app
        .take_query_requests()
        .pop()
        .expect("Enter in a date segment applies");
    assert_eq!(
        request.constraints.capture_time,
        Some(CaptureTimeRange {
            start_unix_nanos: 1_000_000_000,
            end_unix_nanos: 3_000_000_000,
        })
    );
    assert!(!app.layers.time.is_open(), "a submitted window closes Time");
}

#[test]
fn time_fills_apply_and_only_apply() {
    let theme = Theme::LOVE_DARK;
    for (width, height) in [(80, 24), (54, 16)] {
        let (provider, mut app) = demo();
        app.handle(Action::Open(Open::Time), &provider);
        let buffer = draw(&provider, &mut app, width, height, theme);
        let rects: Vec<(Rect, String)> = app
            .layers
            .time
            .control_rects()
            .iter()
            .filter(|(_, control)| {
                matches!(
                    control,
                    TimeControl::Apply | TimeControl::Clear | TimeControl::Recognize
                )
            })
            .map(|(rect, _)| (*rect, labelled(&buffer, *rect)))
            .collect();
        assert_eq!(
            filled_buttons(&buffer, &rects, theme),
            vec!["Apply".to_owned()],
            "{width}x{height}\n{}",
            screen(&buffer)
        );
    }
}

// ---------------------------------------------------------------------------
// External command — Save is the default until a review is waiting.
// ---------------------------------------------------------------------------

fn command_buttons(app: &App, buffer: &Buffer) -> Vec<(Rect, String)> {
    app.layers
        .external_command
        .control_rects()
        .iter()
        .filter(|(_, control)| *control != CommandEnrichmentControl::Field)
        .map(|(rect, _)| (*rect, labelled(buffer, *rect)))
        .collect()
}

#[test]
fn external_command_saves_on_enter_from_program_and_a_multiline_field_takes_a_newline() {
    let theme = Theme::LOVE_DARK;
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    let state = app.layers.external_command.state().unwrap();
    assert_eq!(state.selected_control, CommandEnrichmentControl::Field);
    assert_eq!(state.selected_field, CommandEnrichmentField::Program);
    let buffer = draw(&provider, &mut app, 80, 24, theme);
    assert_eq!(
        filled_buttons(&buffer, &command_buttons(&app, &buffer), theme),
        vec!["Save".to_owned()]
    );

    paste(&mut app, &provider, "/usr/bin/enrich");
    // Enter in the single-line Program field is the default, Save. It used to
    // be the run confirmation, which is a no-op until a review exists.
    key(&mut app, &provider, KeyCode::Enter);
    accept_command_save(&mut app, &view_id);

    // Tab to Arguments: a multi-line field, where Enter is a newline.
    key(&mut app, &provider, KeyCode::Tab);
    assert_eq!(
        app.layers.external_command.state().unwrap().selected_field,
        CommandEnrichmentField::Arguments
    );
    paste(&mut app, &provider, "-c");
    key(&mut app, &provider, KeyCode::Enter);
    paste(&mut app, &provider, ".");
    assert_eq!(
        app.layers.external_command.state().unwrap().arguments,
        "-c\n."
    );
    assert!(
        app.take_command_enrichment_requests().is_empty(),
        "a newline is not a save"
    );
    // Ctrl-Enter is how the default is reached from inside it.
    press(&mut app, &provider, KeyCode::Enter, KeyModifiers::CONTROL);
    assert_eq!(app.take_query_requests().len(), 1, "Ctrl-Enter saves");
}

/// A command save is a chain change: one query request, accepted here.
fn accept_command_save(app: &mut App, view_id: &str) {
    assert!(app.take_command_enrichment_requests().is_empty());
    let request = app.take_query_requests().pop().expect("one chain request");
    assert_eq!(request.view_id, view_id);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
}

#[test]
fn a_pending_run_review_owns_enter_and_moves_the_fill_to_review_and_run() {
    let theme = Theme::LOVE_DARK;
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    paste(&mut app, &provider, "/usr/bin/enrich");
    press(
        &mut app,
        &provider,
        KeyCode::Char('s'),
        KeyModifiers::CONTROL,
    );
    accept_command_save(&mut app, &view_id);
    let stage = app
        .layers
        .external_command
        .state()
        .unwrap()
        .accepted
        .clone()
        .expect("a saved stage");

    // Move into the multi-line Arguments field, then ask for the review.
    key(&mut app, &provider, KeyCode::Tab);
    press(
        &mut app,
        &provider,
        KeyCode::Char('r'),
        KeyModifiers::CONTROL,
    );
    let requests = app.take_command_enrichment_requests();
    let [CommandEnrichmentRequest::PrepareRun { generation, .. }] = requests.as_slice() else {
        panic!("{requests:?}");
    };
    let generation = *generation;
    assert!(app.finish_command_enrichment_review(
        generation,
        &view_id,
        1,
        Ok(CommandEnrichmentReview {
            review_token: "token".into(),
            record_count: 16,
            source_count: 1,
            executable: match &stage.definition.program {
                lvu_core::CommandProgram::Exec { executable, .. } => {
                    executable.display().to_string()
                }
                lvu_core::CommandProgram::Shell { text } => text.clone(),
            },
            arguments: Vec::new(),
            cwd: None,
            environment_keys: Vec::new(),
        })
    ));
    assert_eq!(
        app.layers.external_command.state().unwrap().run_state,
        CommandEnrichmentRunState::Ready
    );
    for (width, height) in [(80, 24), (54, 16)] {
        let buffer = draw(&provider, &mut app, width, height, theme);
        assert_eq!(
            filled_buttons(&buffer, &command_buttons(&app, &buffer), theme),
            vec!["Review and run".to_owned()],
            "{width}x{height}: the fill follows the pending review\n{}",
            screen(&buffer)
        );
    }

    // Enter from the multi-line field confirms the review rather than
    // inserting a newline: the review is the frontmost surface.
    assert_eq!(
        app.layers.external_command.state().unwrap().selected_field,
        CommandEnrichmentField::Arguments
    );
    key(&mut app, &provider, KeyCode::Enter);
    let requests = app.take_command_enrichment_requests();
    assert!(
        matches!(
            requests.as_slice(),
            [CommandEnrichmentRequest::Execute { review_token, .. }] if review_token == "token"
        ),
        "{requests:?}"
    );
    assert_eq!(
        app.layers.external_command.state().unwrap().arguments,
        "",
        "no newline was inserted"
    );
}

// ---------------------------------------------------------------------------
// View — the fill marks Apply, not the first mode button.
// ---------------------------------------------------------------------------

#[test]
fn view_fills_apply_rather_than_the_first_mode_button() {
    let theme = Theme::LOVE_DARK;
    for (width, height) in [(80, 24), (54, 16)] {
        let (provider, mut app) = demo();
        app.handle(Action::Open(Open::View), &provider);
        assert_eq!(app.layers.view.control(), ViewDialogControl::Input);
        let buffer = draw(&provider, &mut app, width, height, theme);
        let rects: Vec<(Rect, String)> = app
            .layers
            .view
            .control_rects()
            .iter()
            .filter(|(_, control)| {
                matches!(
                    control,
                    ViewDialogControl::Mode(_) | ViewDialogControl::Apply
                )
            })
            .map(|(rect, _)| (*rect, labelled(&buffer, *rect)))
            .collect();
        assert_eq!(
            filled_buttons(&buffer, &rects, theme),
            vec!["Apply".to_owned()],
            "{width}x{height}\n{}",
            screen(&buffer)
        );
    }
}

// ---------------------------------------------------------------------------
// Folding — a settings dialog whose one verb is its default.
// ---------------------------------------------------------------------------

/// §8.9: every field in Folding takes effect where it stands, so the dialog has
/// no `Apply` and its action row holds one verb. A dialog with an action row
/// declares a default, so that verb is it and it carries the fill; "no default"
/// is reserved for a dialog with no action row at all (Help).
///
/// The rest of §8.9 is the exception table: every control in this body is a
/// checkbox or a closed dropdown, and Enter belongs to each of those, so the
/// fill marks the row rather than routing Enter out of the body. What is
/// asserted here is that the one declaration and the one fill agree.
#[test]
fn folding_fills_its_one_verb_and_leaves_enter_to_the_controls_that_consume_it() {
    let theme = Theme::LOVE_DARK;
    for (width, height) in [(80u16, 24u16), (54, 16)] {
        let (provider, mut app) = demo();
        app.sync_provider(&provider, 8);
        app.handle(Action::Open(Open::Folding), &provider);
        let buffer = draw(&provider, &mut app, width, height, theme);
        let rendered = screen(&buffer);
        assert!(
            rendered.contains("Folding"),
            "{width}x{height}:\n{rendered}"
        );
        let rects: Vec<(Rect, String)> = app
            .layers
            .folding
            .control_rects()
            .iter()
            .filter(|(_, control)| *control == lvu::components::folding::FoldingControl::Collapse)
            .map(|(rect, _)| (*rect, labelled(&buffer, *rect)))
            .collect();
        assert_eq!(
            filled_buttons(&buffer, &rects, theme),
            vec!["Collapse expanded runs".to_owned()],
            "{width}x{height}: the one verb is the default\n{rendered}"
        );
    }

    // Enter on the checkbox toggles it rather than running the default, and
    // Enter on a dropdown opens its list — §8.9's table, not an exception to it.
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 8);
    app.handle(Action::Open(Open::Folding), &provider);
    let enabled = app.view_state().map(|state| state.fold_enabled);
    for _ in 0..6 {
        if app.layers.folding.control() == Some(lvu::components::folding::FoldingControl::Enabled) {
            break;
        }
        key(&mut app, &provider, KeyCode::Tab);
    }
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(
        app.view_state().map(|state| state.fold_enabled),
        enabled.map(|value| !value),
        "Enter on a checkbox toggles it"
    );
}

// ---------------------------------------------------------------------------
// Colour rules — the fill is Apply, and moves to Add when there is nothing to
// apply yet.
// ---------------------------------------------------------------------------

#[test]
fn color_rules_fills_add_on_an_empty_list_and_apply_once_a_rule_exists() {
    let theme = Theme::LOVE_DARK;
    for (width, height) in [(80, 24), (54, 16)] {
        let (provider, mut app) = demo();
        app.handle(Action::Open(Open::ColorRules), &provider);
        let buffer = draw(&provider, &mut app, width, height, theme);
        assert_eq!(
            filled_buttons(&buffer, &color_rules_buttons(&app, &buffer), theme),
            vec!["Add".to_owned()],
            "empty at {width}x{height}\n{}",
            screen(&buffer)
        );

        // Enter on the list has no row to edit, so it runs the default.
        key(&mut app, &provider, KeyCode::Enter);
        assert_eq!(
            app.layers.color_rules.control(),
            ColorRulesControl::Predicate,
            "Enter on an empty list adds a rule and edits it"
        );
        paste(&mut app, &provider, "timeout");
        let buffer = draw(&provider, &mut app, width, height, theme);
        assert_eq!(
            filled_buttons(&buffer, &color_rules_buttons(&app, &buffer), theme),
            vec!["Apply".to_owned()],
            "with a rule at {width}x{height}\n{}",
            screen(&buffer)
        );
    }
}

fn color_rules_buttons(app: &App, buffer: &Buffer) -> Vec<(Rect, String)> {
    app.layers
        .color_rules
        .control_rects()
        .iter()
        .filter(|(_, control)| {
            matches!(
                control,
                ColorRulesControl::Add | ColorRulesControl::Remove | ColorRulesControl::Apply
            )
        })
        .map(|(rect, _)| (*rect, labelled(buffer, *rect)))
        .collect()
}

// ---------------------------------------------------------------------------
// Every dialog with an action row: exactly one filled button at the two
// acceptance sizes (§13).
// ---------------------------------------------------------------------------

#[test]
fn every_component_dialog_with_actions_fills_exactly_one_button() {
    let theme = Theme::LOVE_DARK;
    // Every component dialog with buttons is here; the Filter dialog's two
    // tabs each draw `[ Apply ] [ Clear ]` with Apply filled (§12.1).
    let opens: [(Open, &str); 12] = [
        (Open::Search, "Filter"),
        (Open::Advanced, "Filter"),
        (Open::Grouping, "Multiline grouping"),
        (Open::Folding, "Folding"),
        (Open::Time, "Time window"),
        (Open::View, "View"),
        (Open::ViewSummary, "View summary"),
        (Open::Fields, "Fields"),
        (Open::Storage, "Storage"),
        (Open::Enrichment, "Enrichment"),
        (
            Open::ExternalCommand {
                stage: None,
                insert_at: usize::MAX,
            },
            "External command",
        ),
        (Open::ColorRules, "Colour rules"),
    ];
    for (open, title) in opens {
        for (width, height) in [(80, 24), (54, 16)] {
            let (provider, mut app) = demo();
            app.sync_provider(&provider, 8);
            app.handle(Action::Open(open.clone()), &provider);
            let buffer = draw(&provider, &mut app, width, height, theme);
            let rendered = screen(&buffer);
            assert!(
                rendered.contains(title),
                "{title} at {width}x{height}:\n{rendered}"
            );
            // Count the distinct button rects (`[ … ]` runs) whose every cell
            // carries the accent fill.
            let mut filled = 0;
            for y in 0..buffer.area.height {
                let mut x = 0;
                while x < buffer.area.width {
                    if buffer[(x, y)].symbol() == "[" && buffer[(x, y)].bg == theme.accent {
                        let mut end = x;
                        while end < buffer.area.width && buffer[(end, y)].symbol() != "]" {
                            end += 1;
                        }
                        if end < buffer.area.width
                            && (x..=end).all(|cell| buffer[(cell, y)].bg == theme.accent)
                        {
                            filled += 1;
                        }
                        x = end.saturating_add(1);
                    } else {
                        x += 1;
                    }
                }
            }
            assert_eq!(
                filled, 1,
                "{title} at {width}x{height}: one default, filled\n{rendered}"
            );
        }
    }
}
