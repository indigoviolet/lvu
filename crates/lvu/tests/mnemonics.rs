//! Acceptance for docs/dialog-system.md §8.10's accelerator rule.
//!
//! The letter a button underlines is the key that presses it. When no text
//! field has focus the bare letter does it, because there the letter is not
//! text; when a text field has focus only Alt+letter does. Alt+letter always
//! works. The rule is implemented once, in `App::dispatch_raw` over
//! `Component::action_labels`, so these assertions are made against every
//! layer rather than against one dialog's keymap.
//!
//! The defect this replaces: Fields drew six underlined letters and bound
//! three of them (`x`, `f`, `d`) only under Alt, which the user's xterm never
//! sends as a chord — it sends `æ`, `ø`, `ä` — while `c` happened to be bound
//! bare from before mnemonics existed. An unbound letter inside a dialog does
//! not fall through to the base screen either, so those three did nothing.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use lvu::{
    Action, App,
    app::Focus,
    component::{Open, RawEvent},
    dialog_controls::{mnemonic_key, mnemonic_press},
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend};

const WIDTH: u16 = 110;
const HEIGHT: u16 = 34;

fn app() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::nested_demo();
    let mut app = App::new(sources, views, true);
    app.sync_provider(&provider, 10);
    app.handle(Action::Top, &provider);
    (provider, app)
}

fn press(app: &mut App, provider: &FixtureProvider, code: KeyCode, modifiers: KeyModifiers) {
    let key = KeyEvent::new(code, modifiers);
    let action = if app.focus == Focus::Layer {
        Action::Raw(RawEvent::Key(key))
    } else {
        app.key_to_action(key)
    };
    app.handle(action, provider);
}

/// Draw once. The shell renders between every key and the next, and
/// `Component::text_focus` defaults to what the last render published (§1), so
/// a test that changes focus and presses a key without a frame in between is
/// asking the layer a question the real loop never asks.
fn settle(app: &mut App, provider: &FixtureProvider) {
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, Theme::LOVE_DARK, None))
        .unwrap();
}

/// Everything a key press could plausibly have changed, as one string: the
/// whole rendered surface, the status notice, where focus is, and any query
/// the press submitted. Comparing digests is how "the bare letter presses the
/// same button as Alt+letter" is asserted without this test having to know
/// what each of the thirty-odd buttons does.
fn digest(app: &mut App, provider: &FixtureProvider) -> String {
    let requests = app
        .take_query_requests()
        .iter()
        .map(|request| format!("{:?}", request.constraints))
        .collect::<Vec<_>>()
        .join("|");
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, Theme::LOVE_DARK, None))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let screen = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "focus={:?}\nnotice={:?}\nqueries={requests}\n{screen}",
        app.focus, app.action_notice
    )
}

/// Every layer that draws an action row, with the `Open` that reaches it.
/// `RecipeHistory` is reached by `Replace` from Recipes rather than opened, and
/// the enrichment step editor is a child; both are listed so the inventory
/// below is over every action row in the product, not a chosen subset.
fn layers() -> Vec<(&'static str, Open)> {
    vec![
        ("Fields", Open::Fields),
        ("Storage", Open::Storage),
        ("Time", Open::Time),
        ("Help", Open::Help),
        ("Settings", Open::Settings),
        ("View", Open::View),
        ("Source", Open::Source),
        ("Folding", Open::Folding),
        ("Search", Open::Search),
        ("Advanced", Open::Advanced),
        ("Grouping", Open::Grouping),
        ("Bookmarks", Open::Bookmarks),
        ("Enrichment", Open::Enrichment),
        (
            "EnrichmentStep",
            Open::EnrichmentStep {
                editing: None,
                prefill: None,
            },
        ),
        (
            "ExternalCommand",
            Open::ExternalCommand {
                stage: None,
                insert_at: usize::MAX,
            },
        ),
        (
            "Recipes",
            Open::Recipes {
                mode: lvu::app::RecipeDialogMode::Browse,
            },
        ),
        ("Ask", Open::Ask(lvu::components::ask::AskOpen::Generic)),
        ("Investigation", Open::Investigation),
        ("View summary", Open::ViewSummary),
    ]
}

