//! Acceptance for the View layer as a component (docs/component-model.md §6.3
//! step 8). This is the first conversion that introduces an outbox for a
//! *view mutation* and the first that closes on a broadcast `ViewEvent`, so
//! what is asserted here is the seam rather than the drawing: a submission
//! leaves through the outbox and nowhere else, a refusal keeps the draft, the
//! shell's `SourcesChanged` broadcast is what closes the layer, and the
//! geometry `render` recorded is the geometry `hit()` answers with.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, RowProvider, SourceItem, ViewDialogMode,
    app::Focus,
    component::{Component, LayerId, Open, RawEvent},
    components::view::{ViewDialogControl, ViewHit},
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

fn ctrl(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::CONTROL))),
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

fn shift_tab(app: &mut App, provider: &FixtureProvider) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::SHIFT,
        ))),
        provider,
    );
}

fn type_name(app: &mut App, provider: &FixtureProvider, name: &str) {
    for _ in 0..64 {
        key(app, provider, KeyCode::Backspace);
    }
    for character in name.chars() {
        key(app, provider, KeyCode::Char(character));
    }
}

#[test]
fn a_submission_leaves_only_through_the_outbox_and_a_refusal_keeps_the_draft() {
    let (provider, mut app) = demo();
    let view = app.active_view_id().unwrap().to_owned();
    app.handle(Action::Open(Open::View), &provider);
    assert_eq!(app.focus, Focus::Layer);
    assert_eq!(app.layers.stack, vec![LayerId::View]);
    draw(&provider, &mut app, 90, 24);

    // An empty name never reaches the worker; it is refused where it was typed.
    type_name(&mut app, &provider, "   ");
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.layers.view.outbox.take().is_empty());
    assert_eq!(app.layers.view.error(), Some("view name cannot be empty"));
    assert!(app.layers.view.is_open(), "a refusal does not close it");

    type_name(&mut app, &provider, "Errors");
    key(&mut app, &provider, KeyCode::Enter);
    let requests = app.layers.view.outbox.take();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].name, "Errors");
    assert_eq!(requests[0].view_id, view);
    assert_eq!(requests[0].mode, ViewDialogMode::Clone);
    // Submitting is not closing: the layer waits for the worker's answer.
    assert!(app.layers.view.is_open());
    assert_eq!(app.layers.stack, vec![LayerId::View]);

    // The worker refuses. The draft survives so the name can be corrected.
    app.view_request_failed("a view with that name already exists".into());
    assert_eq!(
        app.layers.view.error(),
        Some("a view with that name already exists")
    );
    assert_eq!(app.layers.view.draft(), "Errors");
    assert!(app.layers.view.is_open());

    // The worker accepts. §4.2: the shell broadcasts what happened to the view
    // and the layer closes itself.
    app.view_request_succeeded(&view);
    assert!(!app.layers.view.is_open());
    assert!(app.layers.stack.is_empty());
    assert_eq!(app.focus, Focus::Logs);
}

#[test]
fn the_name_field_is_dialog_owned_and_only_it_takes_the_editing_chords() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::View), &provider);
    draw(&provider, &mut app, 90, 24);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Input);
    assert!(
        app.layers.view.surface().text_focus,
        "a focused name field takes q as a character"
    );

    type_name(&mut app, &provider, "abc");
    ctrl(&mut app, &provider, KeyCode::Char('a'));
    key(&mut app, &provider, KeyCode::Char('X'));
    assert_eq!(app.layers.view.draft(), "Xabc");
    ctrl(&mut app, &provider, KeyCode::Char('e'));
    key(&mut app, &provider, KeyCode::Char('Z'));
    assert_eq!(app.layers.view.draft(), "XabcZ");
    // `q` is text while the field has focus, not a dismissal.
    key(&mut app, &provider, KeyCode::Char('q'));
    assert_eq!(app.layers.view.draft(), "XabcZq");
    assert!(app.layers.view.is_open());

    // Tab moves to a button; the chords are then inert and `q` dismisses.
    key(&mut app, &provider, KeyCode::Tab);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Apply);
    draw(&provider, &mut app, 90, 24);
    assert!(!app.layers.view.surface().text_focus);
    ctrl(&mut app, &provider, KeyCode::Char('k'));
    assert_eq!(app.layers.view.draft(), "XabcZq", "a button does not edit");
    key(&mut app, &provider, KeyCode::Char('n'));
    assert_eq!(app.layers.view.draft(), "XabcZq");

    // A mode switch reseeds the draft from the view, so the caret follows it
    // rather than pointing into text that no longer exists.
    alt(&mut app, &provider, KeyCode::Char('b'));
    assert_eq!(app.layers.view.mode(), ViewDialogMode::Blank);
    assert_eq!(app.layers.view.draft(), "New view");
    key(&mut app, &provider, KeyCode::Char('!'));
    assert_eq!(app.layers.view.draft(), "New view!");
}

