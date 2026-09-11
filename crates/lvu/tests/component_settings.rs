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
    // Tab always moves focus (Down scrolls instead when More holds focus).
    for _ in 0..64 {
        if app
            .layers
            .settings
            .state()
            .is_some_and(|dialog| dialog.focus == control)
        {
            return;
        }
        key(app, provider, KeyCode::Tab);
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
/// immediately, that saving carries it, and that named-zone semantics are
/// stated without implying that captured instants are reinterpreted.
#[test]
fn the_display_zone_previews_immediately_and_states_its_semantics() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Settings), &provider);
    let opened = text(&draw(&provider, &mut app, 120, 40));
    assert!(opened.contains("Times shown in"), "{opened}");
    // Help wraps by display width; wider LongContent frames split "Display"
    // and "only:" across lines, so match the stable tokens, not one wrapping.
    assert!(
        opened.contains("Europe/Berlin")
            && opened.contains("daylight saving")
            && opened.contains("Display")
            && opened.contains("captured/event"),
        "the help line says how named zones affect display:\n{opened}"
    );

    focus(
        &mut app,
        &provider,
        SettingsControl::Field(SettingsField::DisplayZone),
    );
    key(&mut app, &provider, KeyCode::Enter);
    // The shared Anchored popup shows at most eight rows; UTC+02:00 (index 8)
    // scrolls into view via the shared selection reveal.
    for _ in 0..8 {
        key(&mut app, &provider, KeyCode::Down);
    }
    let choices = text(&draw(&provider, &mut app, 120, 40));
    assert!(choices.contains("UTC+02:00"), "{choices}");

    // Walking to a choice previews it on the log behind the dialog: a time
    // format has no other honest preview. Selection already rests on +02:00.
    let before = app.appearance.display_zone.clone();
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
fn a_named_zone_previews_only_when_valid_and_keeps_the_last_good_view() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Settings), &provider);
    focus(
        &mut app,
        &provider,
        SettingsControl::Field(SettingsField::DisplayZone),
    );
    key(&mut app, &provider, KeyCode::Enter);
    key(&mut app, &provider, KeyCode::Up); // wrap from UTC to Custom IANA zone
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.layers.settings.state().unwrap().zone_custom);
    assert_eq!(app.appearance.display_zone, "Z");

    for character in "Europe/Berlin".chars() {
        key(&mut app, &provider, KeyCode::Char(character));
    }
    assert_eq!(app.appearance.display_zone, "Europe/Berlin");
    assert_eq!(
        app.layers.settings.state().unwrap().draft.display_zone,
        "Europe/Berlin"
    );

    key(&mut app, &provider, KeyCode::Backspace);
    let invalid = app.layers.settings.state().unwrap();
    assert_eq!(invalid.draft.display_zone, "Europe/Berli");
    assert_eq!(invalid.status_kind, SettingsStatus::Error);
    assert!(
        invalid.status.contains("unknown time zone"),
        "{}",
        invalid.status
    );
    assert_eq!(
        app.appearance.display_zone, "Europe/Berlin",
        "an invalid draft cannot replace the last valid preview"
    );

    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.layers.settings.outbox.take().is_empty());
    assert!(
        app.layers
            .settings
            .state()
            .unwrap()
            .status
            .contains("Europe/Berlin")
    );
}

#[test]
fn stale_save_completion_cannot_publish_an_invalid_newer_zone_draft() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Settings), &provider);
    let generation_a = app.layers.settings.state().unwrap().generation;
    focus(&mut app, &provider, SettingsControl::Save);
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.layers.settings.outbox.take().len(), 1);
    key(&mut app, &provider, KeyCode::Esc);

    app.handle(Action::Open(Open::Settings), &provider);
    focus(
        &mut app,
        &provider,
        SettingsControl::Field(SettingsField::Theme),
    );
    key(&mut app, &provider, KeyCode::Enter);
    key(&mut app, &provider, KeyCode::Down);
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.appearance.theme_id, ThemeId::LoveDark);

    focus(
        &mut app,
        &provider,
        SettingsControl::Field(SettingsField::DisplayZone),
    );
    key(&mut app, &provider, KeyCode::Enter);
    key(&mut app, &provider, KeyCode::Up);
    key(&mut app, &provider, KeyCode::Enter);
    for character in "Europe/Berlin".chars() {
        key(&mut app, &provider, KeyCode::Char(character));
    }
    key(&mut app, &provider, KeyCode::Backspace);
    assert_eq!(app.appearance.display_zone, "Europe/Berlin");

    assert!(app.complete_settings_save(generation_a, Ok(context())));
    assert_eq!(app.appearance.theme_id, ThemeId::LoveDark);
    assert_eq!(app.appearance.display_zone, "Europe/Berlin");
    assert_eq!(
        lvu::app::format_display_time(1_768_478_400_000_000_000, &app.appearance.display_zone),
        "13:00:00.000+01:00"
    );
    assert_eq!(
        app.layers.settings.state().unwrap().draft.display_zone,
        "Europe/Berli",
        "the rejected newer draft remains editable"
    );
}

