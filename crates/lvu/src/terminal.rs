use std::{
    collections::VecDeque,
    env,
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
    terminal::{
        BeginSynchronizedUpdate, DisableLineWrap, EnableLineWrap, EndSynchronizedUpdate,
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{
    app::{Action, App, Focus, QueryCompletion, QueryFailure, QueryPurpose, QueryRequest},
    command_palette::{Palette, PaletteContext, PaletteOutcome},
    component::RawEvent,
    delight::{
        ANIMATION_TICK, ActivityState, DelightConfig, INDICATOR_ANIMATION_TICK,
        STARTUP_ANIMATION_TICK, StartupDelight,
    },
    input::NonBlockingInput,
    provider::RowProvider,
    theme::ColorDepth,
    ui,
};

/// Where the time between a keypress and the app acting on it goes.
///
/// Input is read at the end of a loop iteration, so a byte that arrives just
/// after the poll waits for everything the next iteration does before it. That
/// makes keypress latency the iteration's duration, and this records which part
/// of the iteration is responsible. Off unless `LVU_SHUTDOWN_TIMING` is set;
/// the name is shared with the shutdown breakdown because they answer halves of
/// the same question.
struct LoopProbe {
    enabled: bool,
    worst: Duration,
    /// Reported separately because it would otherwise never be the worst
    /// iteration: an iteration that reads an event has a short `event-wait`,
    /// while an idle one pays the full poll and so dominates the maximum.
    worst_dispatch: Duration,
    worst_phases: [(&'static str, Duration); PHASES],
    phases: [(&'static str, Duration); PHASES],
    iterations: u64,
    counted: bool,
    started: Instant,
    mark: Instant,
    index: usize,
}

const PHASES: usize = 7;
/// `dispatch` is everything after the poll says a byte is ready: reading the
/// event, resolving it to an `Action`, and `App::handle`. It is the phase a
/// layer's `open()` runs in, so a dialog that waits on a query to appear shows
/// up here and nowhere else.
const PHASE_NAMES: [&str; PHASES] = [
    "tick",
    "query",
    "sync-rows",
    "frame-state",
    "render",
    "event-wait",
    "dispatch",
];

impl LoopProbe {
    fn new() -> Self {
        let now = Instant::now();
        let empty = [("", Duration::ZERO); PHASES];
        Self {
            enabled: std::env::var_os("LVU_SHUTDOWN_TIMING").is_some(),
            worst: Duration::ZERO,
            worst_dispatch: Duration::ZERO,
            worst_phases: empty,
            phases: empty,
            iterations: 0,
            counted: false,
            started: now,
            mark: now,
            index: 0,
        }
    }

    fn begin(&mut self) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        self.started = now;
        self.mark = now;
        self.index = 0;
        self.counted = false;
        self.phases = [("", Duration::ZERO); PHASES];
    }

    fn phase(&mut self) {
        if !self.enabled || self.index >= PHASES {
            return;
        }
        let now = Instant::now();
        let elapsed = now.duration_since(self.mark);
        self.phases[self.index] = (PHASE_NAMES[self.index], elapsed);
        if self.index == PHASES - 1 && elapsed > self.worst_dispatch {
            self.worst_dispatch = elapsed;
        }
        self.mark = now;
        self.index += 1;
    }

    /// Callable more than once per iteration. An iteration that reads an event
    /// is closed twice — once after the poll, once after the dispatch it
    /// enables — and only the second knows the whole duration. Counting is
    /// guarded so the extra call does not inflate the iteration total, and the
    /// worst-case comparison is a maximum, so re-recording is harmless.
    fn end(&mut self) {
        if !self.enabled {
            return;
        }
        if !self.counted {
            self.iterations += 1;
            self.counted = true;
        }
        let elapsed = self.mark.duration_since(self.started);
        if elapsed > self.worst {
            self.worst = elapsed;
            self.worst_phases = self.phases;
        }
    }

    fn report(&self) {
        if !self.enabled {
            return;
        }
        let detail = self
            .worst_phases
            .iter()
            .filter(|(name, _)| !name.is_empty())
            .map(|(name, elapsed)| format!("{name} {:.3}s", elapsed.as_secs_f64()))
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!(
            "lvu input loop: {} iterations, slowest {:.3}s: {detail}; \
             slowest dispatch {:.3}s",
            self.iterations,
            self.worst.as_secs_f64(),
            self.worst_dispatch.as_secs_f64()
        );
    }
}

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
    /// Installed before any crossterm event call, because crossterm resolves
    /// its input descriptor once and keeps it for the life of the process.
    input: NonBlockingInput,
    raw: bool,
    alternate: bool,
    mouse: bool,
    paste: bool,
    wrap_disabled: bool,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        if !io::stdout().is_terminal() {
            return Err(io::Error::other("lvu requires an interactive terminal"));
        }
        let mut guard = Self {
            stdout: io::stdout(),
            input: NonBlockingInput::install()?,
            raw: false,
            alternate: false,
            mouse: false,
            paste: false,
            wrap_disabled: false,
        };
        enable_raw_mode()?;
        guard.raw = true;
        execute!(guard.stdout, EnterAlternateScreen)?;
        guard.alternate = true;
        execute!(guard.stdout, DisableLineWrap)?;
        guard.wrap_disabled = true;
        execute!(guard.stdout, EnableMouseCapture)?;
        guard.mouse = true;
        execute!(guard.stdout, EnableBracketedPaste)?;
        guard.paste = true;
        Ok(guard)
    }

    fn restore(&mut self) {
        let _ = execute!(self.stdout, EndSynchronizedUpdate);
        if self.wrap_disabled {
            let _ = execute!(self.stdout, EnableLineWrap);
            self.wrap_disabled = false;
        }
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
        // Last: `disable_raw_mode` resolves the terminal through standard
        // input, so the caller's descriptor goes back only once every terminal
        // mode has been put back on it.
        self.input.restore();
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
    terminal.resize(terminal.size()?.into())?;
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
    // Fixed for the life of the process: a terminal does not gain or lose
    // truecolor support while lvu is attached to it.
    let depth = color_depth();
    // A resize invalidates the emulator's screen, and reclaiming it costs a
    // full clear. That clear must be presented in the same synchronized update
    // as the frame that repaints it, or the terminal shows a blank screen
    // between the two and the whole viewport flashes.
    let mut resize_pending = false;
    let mut palette = Palette::new();
    let mut selection = crate::text_selection::TextSelection::default();
    let mut visible_buffer = None;
    let mut selection_scope = None;
    let mut pending_click = None;
    let mut delight_config = DelightConfig::new(
        app.appearance.delight_enabled,
        app.appearance.reduced_motion,
        app.appearance.ascii,
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
    let mut probe = LoopProbe::new();
    while !app.should_quit {
        probe.begin();
        let current_delight = DelightConfig::new(
            app.appearance.delight_enabled,
            app.appearance.reduced_motion,
            app.appearance.ascii,
            crate::delight::MAX_STARTUP_DURATION,
        );
        dirty |= current_delight != delight_config;
        delight_config = current_delight;
        dirty |= tick(app, provider, dispatcher);
        probe.phase();
        // The tick can publish native membership. Accept its completion before
        // constructing the next search request's applied base snapshot.
        dirty |= poll_query_completions(app, dispatcher);
        dirty |= app.flush_debounced_searches(Instant::now());
        dirty |= submit_query_requests(app, dispatcher);
        dirty |= poll_query_completions(app, dispatcher);
        probe.phase();
        let area = terminal.size()?;
        let geometry = ui::layout(area.into(), app.show_details);
        if !geometry.tiny {
            dirty |= app.sync_provider(provider, usize::from(geometry.log_rows.height));
        }
        probe.phase();
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
        let interval = if visible {
            STARTUP_ANIMATION_TICK
        } else if !delight_config.ascii && area.width >= 80 && area.height >= 24 {
            INDICATOR_ANIMATION_TICK
        } else {
            ANIMATION_TICK
        };
        let beat = elapsed.as_millis() / interval.as_millis();
        let animated = visible
            || matches!(
                activity,
                ActivityState::Active { .. } | ActivityState::Pending { .. }
            );
        if delight_config.enabled
            && !delight_config.ascii
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
        probe.phase();
        if dirty && last_draw.elapsed() >= MIN_REDRAW_INTERVAL {
            let theme = app.appearance.theme_id.theme().with_depth(depth);
            execute!(terminal.backend_mut(), BeginSynchronizedUpdate)?;
            // Inside the block: a same-size reflow still needs the clear that
            // `resize` performs, and a real size change gets one from ratatui's
            // own autoresize during `draw`. Neither is ever presented alone.
            let resize_result = if resize_pending {
                resize_pending = false;
                terminal
                    .size()
                    .and_then(|size| terminal.resize(size.into()))
            } else {
                Ok(())
            };
            let draw_result = terminal
                .draw(|frame| {
                    ui::render_with_theme(
                        frame,
                        app,
                        provider,
                        theme,
                        Some((elapsed, delight_config, activity)),
                    );
                    if visible {
                        startup.render_with_theme(
                            frame,
                            frame.area(),
                            elapsed,
                            delight_config,
                            theme,
                        );
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
                })
                .map(|_| ());
            let end_result = execute!(terminal.backend_mut(), EndSynchronizedUpdate);
            resize_result?;
            draw_result?;
            end_result?;
            dirty = false;
            last_draw = Instant::now();
        }
        probe.phase();
        let ready = event::poll(EVENT_POLL)?;
        probe.phase();
        probe.end();
        if !ready {
            continue;
        }
        let mut event = event::read()?;
        if matches!(event, Event::Resize(_, _)) {
            // Even a resize back to the same dimensions may have reflowed the
            // emulator's cells. Its screen can no longer be diffed against ours.
            // Fullscreen resize resets both viewport geometry and the cached
            // screen even at the same size, without querying cursor position.
            // `clear` preserves a cursor by asking the terminal, which can race
            // with queued keyboard input. Our next frame sets its own cursor.
            resize_pending = true;
            selection.clear();
            visible_buffer = None;
            pending_click = None;
            dirty = true;
        }
        if let Event::Key(key) = event
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('l')
            && !startup_visible
        {
            resize_pending = true;
            selection.clear();
            visible_buffer = None;
            pending_click = None;
            dirty = true;
            continue;
        }

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
        if selection.selected && is_layer_dismissal_key(&event) {
            selection.clear();
            pending_click = None;
            dirty = true;
            continue;
        }
        match &event {
            Event::Mouse(mouse) if !startup_visible => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(buffer) = &visible_buffer {
                        let position = (mouse.column, mouse.row).into();
                        let bounds = if palette.is_open() {
                            palette.selection_area()
                        } else if app.hit_regions.selection_modal.is_some() {
                            app.hit_regions.selection_modal
                        } else {
                            [
                                Some(geometry.log.inner(ratatui::layout::Margin::new(1, 1))),
                                geometry
                                    .sidebar
                                    .map(|area| area.inner(ratatui::layout::Margin::new(1, 1))),
                                geometry
                                    .details
                                    .map(|area| area.inner(ratatui::layout::Margin::new(1, 1))),
                                Some(geometry.status),
                                Some(geometry.header),
                            ]
                            .into_iter()
                            .flatten()
                            .find(|area| area.contains(position))
                        };
                        if let Some(bounds) = bounds {
                            selection.begin(buffer, bounds, position);
                        } else {
                            selection.clear();
                        }
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
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    startup.observe_input();
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
            palette.refresh_context(context.clone());
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
                // §6.4 input dispatch: while a converted layer is on top the
                // component owns its keymap, so the terminal hands the event
                // over raw instead of resolving it to an `Action`.
                Event::Key(key) if app.focus == Focus::Layer => Action::Raw(RawEvent::Key(key)),
                Event::Mouse(mouse) if app.focus == Focus::Layer => {
                    Action::Raw(RawEvent::Mouse(mouse))
                }
                Event::Paste(text) if app.focus == Focus::Layer => {
                    Action::Raw(RawEvent::Paste(text))
                }
                Event::Key(key) => app.key_to_action(key),
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
        probe.phase();
        probe.end();
    }
    probe.report();
    Ok(())
}

/// What the attached terminal can display, from the environment.
///
/// `COLORTERM` is the only reliable signal: `TERM` says what the terminfo entry
/// is, not what the emulator behind it supports, and plenty of truecolor
/// terminals still report `xterm-256color`. Anything that does not positively
/// claim truecolor is treated as the 256-color cube, which every terminal lvu
/// supports can display, so an unset variable degrades rather than guesses.
/// `NO_COLOR` is not read here: crossterm honours it where sequences are
/// emitted, and reading it twice would only let the two disagree.
fn color_depth() -> ColorDepth {
    ColorDepth::detect(
        env::var("COLORTERM").ok().as_deref(),
        env::var("TERM").ok().as_deref(),
        tput_colors,
    )
}

/// `tput colors`, for the one case `TERM` answers nothing: no entry at all.
///
/// Bounded like every other subprocess here — it inherits no stdin, its output
/// is a small number, and a failure to spawn or parse is simply no answer. It
/// is not consulted when `TERM` is set, which is every ordinary run.
fn tput_colors() -> Option<u32> {
    let output = std::process::Command::new("tput")
        .arg("colors")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    std::str::from_utf8(&output.stdout)
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn is_layer_dismissal_key(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && (key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('q') && key.modifiers.is_empty()))
}

fn palette_context(app: &App) -> PaletteContext {
    let mut context = PaletteContext::new(app.focus, app.active_view_id().is_some());
    context.has_selected_row = app
        .view_state()
        .is_some_and(|state| state.selected.is_some());
    // §4.3: the Investigation rows' availability used to be computed here from
    // a peek at the dialog's private state. The layer reports it itself now.
    context.layer_commands = app.layer_commands();
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
    if app.command_work_pending() {
        return ActivityState::Active { label: "" };
    }
    if app.assistance_working() {
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

#[cfg(test)]
mod dismissal_key_tests {
    use super::*;
    use crossterm::event::KeyEvent;

    #[test]
    fn only_plain_dismissal_presses_are_owned_by_a_selection() {
        for code in [KeyCode::Esc, KeyCode::Char('q')] {
            assert!(is_layer_dismissal_key(&Event::Key(KeyEvent::new(
                code,
                KeyModifiers::NONE,
            ))));
        }
        assert!(!is_layer_dismissal_key(&Event::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::CONTROL,
        ))));
        assert!(!is_layer_dismissal_key(&Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        ))));
    }
}
