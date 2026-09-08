use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Widget, Wrap},
};
use unicode_width::UnicodeWidthStr;

use crate::component::Component;
use crate::{
    App,
    app::Focus,
    dialog_controls::{ActionRow, ButtonRole, DialogStyles, button_width, render_role_button},
    dialog_layout::MIN_BODY_ROWS,
    json_spans::{JsonSpan, classify},
    provider::RowProvider,
    theme::{Theme, ensure_contrast},
};

const SIDEBAR_WIDTH: u16 = 22;

/// True while a modal dialog covers the workspace. The scrim (§6.2) and the
/// compact backdrop rule (§5.5) both key off this, so they can never disagree
/// about whether a dialog is open.
pub fn dialog_is_open(app: &App) -> bool {
    !matches!(app.focus, Focus::Logs | Focus::Selector | Focus::Details)
}

/// The marker for a value whose head is scrolled out of a field (§8.1) and for
/// a truncated list cell (§9). ASCII terminals get the same glyph from
/// crossterm; only product labels have an ASCII fallback.
pub(crate) const ELLIPSIS: &str = "…";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UiLayout {
    pub area: Rect,
    pub header: Rect,
    pub status: Rect,
    pub sidebar: Option<Rect>,
    pub log: Rect,
    pub log_rows: Rect,
    pub details: Option<Rect>,
    pub tiny: bool,
}

pub fn layout(area: Rect, show_details: bool) -> UiLayout {
    layout_with_backdrop(area, show_details, false)
}

/// `hide_sidebar` implements dialog-system.md §5.5: while a dialog is open in a
/// compact terminal the sidebar is not drawn and the log takes the full width,
/// so the dialog is not competing with a list nobody can reach.
pub fn layout_with_backdrop(area: Rect, show_details: bool, hide_sidebar: bool) -> UiLayout {
    if area.width < 20 || area.height < 6 {
        return UiLayout {
            area,
            header: Rect::default(),
            status: Rect::default(),
            sidebar: None,
            log: Rect::default(),
            log_rows: Rect::default(),
            details: None,
            tiny: true,
        };
    }
    let outer = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(area);
    let (sidebar, main) = if area.width >= 48 && !hide_sidebar {
        let columns = Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(20)])
            .split(outer[1]);
        (Some(columns[0]), columns[1])
    } else {
        (None, outer[1])
    };
    let (log, details) = if show_details && main.height >= 9 {
        let content =
            Layout::vertical([Constraint::Percentage(65), Constraint::Percentage(35)]).split(main);
        (content[0], Some(content[1]))
    } else {
        (main, None)
    };
    // The table consumes one row for each border and one for its header.
    let log_rows = Rect::new(
        log.x.saturating_add(1),
        log.y.saturating_add(2),
        log.width.saturating_sub(2),
        log.height.saturating_sub(3),
    );
    UiLayout {
        area,
        header: outer[0],
        status: outer[2],
        sidebar,
        log,
        log_rows,
        details,
        tiny: false,
    }
}

pub fn render<P: RowProvider>(frame: &mut Frame<'_>, app: &mut App, provider: &P) {
    render_with_theme(frame, app, provider, Theme::TERMINAL, None);
}

pub fn render_with_delight<P: RowProvider>(
    frame: &mut Frame<'_>,
    app: &mut App,
    provider: &P,
    delight: Option<(
        std::time::Duration,
        crate::delight::DelightConfig,
        crate::delight::ActivityState<'_>,
    )>,
) {
    render_with_theme(
        frame,
        app,
        provider,
        app.appearance.theme_id.theme(),
        delight,
    );
}

pub fn render_with_theme<P: RowProvider>(
    frame: &mut Frame<'_>,
    app: &mut App,
    provider: &P,
    theme: Theme,
    delight: Option<(
        std::time::Duration,
        crate::delight::DelightConfig,
        crate::delight::ActivityState<'_>,
    )>,
) {
    let modal = dialog_is_open(app);
    let geometry = layout_with_backdrop(
        frame.area(),
        app.show_details,
        modal && crate::dialog_layout::is_compact(frame.area()),
    );
    let corner_heart = delight.is_some_and(|(_, config, _)| config.enabled && !config.ascii)
        && geometry.area.width >= 80
        && geometry.area.height >= 24;
    let reserved_sidebar_rows = if corner_heart {
        crate::delight::CORNER_HEART_HEIGHT
    } else {
        0
    };
    let list_geometry = geometry.sidebar.map(|mut sidebar| {
        sidebar.height = sidebar.height.saturating_sub(reserved_sidebar_rows);
        sidebar
    });
    frame.render_widget(
        Block::default().style(Style::default().fg(theme.base_fg).bg(theme.base_bg)),
        geometry.area,
    );
    app.shell.size = (geometry.area.width, geometry.area.height);
    if geometry.tiny {
        app.hit_regions = Default::default();
        render_tiny(frame, app, geometry.area, theme);
        return;
    }

    app.hit_regions.selection_modal = None;
    app.hit_regions.log = Some(geometry.log);
    app.hit_regions.log_rows = Some(geometry.log_rows);
    app.hit_regions.details = geometry.details;
    app.hit_regions.dialog_scroll = None;
    app.hit_regions.sidebar = geometry.sidebar;
    app.hit_regions.sidebar_views = sidebar_view_regions(app, list_geometry);
    app.sync_provider(provider, usize::from(geometry.log_rows.height));

    render_header(frame, app, geometry.header, theme);
    let status_area = Rect::new(
        geometry.log.x,
        geometry.status.y,
        geometry.status.right().saturating_sub(geometry.log.x),
        geometry.status.height,
    );
    if let Some(sidebar) = geometry.sidebar {
        render_selector(frame, app, sidebar, theme, reserved_sidebar_rows);
    }
    if let Some((elapsed, config, activity)) =
        delight.filter(|(_, config, _)| config.enabled && geometry.status.width >= 60)
    {
        let width = geometry.sidebar.map_or(0, |sidebar| sidebar.width);
        let heart_area = if corner_heart {
            let sidebar = geometry.sidebar.expect("corner requires sidebar");
            Rect::new(
                sidebar.x + 1,
                sidebar.bottom() - 1 - crate::delight::CORNER_HEART_HEIGHT,
                sidebar.width.saturating_sub(2),
                crate::delight::CORNER_HEART_HEIGHT,
            )
        } else {
            Rect::new(geometry.status.x, geometry.status.y, width, 1)
        };
        frame.render_widget(Clear, heart_area);
        crate::delight::FooterDelight::render_with_theme(
            frame, heart_area, elapsed, config, activity, theme,
        );
    }
    render_status(frame, app, status_area, theme);
    render_logs(frame, app, provider, geometry.log, theme);
    if let Some(details) = geometry.details {
        render_details(frame, app, provider, details, theme);
    }
    if modal {
        // §6.2: the workspace behind an open dialog goes muted and loses every
        // modifier, so the dialog is the only active surface. A style pass over
        // the finished workspace buffer; it moves nothing and owns no hit region.
        crate::dialog_layout::scrim(frame.buffer_mut(), geometry.area, theme);
    }
    if app.focus == Focus::Layer {
        // §6.4 render dispatch: the base, then the layer stack. Never both a
        // legacy dialog and a layer.
        render_layers(frame, app, provider, geometry.area, theme);
    }
    if app.focus == Focus::Correlation {
        render_correlation(frame, app, geometry.area, theme);
    }
}

/// §5.3: the layer stack renders bottom → top, with one extra scrim per level
/// so a child dims its parent exactly once more than the base. Only the top
/// layer's `Surface` becomes the selection bound (§3).
fn render_layers<P: RowProvider>(
    frame: &mut Frame<'_>,
    app: &mut App,
    provider: &P,
    area: Rect,
    theme: Theme,
) {
    let app_correlating = app.field_correlation_pending();
    let App {
        shell,
        layers,
        views,
        appearance,
        sources,
        hit_regions,
        ..
    } = app;
    let mut ctx = crate::component::RenderCtx {
        views,
        cursors: &shell.cursors,
        sources,
        provider,
        correlating: app_correlating,
        active: true,
        theme,
        ascii: appearance.ascii,
        display_zone: &appearance.display_zone,
        size: shell.size,
        clock: shell.clock(),
    };
    let stack = layers.stack.clone();
    let compact = crate::dialog_layout::is_compact(area);
    let mut top_surface = None;
    for (index, id) in stack.iter().copied().enumerate() {
        let is_top = index + 1 == stack.len();
        if compact && !is_top {
            continue;
        }
        if index > 0 {
            crate::dialog_layout::scrim(frame.buffer_mut(), area, theme);
        }
        ctx.active = is_top;
        let surface = match id {
            crate::component::LayerId::Storage => layers.storage.render(frame, area, &ctx),
            crate::component::LayerId::Time => layers.time.render(frame, area, &ctx),
            crate::component::LayerId::Help => layers.help.render(frame, area, &ctx),
            crate::component::LayerId::Settings => layers.settings.render(frame, area, &ctx),
            crate::component::LayerId::Fields => layers.fields.render(frame, area, &ctx),
            crate::component::LayerId::View => layers.view.render(frame, area, &ctx),
            crate::component::LayerId::Source => layers.source.render(frame, area, &ctx),
            crate::component::LayerId::Folding => layers.folding.render(frame, area, &ctx),
            crate::component::LayerId::Recipes | crate::component::LayerId::RecipeHistory => {
                layers.recipes.render(frame, area, &ctx)
            }
            crate::component::LayerId::Search => layers.search.render(frame, area, &ctx),
            crate::component::LayerId::Advanced => layers.advanced.render(frame, area, &ctx),
            crate::component::LayerId::Grouping => layers.grouping.render(frame, area, &ctx),
            crate::component::LayerId::Enrichment => layers.enrichment.render(frame, area, &ctx),
            crate::component::LayerId::EnrichmentStep => {
                layers.enrichment_step.render(frame, area, &ctx)
            }
            crate::component::LayerId::ExternalCommand => {
                layers.external_command.render(frame, area, &ctx)
            }
            crate::component::LayerId::Bookmarks => layers.bookmarks.render(frame, area, &ctx),
            crate::component::LayerId::Ask => layers.ask.render(frame, area, &ctx),
            crate::component::LayerId::Investigation => {
                layers.investigation.render(frame, area, &ctx)
            }
        };
        if is_top {
            top_surface = Some(surface);
        }
    }
    hit_regions.selection_modal = top_surface.map(|surface| surface.interior);
}