#[test]
fn membership_reads_the_shared_sources_and_hit_testing_matches_what_was_drawn() {
    let (provider, mut app) = demo();
    let view = app.active_view_id().unwrap().to_owned();
    let primary = app.view_source_ids(&view)[0].clone();
    for index in 0..4 {
        app.sources.push(SourceItem {
            id: format!("extra-{index}"),
            name: format!("extra source {index}"),
            health: "open".into(),
        });
    }
    app.handle(Action::Open(Open::View), &provider);
    alt(&mut app, &provider, KeyCode::Char('m'));
    let rendered = screen(&draw(&provider, &mut app, 94, 22));
    assert!(rendered.contains("Apply membership"), "{rendered}");
    // Responsive frame is policy-stable: a 6-row list scrolls inside it rather
    // than growing it, so the last row arrives via selection reveal, not on
    // the first frame. Short lists still show fully; long ones window.
    assert!(
        !app.layers.view.surface().text_focus,
        "a list is not a field"
    );

    // Every drawn row is hit-testable at the rect it was drawn in.
    for (rect, index) in app.layers.view.source_rects().to_vec() {
        assert_eq!(
            app.layers.view.hit((rect.x, rect.y)),
            Some(ViewHit::Source(index))
        );
    }
    // Reveal the last row by moving selection to it; the shared list geometry
    // windows it into view and paint/mouse share those rects.
    for _ in 0..5 {
        key(&mut app, &provider, KeyCode::Down);
    }
    let revealed = screen(&draw(&provider, &mut app, 94, 22));
    assert!(revealed.contains("extra source 3"), "{revealed}");
    for (rect, index) in app.layers.view.source_rects().to_vec() {
        assert_eq!(
            app.layers.view.hit((rect.x, rect.y)),
            Some(ViewHit::Source(index))
        );
    }
    let (rect, index) = *app.layers.view.source_rects().last().unwrap();
    click(&mut app, &provider, (rect.x, rect.y));
    assert_eq!(app.layers.view.selected_source(), index);

    key(&mut app, &provider, KeyCode::Char(' '));
    assert_eq!(
        app.layers.view.source_ids(),
        [primary.clone(), "extra-3".to_string()]
    );
    alt(&mut app, &provider, KeyCode::Up);
    assert_eq!(
        app.layers.view.source_ids(),
        ["extra-3".to_string(), primary.clone()]
    );

    // The owning source is not removable, and saying so is the dialog's job.
    // Selection is on the last row; reveal the first row again so the owning
    // source is back in the shared window before clicking it.
    for _ in 0..5 {
        key(&mut app, &provider, KeyCode::Up);
    }
    draw(&provider, &mut app, 94, 22);
    app.layers.view.outbox.take();
    let owning = app
        .layers
        .view
        .source_rects()
        .iter()
        .find(|(_, index)| app.sources[*index].id == primary)
        .map(|(rect, _)| (rect.x, rect.y))
        .expect("the owning source is on screen");
    click(&mut app, &provider, owning);
    key(&mut app, &provider, KeyCode::Char(' '));
    assert_eq!(
        app.layers.view.error(),
        Some("the owning source stays in this view")
    );

    key(&mut app, &provider, KeyCode::Enter);
    let request = app.layers.view.outbox.take().pop().unwrap();
    assert_eq!(request.mode, ViewDialogMode::Sources);
    assert_eq!(request.source_ids, ["extra-3".to_string(), primary]);
}

#[test]
fn the_shell_contains_the_modal_and_a_button_click_acts_where_a_field_only_focuses() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::View), &provider);
    draw(&provider, &mut app, 90, 24);
    let surface = app.layers.view.surface();
    assert_eq!(app.hit_regions.selection_modal, Some(surface.interior));
    assert!(
        surface.caret.is_some(),
        "the focused name field draws a caret"
    );

    // §5.2: a click outside the popup never reaches the log behind it.
    let selected = app.view_state().unwrap().selected.clone();
    click(&mut app, &provider, (0, 0));
    assert!(app.layers.view.is_open());
    assert_eq!(app.view_state().unwrap().selected, selected);

    // A header segment acts on click; choosing a mode is not submitting.
    let sources_tab = app
        .layers
        .view
        .tab_rects()
        .iter()
        .find(|(_, mode)| *mode == ViewDialogMode::Sources)
        .map(|(rect, _)| (rect.x, rect.y))
        .expect("the Sources segment is drawn");
    click(&mut app, &provider, sources_tab);
    assert_eq!(app.layers.view.mode(), ViewDialogMode::Sources);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Sources);
    assert!(
        app.layers.view.outbox.take().is_empty(),
        "choosing a mode is not submitting"
    );

    key(&mut app, &provider, KeyCode::Esc);
    assert!(!app.layers.view.is_open());
    assert_eq!(app.focus, Focus::Logs);
}