/// The mnemonics each layer offers in the fixture's opening state, as the
/// letters they underline. This is the inventory the rest of the file walks;
/// it is asserted against the running layers so a mnemonic added, removed or
/// respelled shows up here rather than quietly losing its key.
fn inventory(provider: &FixtureProvider, app: &mut App) -> Vec<(&'static str, Vec<char>, bool)> {
    let mut found = Vec::new();
    for (name, open) in layers() {
        app.handle(Action::Open(open), provider);
        settle(app, provider);
        let letters: Vec<char> = app
            .top_layer_action_labels(provider)
            .iter()
            .filter_map(|label| mnemonic_key(label))
            .collect();
        found.push((name, letters, app.top_layer_text_focus()));
        while app.focus == Focus::Layer {
            press(app, provider, KeyCode::Esc, KeyModifiers::NONE);
        }
    }
    found
}

#[test]
fn every_action_row_mnemonic_in_the_product_is_accounted_for() {
    let (provider, mut app) = app();
    let found = inventory(&provider, &mut app);
    // The third column is whether the layer has a text field focused when it
    // opens, which is what decides whether its bare letters are live.
    let expected: Vec<(&str, Vec<char>, bool)> = vec![
        ("Fields", vec!['p', 'f', 'x', 'c', 'd', 'r'], false),
        ("Storage", vec!['r', 'c'], false),
        ("Time", vec!['c', 't'], false),
        // Help has no action row at all (§8.9), so it has no mnemonic and no
        // key of this kind is ever diverted from it.
        ("Help", vec![], false),
        // Settings opens with its theme dropdown owning the surface, so it
        // publishes no text focus; it has no mnemonic either way.
        ("Settings", vec![], false),
        // View opens with the caret in its Name field, so its four letters
        // are text until Tab moves focus off it — which is the rule working,
        // and why its palette rows print the Alt chord.
        ("View", vec!['b', 'c', 'r', 's'], true),
        ("Source", vec![], true),
        ("Folding", vec![], false),
        // The Filter dialog opens with the caret in the active tab's field;
        // `Clear` and the two tab segments carry the letters (§12.1).
        ("Search", vec!['c', 's', 'a'], true),
        ("Advanced", vec!['c', 's', 'a'], true),
        ("Grouping", vec![], true),
        ("Bookmarks", vec![], false),
        ("Enrichment", vec!['a', 'e', 'r', 'c'], false),
        ("EnrichmentStep", vec![], true),
        ("ExternalCommand", vec!['s', 'r', 'm', 'n'], true),
        // Recipes reports a text focus whenever its `More ▾` menu is closed,
        // which suppresses its bare letters; Alt still presses its buttons and
        // `x` stays bound for Reject. Tightening that is tied to what `q` does
        // in Recipes and is not this rule's business.
        ("Recipes", vec!['s', 'u', 'h'], true),
        // Ask opens on its Request field with `&Submit` alone in the row, so
        // `s` is text until Tab moves focus off it and Alt-S presses it from
        // anywhere. Its later rows mark letters this inventory cannot reach by
        // opening the dialog — `&Apply`, `&Cancel request` and, when the
        // sample was thin, `Ask again with a &wider sample`; no row claims a
        // letter twice, which the dedup below checks for the rows it sees and
        // `each_letter_resolves_to_the_button_that_underlines_it` checks live.
        ("Ask", vec!['s'], true),
        // Investigation still marks none: `Send`, `Resume`, `Start`, `Open`,
        // `New snapshot`. Listed so a mnemonic added to it is caught here
        // rather than by a user finding a dead key.
        ("Investigation", vec![], true),
        // The summary is a read-only list with one button, so its letter is
        // live from the moment it opens.
        ("View summary", vec!['o'], false),
    ];
    assert_eq!(found, expected);

    // No layer offers the same letter twice: a row where two buttons claim one
    // letter has a key the user cannot predict.
    for (name, letters, _) in &found {
        let mut sorted = letters.clone();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "{name} claims a letter twice");
    }
}