fn sidebar_view_regions(app: &App, area: Option<Rect>) -> Vec<(Rect, usize)> {
    let Some(area) = area else { return Vec::new() };
    let mut y = area.y.saturating_add(1);
    let mut regions = Vec::new();
    for source in &app.sources {
        y = y.saturating_add(2);
        for (index, _) in app
            .views()
            .iter()
            .enumerate()
            .filter(|(_, view)| view.source_id == source.id)
        {
            if y < area.bottom().saturating_sub(1) {
                regions.push((
                    Rect::new(area.x + 1, y, area.width.saturating_sub(2), 1),
                    index,
                ));
            }
            y = y.saturating_add(1);
        }
    }
    regions
}

fn render_tiny(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    let mode = if app.demo_mode { " DEMO" } else { "" };
    frame.render_widget(
        Paragraph::new(format!(
            "lvu{mode}\nterminal too small\n{}x{}  q quit",
            area.width, area.height
        ))
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(theme.base_fg).bg(theme.base_bg)),
        area,
    );
}

fn render_header(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    let demo = if app.demo_mode {
        Span::styled(
            " DEMO FIXTURE — NOT ACQUISITION ",
            Style::default()
                .fg(theme.selection_fg)
                .bg(theme.severity.warn),
        )
    } else {
        Span::raw(" ")
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                &app.title,
                Style::default()
                    .fg(theme.base_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            demo,
        ])),
        area,
    );
}

fn render_status(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    let mut text = if let (Some(_view_id), Some(state)) = (app.active_view_id(), app.view_state()) {
        let follow = if state.follow { "FOLLOW" } else { "HISTORY" };
        let pending = if state.search.pending_generation.is_some()
            || state.advanced.pending_generation.is_some()
        {
            " | query pending"
        } else {
            ""
        };
        let search = if state.search.applied.is_empty() {
            String::new()
        } else {
            format!(" | search:{:?}", state.search.applied)
        };
        let advanced = if state.advanced.applied.is_empty() {
            ""
        } else {
            " | advanced:on"
        };
        let enrichment = if state.enrichment.applied.is_empty() {
            ""
        } else {
            " | enrich:on"
        };
        let grouping = if state.grouping.applied.is_empty() {
            ""
        } else {
            " | grouping:display-only"
        };
        // Folding never silently changes a count: when a retention cap has
        // evicted older runs, the indicator says so instead of implying the
        // whole stream is folded, and while rows are still being folded it says
        // how many are left rather than leaving a pane of individual events
        // looking like a toggle that did nothing.
        // Runs, not collapsed entries. Expanding one does not stop it being a
        // run, and it is while scrolling through an expanded run that a user
        // most needs to be told the view has folds in it and how much they hide.
        let folding = match state.fold_summary.filter(|_| state.fold_enabled) {
            Some(summary) if summary.evicted_entries > 0 => format!(
                " | fold:{} runs, {} hidden, older runs uncounted{}",
                summary.runs,
                summary.hidden_rows,
                folding_progress(&summary)
            ),
            Some(summary) if summary.runs > 0 => format!(
                " | fold:{} runs, {} hidden{}",
                summary.runs,
                summary.hidden_rows,
                folding_progress(&summary)
            ),
            Some(summary) => format!(" | fold:on{}", folding_progress(&summary)),
            None if state.fold_enabled => " | fold:on".to_owned(),
            None => String::new(),
        };
        // What "14:30" means, always, so it is never something the user has to
        // work out from the rows. `tz:` rather than a spelled-out sentence
        // because the line has a fixed width and the segments after it — the
        // search term, the fold count — are the ones that lose characters
        // first. The row order lives on the Time dialog's read-only line and
        // not here: it is the same for every view and is not settable, so a
        // permanent constant on this line would cost the search term for
        // nothing.
        let display = format!(
            " | tz:{}",
            crate::app::time_zone_label(&app.appearance.display_zone)
        );
        // Where the last gap jump landed. It sits before the constraint
        // indicators because it answers "what just happened", which is what a
        // user reads the status line for straight after pressing a key.
        let gap = state
            .gap_notice
            .as_ref()
            .map_or_else(String::new, |notice| format!(" | {notice}"));
        // docs/raw-context-as-jump.md: while `o` has an origin, the raw view
        // says where it came from and how to get back. `o back` is the one
        // non-routine key the segment may print (§8.10). It sits right after
        // the follow state so a 54-column line still shows it whole.
        let raw_context = app.raw_context_origin().map_or_else(String::new, |origin| {
            let from = app
                .views()
                .iter()
                .find(|view| view.id == origin.view_id)
                .map_or("view", |view| view.name.as_str());
            let way_back = if app.jump_pending() {
                "locating…"
            } else {
                "o back"
            };
            format!(
                " | raw of {from} · #{} · {way_back}",
                origin.anchor.sequence
            )
        });
        let capture_time = match state.applied_capture_time_policy {
            Some(crate::CaptureTimePolicy::Recent { .. })
                if state.applied_time_basis == crate::TimeBasis::Extracted =>
            {
                " | extracted-time:rolling"
            }
            Some(crate::CaptureTimePolicy::Absolute(_))
                if state.applied_time_basis == crate::TimeBasis::Extracted =>
            {
                " | extracted-time:absolute"
            }
            Some(crate::CaptureTimePolicy::Recent { .. })
                if state.applied_time_basis == crate::TimeBasis::Event =>
            {
                " | event-time:rolling"
            }
            Some(crate::CaptureTimePolicy::Absolute(_))
                if state.applied_time_basis == crate::TimeBasis::Event =>
            {
                " | event-time:absolute"
            }
            Some(crate::CaptureTimePolicy::Recent { .. }) => " | capture-time:rolling",
            Some(crate::CaptureTimePolicy::Absolute(_)) => " | capture-time:absolute",
            None => "",
        };
        let runtime = app
            .active_view_runtime_status()
            .map_or_else(String::new, |status| format!(" | {status}"));
        format!(
            " {follow}{raw_context}{capture_time}{runtime} | {}-{}/{}{}{}{}{enrichment}{grouping}{folding}{gap}{display} | ? help · Ctrl-P commands ",
            state.top.saturating_add(1).min(state.last_total),
            state
                .top
                .saturating_add(state.viewport_height)
                .min(state.last_total),
            state.last_total,
            search,
            advanced,
            pending
        )
    } else {
        " NO VIEW | add or discover a source to begin | ? help · Ctrl-P commands ".into()
    };
    if let Some(notice) = &app.action_notice {
        text = format!(" {notice} | {text}");
    }
    if let Some(notice) = &app.source_notice {
        text.push_str(" | ");
        text.push_str(notice);
    }
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(theme.selection_fg).bg(theme.accent)),
        area,
    );
}

