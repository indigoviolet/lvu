use std::{
    collections::VecDeque,
    io::{self, IsTerminal, Stdout, Write},
    panic::{self, AssertUnwindSafe},
    time::{Duration, Instant},
};

use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{
    app::{Action, App, QueryCompletion, QueryFailure, QueryPurpose, QueryRequest, key_to_action},
    command_palette::{Palette, PaletteContext, PaletteOutcome},
    delight::{ANIMATION_TICK, ActivityState, DelightConfig, StartupDelight},
    provider::RowProvider,
    ui,
};

const EVENT_POLL: Duration = Duration::from_millis(25);
const MIN_REDRAW_INTERVAL: Duration = Duration::from_millis(33);
const MAX_QUERY_COMPLETIONS_PER_TICK: usize = 32;

/// Nonblocking query seam. `submit` must only enqueue bounded work and `poll`
/// must return immediately. Adapters may finish requests out of order, but must
/// publish provider membership only for the newest per-view composite revision.
pub trait QueryDispatcher {
    fn submit(&mut self, request: QueryRequest) -> Result<(), String>;
    fn poll(&mut self) -> Option<QueryCompletion>;
}

/// Explicit demo placeholder. It reports unsupported requests asynchronously
/// through the same completion path used by a production worker.
pub struct UnwiredQueryDispatcher {
    completions: VecDeque<QueryCompletion>,
}

impl UnwiredQueryDispatcher {
    pub fn new() -> Self {
        Self {
            completions: VecDeque::new(),
        }
    }
}

impl Default for UnwiredQueryDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryDispatcher for UnwiredQueryDispatcher {
    fn submit(&mut self, request: QueryRequest) -> Result<(), String> {
        if self.completions.len() >= MAX_QUERY_COMPLETIONS_PER_TICK {
            return Err("query completion queue is full".into());
        }
        let failed_purpose = if request.purpose == QueryPurpose::Enrichment {
            QueryPurpose::Enrichment
        } else if request.constraints.advanced_polars.is_some() {
            QueryPurpose::Advanced
        } else {
            request.purpose
        };
        let message = match failed_purpose {
            QueryPurpose::Search => {
                "native text-query adapter is not wired; applied search is unchanged"
            }
            QueryPurpose::Advanced => {
                "advanced Polars adapter is not wired; applied filter is unchanged"
            }
            QueryPurpose::Enrichment => {
                "native enrichment adapter is not wired; applied enrichment is unchanged"
            }
            QueryPurpose::Grouping => {
                "display grouping adapter is not wired; applied grouping is unchanged"
            }
        };
        self.completions.push_back(QueryCompletion {
            view_id: request.view_id,
            generation: request.generation,
            revision: request.revision,
            purpose: request.purpose,
            result: Err(QueryFailure {
                purpose: failed_purpose,
                message: message.into(),
            }),
        });
        Ok(())
    }

    fn poll(&mut self) -> Option<QueryCompletion> {
        self.completions.pop_front()
    }
}