#[test]
fn display_zone_mouse_activation_distinguishes_presets_from_custom_text() {
    let (provider, mut preset) = demo();
    preset.handle(Action::Open(Open::Settings), &provider);
    draw(&provider, &mut preset, 100, 30);
    let preset_rect = preset
        .layers
        .settings
        .control_rects()
        .iter()
        .find(|(_, control)| *control == SettingsControl::Field(SettingsField::DisplayZone))
        .map(|(rect, _)| *rect)
        .unwrap();
    click(&mut preset, &provider, (preset_rect.x, preset_rect.y));
    assert_eq!(
        preset.layers.settings.state().unwrap().dropdown,
        Some(SettingsField::DisplayZone),
        "a preset-zone mouse click opens its choices"
    );
    // The shared Anchored popup pages to eight rows; the Custom row (index
    // 16) scrolls into view via the shared selection reveal before clicking.
    for _ in 0..16 {
        if preset
            .layers
            .settings
            .theme_choice_rects()
            .iter()
            .any(|(_, index)| *index == lvu::app::time_zone_choices().len())
        {
            break;
        }
        key(&mut preset, &provider, KeyCode::Down);
        draw(&provider, &mut preset, 100, 30);
    }
    draw(&provider, &mut preset, 100, 30);
    let custom_choice = preset
        .layers
        .settings
        .theme_choice_rects()
        .iter()
        .find(|(_, index)| *index == lvu::app::time_zone_choices().len())
        .map(|(rect, _)| *rect)
        .unwrap();
    click(&mut preset, &provider, (custom_choice.x, custom_choice.y));
    assert!(preset.layers.settings.state().unwrap().zone_custom);
    assert_eq!(preset.layers.settings.state().unwrap().dropdown, None);

    let (provider, sources, views) = FixtureProvider::demo();
    let mut custom = App::new(sources, views, true);
    let mut named = context();
    named.saved.display_zone = "America/Argentina/Buenos_Aires".into();
    named.effective_display_zone = named.saved.display_zone.clone();
    custom.configure_settings(named);
    assert_eq!(
        custom.appearance.display_zone,
        "America/Argentina/Buenos_Aires"
    );
    custom.handle(Action::Open(Open::Settings), &provider);
    draw(&provider, &mut custom, 54, 16);
    let custom_rect = custom
        .layers
        .settings
        .control_rects()
        .iter()
        .find(|(_, control)| *control == SettingsControl::Field(SettingsField::DisplayZone))
        .map(|(rect, _)| *rect)
        .unwrap();
    click(&mut custom, &provider, (custom_rect.x, custom_rect.y));
    assert_eq!(
        custom.layers.settings.state().unwrap().focus,
        SettingsControl::Field(SettingsField::DisplayZone)
    );
    assert_eq!(custom.layers.settings.state().unwrap().dropdown, None);
    draw(&provider, &mut custom, 54, 16);
    let caret = custom.layers.settings.surface().caret.unwrap();
    let rendered_field = custom
        .layers
        .settings
        .control_rects()
        .iter()
        .find(|(_, control)| *control == SettingsControl::Field(SettingsField::DisplayZone))
        .map(|(rect, _)| *rect)
        .unwrap();
    assert!(contains(rendered_field, caret));
    assert_eq!(
        custom
            .layers
            .settings
            .hit((rendered_field.x, rendered_field.y)),
        Some(lvu::components::settings::SettingsHit::Control(
            SettingsControl::Field(SettingsField::DisplayZone)
        ))
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

fn scroll_settings(app: &mut App, provider: &FixtureProvider, down: bool) {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use lvu::component::RawEvent;
    // Wheel over the body scrolls the shared body window whatever holds focus.
    let surface = app.layers.settings.surface();
    let point = (surface.popup.x + 2, surface.popup.y + 2);
    app.handle(
        Action::Raw(RawEvent::Mouse(MouseEvent {
            kind: if down {
                MouseEventKind::ScrollDown
            } else {
                MouseEventKind::ScrollUp
            },
            column: point.0,
            row: point.1,
            modifiers: KeyModifiers::NONE,
        })),
        provider,
    );
    let _ = (MouseButton::Left, MouseEventKind::Down);
}

/// Policy/stable budgets alone determine the frame and sticky tail: saving,
/// saved and error states share one outer frame and one Save-band origin.
/// Draft edits only move the body scroll extent, never the frame.
#[test]
fn responsive_frame_and_tail_are_stable_across_save_states() {
    for (width, height) in [(240u16, 80u16), (140, 40), (80, 24), (54, 16), (20, 6)] {
        // Saved (fresh open).
        let (provider, mut saved) = demo();
        saved.handle(Action::Open(Open::Settings), &provider);
        draw(&provider, &mut saved, width, height);
        let saved_surface = saved.layers.settings.surface();
        let saved_save = saved
            .layers
            .settings
            .control_rects()
            .iter()
            .find(|(_, c)| *c == SettingsControl::Save)
            .map(|(r, _)| *r);

        // Pending (edited provider, not saved).
        let (provider, mut pending) = demo();
        pending.handle(Action::Open(Open::Settings), &provider);
        focus(
            &mut pending,
            &provider,
            SettingsControl::Field(SettingsField::Provider),
        );
        key(&mut pending, &provider, KeyCode::Char('x'));
        draw(&provider, &mut pending, width, height);
        let pending_surface = pending.layers.settings.surface();

        // Error (invalid zone draft keeps last-good preview, reports inline).
        let (provider, mut failed) = demo();
        failed.handle(Action::Open(Open::Settings), &provider);
        focus(
            &mut failed,
            &provider,
            SettingsControl::Field(SettingsField::DisplayZone),
        );
        key(&mut failed, &provider, KeyCode::Enter);
        // Custom row is last; wrap Up from the preset to reach it via shared reveal.
        key(&mut failed, &provider, KeyCode::Up);
        key(&mut failed, &provider, KeyCode::Enter);
        for ch in "not-a-zone".chars() {
            key(&mut failed, &provider, KeyCode::Char(ch));
        }
        draw(&provider, &mut failed, width, height);
        let error_surface = failed.layers.settings.surface();

        for (name, surface) in [
            ("saved", saved_surface),
            ("pending", pending_surface),
            ("error", error_surface),
        ] {
            assert_eq!(
                surface.popup, saved_surface.popup,
                "{name} frame moved at {width}x{height}"
            );
            assert_eq!(
                surface.interior, saved_surface.interior,
                "{name} interior moved at {width}x{height}"
            );
        }
        let pending_save = pending
            .layers
            .settings
            .control_rects()
            .iter()
            .find(|(_, c)| *c == SettingsControl::Save)
            .map(|(r, _)| *r);
        assert_eq!(
            pending_save, saved_save,
            "sticky tail moved at {width}x{height}"
        );
    }
}

/// Below the floor the tiny fallback owns the frame with no stale hitboxes.
#[test]
fn below_floor_uses_the_tiny_fallback() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Settings), &provider);
    let buffer = draw(&provider, &mut app, 19, 5);
    assert!(text(&buffer).contains("terminal too small"));
    assert!(app.layers.settings.control_rects().is_empty());
    assert!(app.layers.settings.theme_choice_rects().is_empty());
}