fn render_selector(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme, reserved_rows: u16) {
    let mut items = Vec::new();
    for source in &app.sources {
        items.push(ListItem::new(Line::from(vec![
            Span::styled("● ", Style::default().fg(theme.severity.info)),
            Span::styled(
                source.name.as_str(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ])));
        items.push(ListItem::new(format!("  {}", source.health)));
        for (index, view) in app
            .views()
            .iter()
            .enumerate()
            .filter(|(_, view)| view.source_id == source.id)
        {
            let marker = if index == app.selected_view() {
                "›"
            } else {
                " "
            };
            let style = if index == app.selected_view() {
                Style::default()
                    .fg(theme.selection_fg)
                    .bg(theme.selection_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            items.push(ListItem::new(format!(" {marker} {}", view.name)).style(style));
        }
    }
    if items.is_empty() {
        items.push(ListItem::new("No sources"));
        items.push(ListItem::new("Add/discover to begin"));
    }
    let border = if app.focus == Focus::Selector {
        theme.active_border
    } else {
        theme.border
    };
    let block = Block::default()
        .title(" Sources / views ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border));
    let mut inner = block.inner(area);
    inner.height = inner.height.saturating_sub(reserved_rows);
    frame.render_widget(block, area);
    frame.render_widget(List::new(items), inner);
}

/// What is left of a fold that has not consumed the whole stream yet.
///
/// Folding is presentation-only and incremental: the rows the user is looking
/// at fold first and the feed continues from there, so a large view is usable
/// throughout rather than blank until the whole walk finishes. The rows it has
/// not reached render individually, and this is what says so.
fn folding_progress(summary: &crate::FoldSummary) -> String {
    if summary.pending_rows == 0 {
        return String::new();
    }
    format!(", folding {} more", summary.pending_rows)
}

/// How a row sits in a fold, read from the display-only details the view
/// projection attaches. `None` is an ordinary row, which is every row when
/// folding is off.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FoldMark {
    /// A collapsed run standing for `count` records.
    Collapsed,
    /// The first displayed member of an expanded run.
    RunStart,
    /// A member with more of the run above and below it.
    RunBody,
    /// The last displayed member of an expanded run.
    RunEnd,
}

impl FoldMark {
    /// The gutter glyph. Collapsed reuses the pane's own disclosure mark, and an
    /// expanded run is bracketed with the box-drawing set the panes' borders are
    /// already drawn from, so this is the existing vocabulary rather than a
    /// second one. The ASCII forms keep the same three shapes: a mark, a
    /// continuing line, and a cap at each end.
    fn glyph(self, ascii: bool) -> char {
        match (self, ascii) {
            (FoldMark::Collapsed, false) => '›',
            (FoldMark::Collapsed, true) => '>',
            (FoldMark::RunStart, false) => '┌',
            (FoldMark::RunStart, true) => '+',
            (FoldMark::RunBody, false) => '│',
            (FoldMark::RunBody, true) => '|',
            (FoldMark::RunEnd, false) => '└',
            (FoldMark::RunEnd, true) => '+',
        }
    }

    /// The glyph for a continuation line of a multi-line fold entry.
    fn continuation(self, ascii: bool) -> char {
        match (self, ascii) {
            (FoldMark::Collapsed, false) => '│',
            (FoldMark::Collapsed, true) => '|',
            (mark, ascii) => mark.glyph(ascii),
        }
    }

    fn from_details(details: &[(String, String)]) -> Option<Self> {
        if details.iter().any(|(key, _)| key == "fold_entry") {
            return Some(FoldMark::Collapsed);
        }
        let member = details
            .iter()
            .find(|(key, _)| key == "fold_member")
            .map(|(_, value)| value.as_str())?;
        Some(match member {
            "first" => FoldMark::RunStart,
            "last" => FoldMark::RunEnd,
            _ => FoldMark::RunBody,
        })
    }
}

fn detail<'a>(details: &'a [(String, String)], key: &str) -> Option<&'a str> {
    details
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

/// The line a collapsed entry carries under its pattern: how many records it
/// stands for and the span they cover. The count is the fact the user asked the
/// fold for, so it leads.
fn fold_summary_line(row: &crate::DisplayRow, ascii: bool) -> Option<String> {
    let count = detail(&row.details, "fold_count")?;
    let arrow = if ascii { "->" } else { "→" };
    let times = if ascii { "x" } else { "×" };
    let mut line = match detail(&row.details, "fold_last_time") {
        Some(last) if !row.timestamp.is_empty() && last != row.timestamp => {
            format!("{times}{count} events  {} {arrow} {last}", row.timestamp)
        }
        _ => format!("{times}{count} events"),
    };
    // A run keyed on a column has no shape of its own to show, so the line that
    // carries the count names what grouped it instead.
    if let Some(column) = detail(&row.details, "fold_key_column") {
        line.push_str(&format!("  on {column}"));
    }
    Some(line)
}

/// Split a fold pattern so its `<ts>`, `<num>`, `<uuid>` and `<ip>` placeholders
/// can be styled apart from the literal text. They stand for the parts the
/// members differ in; left unmarked they read as log text that happens to
/// contain angle brackets.
fn pattern_segments(pattern: &str) -> Vec<(String, bool)> {
    let mut segments = Vec::new();
    let mut literal = String::new();
    let mut rest = pattern;
    while let Some(open) = rest.find('<') {
        match rest[open..].find('>') {
            Some(close) => {
                literal.push_str(&rest[..open]);
                let placeholder = &rest[open..=open + close];
                // Only the machine placeholders the key grammar produces, so a
                // record that genuinely contains `<foo>` is not restyled.
                if matches!(
                    placeholder,
                    "<ts>" | "<num>" | "<uuid>" | "<ip>" | "<hex>" | "<path>" | "<quoted>"
                ) {
                    if !literal.is_empty() {
                        segments.push((std::mem::take(&mut literal), false));
                    }
                    segments.push((placeholder.to_owned(), true));
                } else {
                    literal.push_str(placeholder);
                }
                rest = &rest[open + close + 1..];
            }
            None => break,
        }
    }
    literal.push_str(rest);
    if !literal.is_empty() {
        segments.push((literal, false));
    }
    segments
}

/// A fold entry's pattern: the gutter, then the shape, with the placeholders
/// that stand for what its members differ in styled apart from the literal text.
fn styled_pattern_line(
    pattern: &str,
    prefix: &str,
    horizontal: usize,
    row_style: Style,
    theme: Theme,
) -> Line<'static> {
    let mut spans = vec![Span::styled(
        prefix.to_owned(),
        row_style.fg(theme.accent).add_modifier(Modifier::BOLD),
    )];
    let visible: String = pattern.chars().skip(horizontal).collect();
    for (text, placeholder) in pattern_segments(&visible) {
        let style = if placeholder {
            row_style.fg(theme.muted).add_modifier(Modifier::ITALIC)
        } else {
            row_style
        };
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

/// The count-and-span line under a fold entry's pattern, under a continuing
/// gutter so the two lines read as one entry.
fn styled_fold_summary_line(
    summary: &str,
    gutter: char,
    row_style: Style,
    theme: Theme,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{gutter} "),
            row_style.fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(summary.to_owned(), row_style.fg(theme.muted)),
    ])
}

/// Columns the event cell needs before the fold gutter is worth its width.
///
/// The smallest supported terminal leaves the event column about nine
/// characters. Spending two of them on a marker, and a whole screen row on a
/// count, buys a cue at the cost of the text it is a cue about. Below this the
/// pane renders folds exactly as it did before the gutter existed; the status
/// line still says the view has runs in it and how much they hide.
const MIN_FOLD_GUTTER_EVENT_COLUMNS: usize = 20;

/// Width the event column is about to be given, which is whatever the fixed
/// columns before it leave. It has to be known before the row is built, because
/// it decides whether the fold gutter fits.
fn event_column_width(area: Rect, merged: bool, pinned: usize) -> usize {
    const BORDERS: usize = 2;
    const SPACING: usize = 1;
    const TIME: usize = 13;
    const LEVEL: usize = 6;
    const NAMED: usize = 14;
    let fixed = BORDERS
        + TIME
        + SPACING
        + LEVEL
        + SPACING
        + usize::from(merged) * (NAMED + SPACING)
        + pinned * (NAMED + SPACING);
    (area.width as usize).saturating_sub(fixed)
}

fn render_logs<P: RowProvider>(
    frame: &mut Frame<'_>,
    app: &mut App,
    provider: &P,
    area: Rect,
    theme: Theme,
) {
    if app.active_view_id().is_none() {
        frame.render_widget(
            Paragraph::new(
                app.action_notice
                    .as_deref()
                    .unwrap_or("No view selected. Add or discover a source, then create a view."),
            )
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .title(" Log viewport ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border)),
            ),
            area,
        );
        return;
    }
    if app.view_state().is_some_and(|state| state.last_total == 0) {
        let active = app
            .search_state()
            .is_some_and(|search| !search.applied.is_empty());
        let message = if active {
            "No matches. Clear the search to restore all rows."
        } else {
            "No rows in this view."
        };
        frame.render_widget(
            Paragraph::new(message).block(
                Block::default()
                    .title(" Log viewport ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border)),
            ),
            area,
        );
        return;
    }
    let selected = app.view_state().and_then(|state| state.selected.clone());
    let (pinned, color_field, expanded, top, horizontal) = {
        let state = app.view_state().expect("active view state");
        (
            state.pinned_columns.clone(),
            state.color_field.clone(),
            state.expanded_groups.clone(),
            state.top,
            state.horizontal_offset,
        )
    };
    let merged = app
        .view_source_ids(app.active_view_id().unwrap_or(""))
        .len()
        > 1;
    let fold_gutter_fits =
        event_column_width(area, merged, pinned.len()) >= MIN_FOLD_GUTTER_EVENT_COLUMNS;
    let visible = app.visible_rows(provider);
    app.hit_regions.log_row_indices.clear();
    let mut screen_y = area.y.saturating_add(2);
    let rows = visible
        .into_iter()
        .enumerate()
        .map(|(offset, row)| {
            let selected_row = selected.as_ref() == Some(&row.id);
            let style = record_style(&row, selected_row, color_field.as_deref(), theme);
            // The provider formats in UTC; the reader chooses the offset. A
            // row with no capture time keeps whatever the provider wrote,
            // because there is nothing to re-format from.
            let stamp = row.captured_at_unix_nanos.map_or_else(
                || row.timestamp.clone(),
                |nanos| crate::app::format_display_time(nanos, &app.appearance.display_zone),
            );
            let mut cells: Vec<Cell<'static>> = vec![stamp.into(), row.level.clone().into()];
            if merged {
                cells.push(
                    app.sources
                        .iter()
                        .find(|source| source.id == row.id.source_id)
                        .map(|source| source.name.clone())
                        .unwrap_or_else(|| row.id.source_id.clone())
                        .into(),
                );
            }
            cells.extend(
                pinned
                    .iter()
                    .map(|field| Cell::from(field_value(&row, field).unwrap_or("—").to_owned())),
            );
            let group_lines = row
                .details
                .iter()
                .filter(|(key, _)| key.starts_with("group_line_"))
                .map(|(_, value)| value.clone())
                .collect::<Vec<_>>();
            let is_expanded = expanded.contains(&row.id) && group_lines.len() > 1;
            let ascii = app.appearance.ascii;
            let fold = FoldMark::from_details(&row.details).filter(|_| fold_gutter_fits);
            let fold_pattern = fold
                .and_then(|_| detail(&row.details, "fold_pattern"))
                .map(str::to_owned);
            let fold_summary = fold
                .filter(|mark| *mark == FoldMark::Collapsed)
                .and_then(|_| fold_summary_line(&row, ascii));
            let bookmarked = app
                .bookmarks_for_view(app.active_view_id().unwrap_or(""))
                .iter()
                .any(|bookmark| bookmark.id == row.id);
            let star = if ascii { '*' } else { '★' };
            // The gutter is the prefix slot bookmarks already use. A folding
            // view spends its first column on the fold mark and its second on
            // the bookmark; a view that is not folding is exactly as it was.
            let prefix = match (fold, bookmarked) {
                (None, false) => None,
                (None, true) => Some(format!("{star} ")),
                (Some(mark), true) => Some(format!("{}{star}", mark.glyph(ascii))),
                (Some(mark), false) => Some(format!("{} ", mark.glyph(ascii))),
            };
            let lines = match (&fold_pattern, fold) {
                // A collapsed entry is not a log line, so it is not rendered as
                // one: its shape carries the placeholders that stand for what
                // its members differ in, and the count and span sit under it.
                (Some(pattern), Some(mark)) => {
                    let mut lines = vec![styled_pattern_line(
                        pattern,
                        prefix.as_deref().unwrap_or_default(),
                        horizontal,
                        style,
                        theme,
                    )];
                    lines.extend(fold_summary.as_ref().map(|summary| {
                        styled_fold_summary_line(summary, mark.continuation(ascii), style, theme)
                    }));
                    lines
                }
                _ => {
                    let event = if is_expanded {
                        group_lines.join("\n")
                    } else {
                        row.text
                    };
                    styled_event_lines(
                        &event,
                        prefix.as_deref(),
                        horizontal,
                        area.width as usize,
                        style,
                        selected_row,
                        theme,
                    )
                }
            };
            cells.push(Cell::from(Text::from(lines)));
            let height = if is_expanded {
                u16::try_from(group_lines.len()).unwrap_or(u16::MAX)
            } else if fold_summary.is_some() {
                2
            } else {
                1
            };
            let available = area.y.saturating_add(area.height).saturating_sub(1);
            let shown = height.min(available.saturating_sub(screen_y));
            if shown > 0 {
                app.hit_regions.log_row_indices.push((
                    Rect::new(
                        area.x.saturating_add(1),
                        screen_y,
                        area.width.saturating_sub(2),
                        shown,
                    ),
                    top + offset,
                ));
                screen_y = screen_y.saturating_add(shown);
            }
            Row::new(cells).height(height).style(style)
        })
        .collect::<Vec<_>>();
    let border = if app.focus == Focus::Logs {
        theme.active_border
    } else {
        theme.border
    };
    // The time column is as wide as the zone it shows: `Z` is one character,
    // an offset is six. Widening rather than truncating is what keeps a
    // timestamp readable as a timestamp.
    let mut widths = vec![
        Constraint::Length(crate::app::display_time_width(&app.appearance.display_zone)),
        Constraint::Length(6),
    ];
    if merged {
        widths.push(Constraint::Length(14));
    }
    widths.extend(pinned.iter().map(|_| Constraint::Length(14)));
    widths.push(Constraint::Min(1));
    let title = if horizontal == 0 {
        " Log viewport ".to_owned()
    } else {
        format!(" Log viewport · x={horizontal} ")
    };
    let mut headers = vec!["time".to_owned(), "level".to_owned()];
    if merged {
        headers.push("source".into());
    }
    headers.extend(pinned.iter().cloned());
    headers.push("event".into());
    frame.render_widget(
        Table::new(rows, widths)
            .header(Row::new(headers).style(Style::default().add_modifier(Modifier::BOLD)))
            .column_spacing(1)
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border)),
            ),
        area,
    );
}