struct TerminalGuard {
    stdout: Stdout,
    raw: bool,
    alternate: bool,
    mouse: bool,
    paste: bool,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        if !io::stdout().is_terminal() {
            return Err(io::Error::other("lvu requires an interactive terminal"));
        }
        let mut guard = Self {
            stdout: io::stdout(),
            raw: false,
            alternate: false,
            mouse: false,
            paste: false,
        };
        enable_raw_mode()?;
        guard.raw = true;
        execute!(guard.stdout, EnterAlternateScreen)?;
        guard.alternate = true;
        execute!(guard.stdout, EnableMouseCapture)?;
        guard.mouse = true;
        execute!(guard.stdout, EnableBracketedPaste)?;
        guard.paste = true;
        Ok(guard)
    }

    fn restore(&mut self) {
        if self.paste {
            let _ = execute!(self.stdout, DisableBracketedPaste);
            self.paste = false;
        }
        if self.mouse {
            let _ = execute!(self.stdout, DisableMouseCapture);
            self.mouse = false;
        }
        if self.alternate {
            let _ = execute!(self.stdout, LeaveAlternateScreen);
            self.alternate = false;
        }
        if self.raw {
            let _ = disable_raw_mode();
            self.raw = false;
        }
        let _ = self.stdout.flush();
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

pub fn run<P: RowProvider, Q: QueryDispatcher>(
    app: App,
    provider: &mut P,
    dispatcher: &mut Q,
    demo_advance: impl Fn(&mut P) -> bool,
) -> io::Result<()> {
    run_with_tick(app, provider, dispatcher, demo_advance, |_, _, _| false)
}

pub fn run_with_tick<P: RowProvider, Q: QueryDispatcher>(
    mut app: App,
    provider: &mut P,
    dispatcher: &mut Q,
    demo_advance: impl Fn(&mut P) -> bool,
    tick: impl FnMut(&mut App, &mut P, &mut Q) -> bool,
) -> io::Result<()> {
    run_with_tick_mut(&mut app, provider, dispatcher, demo_advance, tick)
}

pub fn run_with_tick_mut<P: RowProvider, Q: QueryDispatcher>(
    app: &mut App,
    provider: &mut P,
    dispatcher: &mut Q,
    demo_advance: impl Fn(&mut P) -> bool,
    tick: impl FnMut(&mut App, &mut P, &mut Q) -> bool,
) -> io::Result<()> {
    let mut guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    let result = event_loop(&mut terminal, app, provider, dispatcher, demo_advance, tick);
    guard.restore();
    result
}

fn event_loop<P: RowProvider, Q: QueryDispatcher>(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    provider: &mut P,
    dispatcher: &mut Q,
    demo_advance: impl Fn(&mut P) -> bool,
    mut tick: impl FnMut(&mut App, &mut P, &mut Q) -> bool,
) -> io::Result<()> {
    let mut dirty = true;
    let mut palette = Palette::new();
    let mut selection = crate::text_selection::TextSelection::default();
    let mut visible_buffer = None;
    let mut selection_scope = None;
    let mut pending_click = None;
    let mut delight_config = DelightConfig::new(
        app.delight_enabled,
        app.reduced_motion,
        app.ascii,
        crate::delight::MAX_STARTUP_DURATION,
    );
    let started = Instant::now();
    let mut startup = StartupDelight::new();
    let mut startup_visible =
        app.show_startup_title && startup.is_visible(started.elapsed(), delight_config);
    let mut animation_tick = 0;
    let mut last_activity = ActivityState::Idle;
    let mut last_revision = None;
    let mut updated_at: Option<Instant> = None;
    let mut last_draw = Instant::now() - MIN_REDRAW_INTERVAL;
    while !app.should_quit {
        let current_delight = DelightConfig::new(
            app.delight_enabled,
            app.reduced_motion,
            app.ascii,
            crate::delight::MAX_STARTUP_DURATION,
        );
        dirty |= current_delight != delight_config;
        delight_config = current_delight;
        dirty |= tick(app, provider, dispatcher);
        // The tick can publish native membership. Accept its completion before
        // constructing the next search request's applied base snapshot.
        dirty |= poll_query_completions(app, dispatcher);
        dirty |= app.flush_debounced_searches(Instant::now());
        dirty |= submit_query_requests(app, dispatcher);
        dirty |= poll_query_completions(app, dispatcher);
        let area = terminal.size()?;
        let geometry = ui::layout(area.into(), app.show_details);
        if !geometry.tiny {
            dirty |= app.sync_provider(provider, usize::from(geometry.log_rows.height));
        }
        let now = Instant::now();
        let elapsed = now.duration_since(started);
        let visible = app.show_startup_title && startup.is_visible(elapsed, delight_config);
        dirty |= visible != startup_visible;
        startup_visible = visible;
        let revision = app.view_state().map(|state| {
            (
                app.active_view_id().unwrap_or("").to_owned(),
                state.provider_revision,
            )
        });
        if revision != last_revision {
            updated_at = match (&last_revision, &revision) {
                (Some((previous_view, _)), Some((view, _))) if previous_view == view => Some(now),
                _ => None,
            };
            last_revision = revision;
        }
        let activity = app_activity(
            app,
            updated_at.is_some_and(|at| now.duration_since(at) < Duration::from_secs(1)),
        );
        dirty |= activity != last_activity;
        last_activity = activity;
        let beat = elapsed.as_millis() / ANIMATION_TICK.as_millis();
        let animated = visible
            || matches!(
                activity,
                ActivityState::Active { .. } | ActivityState::Pending { .. }
            );
        if delight_config.enabled
            && !delight_config.reduced_motion
            && animated
            && beat != animation_tick
        {
            dirty = true;
        }
        animation_tick = beat;
        let scope = (
            app.focus,
            app.active_view_id().map(str::to_owned),
            palette.is_open(),
            visible,
        );
        if selection_scope.as_ref() != Some(&scope) {
            selection.clear();
            pending_click = None;
            selection_scope = Some(scope);
            dirty = true;
        }
        if dirty && last_draw.elapsed() >= MIN_REDRAW_INTERVAL {
            let theme = app.theme_id.theme();
            terminal.draw(|frame| {
                ui::render_with_theme(
                    frame,
                    app,
                    provider,
                    theme,
                    Some((elapsed, delight_config, activity)),
                );
                if visible {
                    startup.render_with_theme(frame, frame.area(), elapsed, delight_config, theme);
                }
                if palette.is_open() {
                    palette.refresh_context(palette_context(app));
                    palette.render_with_theme(frame, frame.area(), theme);
                }
                selection.paint(
                    frame.buffer_mut(),
                    ratatui::style::Style::default()
                        .fg(theme.selection_fg)
                        .bg(theme.selection_bg),
                );
                if frame.buffer_mut().content.len() <= 128 * 1024 {
                    visible_buffer = Some(frame.buffer_mut().clone());
                } else {
                    visible_buffer = None;
                }
            })?;
            dirty = false;
            last_draw = Instant::now();
        }
        if !event::poll(EVENT_POLL)? {
            continue;
        }
        let mut event = event::read()?;
        if let Event::Key(key) = event
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
            && selection.selected
        {
            match selection.text() {
                Ok(text) => {
                    execute!(
                        terminal.backend_mut(),
                        crossterm::clipboard::CopyToClipboard {
                            content: text,
                            destination: crossterm::clipboard::ClipboardSelection(vec![
                                crossterm::clipboard::ClipboardType::Clipboard
                            ]),
                        }
                    )?;
                    app.action_notice = Some("Copy sent to terminal clipboard (OSC 52)".into());
                }
                Err(message) => app.action_notice = Some(message.into()),
            }
            selection.clear();
            dirty = true;
            continue;
        }
        match &event {
            Event::Mouse(mouse) if !startup_visible => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(buffer) = &visible_buffer {
                        selection.begin(buffer, (mouse.column, mouse.row).into());
                    }
                    pending_click = Some(*mouse);
                    dirty = true;
                    continue;
                }
                MouseEventKind::Drag(MouseButton::Left) if selection.dragging => {
                    selection.extend((mouse.column, mouse.row).into());
                    dirty = true;
                    continue;
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    let dragged = selection.selected;
                    selection.finish();
                    dirty = true;
                    if dragged {
                        pending_click = None;
                        continue;
                    }
                    if let Some(click) = pending_click.take() {
                        event = Event::Mouse(click);
                    } else {
                        continue;
                    }
                }
                MouseEventKind::Moved => {}
                _ => {
                    selection.clear();
                    pending_click = None;
                    dirty = true;
                }
            },
            Event::Key(_) | Event::Paste(_) | Event::Resize(_, _) => {
                selection.clear();
                pending_click = None;
                dirty = true;
            }
            _ => {}
        }

        if startup_visible {
            match event {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c') =>
                {
                    app.handle(Action::Quit, provider);
                }
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                        && key.code == KeyCode::Esc =>
                {
                    startup.dismiss();
                    startup_visible = false;
                    dirty = true;
                }
                Event::Resize(width, height) => {
                    app.handle(Action::Resize(width, height), provider);
                    dirty = true;
                }
                _ => {}
            }
            continue;
        }
        if let Event::Key(key) = event
            && Palette::is_toggle_key(key)
            && !palette.is_open()
        {
            palette.open(palette_context(app));
            dirty = true;
            continue;
        }
        let action = if palette.is_open() {
            let context = palette_context(app);
            palette.refresh_context(context);
            let outcome = match event {
                Event::Key(key) => palette.handle_key(key, context),
                Event::Mouse(mouse) => palette.handle_mouse(mouse),
                Event::Paste(text) => {
                    palette.handle_paste(&text);
                    PaletteOutcome::None
                }
                Event::Resize(width, height) => {
                    palette.resize(ratatui::layout::Rect::new(0, 0, width, height));
                    PaletteOutcome::None
                }
                _ => PaletteOutcome::None,
            };
            dirty = true;
            match outcome {
                PaletteOutcome::Execute(action) => action,
                // The overlay never changes application focus. Preserve newer
                // async focus changes as well as the original editor draft.
                PaletteOutcome::Closed { .. } | PaletteOutcome::None => Action::None,
            }
        } else {
            match event {
                Event::Key(key) => key_to_action(key, app.focus),
                Event::Mouse(mouse) => Action::Mouse(mouse),
                Event::Resize(width, height) => Action::Resize(width, height),
                Event::Paste(text) => Action::EditorPaste(text),
                Event::FocusGained | Event::FocusLost => Action::None,
            }
        };
        if action == Action::FixtureAdvance {
            dirty |= demo_advance(provider);
        } else if action != Action::None {
            app.handle(action, provider);
            dirty = true;
        }
        dirty |= submit_query_requests(app, dispatcher);
    }
    Ok(())
}