#[test]
fn the_bare_underlined_letter_presses_the_same_button_as_alt() {
    let (provider, mut reference) = app();
    let inventory = inventory(&provider, &mut reference);
    for (name, open) in layers() {
        let Some((_, letters, text_focus)) =
            inventory.iter().find(|(layer, ..)| *layer == name).cloned()
        else {
            continue;
        };
        let mut acted = false;
        for letter in letters {
            let (provider, mut with_alt) = app();
            with_alt.handle(Action::Open(open.clone()), &provider);
            let untouched = digest(&mut with_alt, &provider);
            press(
                &mut with_alt,
                &provider,
                KeyCode::Char(letter),
                KeyModifiers::ALT,
            );
            let alt = digest(&mut with_alt, &provider);
            acted |= alt != untouched;

            let (provider, mut bare) = app();
            bare.handle(Action::Open(open.clone()), &provider);
            settle(&mut bare, &provider);
            press(
                &mut bare,
                &provider,
                KeyCode::Char(letter),
                KeyModifiers::NONE,
            );
            let plain = digest(&mut bare, &provider);

            if text_focus {
                // A focused text field takes the letter as text (§8.10). The
                // press and the typed character cannot both be no-ops, so a
                // difference here is the rule holding.
                assert_ne!(
                    plain, alt,
                    "{name}: a focused text field takes `{letter}` as text"
                );
                continue;
            }
            assert_eq!(
                plain, alt,
                "{name}: bare `{letter}` and Alt-{letter} must press the same button"
            );

            let (provider, mut upper) = app();
            upper.handle(Action::Open(open.clone()), &provider);
            settle(&mut upper, &provider);
            press(
                &mut upper,
                &provider,
                KeyCode::Char(letter.to_ascii_uppercase()),
                KeyModifiers::SHIFT,
            );
            assert_eq!(
                digest(&mut upper, &provider),
                alt,
                "{name}: `{letter}` matches case-insensitively"
            );
        }
        // A row whose every letter is inert is the defect this rule replaces.
        // Not every letter can act in the fixture's opening state — View opens
        // in Clone mode, so `c` is idempotent there — but a whole row cannot be
        // dead.
        if inventory
            .iter()
            .any(|(layer, letters, _)| *layer == name && !letters.is_empty())
        {
            assert!(acted, "{name}: no letter on this action row does anything");
        }
    }
}

/// The routing itself: each underlined letter resolves to the button that
/// underlines it and to no other, over the labels the layers really draw.
#[test]
fn each_letter_resolves_to_the_button_that_underlines_it() {
    let (provider, mut app) = app();
    for (name, open) in layers() {
        app.handle(Action::Open(open), &provider);
        settle(&mut app, &provider);
        let labels = app.top_layer_action_labels(&provider);
        for (index, label) in labels.iter().enumerate() {
            let Some(letter) = mnemonic_key(label) else {
                continue;
            };
            let bare = KeyEvent::new(KeyCode::Char(letter), KeyModifiers::NONE);
            let alt = KeyEvent::new(KeyCode::Char(letter), KeyModifiers::ALT);
            assert_eq!(
                mnemonic_press(&labels, &bare, false),
                Some(index),
                "{name}: `{letter}` must resolve to {label}"
            );
            assert_eq!(
                mnemonic_press(&labels, &alt, true),
                Some(index),
                "{name}: Alt-{letter} must resolve to {label} even in a text field"
            );
            assert_eq!(
                mnemonic_press(&labels, &bare, true),
                None,
                "{name}: `{letter}` is text while a text field has focus"
            );
        }
        while app.focus == Focus::Layer {
            press(&mut app, &provider, KeyCode::Esc, KeyModifiers::NONE);
        }
    }
}

#[test]
fn a_focused_text_field_takes_the_letter_as_text_and_alt_still_presses_the_button() {
    // View opens with the caret in its Name field. `r` is `&Rename`'s
    // mnemonic; typed there it must be a character.
    use lvu::components::view::ViewDialogControl;
    let (provider, mut app) = app();
    app.handle(Action::Open(Open::View), &provider);
    settle(&mut app, &provider);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Input);
    assert!(app.top_layer_text_focus());

    let before = app.layers.view.draft().to_owned();
    let mode = app.layers.view.mode();
    press(&mut app, &provider, KeyCode::Char('r'), KeyModifiers::NONE);
    assert_eq!(
        app.layers.view.draft(),
        format!("{before}r"),
        "in a focused text field the letter is text, not a button press"
    );
    assert_eq!(app.layers.view.mode(), mode, "and it did not press Rename");

    // Alt reaches the button from inside the field.
    press(&mut app, &provider, KeyCode::Char('r'), KeyModifiers::ALT);
    assert_eq!(app.layers.view.mode(), lvu::ViewDialogMode::Rename);

    // Tab off the field and the bare letter is a key again. The shell draws
    // between the Tab and the next key, and View publishes its text focus from
    // that frame (§1).
    while app.layers.view.control() == ViewDialogControl::Input {
        press(&mut app, &provider, KeyCode::Tab, KeyModifiers::NONE);
    }
    settle(&mut app, &provider);
    assert!(!app.top_layer_text_focus());
    let draft = app.layers.view.draft().to_owned();
    press(&mut app, &provider, KeyCode::Char('b'), KeyModifiers::NONE);
    assert_eq!(app.layers.view.mode(), lvu::ViewDialogMode::Blank);
    assert_ne!(
        app.layers.view.draft(),
        format!("{draft}b"),
        "off the field the letter is a key, not text"
    );
}