/// The style one record is painted in, wherever it is shown.
///
/// The log pane and the docked Details pane show the same records, so they
/// resolve their colours here rather than each deciding for itself. The ladder
/// is: the selection highlight, which is a cursor rather than a property of the
/// record; then the hashed colour of the field the view is coloured by; then
/// the record's severity. `Style::default()` inherits the surface, which is why
/// both panes must draw on the same background — the hashed identity colours
/// are lifted to [`crate::theme::MIN_IDENTITY_CONTRAST`] against `base_bg` and
/// only read as measured there.
///
/// Details passes `selected = false`: the record it shows is by definition the
/// selected one, and painting the whole pane in the selection colours would
/// tell the user where the cursor is, which they can already see.
///
/// This and [`styled_event_lines`] are the two seams record colouring goes
/// through. Anything that decides what colour a record is — a matched colour
/// rule, a highlighted search span — belongs in one of them, so that every
/// surface showing the record picks it up rather than the log picking it up
/// and the panes drifting again.
fn record_style(
    row: &crate::provider::DisplayRow,
    selected: bool,
    color_field: Option<&str>,
    theme: Theme,
) -> Style {
    if selected {
        Style::default()
            .fg(theme.selection_fg)
            .bg(theme.selection_bg)
    } else if let Some(value) = color_field.and_then(|field| field_value(row, field)) {
        // `value_style`, not `value_color`: at sixteen colours an identity may
        // also be bold, because six hues is not enough on its own.
        theme.value_style(value)
    } else if let Some(color) = theme.severity_color(&row.level) {
        Style::default().fg(color)
    } else {
        Style::default()
    }
}

/// One piece of a record's text, styled exactly as the log pane styles it, with
/// no clipping: JSON tokens take their own colours through
/// [`crate::details::json_kind_style`] and everything else keeps `row_style`.
///
/// The Details pane draws the same records as the log and wraps rather than
/// scrolling sideways, so it shares this rather than restating the rules
/// (§12.20). The horizontal window belongs to the log pane alone, which is why
/// this takes none.
pub(crate) fn styled_record_text(text: &str, row_style: Style, theme: Theme) -> Line<'static> {
    let tokens = classify(text);
    styled_event_line_with_tokens(
        text,
        None,
        EventRender {
            horizontal: 0,
            width: usize::MAX,
            row_style,
            selected: false,
            theme,
        },
        tokens.as_deref(),
    )
}

#[cfg(test)]
fn styled_event_line(
    text: &str,
    prefix: Option<&str>,
    horizontal: usize,
    width: usize,
    row_style: Style,
    selected: bool,
    theme: Theme,
) -> Line<'static> {
    let tokens = classify(text);
    styled_event_line_with_tokens(
        text,
        prefix,
        EventRender {
            horizontal,
            width,
            row_style,
            selected,
            theme,
        },
        tokens.as_deref(),
    )
}

#[derive(Clone, Copy)]
struct EventRender {
    horizontal: usize,
    width: usize,
    row_style: Style,
    selected: bool,
    theme: Theme,
}

fn styled_event_lines(
    text: &str,
    prefix: Option<&str>,
    horizontal: usize,
    width: usize,
    row_style: Style,
    selected: bool,
    theme: Theme,
) -> Vec<Line<'static>> {
    // The displayed event is the JSON record boundary. Validate it before
    // splitting visual lines so valid scalar fragments inside malformed
    // multiline input cannot receive misleading partial highlighting.
    let json_tokens = if selected { None } else { classify(text) };
    let mut line_start = 0usize;
    text.split_inclusive('\n')
        .enumerate()
        .map(|(index, chunk)| {
            let start = line_start;
            line_start = line_start.saturating_add(chunk.len());
            // Match `str::lines` display behavior while retaining the actual
            // consumed byte length for token offsets after LF or CRLF.
            let line = if let Some(without_lf) = chunk.strip_suffix('\n') {
                without_lf.strip_suffix('\r').unwrap_or(without_lf)
            } else {
                chunk
            };
            let line_tokens = json_tokens.as_ref().map(|tokens| {
                tokens
                    .iter()
                    .filter(|token| {
                        token.bytes.start >= start
                            && token.bytes.end <= start.saturating_add(line.len())
                    })
                    .map(|token| JsonSpan {
                        bytes: token.bytes.start - start..token.bytes.end - start,
                        kind: token.kind.clone(),
                    })
                    .collect::<Vec<_>>()
            });
            styled_event_line_with_tokens(
                line,
                (index == 0).then_some(prefix).flatten(),
                EventRender {
                    horizontal,
                    width,
                    row_style,
                    selected,
                    theme,
                },
                line_tokens.as_deref(),
            )
        })
        .collect()
}

fn styled_event_line_with_tokens(
    text: &str,
    prefix: Option<&str>,
    render: EventRender,
    tokens: Option<&[JsonSpan]>,
) -> Line<'static> {
    let EventRender {
        horizontal,
        width,
        row_style,
        selected,
        theme,
    } = render;
    let mut pieces = Vec::new();
    if let Some(prefix) = prefix {
        pieces.push((prefix, row_style));
    }
    if selected {
        pieces.push((text, row_style));
    } else if let Some(tokens) = tokens {
        let mut at = 0;
        for token in tokens {
            if token.bytes.start > at {
                pieces.push((&text[at..token.bytes.start], row_style));
            }
            // §12.20: one function decides what a JSON token looks like, so the
            // log line and the Details tree cannot disagree about a number.
            let foreground = crate::details::json_kind_style(&token.kind, theme);
            pieces.push((
                &text[token.bytes.clone()],
                foreground
                    .fg
                    .map_or(row_style, |colour| row_style.fg(colour)),
            ));
            at = token.bytes.end;
        }
        if at < text.len() {
            pieces.push((&text[at..], row_style));
        }
    } else {
        pieces.push((text, row_style));
    }
    clip_styled_columns(pieces, horizontal, width)
}

fn clip_styled_columns(pieces: Vec<(&str, Style)>, offset: usize, width: usize) -> Line<'static> {
    let mut output: Vec<(String, Style)> = Vec::new();
    let mut position = 0usize;
    let mut written = 0usize;
    let mut visible_base = false;
    'stream: for (piece, style) in pieces {
        for character in piece.chars() {
            if character.is_control() {
                continue;
            }
            let columns = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
            if columns == 0 {
                if visible_base {
                    push_styled_character(&mut output, character, style);
                }
                continue;
            }
            let start = position;
            position = position.saturating_add(columns);
            if position <= offset {
                visible_base = false;
                continue;
            }
            if start < offset {
                let remaining = position - offset;
                if written.saturating_add(remaining) > width {
                    break 'stream;
                }
                for _ in 0..remaining {
                    push_styled_character(&mut output, ' ', style);
                }
                written += remaining;
                visible_base = false;
            } else {
                if written.saturating_add(columns) > width {
                    break 'stream;
                }
                push_styled_character(&mut output, character, style);
                written += columns;
                visible_base = true;
            }
        }
    }
    Line::from(
        output
            .into_iter()
            .map(|(text, style)| Span::styled(text, style))
            .collect::<Vec<_>>(),
    )
}

fn push_styled_character(output: &mut Vec<(String, Style)>, character: char, style: Style) {
    if let Some((text, _)) = output.last_mut().filter(|(_, previous)| *previous == style) {
        text.push(character);
    } else {
        output.push((character.to_string(), style));
    }
}

fn render_details<P: RowProvider>(
    frame: &mut Frame<'_>,
    app: &mut App,
    provider: &P,
    area: Rect,
    theme: Theme,
) {
    let styles = DialogStyles::new(theme);
    let row = app.selected_row(provider);
    let row_id = row.as_ref().map(|row| row.id.clone());
    // §8.11: a JSON record is a tree with per-view expansion memory; the
    // cursor row is what Enter, Left and Right act on while the pane has focus.
    let (expanded, cursor) = app
        .view_state()
        .map(|state| (state.expanded_paths.clone(), state.details_cursor))
        .unwrap_or_default();
    // The record's own colours, resolved by the ladder the log pane uses, so
    // the same record reads the same in both.
    let color_field = app.view_state().and_then(|state| state.color_field.clone());
    let base = row
        .as_ref()
        .map(|row| record_style(row, false, color_field.as_deref(), theme))
        .unwrap_or_default();
    let view = row.as_ref().map(|row| {
        crate::details::details_view(
            row,
            &expanded,
            cursor,
            app.focus == Focus::Details,
            base,
            theme,
            app.appearance.ascii,
        )
    });
    let lines = match &view {
        Some(view) => view.lines.clone(),
        None => vec![Line::styled("No selected event", styles.unavailable)],
    };
    let block = Block::default()
        .title(" Selected event details ")
        .borders(Borders::ALL)
        // Details is a docked pane, not a dialog (dialog-system.md §12.20), and
        // it shares the workspace surface with the log it describes. It is also
        // what makes the hashed identity colours honest: `Theme::value_color`
        // lifts them until they clear `MIN_IDENTITY_CONTRAST` against `base_bg`,
        // and a value drawn on any other surface is a colour whose readability
        // nothing measured.
        .style(Style::default().bg(theme.base_bg))
        .border_style(Style::default().fg(if app.focus == Focus::Details {
            theme.focused_input_border
        } else {
            theme.border
        }));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    // §8.10: the docked pane carries no key footer; the arrows are routine.
    let content = inner;
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let limit = paragraph
        .line_count(content.width)
        .saturating_sub(usize::from(content.height));
    let scroll = app.set_details_viewport(row_id, limit);
    // Keep the cursor row on screen: the tree rows above it are one visual
    // line each only when they fit, so measure the wrapped height of the
    // lines before the cursor rather than assuming it.
    let scroll = match view.as_ref().and_then(|view| view.cursor_line) {
        Some(cursor_line) if app.focus == Focus::Details => {
            let above = Paragraph::new(
                view.as_ref()
                    .map(|view| view.lines[..cursor_line].to_vec())
                    .unwrap_or_default(),
            )
            .wrap(Wrap { trim: false })
            .line_count(content.width);
            app.reveal_details_line(above, usize::from(content.height))
        }
        _ => scroll,
    };
    frame.render_widget(
        paragraph.scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        content,
    );
}

