use std::{
    collections::VecDeque,
    io::{self, IsTerminal, Stdout, Write},
    panic::{self, AssertUnwindSafe},
    time::{Duration, Instant},
};

use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{
    app::{Action, App, QueryCompletion, QueryRequest, key_to_action},
    provider::RowProvider,
    ui,
};

const EVENT_POLL: Duration = Duration::from_millis(25);
const MIN_REDRAW_INTERVAL: Duration = Duration::from_millis(33);
const MAX_QUERY_COMPLETIONS_PER_TICK: usize = 32;

/// Nonblocking compiler seam. `submit` must only enqueue bounded work and
/// `poll` must return immediately; compilation belongs to an external worker.
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
        self.completions.clear();
        self.completions.push_back(QueryCompletion {
            view_id: request.view_id,
            generation: request.generation,
            result: Err(
                "query adapter is not wired in demo mode; last applied view is unchanged".into(),
            ),
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
    mut app: App,
    provider: &mut P,
    dispatcher: &mut Q,
    demo_advance: impl Fn(&mut P) -> bool,
) -> io::Result<()> {
    let mut guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    let result = event_loop(&mut terminal, &mut app, provider, dispatcher, demo_advance);
    guard.restore();
    result
}

fn event_loop<P: RowProvider, Q: QueryDispatcher>(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    provider: &mut P,
    dispatcher: &mut Q,
    demo_advance: impl Fn(&mut P) -> bool,
) -> io::Result<()> {
    let mut dirty = true;
    let mut last_draw = Instant::now() - MIN_REDRAW_INTERVAL;
    while !app.should_quit {
        dirty |= poll_query_completions(app, dispatcher);
        let area = terminal.size()?;
        let geometry = ui::layout(area.into(), app.show_details);
        if !geometry.tiny {
            dirty |= app.sync_provider(provider, usize::from(geometry.log_rows.height));
        }
        if dirty && last_draw.elapsed() >= MIN_REDRAW_INTERVAL {
            terminal.draw(|frame| ui::render(frame, app, provider))?;
            dirty = false;
            last_draw = Instant::now();
        }
        if !event::poll(EVENT_POLL)? {
            continue;
        }
        let action = match event::read()? {
            Event::Key(key) => key_to_action(key, app.focus),
            Event::Mouse(mouse) => Action::Mouse(mouse),
            Event::Resize(width, height) => Action::Resize(width, height),
            Event::Paste(text) => Action::EditorPaste(text),
            Event::FocusGained | Event::FocusLost => Action::None,
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

pub fn submit_query_requests<Q: QueryDispatcher>(app: &mut App, dispatcher: &mut Q) -> bool {
    let requests = app.take_query_requests();
    let changed = !requests.is_empty();
    for request in requests {
        let identity = (request.view_id.clone(), request.generation);
        if let Err(error) = dispatcher.submit(request) {
            app.apply_query_completion(QueryCompletion {
                view_id: identity.0,
                generation: identity.1,
                result: Err(error),
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