fn palette_context(app: &App) -> PaletteContext {
    use crate::app::InvestigationStage;
    let mut context = PaletteContext::new(app.focus, app.active_view_id().is_some());
    context.has_selected_row = app
        .view_state()
        .is_some_and(|state| state.selected.is_some());
    context.storage_confirmation_ready =
        app.storage_dialog.as_ref().is_some_and(|d| d.confirm_clear);
    context.recipe_mode = app.recipe_dialog.as_ref().map(|d| d.mode);
    if let Some(dialog) = &app.investigation_dialog {
        context.investigation_can_resume = dialog.stage == InvestigationStage::Input
            && dialog.input.trim().is_empty()
            && dialog.items.get(dialog.selected).is_some();
        context.investigation_can_follow_up = matches!(
            dialog.stage,
            InvestigationStage::Conversation | InvestigationStage::Error
        ) && dialog.session_id.is_some()
            && !dialog.input.trim().is_empty();
    }
    context
}

pub fn submit_query_requests<Q: QueryDispatcher>(app: &mut App, dispatcher: &mut Q) -> bool {
    let requests = app.take_query_requests();
    let changed = !requests.is_empty();
    for request in requests {
        let identity = (
            request.view_id.clone(),
            request.generation,
            request.revision,
            request.purpose,
        );
        if let Err(error) = dispatcher.submit(request) {
            app.apply_query_completion(QueryCompletion {
                view_id: identity.0,
                generation: identity.1,
                revision: identity.2,
                purpose: identity.3,
                result: Err(QueryFailure {
                    purpose: identity.3,
                    message: error,
                }),
            });
        }
    }
    changed
}