fn field_value<'a>(row: &'a crate::DisplayRow, field: &str) -> Option<&'a str> {
    row.fields
        .iter()
        .find(|(key, _)| key == field)
        .map(|(_, value)| value.as_str())
}

/// §6.3: a placeholder marks an empty field without pretending to be a value.
pub(crate) fn render_placeholder(frame: &mut Frame<'_>, field: Rect, text: &str, theme: Theme) {
    if field.width == 0 || text.is_empty() {
        return;
    }
    frame.render_widget(
        Paragraph::new(truncated(text, usize::from(field.width))).style(
            Style::default()
                .fg(ensure_contrast(theme.muted, theme.input_bg, 4.5))
                .bg(theme.input_bg)
                .add_modifier(Modifier::ITALIC),
        ),
        field,
    );
}

// The enrichment work landed private copies of these while dialog_layout.rs did
// not exist yet. They are now thin adapters over the shared primitives so there
// is one implementation of the spec, and its call sites did not have to move.
pub(crate) fn dialog_compact(area: Rect) -> bool {
    crate::dialog_layout::is_compact(area)
}

pub(crate) fn class_l_width(area: Rect) -> u16 {
    crate::dialog_layout::DialogClass::L.width(area)
}

pub(crate) type DialogRegions = crate::dialog_layout::DialogRegions;

pub(crate) fn class_l_content(
    body_rows: u16,
    message: u16,
    help: u16,
    actions: u16,
) -> crate::dialog_layout::DialogContent {
    crate::dialog_layout::DialogContent {
        header: 0,
        body: body_rows,
        message,
        help,
        actions,
    }
}

pub(crate) fn class_l_popup(
    area: Rect,
    body_rows: u16,
    message: u16,
    help: u16,
    actions: u16,
) -> Rect {
    crate::dialog_layout::dialog_rect(
        area,
        crate::dialog_layout::DialogClass::L,
        &class_l_content(body_rows, message, help, actions),
    )
}

pub(crate) fn dialog_regions(popup: Rect, message: u16, help: u16, actions: u16) -> DialogRegions {
    // These callers size their own popup first and then take whatever the body
    // has left, so the body they "want" is only the floor that decides whether
    // the layout is squeezed enough to shed help and padding.
    crate::dialog_layout::regions(
        popup,
        &class_l_content(MIN_BODY_ROWS, message, help, actions),
    )
}

/// §6.3/§7.4 message row: glyph, padded state word, one sentence. The
/// vocabulary is closed by the spec; `Scanned` and `Unrun` belong to Storage
/// and External command, which are not on the anatomy yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum MessageState {
    Ready,
    Applied,
    NoFilter,
    Disabled,
    Pending,
    Updating,
    Saved,
    Scanned,
    Unrun,
    Error,
}

/// §4.4: word wrapping whose continuation starts at the sentence column, and
/// which ends with an ellipsis rather than clipping silently.
pub(crate) fn wrap_sentence(sentence: &str, width: usize, rows: usize) -> Vec<String> {
    if width == 0 || rows == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in sentence.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_owned()
        } else {
            format!("{current} {word}")
        };
        if UnicodeWidthStr::width(candidate.as_str()) <= width {
            current = candidate;
            continue;
        }
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            if lines.len() == rows {
                break;
            }
        }
        current = truncated(word, width);
    }
    if lines.len() < rows && !current.is_empty() {
        lines.push(current);
    }
    let consumed: usize = lines
        .iter()
        .map(|line| line.split_whitespace().count())
        .sum();
    if consumed < sentence.split_whitespace().count()
        && let Some(last) = lines.last_mut()
    {
        *last = truncated(&format!("{last} …"), width);
    }
    lines
}

/// §7.4: glyph, space, then the state word padded to 9 cells and a space.
pub(crate) const MESSAGE_SENTENCE_COLUMN: u16 = 12;

pub(crate) fn message_rows(sentence: &str, content_width: u16) -> u16 {
    wrap_sentence(
        sentence,
        usize::from(content_width.saturating_sub(MESSAGE_SENTENCE_COLUMN)).max(1),
        2,
    )
    .len() as u16
}

fn message_line(
    state: MessageState,
    sentence: String,
    theme: Theme,
    ascii: bool,
) -> (Line<'static>, Style) {
    let styles = DialogStyles::new(theme);
    let (glyph, ascii_glyph, word, role) = match state {
        MessageState::Ready => ("○", "o", "Ready", styles.applied),
        MessageState::Applied => ("●", "*", "Applied", styles.applied),
        MessageState::NoFilter => ("○", "o", "No filter", styles.description),
        MessageState::Disabled => ("○", "o", "Disabled", styles.description),
        MessageState::Pending => ("◐", "~", "Pending", styles.pending),
        MessageState::Updating => ("◐", "~", "Updating", styles.pending),
        MessageState::Saved => ("●", "*", "Saved", styles.applied),
        MessageState::Scanned => ("●", "*", "Scanned", styles.applied),
        MessageState::Unrun => ("○", "o", "Unrun", styles.description),
        MessageState::Error => ("✖", "x", "Error", styles.error),
    };
    let glyph = if ascii { ascii_glyph } else { glyph };
    (
        Line::from(vec![
            Span::styled(format!("{glyph} "), role),
            Span::styled(format!("{word:<9} "), role.add_modifier(Modifier::BOLD)),
            Span::styled(sentence, styles.description),
        ]),
        role,
    )
}

/// §8.7 pane: bold heading, optional right-aligned count, indented viewport and
/// a one-column scrollbar only when the content actually overflows.
pub(crate) struct PaneRects {
    pub(crate) viewport: Rect,
    pub(crate) scrollbar: Option<Rect>,
}

pub(crate) fn render_message(
    frame: &mut Frame<'_>,
    area: Rect,
    state: MessageState,
    sentence: &str,
    theme: Theme,
    ascii: bool,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let styles = DialogStyles::new(theme);
    let width = usize::from(area.width.saturating_sub(12)).max(1);
    let wrapped = wrap_sentence(sentence, width, usize::from(area.height).min(2));
    let (head, role) = message_line(
        state,
        wrapped.first().cloned().unwrap_or_default(),
        theme,
        ascii,
    );
    let _ = role;
    let mut lines = vec![head];
    for continuation in wrapped.iter().skip(1) {
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(12)),
            Span::styled(continuation.clone(), styles.description),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

pub(crate) fn render_pane_heading(
    frame: &mut Frame<'_>,
    area: Rect,
    heading: &str,
    count: Option<String>,
    lines: usize,
    theme: Theme,
) -> PaneRects {
    let styles = DialogStyles::new(theme);
    if area.height == 0 || area.width == 0 {
        return PaneRects {
            viewport: Rect::new(area.x, area.y, 0, 0),
            scrollbar: None,
        };
    }
    let heading_row = Rect::new(area.x, area.y, area.width, 1);
    let mut spans = vec![Span::styled(
        heading.to_owned(),
        styles.label.add_modifier(Modifier::BOLD),
    )];
    if let Some(count) = count {
        let used =
            UnicodeWidthStr::width(heading).saturating_add(UnicodeWidthStr::width(count.as_str()));
        let filler = usize::from(area.width).saturating_sub(used);
        spans.push(Span::styled(" ".repeat(filler), styles.description));
        spans.push(Span::styled(count, styles.description));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), heading_row);
    let body = Rect::new(
        area.x.saturating_add(2),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(1),
    );
    if body.height == 0 || body.width == 0 {
        return PaneRects {
            viewport: body,
            scrollbar: None,
        };
    }
    let overflows = lines > usize::from(body.height) && body.height >= 3;
    let scrollbar =
        overflows.then(|| Rect::new(body.right().saturating_sub(1), body.y, 1, body.height));
    let viewport = if overflows {
        Rect::new(body.x, body.y, body.width.saturating_sub(1), body.height)
    } else {
        body
    };
    PaneRects {
        viewport,
        scrollbar,
    }
}

