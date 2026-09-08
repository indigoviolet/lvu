//! Acceptance for the Settings layer as a component (docs/component-model.md
//! §6.3 step 10). What is asserted here is the two seams the step introduces —
//! `ctx.appearance` previewing and rolling back, and the `SettingsRequest`
//! outbox with its generation fence — plus the contract every layer owes: the
//! geometry `render` recorded is the geometry `hit()` answers with, the shell's
//! selection bound is the surface the component published, and a click outside
//! the popup is contained.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, RowProvider, SettingsContext, SettingsValues,
    component::{Component, LayerId, Open, RawEvent},
    components::settings::{SettingsControl, SettingsField, SettingsStatus},
    fixture::FixtureProvider,
    theme::{Theme, ThemeId},
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};

/// The four terminals dialog-system.md quotes; 54x16 is where the Settings body
/// overflows, so `More` and the scrollbar only exist there.
const SIZES: [(u16, u16); 4] = [(140, 40), (100, 30), (80, 24), (54, 16)];

fn context() -> SettingsContext {
    SettingsContext {
        saved: SettingsValues {
            provider: "codex/old".into(),
            mode: "full-access".into(),
            thinking: "medium".into(),
            theme: ThemeId::Terminal,
            display_zone: "Z".into(),
            delight_enabled: true,
            reduced_motion: false,
            ascii: false,
            rows_mib: "4".into(),
            membership_mib: "256".into(),
            disk_total_mib: "5120".into(),
            index_per_source_mib: "256".into(),
        },
        effective_provider: "codex/env".into(),
        effective_mode: "full-access".into(),
        effective_thinking: "medium".into(),
        effective_theme: ThemeId::Terminal,
        effective_display_zone: "Z".into(),
        display_zone_source: "default",
        effective_delight_enabled: true,
        effective_reduced_motion: false,
        effective_ascii: false,
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

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    let mut app = App::new(sources, views, true);
    app.configure_settings(context());
    (provider, app)
}

fn draw<P: RowProvider>(provider: &P, app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, Theme::TERMINAL, None))
        .unwrap();
    terminal.backend().buffer().clone()
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

fn focus(app: &mut App, provider: &FixtureProvider, control: SettingsControl) {
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

/// The rendered buffer as one string.
fn text(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

#[test]
fn every_recorded_control_was_painted_and_answers_the_hit_test() {
    for (width, height) in SIZES {
        let (provider, mut app) = demo();
        app.handle(Action::Open(Open::Settings), &provider);
        // Open the dropdown too: its rows are the component's geometry, not a
        // second layer, and they are allowed to extend past the dialog (§5.3).
        focus(
            &mut app,
            &provider,
            SettingsControl::Field(SettingsField::Theme),
        );
        key(&mut app, &provider, KeyCode::Enter);
        draw(&provider, &mut app, width, height);
        let surface = app.layers.settings.surface();
        for (rect, control) in app.layers.settings.control_rects() {
            let point = (rect.x, rect.y);
            assert!(
                contains(surface.popup, point),
                "{control:?} at {width}x{height} is outside what the layer drew"
            );
        }
        for (rect, index) in app.layers.settings.theme_choice_rects() {
            let point = (rect.x, rect.y);
            assert!(
                contains(surface.popup, point),
                "theme choice {index} at {width}x{height} escapes the popup"
            );
            assert_eq!(
                app.layers.settings.hit(point),
                Some(lvu::components::settings::SettingsHit::Choice(*index)),
                "a drawn dropdown row must answer its own hit test"
            );
        }
        assert_eq!(
            app.hit_regions.selection_modal,
            Some(surface.interior),
            "the shell's selection bound is the surface the component published"
        );
    }
}

#[test]
fn appearance_previews_live_and_dismissal_rolls_it_back() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Settings), &provider);
    assert_eq!(app.layers.top(), Some(LayerId::Settings));
    assert!(app.appearance.delight_enabled);

    focus(
        &mut app,
        &provider,
        SettingsControl::Field(SettingsField::Delight),
    );
    key(&mut app, &provider, KeyCode::Char(' '));
    assert!(
        !app.appearance.delight_enabled,
        "a toggled draft previews through ctx.appearance immediately"
    );
    assert_eq!(
        app.layers.settings.state().unwrap().status_kind,
        SettingsStatus::Pending
    );

    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.layers.settings.state().is_none());
    assert!(
        app.appearance.delight_enabled,
        "dismissal restores the effective value the layer opened with"
    );
}

