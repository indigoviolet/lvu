//! A late restore must not overwrite drafts typed after its fence was seeded.
//!
//! Regression context: opening Filter and typing immediately could lose the
//! first characters when a memory load completed mid-typing and bulk-restored
//! persisted drafts over them. The load-side fence is seeded before any typing
//! happens; every keystroke bumps the view's interaction revision through
//! `touch`, so a restore carrying the pre-typing fence must refuse once the
//! user has typed, while a fence taken against the untouched view still
//! applies. Both halves are pinned here at the `App` seam the shell and the
//! memory lifecycle share.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lvu::{
    Action, App, PersistentViewState,
    component::{Open, RawEvent},
    fixture::FixtureProvider,
};

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

fn active_id(app: &App) -> String {
    app.active_view_id().expect("demo view").to_owned()
}

fn type_text(app: &mut App, provider: &FixtureProvider, text: &str) {
    for character in text.chars() {
        app.handle(
            Action::Raw(RawEvent::Key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::NONE,
            ))),
            provider,
        );
    }
}

fn search_draft(app: &App) -> Option<String> {
    app.search_state().map(|state| state.draft.clone())
}

#[test]
fn a_restore_fenced_before_typing_refuses_once_the_user_types() {
    let (provider, mut app) = demo();
    let view_id = active_id(&app);
    // The fence a load request seeds before any typing happens.
    let fence = app.view_interaction_revision(&view_id).unwrap_or_default();
    app.handle(Action::Open(Open::Search), &provider);
    type_text(&mut app, &provider, "qzz");
    assert_eq!(search_draft(&app).as_deref(), Some("qzz"));
    // A load completing now carries the pre-typing fence and must refuse:
    // the persisted (empty) drafts must not clobber what was just typed.
    let applied =
        app.restore_persistent_view_if_unmodified(&view_id, fence, PersistentViewState::default());
    assert!(
        !applied,
        "stale-fenced restore overwrote freshly typed drafts"
    );
    assert_eq!(search_draft(&app).as_deref(), Some("qzz"));
}

#[test]
fn a_fresh_fence_still_restores_into_an_untouched_view() {
    let (_provider, mut app) = demo();
    let view_id = active_id(&app);
    // No typing happened, so the fence matches and the remembered draft lands.
    let fence = app.view_interaction_revision(&view_id).unwrap_or_default();
    let restored = PersistentViewState {
        search_draft: "remembered".to_owned(),
        ..Default::default()
    };
    assert!(app.restore_persistent_view_if_unmodified(&view_id, fence, restored));
    assert_eq!(search_draft(&app).as_deref(), Some("remembered"));
}