/// §6.3 scrollbar glyphs; drawn only for real overflow.
pub(crate) fn render_scrollbar(
    frame: &mut Frame<'_>,
    area: Rect,
    offset: usize,
    limit: usize,
    theme: Theme,
    ascii: bool,
) {
    if area.height == 0 || limit == 0 {
        return;
    }
    let styles = DialogStyles::new(theme);
    let (up, down, track, thumb) = if ascii {
        ("^", "v", "|", "#")
    } else {
        ("▲", "▼", "│", "█")
    };
    let height = usize::from(area.height);
    let inner = height.saturating_sub(2);
    let thumb_row = if inner == 0 {
        0
    } else {
        offset.saturating_mul(inner.saturating_sub(1)) / limit.max(1)
    };
    let mut lines = Vec::with_capacity(height);
    for row in 0..height {
        let (glyph, style) = if row == 0 && height > 1 {
            (up, styles.description)
        } else if row + 1 == height && height > 1 {
            (down, styles.description)
        } else if inner > 0 && row.saturating_sub(1) == thumb_row.min(inner.saturating_sub(1)) {
            (thumb, styles.shortcut)
        } else {
            (track, styles.description)
        };
        lines.push(Line::styled(glyph.to_owned(), style));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// §9: a clipped value ends with an ellipsis so the truncation is visible.
pub(crate) fn truncated(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width || width == 0 {
        return text.to_owned();
    }
    let mut result = clipped_width(text, width.saturating_sub(1));
    result.push('…');
    result
}

pub(crate) fn step_summary(source: &str, width: usize) -> String {
    truncated(&source.replace('\n', " ⏎ "), width)
}

/// Packs button labels the way `button_layout` does, returning the row count.
pub(crate) fn packed_button_rows(width: u16, labels: &[&str]) -> u16 {
    if width == 0 || labels.is_empty() {
        return 0;
    }
    let mut rows = 1u16;
    let mut x = 0u16;
    for label in labels {
        let label_width = button_width(label).min(width);
        if x != 0 && x.saturating_add(label_width) > width {
            rows = rows.saturating_add(1);
            x = 0;
        }
        x = x.saturating_add(label_width).saturating_add(ACTION_GUTTER);
    }
    rows
}

pub(crate) fn render_dialog_frame(
    frame: &mut Frame<'_>,
    popup: Rect,
    title: String,
    active: bool,
    theme: Theme,
) {
    let colour = if active {
        theme.active_border
    } else {
        theme.border
    };
    frame.render_widget(
        Block::default()
            .title(Span::styled(
                title,
                Style::default().fg(colour).add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(colour)),
        popup,
    );
}

/// One button of a `button_layout` row (the focus-revealing layout the two
/// enrichment layers use), styled by its §8.9 role: `default` is the one
/// Enter executes, and it is filled.
pub(crate) fn render_enrichment_button(
    frame: &mut Frame<'_>,
    rect: Rect,
    label: &str,
    focused: bool,
    default: bool,
    theme: Theme,
) {
    let role = if default {
        ButtonRole::Default
    } else {
        ButtonRole::Normal
    };
    render_role_button(frame, rect, label, role, focused, theme);
}

/// The completion popup, drawn from state its owner holds. Returns the popup
/// rect and the row rects it painted, so the owner records exactly what was
/// drawn: the two layers that offer completion — Advanced and the enrichment
/// step editor — each keep them in their own geometry (§5.1).
pub(crate) fn draw_editor_completion(
    frame: &mut Frame<'_>,
    area: Rect,
    completion: &crate::app::EditorCompletionState,
    theme: Theme,
) -> (Rect, Vec<(Rect, usize)>) {
    let popup = centered(area, 76, 12);
    clear_themed(frame, popup, theme);
    let mut rows: Vec<(Rect, usize)> = Vec::new();
    let styles = DialogStyles::new(theme);
    let inner = Block::default()
        .title(match completion.kind {
            crate::app::EditorCompletionKind::Field => " Complete field ",
            crate::app::EditorCompletionKind::SampledValue => " Static sampled string literals ",
        })
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent));
    let content = dialog_body(popup);
    frame.render_widget(inner, popup);
    if content.height == 0 {
        return (popup, rows);
    }
    let sections = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(content);
    let list_area = sections[0];
    let visible = usize::from(list_area.height).max(1);
    let top = completion
        .top
        .min(completion.items.len().saturating_sub(visible));
    let mut lines = Vec::new();
    for (offset, (index, item)) in completion
        .items
        .iter()
        .enumerate()
        .skip(top)
        .take(visible)
        .enumerate()
    {
        let selected = index == completion.selected;
        lines.push(Line::styled(
            format!(
                "{} {}",
                if selected { ">" } else { " " },
                clipped_width(&item.label, usize::from(content.width.saturating_sub(3)))
            ),
            if selected {
                styles.selection
            } else {
                styles.description
            },
        ));
        rows.push((
            Rect::new(list_area.x, list_area.y + offset as u16, list_area.width, 1),
            index,
        ));
    }
    if completion.items.is_empty() {
        lines.push(Line::styled("(no sampled completions)", styles.unavailable));
    }
    frame.render_widget(Paragraph::new(lines), list_area);
    frame.render_widget(
        Paragraph::new(clipped_width(
            &completion.status,
            usize::from(sections[1].width),
        ))
        .style(styles.description),
        sections[1],
    );
    (popup, rows)
}

/// §12.7. Rows the proposal preview keeps for its own border and heading.
/// Draws a focused single-line input window and its caret, and returns the
/// cell the caret landed on so a component can report it in its `Surface`.
pub(crate) fn place_input_cursor_at(
    frame: &mut Frame<'_>,
    area: Rect,
    first_row: usize,
    prefix_width: usize,
    value: &str,
    cursor: usize,
    theme: Theme,
) -> Option<(u16, u16)> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let prefix = u16::try_from(prefix_width)
        .unwrap_or(u16::MAX)
        .min(area.width);
    let field = Rect::new(
        area.x.saturating_add(prefix),
        area.y
            .saturating_add(u16::try_from(first_row).unwrap_or(u16::MAX))
            .min(area.bottom().saturating_sub(1)),
        area.width.saturating_sub(prefix),
        1,
    );
    if field.width == 0 {
        return None;
    }
    InputSurface {
        style: Style::default().fg(theme.input_fg).bg(theme.input_bg),
    }
    .render(field, frame.buffer_mut());
    let cursor = cursor.min(value.chars().count());
    let byte_at = value
        .char_indices()
        .nth(cursor)
        .map_or(value.len(), |(index, _)| index);
    let line_start = value[..byte_at].rfind('\n').map_or(0, |index| index + 1);
    let line_end = value[byte_at..]
        .find('\n')
        .map_or(value.len(), |index| byte_at + index);
    let before = input_tail(&value[line_start..byte_at], usize::from(field.width - 1));
    let remaining =
        usize::from(field.width - 1).saturating_sub(UnicodeWidthStr::width(before.as_str()));
    let after = clipped_width(&value[byte_at..line_end], remaining);
    let scrolled = before.len() < byte_at.saturating_sub(line_start);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                before.clone(),
                Style::default().fg(theme.input_fg).bg(theme.input_bg),
            ),
            Span::styled(
                after,
                Style::default().fg(theme.input_fg).bg(theme.input_bg),
            ),
        ]))
        .style(Style::default().fg(theme.input_fg).bg(theme.input_bg)),
        field,
    );
    if scrolled {
        // The leading cell marks the hidden head of the value.
        frame.render_widget(
            Paragraph::new(ELLIPSIS).style(
                Style::default()
                    .fg(ensure_contrast(theme.muted, theme.input_bg, 4.5))
                    .bg(theme.input_bg),
            ),
            Rect::new(field.x, field.y, 1, 1),
        );
    }
    let column = UnicodeWidthStr::width(before.as_str());
    let x = field
        .x
        .saturating_add(u16::try_from(column).unwrap_or(u16::MAX))
        .min(field.right().saturating_sub(1));
    let y = field.y;
    frame.render_widget(
        Block::default().style(Style::default().fg(theme.input_fg).bg(theme.cursor)),
        Rect::new(x, y, 1, 1),
    );
    frame.set_cursor_position((x, y));
    Some((x, y))
}

pub(crate) fn input_tail(value: &str, maximum_width: usize) -> String {
    let mut width = 0usize;
    let mut start = value.len();
    for (index, character) in value.char_indices().rev() {
        let character_width = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if width.saturating_add(character_width) > maximum_width {
            break;
        }
        width = width.saturating_add(character_width);
        start = index;
    }
    // A zero-width combining mark belongs to the clipped character before it;
    // never start the viewport with an orphaned mark.
    while let Some(character) = value[start..].chars().next() {
        if unicode_width::UnicodeWidthChar::width(character).unwrap_or(0) != 0 {
            break;
        }
        start = start.saturating_add(character.len_utf8());
    }
    value[start..].to_owned()
}

pub(crate) fn time_input_window(
    value: &str,
    caret: usize,
    maximum_width: usize,
) -> (String, usize) {
    if maximum_width == 0 {
        return (String::new(), 0);
    }
    let chars: Vec<char> = value.chars().collect();
    let caret = caret.min(chars.len());
    let before: String = chars[..caret].iter().collect();
    let visible_before = input_tail(&before, maximum_width.saturating_sub(1));
    let caret_column = visible_before.width().min(maximum_width.saturating_sub(1));
    let mut visible = visible_before;
    let mut width = visible.width();
    for ch in &chars[caret..] {
        let char_width = unicode_width::UnicodeWidthChar::width(*ch).unwrap_or(0);
        if width.saturating_add(char_width) > maximum_width {
            break;
        }
        visible.push(*ch);
        width += char_width;
    }
    (visible, caret_column)
}

/// Rows a help sentence needs (§3 `help_h`, capped at 2).
pub(crate) fn help_rows(help: &str, width: u16) -> u16 {
    if help.is_empty() || width == 0 {
        return 0;
    }
    u16::try_from(
        Paragraph::new(help)
            .wrap(Wrap { trim: true })
            .line_count(width),
    )
    .unwrap_or(1)
    .min(2)
}

pub(crate) fn render_help_text(frame: &mut Frame<'_>, rect: Rect, help: &str, theme: Theme) {
    if rect.height == 0 || help.is_empty() {
        return;
    }
    frame.render_widget(
        Paragraph::new(help)
            .wrap(Wrap { trim: true })
            .style(DialogStyles::new(theme).description),
        rect,
    );
}

/// §4.1 `gutter` between buttons: the one `dialog_controls` measures and
/// places with.
pub(crate) const ACTION_GUTTER: u16 = crate::dialog_controls::BUTTON_GUTTER;

/// §4.1 `gutter` between the label column and the field column.
pub(crate) const FIELD_GUTTER: u16 = 2;

/// §8.2 button row with the first button as the default (§8.9). The shape
/// every dialog whose default *is* its first button uses; a dialog whose
/// default moves with state names it through `render_actions`.
pub(crate) fn render_action_row(
    frame: &mut Frame<'_>,
    rect: Rect,
    labels: &[&str],
    focused: Option<usize>,
    destructive: &[usize],
    theme: Theme,
) -> Vec<(usize, Rect)> {
    render_actions(
        frame,
        rect,
        ActionRow {
            labels,
            default: Some(0),
            destructive,
            focused,
        },
        theme,
    )
}

/// §8.2 / §8.9 button row: buttons in the declared order, each styled by its
/// role through `dialog_controls::role_style`, the focused one in the
/// selection style. Returns the hitboxes actually drawn, which are the same
/// rects the mouse handler is given.
pub(crate) fn render_actions(
    frame: &mut Frame<'_>,
    rect: Rect,
    row: ActionRow<'_>,
    theme: Theme,
) -> Vec<(usize, Rect)> {
    if rect.height == 0 || rect.width == 0 {
        return Vec::new();
    }
    let mut placed = Vec::new();
    let mut x = rect.x;
    let mut y = rect.y;
    for (index, label) in row.labels.iter().enumerate() {
        let width = button_width(label).min(rect.width);
        if x > rect.x && x.saturating_add(width) > rect.right() {
            x = rect.x;
            y = y.saturating_add(1);
        }
        if y >= rect.bottom() {
            break;
        }
        let button = Rect::new(x, y, width, 1);
        render_role_button(
            frame,
            button,
            label,
            row.role(index),
            row.focused == Some(index),
            theme,
        );
        placed.push((index, button));
        x = x.saturating_add(width).saturating_add(ACTION_GUTTER);
    }
    placed
}