#[test]
fn modes_render_as_a_segmented_header_with_apply_as_the_only_button() {
    for (width, height) in [(90u16, 24u16), (80, 24), (54, 16)] {
        let (provider, mut app) = demo();
        app.handle(Action::Open(Open::View), &provider);
        let rendered = screen(&draw(&provider, &mut app, width, height));
        // The four modes are segments, never action buttons.
        for bracketed in ["[ New blank ]", "[ Clone ]", "[ Rename ]", "[ Sources ]"] {
            assert!(
                !rendered.contains(bracketed),
                "{width}x{height}: {bracketed} must not render as a button:\n{rendered}"
            );
        }
        for segment in ["New blank", "Clone", "Rename", "Sources"] {
            assert!(
                rendered.contains(segment),
                "{width}x{height}: missing header segment {segment}:\n{rendered}"
            );
        }
        // Exactly one action button.
        assert!(
            rendered.contains("[ Apply ]"),
            "{width}x{height}:\n{rendered}"
        );
        assert!(
            !rendered.contains("[ Apply membership ]"),
            "Clone mode applies a name, not membership:\n{rendered}"
        );
        // Geometry agrees: four header rects plus one action rect.
        assert_eq!(app.layers.view.tab_rects().len(), 4, "{width}x{height}");
        let controls = app.layers.view.control_rects();
        assert_eq!(controls.len(), 1, "{width}x{height}: {controls:?}");
        assert_eq!(controls[0].1, ViewDialogControl::Apply);
        // Every drawn rect hit-tests to what it drew.
        for (rect, mode) in app.layers.view.tab_rects().to_vec() {
            assert_eq!(
                app.layers.view.hit((rect.x, rect.y)),
                Some(ViewHit::Tab(mode)),
                "{width}x{height}"
            );
        }
        for (rect, control) in app.layers.view.control_rects().to_vec() {
            assert_eq!(
                app.layers.view.hit((rect.x, rect.y)),
                Some(ViewHit::Control(control)),
                "{width}x{height}"
            );
        }
    }

    // Sources mode relabels the one action and keeps the same header.
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::View), &provider);
    alt(&mut app, &provider, KeyCode::Char('m'));
    let rendered = screen(&draw(&provider, &mut app, 90, 24));
    assert!(rendered.contains("[ Apply membership ]"), "{rendered}");
    for bracketed in ["[ New blank ]", "[ Clone ]", "[ Rename ]", "[ Sources ]"] {
        assert!(
            !rendered.contains(bracketed),
            "{bracketed} leaked:\n{rendered}"
        );
    }
    assert_eq!(app.layers.view.tab_rects().len(), 4);
    assert_eq!(app.layers.view.control_rects().len(), 1);
}