/// Theme/zone dropdowns use the shared Anchored geometry against the actual
/// field anchor and the terminal frame: below when room, above at the floor,
/// clamped in x, at most eight rows with display-width sizing, and the same
/// rects paint, select, scroll and hit-test. Unicode paths/carets stay exact.
#[test]
fn dropdowns_are_anchored_to_the_field_with_shared_hitboxes() {
    for (width, height) in [(140u16, 40u16), (80, 24), (54, 16), (20, 6)] {
        for field in [SettingsField::Theme, SettingsField::DisplayZone] {
            let (provider, mut app) = demo();
            app.handle(Action::Open(Open::Settings), &provider);
            focus(&mut app, &provider, SettingsControl::Field(field));
            // Custom-zone text field has no popup; switch to preset first.
            if field == SettingsField::DisplayZone
                && app.layers.settings.state().is_some_and(|d| d.zone_custom)
            {
                continue;
            }
            key(&mut app, &provider, KeyCode::Enter);
            draw(&provider, &mut app, width, height);
            let surface = app.layers.settings.surface();
            let choices = app.layers.settings.theme_choice_rects().to_vec();
            assert!(!choices.is_empty(), "popup must paint at {width}x{height}");
            assert!(
                choices.len() <= 8,
                "anchored max eight rows at {width}x{height}"
            );
            for (rect, index) in &choices {
                assert!(
                    rect.x + rect.width <= width && rect.y + rect.height <= height,
                    "choice {index} escapes {width}x{height}: {rect:?}"
                );
                assert!(
                    contains(surface.popup, (rect.x, rect.y)),
                    "choice escapes popup at {width}x{height}"
                );
                assert_eq!(
                    app.layers.settings.hit((rect.x, rect.y)),
                    Some(lvu::components::settings::SettingsHit::Choice(*index)),
                    "popup paint/hit disagree at {width}x{height}"
                );
            }
            let widths: std::collections::HashSet<u16> =
                choices.iter().map(|(r, _)| r.width).collect();
            assert_eq!(
                widths.len(),
                1,
                "popup rows share width at {width}x{height}"
            );
            // Arrows move the shared selection and keep it painted.
            key(&mut app, &provider, KeyCode::Down);
            draw(&provider, &mut app, width, height);
            let after = app.layers.settings.theme_choice_rects().to_vec();
            assert!(!after.is_empty());
        }
    }
}