/// §8.6 segmented mode control: `␣A␣│␣B␣│␣C␣` starting at `content.x`. The
/// active segment carries the selection style; separators use the border role.
/// Returns the hitbox for each segment so a click lands where it is drawn.
pub(crate) fn render_segmented_control(
    frame: &mut Frame<'_>,
    rect: Rect,
    labels: &[&str],
    active: usize,
    focused: Option<usize>,
    theme: Theme,
) -> Vec<Rect> {
    if rect.height == 0 || rect.width == 0 {
        return Vec::new();
    }
    let styles = DialogStyles::new(theme);
    let roomy: usize = labels
        .iter()
        .map(|label| UnicodeWidthStr::width(*label) + 2)
        .sum::<usize>()
        + labels.len().saturating_sub(1) * 3;
    let padded = roomy <= usize::from(rect.width);
    let separator = if padded { " │ " } else { "│" };
    let separator_width = u16::try_from(UnicodeWidthStr::width(separator)).unwrap_or(1);
    let mut spans = Vec::new();
    let mut rects = Vec::new();
    let mut x = rect.x;
    for (index, label) in labels.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(separator, styles.unavailable));
            x = x.saturating_add(separator_width);
        }
        let text = if padded {
            format!(" {label} ")
        } else {
            (*label).to_owned()
        };
        let width = u16::try_from(UnicodeWidthStr::width(text.as_str())).unwrap_or(0);
        let style = if focused == Some(index) {
            styles
                .selection
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else if index == active {
            styles.selection.add_modifier(Modifier::BOLD)
        } else {
            styles.label
        };
        spans.push(Span::styled(text, style));
        rects.push(Rect::new(x.min(rect.right()), rect.y, width, 1));
        x = x.saturating_add(width);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
    rects
}

/// §8.4 radio group: `● File   ○ Command`, sharing one row. Returns each
/// option's hitbox.
pub(crate) fn render_radio_row(
    frame: &mut Frame<'_>,
    rect: Rect,
    labels: &[&str],
    active: usize,
    focused: Option<usize>,
    ascii: bool,
    theme: Theme,
) -> Vec<Rect> {
    if rect.height == 0 || rect.width == 0 {
        return Vec::new();
    }
    let styles = DialogStyles::new(theme);
    let mut spans = Vec::new();
    let mut rects = Vec::new();
    let mut x = rect.x;
    for (index, label) in labels.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("   ", styles.description));
            x = x.saturating_add(3);
        }
        let glyph = match (index == active, ascii) {
            (true, true) => "(*)",
            (false, true) => "( )",
            (true, false) => "●",
            (false, false) => "○",
        };
        let text = format!("{glyph} {label}");
        let width = u16::try_from(UnicodeWidthStr::width(text.as_str())).unwrap_or(0);
        spans.push(Span::styled(
            text,
            if focused == Some(index) {
                styles.shortcut.add_modifier(Modifier::BOLD)
            } else {
                styles.label
            },
        ));
        rects.push(Rect::new(x.min(rect.right()), rect.y, width, 1));
        x = x.saturating_add(width);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
    rects
}

/// §4.2 label column plus a fill field. The painted input rect is exactly the
/// field, and the caret is placed inside it when `focused`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_form_field(
    frame: &mut Frame<'_>,
    row: Rect,
    label_width: u16,
    label: &str,
    value: &str,
    placeholder: &str,
    focused: bool,
    cursor: Option<usize>,
    theme: Theme,
) -> (Rect, Option<(u16, u16)>) {
    let styles = DialogStyles::new(theme);
    frame.render_widget(
        Paragraph::new(label.to_owned()).style(if focused {
            styles.shortcut
        } else {
            styles.label
        }),
        Rect::new(row.x, row.y, label_width.min(row.width), 1),
    );
    let field_x = row
        .x
        .saturating_add(label_width)
        .saturating_add(FIELD_GUTTER);
    if field_x >= row.right() {
        return (Rect::new(row.right(), row.y, 0, 1), None);
    }
    let field = Rect::new(field_x, row.y, row.right().saturating_sub(field_x), 1);
    let mut caret = None;
    if focused {
        caret = place_input_cursor_at(
            frame,
            field,
            0,
            0,
            value,
            cursor.unwrap_or_else(|| value.chars().count()),
            theme,
        );
    } else {
        InputSurface {
            style: styles.input,
        }
        .render(field, frame.buffer_mut());
        frame.render_widget(
            Paragraph::new(input_tail(value, usize::from(field.width))).style(styles.input),
            field,
        );
    }
    if value.is_empty() {
        render_placeholder(
            frame,
            Rect::new(
                field.x.saturating_add(u16::from(focused)),
                field.y,
                field.width.saturating_sub(u16::from(focused)),
                1,
            ),
            placeholder,
            theme,
        );
    }
    (field, caret)
}

/// §3: draw the border and title and return the region rects. Every dialog
/// starts here, so rendering, hit-testing, scrolling and selection agree.
fn dialog_frame(
    frame: &mut Frame<'_>,
    app: &mut App,
    area: Rect,
    class: crate::dialog_layout::DialogClass,
    title: &str,
    content: &crate::dialog_layout::DialogContent,
    theme: Theme,
) -> crate::dialog_layout::DialogRegions {
    let regions = dialog_frame_regions(frame, area, class, title, content, theme);
    app.hit_regions.selection_modal = Some(regions.interior);
    regions
}

/// The same frame without the `App` write: a component records the interior in
/// its own `Surface` and the shell copies it into `selection_modal` (§3).
pub(crate) fn dialog_frame_regions(
    frame: &mut Frame<'_>,
    area: Rect,
    class: crate::dialog_layout::DialogClass,
    title: &str,
    content: &crate::dialog_layout::DialogContent,
    theme: Theme,
) -> crate::dialog_layout::DialogRegions {
    let popup = crate::dialog_layout::dialog_rect(area, class, content);
    clear_themed(frame, popup, theme);
    frame.render_widget(
        Block::default()
            .title(Span::styled(
                format!(" {title} "),
                Style::default()
                    .fg(theme.active_border)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.active_border)),
        popup,
    );
    crate::dialog_layout::regions(popup, content)
}

fn dialog_body(popup: Rect) -> Rect {
    dialog_body_with_footer(popup, 1)
}

fn dialog_body_with_footer(popup: Rect, footer_height: u16) -> Rect {
    let inner = popup.inner(ratatui::layout::Margin::new(1, 1));
    if inner.width == 0 || inner.height <= 1 {
        return Rect::new(inner.x, inner.y, inner.width, 0);
    }
    let horizontal_padding = u16::from(inner.width > 2);
    Rect::new(
        inner.x.saturating_add(horizontal_padding),
        inner.y,
        inner
            .width
            .saturating_sub(horizontal_padding.saturating_mul(2)),
        inner.height.saturating_sub(footer_height),
    )
}

pub(crate) struct InputSurface {
    pub(crate) style: Style,
}

impl Widget for InputSurface {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                // Blank the symbol as well as the style. A field is painted
                // once (dialog-system.md §8.1); leaving symbols behind is what
                // let a wider earlier paint survive in the final column and
                // render a duplicated trailing glyph.
                let cell = &mut buffer[(x, y)];
                cell.set_symbol(" ");
                cell.set_style(self.style);
            }
        }
    }
}

pub(crate) fn clear_themed(frame: &mut Frame<'_>, area: Rect, theme: Theme) {
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().fg(theme.base_fg).bg(theme.dialog_bg)),
        area,
    );
}