#[test]
fn header_focus_moves_and_selects_modes_without_touching_drafts_or_membership() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::View), &provider);
    draw(&provider, &mut app, 90, 24);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Input);

    // Tab cycles header → body → action, never through four faux buttons.
    key(&mut app, &provider, KeyCode::Tab);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Apply);
    key(&mut app, &provider, KeyCode::Tab);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Tabs);
    key(&mut app, &provider, KeyCode::Tab);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Input);
    shift_tab(&mut app, &provider);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Tabs);
    shift_tab(&mut app, &provider);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Apply);

    // Left/Right on the header moves and selects immediately, keeping focus.
    key(&mut app, &provider, KeyCode::Tab);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Tabs);
    // Opened in Clone; Left reaches Blank, Right returns to Clone.
    key(&mut app, &provider, KeyCode::Left);
    assert_eq!(app.layers.view.mode(), ViewDialogMode::Blank);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Tabs);
    assert_eq!(app.layers.view.draft(), "New view");
    key(&mut app, &provider, KeyCode::Right);
    assert_eq!(app.layers.view.mode(), ViewDialogMode::Clone);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Tabs);

    // Enter and Space on the header only select; they never submit.
    app.layers.view.outbox.take();
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.layers.view.outbox.take().is_empty());
    key(&mut app, &provider, KeyCode::Char(' '));
    assert!(app.layers.view.outbox.take().is_empty());
    assert_eq!(app.layers.view.mode(), ViewDialogMode::Clone);

    // A mode switch reseeds the name; typing then Alt reaches Sources, whose
    // Space toggles membership rather than typing. An extra source makes the
    // toggle observable: the owning source alone can never leave.
    alt(&mut app, &provider, KeyCode::Char('b'));
    assert_eq!(app.layers.view.mode(), ViewDialogMode::Blank);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Input);
    key(&mut app, &provider, KeyCode::Char('!'));
    assert_eq!(app.layers.view.draft(), "New view!");
    app.sources.push(SourceItem {
        id: "extra-0".into(),
        name: "extra source 0".into(),
        health: "open".into(),
    });
    alt(&mut app, &provider, KeyCode::Char('m'));
    assert_eq!(app.layers.view.mode(), ViewDialogMode::Sources);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Sources);
    // Select the extra source so Space has something to add.
    draw(&provider, &mut app, 90, 24);
    let extra = app
        .layers
        .view
        .source_rects()
        .iter()
        .find(|(_, index)| app.sources[*index].id == "extra-0")
        .map(|(rect, _)| (rect.x, rect.y))
        .expect("the extra source is drawn");
    click(&mut app, &provider, extra);
    let before = app.layers.view.source_ids().to_vec();
    // Header Space must not toggle; body Space must. From Sources, Tab runs
    // Sources → Apply → Tabs.
    key(&mut app, &provider, KeyCode::Tab);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Apply);
    key(&mut app, &provider, KeyCode::Tab);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Tabs);
    key(&mut app, &provider, KeyCode::Char(' '));
    assert_eq!(app.layers.view.source_ids(), before);
    key(&mut app, &provider, KeyCode::Tab);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Sources);
    draw(&provider, &mut app, 90, 24);
    key(&mut app, &provider, KeyCode::Char(' '));
    assert_ne!(app.layers.view.source_ids(), before);
}