pub fn poll_query_completions<Q: QueryDispatcher>(app: &mut App, dispatcher: &mut Q) -> bool {
    let mut changed = false;
    for _ in 0..MAX_QUERY_COMPLETIONS_PER_TICK {
        let Some(completion) = dispatcher.poll() else {
            break;
        };
        changed |= app.apply_query_completion(completion);
    }
    changed
}

/// Hidden executable probe used by the PTY test to verify unwind restoration.
pub fn panic_restoration_probe() -> io::Result<()> {
    let guard = TerminalGuard::enter()?;
    let old_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let outcome = panic::catch_unwind(AssertUnwindSafe(move || {
        let _guard = guard;
        panic!("controlled terminal restoration probe");
    }));
    panic::set_hook(old_hook);
    if outcome.is_err() {
        Ok(())
    } else {
        Err(io::Error::other("panic probe did not panic"))
    }
}

/// Use actual pending state and observed provider updates, never a fake percentage.
fn app_activity(app: &App, recently_updated: bool) -> ActivityState<'static> {
    use crate::{AskAiStage, InvestigationStage};
    if app.ask_ai_dialog.as_ref().is_some_and(|dialog| {
        matches!(
            dialog.stage,
            AskAiStage::Snapshot | AskAiStage::StartingSession | AskAiStage::Proposing
        )
    }) || app.investigation_dialog.as_ref().is_some_and(|dialog| {
        matches!(
            dialog.stage,
            InvestigationStage::Snapshot
                | InvestigationStage::StartingSession
                | InvestigationStage::Resuming
                | InvestigationStage::Sending
                | InvestigationStage::Cancelling
        )
    }) {
        return ActivityState::Active {
            label: "agent working",
        };
    }
    if let Some(state) = app.view_state() {
        let editors = [
            &state.search,
            &state.advanced,
            &state.enrichment,
            &state.grouping,
        ];
        if editors
            .iter()
            .any(|editor| editor.pending_generation.is_some())
        {
            return ActivityState::Active {
                label: "query working",
            };
        }
        if editors.iter().any(|editor| editor.error.is_some()) || state.time_error.is_some() {
            return ActivityState::Error {
                label: "definition",
            };
        }
    }
    if recently_updated {
        ActivityState::Active {
            label: "view updating",
        }
    } else {
        ActivityState::Idle
    }
}