/// Named, because a keystroke story depends on it: Alt-B in View selects the
/// blank mode, so a new view starts without the constraints of the one it was
/// opened from.
#[test]
fn alt_selects_a_view_mode_from_inside_the_name_field() {
    let (provider, mut app) = app();
    app.handle(Action::Open(Open::View), &provider);
    settle(&mut app, &provider);
    for (letter, mode) in [
        ('b', lvu::ViewDialogMode::Blank),
        ('r', lvu::ViewDialogMode::Rename),
        ('s', lvu::ViewDialogMode::Sources),
        ('c', lvu::ViewDialogMode::Clone),
    ] {
        press(
            &mut app,
            &provider,
            KeyCode::Char(letter),
            KeyModifiers::ALT,
        );
        settle(&mut app, &provider);
        assert_eq!(
            app.layers.view.mode(),
            mode,
            "Alt-{letter} must select {mode:?}"
        );
    }
}

#[test]
fn inside_a_dialog_the_mnemonic_wins_over_the_base_screen_key() {
    // `d` toggles the Details pane and `f` toggles follow on the base screen.
    // Inside Fields they are `Fol&d` and `&Filter`, and the layer owns the
    // keys (§7.5): the base bindings do not reach it, and neither does an
    // unbound letter fall through to them.
    let (provider, mut app) = app();
    let details_before = app.show_details;
    let follow_before = app.view_state().map(|state| state.follow);
    app.handle(Action::Open(Open::Fields), &provider);
    press(&mut app, &provider, KeyCode::Char('d'), KeyModifiers::NONE);
    assert_eq!(
        app.show_details, details_before,
        "`d` in Fields folds; it does not reach the base screen's Details toggle"
    );
    assert!(
        app.view_state().is_some_and(|state| state.fold_enabled),
        "`d` folded by the selected field"
    );
    press(&mut app, &provider, KeyCode::Char('f'), KeyModifiers::NONE);
    assert_eq!(
        app.view_state().map(|state| state.follow),
        follow_before,
        "`f` in Fields filters; it does not toggle follow"
    );
    assert!(
        !app.take_query_requests().is_empty(),
        "`f` submitted the filter"
    );
}

#[test]
fn a_chord_that_is_not_alt_is_not_a_mnemonic() {
    let labels = ["&Filter", "E&xclude", "Fol&d"];
    let bare = |code| KeyEvent::new(KeyCode::Char(code), KeyModifiers::NONE);
    let with = |code, modifiers| KeyEvent::new(KeyCode::Char(code), modifiers);

    let labels = &labels[..];
    assert_eq!(mnemonic_press(labels, &bare('x'), false), Some(1));
    assert_eq!(mnemonic_press(labels, &bare('X'), false), Some(1));
    assert_eq!(mnemonic_press(labels, &bare('x'), true), None);
    assert_eq!(
        mnemonic_press(labels, &with('x', KeyModifiers::ALT), true),
        Some(1)
    );
    // Ctrl-X stays Ctrl-X in both states, and a key that is not a letter of
    // any label presses nothing.
    assert_eq!(
        mnemonic_press(labels, &with('x', KeyModifiers::CONTROL), false),
        None
    );
    assert_eq!(mnemonic_press(labels, &bare('z'), false), None);
    // A release is not a press.
    let mut release = bare('x');
    release.kind = KeyEventKind::Release;
    assert_eq!(mnemonic_press(labels, &release, false), None);
}
