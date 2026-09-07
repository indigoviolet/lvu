use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lvu::{Action, App, app::Focus, fixture::FixtureProvider};
#[test]
fn dbg() {
    let (provider, sources, views) = FixtureProvider::nested_demo();
    let mut app = App::new(sources, views, true);
    app.sync_provider(&provider, 10);
    app.handle(Action::Top, &provider);
    app.handle(Action::ToggleDetails, &provider);
    app.focus = Focus::Details;
    for code in [
        KeyCode::Down,
        KeyCode::Down,
        KeyCode::Enter,
        KeyCode::Down,
        KeyCode::Down,
        KeyCode::Down,
        KeyCode::Right,
    ] {
        let action = app.key_to_action(KeyEvent::new(code, KeyModifiers::NONE));
        println!("{code:?} -> {action:?}");
        app.handle(action, &provider);
        println!(
            "  cursor={} rows={} focus={:?}",
            app.view_state().unwrap().details_cursor,
            app.details_rows(&provider).len(),
            app.focus
        );
    }
}