#[test]
fn responsive_frame_is_policy_stable_across_modes_lists_and_sizes() {
    use lvu::dialog_layout::{PresentationKind, policy_size};

    // Required matrix: huge, medium, standard, compact, floor and below-floor.
    // Outer frames come from SelfContainedForm policy alone; Clone (1-row form)
    // and Sources (list) share one frame and sticky tail origins at each size.
    for (width, height) in [(240u16, 80u16), (140, 40), (80, 24), (54, 16), (20, 6)] {
        for mode in [ViewDialogMode::Clone, ViewDialogMode::Sources] {
            let (provider, mut app) = demo();
            // Long list for the Sources mode: 6 rows force the shared window.
            if mode == ViewDialogMode::Sources {
                for index in 0..4 {
                    app.sources.push(SourceItem {
                        id: format!("extra-{index}"),
                        name: format!("extra source {index}"),
                        health: "open".into(),
                    });
                }
            }
            app.handle(Action::Open(Open::View), &provider);
            if mode == ViewDialogMode::Sources {
                alt(&mut app, &provider, KeyCode::Char('m'));
            }
            let buffer = draw(&provider, &mut app, width, height);
            let rendered = screen(&buffer);
            let surface = app.layers.view.surface();
            let (want_w, want_h) = policy_size(
                ratatui::layout::Rect::new(0, 0, width, height),
                PresentationKind::SelfContainedForm,
            );
            assert_eq!(
                (surface.popup.width, surface.popup.height),
                (want_w, want_h),
                "{width}x{height} {mode:?} frame must be policy"
            );
            // No ordinary dialog becomes full-frame except the 20x6 safety floor.
            if (width, height) != (20, 6) {
                assert!(
                    surface.popup.width < width || surface.popup.height < height,
                    "{width}x{height} became full frame"
                );
            }
            // Segmented header is the only mode control; sole Apply default.
            assert_eq!(app.layers.view.tab_rects().len(), 4, "{width}x{height}");
            assert_eq!(app.layers.view.control_rects().len(), 1, "{width}x{height}");
            if mode == ViewDialogMode::Sources {
                // At the 20x6 floor the 20-cell "Apply membership" button clips
                // to its 16-cell band; the default still survives as "Apply".
                if (width, height) == (20, 6) {
                    assert!(rendered.contains("Apply"), "{rendered}");
                } else {
                    assert!(rendered.contains("Apply membership"), "{rendered}");
                }
            } else {
                assert!(rendered.contains("[ Apply ]"), "{rendered}");
                // Caret lives inside the Name field rect, same authority paint
                // and hitboxes share.
                let caret = surface.caret.expect("name field draws a caret");
                assert!(
                    caret.0 >= surface.popup.x && caret.0 < surface.popup.right(),
                    "{width}x{height} caret escapes frame"
                );
                // The name form never scrolls: name modes are never scrollable.
                assert!(
                    !surface.scrollable,
                    "{width}x{height} {mode:?} form must not want the wheel"
                );
            }
            // Six sources overflow the shared list viewport at 80x24 and below,
            // so the Sources mode wants the wheel exactly there; at roomy
            // 140x40 the same six fit and it does not.
            if mode == ViewDialogMode::Sources {
                if (width, height) == (80, 24) {
                    assert!(
                        surface.scrollable,
                        "80x24 Sources list overflows and must want the wheel"
                    );
                } else if (width, height) == (140, 40) {
                    assert!(
                        !surface.scrollable,
                        "140x40 fits six sources and must not want the wheel"
                    );
                }
            }
            // Every drawn rect hit-tests to what it drew (shared projection).
            // At the 20x6 floor the four segments overflow their 16-cell header
            // and clip; the frame and default still survive, but exact tab
            // hitboxes cannot all be distinct there.
            if (width, height) != (20, 6) {
                for (rect, m) in app.layers.view.tab_rects().to_vec() {
                    assert_eq!(
                        app.layers.view.hit((rect.x, rect.y)),
                        Some(ViewHit::Tab(m)),
                        "{width}x{height}"
                    );
                }
            }
            for (rect, control) in app.layers.view.control_rects().to_vec() {
                assert_eq!(
                    app.layers.view.hit((rect.x, rect.y)),
                    Some(ViewHit::Control(control)),
                    "{width}x{height}"
                );
            }
            for (rect, index) in app.layers.view.source_rects().to_vec() {
                assert_eq!(
                    app.layers.view.hit((rect.x, rect.y)),
                    Some(ViewHit::Source(index)),
                    "{width}x{height}"
                );
            }
            // Keyboard: Tab cycles Tabs→body→Apply; mouse: header click switches
            // mode without submitting, Apply click submits via outbox.
            if (width, height) == (80, 24) {
                if mode == ViewDialogMode::Clone {
                    key(&mut app, &provider, KeyCode::Tab);
                    assert_eq!(app.layers.view.control(), ViewDialogControl::Apply);
                    // Enter on Apply submits (outbox), not closes.
                    key(&mut app, &provider, KeyCode::Enter);
                    assert_eq!(app.layers.view.outbox.take().len(), 1);
                } else {
                    // Sources list: Down moves selection, Space toggles, Enter
                    // submits membership; all preserve stable frame.
                    let before_frame = app.layers.view.surface().popup;
                    key(&mut app, &provider, KeyCode::Down);
                    draw(&provider, &mut app, width, height);
                    assert_eq!(app.layers.view.surface().popup, before_frame);
                }
            }
        }
        // Identical outer geometry across modes at this size.
        let (provider, mut app) = demo();
        for index in 0..4 {
            app.sources.push(SourceItem {
                id: format!("x-{index}"),
                name: format!("x {index}"),
                health: "open".into(),
            });
        }
        app.handle(Action::Open(Open::View), &provider);
        draw(&provider, &mut app, width, height);
        let clone_frame = app.layers.view.surface().popup;
        alt(&mut app, &provider, KeyCode::Char('m'));
        draw(&provider, &mut app, width, height);
        let sources_frame = app.layers.view.surface().popup;
        assert_eq!(
            clone_frame, sources_frame,
            "{width}x{height} frame must not move with mode"
        );
    }

    // Below the 20x6 floor the tiny fallback owns the frame, not the dialog.
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::View), &provider);
    let tiny = screen(&draw(&provider, &mut app, 19, 5));
    assert!(tiny.contains("terminal too small"), "{tiny}");

    // At the 20x6 floor the default survives with one body row.
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::View), &provider);
    let floor = screen(&draw(&provider, &mut app, 20, 6));
    assert!(floor.contains("Apply"), "{floor}");
    assert_eq!(app.layers.view.tab_rects().len(), 4);
}

#[test]
fn a_layer_that_edits_the_active_view_does_not_open_without_one() {
    let (provider, sources, _) = FixtureProvider::demo();
    let mut app = App::new(sources, Vec::new(), true);
    assert!(app.active_view_id().is_none());
    app.handle(Action::Open(Open::View), &provider);
    // An empty workspace already has Add source on the stack, so what this
    // asserts is that View did not join it.
    assert!(
        !app.layers.stack.contains(&LayerId::View),
        "nothing to edit, nothing opens"
    );
    assert!(!app.layers.view.is_open());
}