fn centered(area: Rect, percent_width: u16, height: u16) -> Rect {
    let width = area
        .width
        .saturating_mul(percent_width)
        .saturating_div(100)
        .max(1)
        .min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

pub fn clipped_width(text: &str, maximum: usize) -> String {
    let mut result = String::new();
    let mut width = 0;
    for character in text.chars() {
        let character_width = UnicodeWidthStr::width(character.encode_utf8(&mut [0; 4]));
        if width + character_width > maximum {
            break;
        }
        result.push(character);
        width += character_width;
    }
    result
}

/// The correlation mapping layer (§3 anatomy, class M). Every source names the
/// identity itself, so this is where the user says which field carries the
/// value in each one. Nothing is chosen for them.
fn render_correlation(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    use crate::dialog_layout::{DialogClass, DialogContent, content_width, pane};

    let styles = DialogStyles::new(theme);
    let ascii = app.appearance.ascii;
    let Some(dialog) = app.correlation_dialog.clone() else {
        return;
    };
    app.hit_regions.correlation_rows.clear();
    app.hit_regions.correlation_controls.clear();
    app.hit_regions.correlation_choices.clear();

    let width = content_width(area, DialogClass::M);
    let header = format!(
        "{} = {} · from the selected record",
        dialog.field, dialog.value_label
    );
    let mapped = dialog.mapped_sources();
    let sampled = dialog.sources.iter().any(|source| source.incomplete);
    let (state, sentence) = if let Some(error) = dialog.error.as_deref() {
        (MessageState::Error, error.to_owned())
    } else if dialog.submitting {
        (
            MessageState::Updating,
            "opening the correlated view".to_owned(),
        )
    } else if mapped == 0 {
        (
            MessageState::Disabled,
            "no source is mapped yet, so there is nothing to correlate".to_owned(),
        )
    } else if sampled {
        (
            MessageState::Scanned,
            format!(
                "{mapped} of {} sources mapped · field names come from a bounded sample, \
                 so a rarely used field may be missing",
                dialog.sources.len()
            ),
        )
    } else {
        (
            MessageState::Applied,
            format!("{mapped} of {} sources mapped", dialog.sources.len()),
        )
    };
    let help =
        "Sources name the same identity differently; unmapped sources contribute no records.";
    let labels = ["Correlate", "Cancel"];
    let content = DialogContent {
        header: 1,
        // Pane heading plus one row per source.
        body: u16::try_from(dialog.sources.len().saturating_add(1)).unwrap_or(u16::MAX),
        message: message_rows(&sentence, width),
        help: help_rows(help, width),
        actions: packed_button_rows(width, &labels),
    };
    let regions = dialog_frame(
        frame,
        app,
        area,
        DialogClass::M,
        "Correlate across sources",
        &content,
        theme,
    );
    if regions.header.height > 0 {
        frame.render_widget(
            Paragraph::new(truncated(&header, usize::from(regions.header.width)))
                .style(styles.label.add_modifier(Modifier::BOLD)),
            regions.header,
        );
    }
    let mut field_rects: Vec<Rect> = Vec::new();
    if regions.body.height > 0 && regions.body.width > 0 {
        let rects = pane(regions.body, 0, dialog.sources.len());
        if rects.heading.height > 0 {
            frame.render_widget(
                Paragraph::new("Source").style(styles.label.add_modifier(Modifier::BOLD)),
                rects.heading,
            );
            let count = format!("{} of {}", mapped, dialog.sources.len());
            let count_width = u16::try_from(count.chars().count()).unwrap_or(0);
            if rects.heading.width > count_width {
                frame.render_widget(
                    Paragraph::new(count).style(styles.description),
                    Rect::new(
                        rects.heading.right().saturating_sub(count_width),
                        rects.heading.y,
                        count_width,
                        1,
                    ),
                );
            }
        }
        let name_width = usize::from(rects.viewport.width).saturating_sub(24).max(8);
        for (index, source) in dialog.sources.iter().enumerate() {
            let Some(y) = u16::try_from(index)
                .ok()
                .map(|offset| rects.viewport.y.saturating_add(offset))
                .filter(|y| *y < rects.viewport.bottom())
            else {
                continue;
            };
            let row = Rect::new(rects.viewport.x, y, rects.viewport.width, 1);
            let selected = dialog.control == crate::app::CorrelationControl::Sources
                && index == dialog.selected;
            let gutter = if selected {
                if ascii { "> " } else { "› " }
            } else {
                "  "
            };
            let chosen = source
                .chosen
                .clone()
                .unwrap_or_else(|| crate::app::NOT_CORRELATED.to_owned());
            let text = format!(
                "{gutter}{:<name_width$}  {chosen} ▾",
                truncated(&source.name, name_width),
            );
            frame.render_widget(
                Paragraph::new(clipped_width(&text, usize::from(row.width))).style(if selected {
                    styles.selection
                } else if source.chosen.is_some() {
                    styles.applied
                } else {
                    styles.description
                }),
                row,
            );
            app.hit_regions.correlation_rows.push((row, index));
            let field_x = row
                .x
                .saturating_add(u16::try_from(name_width.saturating_add(4)).unwrap_or(0))
                .min(row.right().saturating_sub(1));
            field_rects.push(Rect::new(
                field_x,
                y,
                row.right().saturating_sub(field_x),
                1,
            ));
        }
    }
    render_message(frame, regions.message, state, &sentence, theme, ascii);
    render_help_text(frame, regions.help, help, theme);
    let focused = match dialog.control {
        crate::app::CorrelationControl::Correlate => Some(0),
        crate::app::CorrelationControl::Cancel => Some(1),
        crate::app::CorrelationControl::Sources => None,
    };
    app.hit_regions.correlation_controls =
        render_action_row(frame, regions.actions, &labels, focused, &[], theme)
            .into_iter()
            .map(|(index, rect)| {
                (
                    rect,
                    if index == 0 {
                        crate::app::CorrelationControl::Correlate
                    } else {
                        crate::app::CorrelationControl::Cancel
                    },
                )
            })
            .collect();

    // §8.3: the field choice is a dropdown, so the options open as an anchored
    // popup over the dialog rather than cycling invisibly in place.
    let Some(highlighted) = dialog.popup else {
        return;
    };
    let options = dialog.options(dialog.selected);
    let Some(anchor) = field_rects.get(dialog.selected).copied() else {
        return;
    };
    let hint = options
        .iter()
        .map(|option| u16::try_from(option.chars().count()).unwrap_or(0))
        .max()
        .unwrap_or(12)
        .saturating_add(4);
    let popup = crate::dialog_layout::anchored_rect(regions.interior, anchor, options.len(), hint);
    clear_themed(frame, popup, theme);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.active_border)),
        popup,
    );
    let inner = popup.inner(ratatui::layout::Margin::new(1, 1));
    let visible = usize::from(inner.height);
    let top = highlighted.saturating_sub(visible.saturating_sub(1));
    for (offset, index) in (top..options.len()).take(visible).enumerate() {
        let Some(y) = u16::try_from(offset)
            .ok()
            .map(|offset| inner.y.saturating_add(offset))
            .filter(|y| *y < inner.bottom())
        else {
            continue;
        };
        let rect = Rect::new(inner.x, y, inner.width, 1);
        frame.render_widget(
            Paragraph::new(truncated(&options[index], usize::from(rect.width))).style(
                if index == highlighted {
                    styles.selection
                } else if index == 0 {
                    styles.unavailable
                } else {
                    styles.description
                },
            ),
            rect,
        );
        app.hit_regions.correlation_choices.push((rect, index));
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::{clip_styled_columns, input_tail, styled_event_line, styled_event_lines};
    use crate::theme::{Theme, ThemeId};
    use ratatui::{Terminal, backend::TestBackend, style::Style, widgets::Paragraph};
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn value_colors_are_stable_and_null_remains_visible() {
        assert_eq!(
            Theme::TERMINAL.value_color("same-request"),
            Theme::TERMINAL.value_color("same-request")
        );
        assert_eq!(
            Theme::TERMINAL.value_color("null"),
            Theme::TERMINAL.value_color("null")
        );
    }

    #[test]
    fn input_tail_clips_wide_text_without_orphaning_combining_marks() {
        let visible = input_tail("prefix e\u{301}界", 3);
        assert_eq!(visible, "e\u{301}界");
        assert_eq!(UnicodeWidthStr::width(visible.as_str()), 3);
        assert_eq!(input_tail("e\u{301}", 0), "");
    }

    #[test]
    fn json_roles_and_decoded_keys_render_in_every_theme() {
        let json = r#"{"a":"東京e\u0301","\u0061":-2.5e3,"ok":true,"none":null}"#;
        for theme in ThemeId::ALL.map(Theme::builtin) {
            let backend = TestBackend::new(80, 1);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    frame.render_widget(
                        Paragraph::new(styled_event_line(
                            json,
                            None,
                            0,
                            80,
                            Style::default().fg(theme.severity.error),
                            false,
                            theme,
                        )),
                        frame.area(),
                    );
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(2, 0)].fg, theme.value_color("a"));
            assert_eq!(buffer[(21, 0)].fg, theme.value_color("a"));
            assert_eq!(buffer[(6, 0)].fg, theme.json.string);
            assert_eq!(buffer[(30, 0)].fg, theme.json.number);
            assert_eq!(buffer[(42, 0)].fg, theme.json.boolean);
            assert_eq!(buffer[(54, 0)].fg, theme.json.null);
            assert_eq!(buffer[(0, 0)].fg, theme.json.punctuation);

            let nested = styled_event_line(
                r#"{"outer":[{"inner":"value"}]}"#,
                None,
                0,
                80,
                Style::default(),
                false,
                theme,
            );
            assert_eq!(
                nested
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>(),
                r#"{"outer":[{"inner":"value"}]}"#
            );
        }
    }

    #[test]
    fn selection_wins_and_malformed_json_falls_back_as_one_row_style() {
        for theme in ThemeId::ALL.map(Theme::builtin) {
            let selected = styled_event_line(
                r#"{"key":true}"#,
                None,
                0,
                80,
                Style::default()
                    .fg(theme.selection_fg)
                    .bg(theme.selection_bg),
                true,
                theme,
            );
            assert!(selected.spans.iter().all(|span| {
                span.style.fg == Some(theme.selection_fg)
                    && span.style.bg == Some(theme.selection_bg)
            }));
            let fallback = styled_event_line(
                r#"{"key":true broken}"#,
                None,
                0,
                80,
                Style::default().fg(theme.severity.warn),
                false,
                theme,
            );
            assert_eq!(fallback.spans.len(), 1);
            assert_eq!(fallback.spans[0].style.fg, Some(theme.severity.warn));

            for limited in [
                format!("{}0{}", "[".repeat(65), "]".repeat(65)),
                " ".repeat(crate::json_spans::MAX_JSON_CHARS + 1),
            ] {
                let line = styled_event_line(
                    &limited,
                    None,
                    0,
                    limited.len(),
                    Style::default().fg(theme.severity.warn),
                    false,
                    theme,
                );
                assert_eq!(line.spans.len(), 1);
                assert_eq!(line.spans[0].style.fg, Some(theme.severity.warn));
            }
        }
    }

    #[test]
    fn styled_horizontal_clip_handles_wide_and_combining_text_after_tokenization() {
        let line = styled_event_line(
            r#"{"界é":"value"}"#,
            None,
            3,
            8,
            Style::default(),
            false,
            Theme::LOVE_DARK,
        );
        let visible = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(visible, " é\":\"val");
        assert_eq!(UnicodeWidthStr::width(visible.as_str()), 8);

        let stops_at_wide = styled_event_line(
            r#"{"k":"ab界","later":1}"#,
            None,
            0,
            9,
            Style::default(),
            false,
            Theme::LOVE_DARK,
        );
        assert_eq!(
            stops_at_wide
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            r#"{"k":"ab"#,
            "clipping must not jump past an unrenderable wide character to later JSON tokens"
        );

        let split_combining = clip_styled_columns(
            vec![
                ("e", Style::default().fg(Theme::LOVE_DARK.json.string)),
                ("\u{301}", Style::default().fg(Theme::LOVE_DARK.accent)),
                ("x", Style::default()),
            ],
            0,
            1,
        );
        assert_eq!(
            split_combining
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "e\u{301}",
            "a right-edge combining mark remains attached across a style-span boundary"
        );
    }

    #[test]
    fn multiline_json_uses_the_complete_displayed_record_as_its_boundary() {
        let theme = Theme::LOVE_DARK;
        let pretty = "{\n  \"key\": [\n    true\n  ]\n}";
        let pretty_lines = styled_event_lines(
            pretty,
            None,
            0,
            80,
            Style::default().fg(theme.severity.error),
            false,
            theme,
        );
        assert_eq!(
            pretty_lines[1].spans[1].style.fg,
            Some(theme.value_color("key"))
        );
        assert!(
            pretty_lines[2]
                .spans
                .iter()
                .any(|span| span.style.fg == Some(theme.json.boolean))
        );

        let malformed = "{\n  \"looks_valid\"\nBROKEN\n}";
        let malformed_lines = styled_event_lines(
            malformed,
            None,
            0,
            80,
            Style::default().fg(theme.severity.warn),
            false,
            theme,
        );
        assert!(
            malformed_lines
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| { span.style.fg == Some(theme.severity.warn) })
        );

        let mixed = "{\r\n  \"鍵\": \"東京\",\n  \"é\": true,\r\n  \"tail\": null\n}";
        let mixed_lines = styled_event_lines(
            mixed,
            None,
            0,
            80,
            Style::default().fg(theme.severity.error),
            false,
            theme,
        );
        let visible = mixed_lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(visible, mixed.lines().collect::<Vec<_>>());
        for (line_index, key) in [(1, "鍵"), (2, "é"), (3, "tail")] {
            assert!(
                mixed_lines[line_index]
                    .spans
                    .iter()
                    .any(|span| span.style.fg == Some(theme.value_color(key)))
            );
        }
        assert!(
            mixed_lines[1]
                .spans
                .iter()
                .any(|span| span.style.fg == Some(theme.json.string))
        );
        assert!(
            mixed_lines[2]
                .spans
                .iter()
                .any(|span| span.style.fg == Some(theme.json.boolean))
        );
        assert!(
            mixed_lines[3]
                .spans
                .iter()
                .any(|span| span.style.fg == Some(theme.json.null))
        );
    }
}