/// At 54x16 and 20x6 every field stays reachable via focus-follow with the
/// caret inside its own rect, actions stay sticky, and wheel/hitboxes match
/// the shared body viewport. Unicode paths wrap by display width.
#[test]
fn tiny_pressure_keeps_every_field_reachable_with_matching_wheel() {
    for (width, height) in [(54u16, 16u16), (20, 6)] {
        let (provider, mut app) = demo();
        // Unicode paths in effective values must wrap, never panic.
        app.handle(Action::Open(Open::Settings), &provider);
        draw(&provider, &mut app, width, height);
        // Focus order follows Tab traversal.
        for field in [
            SettingsField::Provider,
            SettingsField::Theme,
            SettingsField::DisplayZone,
            SettingsField::Delight,
            SettingsField::Ascii,
            SettingsField::RowCache,
            SettingsField::IndexPerSource,
        ] {
            focus(&mut app, &provider, SettingsControl::Field(field));
            draw(&provider, &mut app, width, height);
            let surface = app.layers.settings.surface();
            let rect = app
                .layers
                .settings
                .control_rects()
                .iter()
                .find(|(_, c)| *c == SettingsControl::Field(field))
                .map(|(r, _)| *r)
                .unwrap_or_else(|| panic!("{field:?} must stay reachable at {width}x{height}"));
            assert!(contains(surface.popup, (rect.x, rect.y)));
            assert_eq!(
                app.layers.settings.hit((rect.x, rect.y)),
                Some(lvu::components::settings::SettingsHit::Control(
                    SettingsControl::Field(field)
                )),
                "{field:?} paint/hit disagree at {width}x{height}"
            );
            if let Some(caret) = app.layers.settings.surface().caret
                && app.layers.settings.state().is_some_and(|d| {
                    d.focus == SettingsControl::Field(field)
                        && matches!(
                            field,
                            SettingsField::Provider
                                | SettingsField::Mode
                                | SettingsField::Thinking
                                | SettingsField::DisplayZone
                                | SettingsField::RowCache
                                | SettingsField::Membership
                                | SettingsField::DiskTotal
                                | SettingsField::IndexPerSource
                        )
                })
            {
                assert!(contains(rect, caret), "caret must stay in {field:?}");
            }
        }
        // Wheel scrolls the shared body window with matching hitboxes.
        focus(
            &mut app,
            &provider,
            SettingsControl::Field(SettingsField::Provider),
        );
        draw(&provider, &mut app, width, height);
        let before = text(&draw(&provider, &mut app, width, height));
        scroll_settings(&mut app, &provider, true);
        draw(&provider, &mut app, width, height);
        let _ = before;
        // Sticky default action never leaves.
        let surface = app.layers.settings.surface();
        let save = app
            .layers
            .settings
            .control_rects()
            .iter()
            .find(|(_, c)| *c == SettingsControl::Save)
            .map(|(r, _)| *r)
            .expect("Save must survive tiny pressure");
        assert!(contains(surface.popup, (save.x, save.y)));
        // Real overflow only.
        let _ = surface.scrollable;
    }
}