#[test]
fn a_save_goes_through_the_outbox_and_an_older_completion_is_fenced() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Settings), &provider);
    let first = app.layers.settings.state().unwrap().generation;
    focus(
        &mut app,
        &provider,
        SettingsControl::Field(SettingsField::Provider),
    );
    key(&mut app, &provider, KeyCode::Char('x'));
    focus(&mut app, &provider, SettingsControl::Save);
    key(&mut app, &provider, KeyCode::Enter);
    let requests = app.layers.settings.outbox.take();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].generation, first);
    assert_eq!(requests[0].values.provider, "codex/oldx");
    assert!(app.layers.settings.state().unwrap().saving);

    // Reopen before the worker answers: the older completion must not replace
    // the newer dialog's draft.
    key(&mut app, &provider, KeyCode::Esc);
    app.handle(Action::Open(Open::Settings), &provider);
    let second = app.layers.settings.state().unwrap().generation;
    assert_ne!(second, first);
    assert!(app.complete_settings_save(first, Ok(context())));
    let dialog = app.layers.settings.state().unwrap();
    assert_eq!(dialog.generation, second);
    assert!(!dialog.saving, "the newer dialog never started a save");

    // A failure for a generation no dialog is showing falls back to the notice.
    assert!(!app.complete_settings_save(first, Err("invalid cache limit".into())));
    assert_eq!(
        app.source_notice.as_deref(),
        Some("settings save failed: invalid cache limit")
    );
}

#[test]
fn clicks_outside_the_popup_are_contained_and_the_layer_owns_its_keymap() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Settings), &provider);
    draw(&provider, &mut app, 140, 40);
    let popup = app.layers.settings.surface().popup;
    let generation = app.layers.settings.state().unwrap().generation;
    click(&mut app, &provider, (0, 0));
    assert!(!contains(popup, (0, 0)));
    assert_eq!(app.layers.top(), Some(LayerId::Settings));
    assert_eq!(
        app.layers.settings.state().unwrap().generation,
        generation,
        "a click the shell contained cannot reach the layer behind it"
    );

    // The base key table does not leak into a layer's shortcut column (§3).
    assert_eq!(
        lvu::app::key_to_action(
            KeyEvent::new(KeyCode::Char('S'), KeyModifiers::NONE),
            app.focus
        ),
        Action::None
    );
}

#[test]
fn settings_without_a_configured_snapshot_do_not_open() {
    let (provider, sources, views) = FixtureProvider::demo();
    let mut app = App::new(sources, views, true);
    app.handle(Action::Open(Open::Settings), &provider);
    assert!(app.layers.top().is_none());
    assert_eq!(
        app.source_notice.as_deref(),
        Some("settings are unavailable in this build")
    );
}

/// The display zone is the reader's setting, so it lives here rather than per
/// view: two views of one source disagreeing about what `14:30` means would be
/// worse than setting it once. What matters is that choosing one previews
/// immediately, that saving carries it, and that the limitation is stated.
#[test]
fn the_display_zone_previews_immediately_and_states_its_limitation() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Settings), &provider);
    let opened = text(&draw(&provider, &mut app, 120, 40));
    assert!(opened.contains("Times shown in"), "{opened}");
    assert!(
        opened.contains("no timezone database") && opened.contains("daylight saving"),
        "the help line says what a fixed offset cannot do:\n{opened}"
    );

    focus(
        &mut app,
        &provider,
        SettingsControl::Field(SettingsField::DisplayZone),
    );
    key(&mut app, &provider, KeyCode::Enter);
    let choices = text(&draw(&provider, &mut app, 120, 40));
    assert!(choices.contains("UTC+02:00"), "{choices}");

    // Walking to a choice previews it on the log behind the dialog: a time
    // format has no other honest preview.
    let before = app.appearance.display_zone.clone();
    key(&mut app, &provider, KeyCode::Down);
    key(&mut app, &provider, KeyCode::Enter);
    assert_ne!(app.appearance.display_zone, before);
    let previewed = app.appearance.display_zone.clone();
    assert_eq!(
        app.layers.settings.state().unwrap().draft.display_zone,
        previewed
    );

    // Saving sends it; dismissing without saving rolls the preview back.
    assert!(
        app.layers.settings.outbox.take().is_empty(),
        "choosing a zone previews it; only Save sends it"
    );
    key(&mut app, &provider, KeyCode::Esc);
    assert_eq!(
        app.appearance.display_zone, before,
        "an unsaved preview is rolled back with the rest of the appearance"
    );
}

#[test]
fn a_saved_display_zone_travels_in_the_request() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Settings), &provider);
    focus(
        &mut app,
        &provider,
        SettingsControl::Field(SettingsField::DisplayZone),
    );
    key(&mut app, &provider, KeyCode::Enter);
    key(&mut app, &provider, KeyCode::Down);
    key(&mut app, &provider, KeyCode::Enter);
    let chosen = app.appearance.display_zone.clone();
    focus(&mut app, &provider, SettingsControl::Save);
    key(&mut app, &provider, KeyCode::Enter);
    let request = app
        .layers
        .settings
        .outbox
        .take()
        .pop()
        .expect("the save reaches the outbox");
    assert_eq!(request.values.display_zone, chosen);
}
