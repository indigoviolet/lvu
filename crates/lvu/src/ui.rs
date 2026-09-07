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
    dialog_controls::{DialogStyles, action_line, button_text, button_width},
    dialog_layout::MIN_BODY_ROWS,
    json_spans::{JsonKind, JsonSpan, classify},
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
    if app.focus != Focus::Context {
        render_logs(frame, app, provider, geometry.log, theme);
    }
    if let Some(details) = geometry.details.filter(|_| app.focus != Focus::Context) {
        render_details(frame, app, provider, details, theme);
    }
    if modal {
        // §6.2: the workspace behind an open dialog goes muted and loses every
        // modifier, so the dialog is the only active surface. A style pass over
        // the finished workspace buffer; it moves nothing and owns no hit region.
        crate::dialog_layout::scrim(frame.buffer_mut(), geometry.area, theme);
    }
    if app.focus == Focus::AskAi {
        render_ask_ai(frame, app, geometry.area, theme);
    } else if app.focus == Focus::Investigation {
        render_investigation(frame, app, geometry.area, theme);
    } else if app.focus == Focus::Layer {
        // §6.4 render dispatch: the base, then the layer stack. Never both a
        // legacy dialog and a layer.
        render_layers(frame, app, provider, geometry.area, theme);
    }
    if app.focus == Focus::Bookmarks {
        render_bookmarks(frame, app, provider, geometry.area, theme);
    }
    if app.focus == Focus::Context {
        render_context(frame, app, provider, geometry.area, theme);
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
        };
        if is_top {
            top_surface = Some(surface);
        }
    }
    hit_regions.selection_modal = top_surface.map(|surface| surface.interior);
}

#[doc(hidden)]
/// §12.10 row columns: the record id, then its capture time, then the record
/// text. The note is the row's second line.
const BOOKMARK_ID_WIDTH: u16 = 5;
const BOOKMARK_TIME_WIDTH: u16 = 8;
const BOOKMARK_LABEL_WIDTH: u16 = 6;
/// The cap the store enforces, shown beside the count so reaching it is not a
/// surprise.
const BOOKMARK_CAPACITY: usize = 128;

pub fn render_bookmarks(
    frame: &mut Frame<'_>,
    app: &mut App,
    provider: &dyn crate::provider::RowProvider,
    area: Rect,
    theme: Theme,
) {
    use crate::app::BookmarkDialogControl as C;
    use crate::dialog_layout::{DialogClass, DialogContent, content_width, pane};
    let styles = DialogStyles::new(theme);
    let ascii = app.appearance.ascii;
    let cursor = app.active_text_cursor();
    app.hit_regions.bookmark_rows.clear();
    app.hit_regions.bookmark_controls.clear();
    let Some(dialog) = app.bookmark_dialog.clone() else {
        return;
    };
    let bookmarks = app.bookmarks_for_view(&dialog.view_id).to_vec();
    let view_name = app
        .views()
        .iter()
        .find(|view| view.id == dialog.view_id)
        .map_or_else(|| "this view".to_owned(), |view| view.name.clone());
    let width = content_width(area, DialogClass::M);

    // §12.10: editing a note is its own small dialog, named for the record it
    // belongs to, so the list behind it is not competing for attention.
    if let Some(editing) = dialog.editing.clone() {
        // The child is class S, so it measures at the S content width; using
        // the list's width would under-count the help and clip it.
        let width = content_width(area, DialogClass::S);
        let help = "Notes are capped at 1024 bytes and saved with the view. \
Escape leaves the note unchanged.";
        let (state, sentence) = if dialog.status.is_empty() {
            (MessageState::Ready, String::new())
        } else {
            (MessageState::Applied, dialog.status.clone())
        };
        let action_labels = ["Save note"];
        let content = DialogContent {
            header: 0,
            body: 1,
            message: message_rows(&sentence, width).max(1),
            help: help_rows(help, width),
            actions: packed_button_rows(width, &action_labels),
        };
        let title = format!("Bookmarks › Note for #{}", editing.sequence);
        let regions = dialog_frame(frame, app, area, DialogClass::S, &title, &content, theme);
        if regions.body.height > 0 {
            let (input, _) = render_form_field(
                frame,
                Rect::new(regions.body.x, regions.body.y, regions.body.width, 1),
                BOOKMARK_LABEL_WIDTH,
                "Note",
                &dialog.draft,
                "what this record shows",
                dialog.control == C::Input,
                cursor,
                theme,
            );
            app.hit_regions.bookmark_controls.push((input, C::Input));
        }
        render_message(frame, regions.message, state, &sentence, theme, ascii);
        render_help_text(frame, regions.help, help, theme);
        let controls = [C::Save];
        let focused = controls
            .iter()
            .position(|control| *control == dialog.control);
        for (index, rect) in
            render_action_row(frame, regions.actions, &action_labels, focused, &[], theme)
        {
            app.hit_regions
                .bookmark_controls
                .push((rect, controls[index]));
        }
        return;
    }

    let help = if bookmarks.is_empty() {
        "Press b on a record to bookmark it."
    } else {
        concat!(
            "Go to selects the record in its source's All events view, where it ",
            "is always present. Raw context shows its neighbours without leaving."
        )
    };
    let (state, sentence) = if dialog.status.contains("limit") {
        (MessageState::Error, dialog.status.clone())
    } else if dialog.status.is_empty() {
        (MessageState::Ready, String::new())
    } else {
        (MessageState::Applied, dialog.status.clone())
    };
    // §12.10 `[ Go to ]`, honest now that bookmarks are source-scoped: the
    // record is always present in its source's All events view.
    let mut actions: Vec<(&str, C)> = Vec::new();
    if !bookmarks.is_empty() {
        actions.extend([
            ("Go to", C::Goto),
            ("Edit note", C::Edit),
            ("Raw context", C::Context),
            ("Remove", C::Delete),
        ]);
    }
    let action_labels = actions.iter().map(|(label, _)| *label).collect::<Vec<_>>();

    // Each bookmark is two rows: the record, then its note.
    let row_pairs = bookmarks.len().clamp(1, 8);
    let content = DialogContent {
        header: 0,
        body: u16::try_from(row_pairs * 2 + 1).unwrap_or(u16::MAX),
        message: message_rows(&sentence, width).max(1),
        help: help_rows(help, width),
        actions: packed_button_rows(width, &action_labels),
    };
    let title = format!("Bookmarks · {view_name}");
    let regions = dialog_frame(frame, app, area, DialogClass::M, &title, &content, theme);
    let inner = regions.body;
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let count = format!("{} of {BOOKMARK_CAPACITY}", bookmarks.len());
    let rects = pane(
        inner,
        u16::try_from(UnicodeWidthStr::width(count.as_str())).unwrap_or(0),
        bookmarks.len() * 2,
    );
    if rects.heading.height > 0 {
        frame.render_widget(
            Paragraph::new("Bookmarks").style(styles.label.add_modifier(Modifier::BOLD)),
            rects.heading,
        );
        if rects.count.width > 0 {
            frame.render_widget(Paragraph::new(count).style(styles.description), rects.count);
        }
    }
    let visible_pairs = usize::from(rects.viewport.height / 2).max(1);
    let first = dialog
        .selected
        .saturating_add(1)
        .saturating_sub(visible_pairs);
    if bookmarks.is_empty() {
        if rects.viewport.height > 0 {
            frame.render_widget(
                Paragraph::new(truncated(
                    "No bookmarks in this view yet",
                    usize::from(rects.viewport.width),
                ))
                .style(styles.description),
                Rect::new(rects.viewport.x, rects.viewport.y, rects.viewport.width, 1),
            );
        }
    } else {
        for (offset, (index, bookmark)) in bookmarks
            .iter()
            .enumerate()
            .skip(first)
            .take(visible_pairs)
            .enumerate()
        {
            let y = rects.viewport.y.saturating_add((offset * 2) as u16);
            if y >= rects.viewport.bottom() {
                break;
            }
            let focused = index == dialog.selected;
            let row = Rect::new(rects.viewport.x, y, rects.viewport.width, 1);
            let record = provider.row_by_id(&dialog.view_id, &bookmark.id);
            let marker = if focused {
                if ascii { "> " } else { "› " }
            } else {
                "  "
            };
            let id = format!("{marker}#{}", bookmark.id.sequence);
            let id_width = (BOOKMARK_ID_WIDTH + 2).min(row.width);
            frame.render_widget(
                Paragraph::new(truncated(&id, usize::from(id_width))).style(if focused {
                    styles.selection
                } else {
                    styles.label
                }),
                Rect::new(row.x, y, id_width, 1),
            );
            let time_x = row.x.saturating_add(id_width).saturating_add(FIELD_GUTTER);
            let time = record.as_ref().map_or_else(String::new, |row| {
                // The seconds-resolution clock is what the log column shows.
                row.timestamp
                    .split('.')
                    .next()
                    .unwrap_or(&row.timestamp)
                    .rsplit('T')
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            });
            if time_x < row.right() {
                let time_width = BOOKMARK_TIME_WIDTH.min(row.right().saturating_sub(time_x));
                frame.render_widget(
                    Paragraph::new(truncated(&time, usize::from(time_width)))
                        .style(styles.description),
                    Rect::new(time_x, y, time_width, 1),
                );
                let text_x = time_x
                    .saturating_add(time_width)
                    .saturating_add(FIELD_GUTTER);
                if text_x < row.right() {
                    let text = record.as_ref().map_or_else(
                        || "record is no longer loaded".to_owned(),
                        |row| row.text.clone(),
                    );
                    frame.render_widget(
                        Paragraph::new(truncated(
                            &text,
                            usize::from(row.right().saturating_sub(text_x)),
                        ))
                        .style(styles.description),
                        Rect::new(text_x, y, row.right().saturating_sub(text_x), 1),
                    );
                }
            }
            // §12.10: the note is the second line, muted only when absent —
            // "no note" is not information, the note itself is.
            let note_y = y.saturating_add(1);
            if note_y < rects.viewport.bottom() {
                let note_x = row.x.saturating_add(id_width).saturating_add(FIELD_GUTTER);
                let empty = bookmark.note.is_empty();
                frame.render_widget(
                    Paragraph::new(truncated(
                        if empty { "no note" } else { &bookmark.note },
                        usize::from(row.right().saturating_sub(note_x)),
                    ))
                    .style(if empty {
                        styles.unavailable
                    } else {
                        styles.description
                    }),
                    Rect::new(note_x, note_y, row.right().saturating_sub(note_x), 1),
                );
            }
            // The whole two-line block is the row's hitbox, so clicking a note
            // selects the bookmark it belongs to.
            app.hit_regions.bookmark_rows.push((
                Rect::new(
                    row.x,
                    y,
                    row.width,
                    2.min(rects.viewport.bottom().saturating_sub(y)),
                ),
                index,
            ));
        }
    }
    if let Some(bar) = rects.scrollbar {
        render_scrollbar(
            frame,
            bar,
            first * 2,
            (bookmarks.len() * 2).saturating_sub(usize::from(rects.viewport.height)),
            theme,
            ascii,
        );
    }
    render_message(frame, regions.message, state, &sentence, theme, ascii);
    render_help_text(frame, regions.help, help, theme);
    let focused = actions
        .iter()
        .position(|(_, control)| *control == dialog.control);
    for (index, rect) in render_action_row(
        frame,
        regions.actions,
        &action_labels,
        focused,
        // Removing a bookmark is the destructive one.
        &[3],
        theme,
    ) {
        app.hit_regions
            .bookmark_controls
            .push((rect, actions[index].1));
    }
}

/// §12.12: the sequence gutter, wide enough for the record numbers a capture
/// reaches without stealing columns from the record itself.
const CONTEXT_SEQUENCE_WIDTH: u16 = 6;

fn render_context<P: RowProvider>(
    frame: &mut Frame<'_>,
    app: &mut App,
    provider: &P,
    area: Rect,
    theme: Theme,
) {
    use crate::dialog_layout::{DialogClass, DialogContent, content_width};
    let styles = DialogStyles::new(theme);
    let ascii = app.appearance.ascii;
    app.hit_regions.context_actions.clear();
    let Some(dialog) = app.context_dialog.clone() else {
        return;
    };
    let width = content_width(area, DialogClass::XL);
    let view_name = app
        .views()
        .iter()
        .find(|view| view.id == dialog.view_id)
        .map_or_else(|| "this view".to_owned(), |view| view.name.clone());

    // Measure against the class before the popup exists: the body takes every
    // row the frame can spare, so ask for more than it can have and let §5.4
    // hand back what is left.
    let action_labels = ["Back to anchor"];
    let probe = DialogContent {
        header: 1,
        body: u16::MAX,
        message: 1,
        help: 0,
        actions: packed_button_rows(width, &action_labels),
    };
    let probe_rect = crate::dialog_layout::dialog_rect(area, DialogClass::XL, &probe);
    let rows = usize::from(
        crate::dialog_layout::regions(probe_rect, &probe)
            .body
            .height,
    )
    .min(64);
    let page = provider.context_page(&dialog.view_id, &dialog.anchor, dialog.offset, rows);

    // §7.4: the state word says what this dialog is, and the sentence says the
    // thing a user needs to be told — that it is not what the log behind it is
    // showing.
    let (state, sentence) = match (&page.diagnostic, page.pending) {
        (Some(diagnostic), _) => (MessageState::Error, diagnostic.clone()),
        (None, true) => (
            MessageState::Updating,
            "reading physical source records".to_owned(),
        ),
        (None, false) => (
            MessageState::Ready,
            "the accepted filter still applies to the log behind this dialog".to_owned(),
        ),
    };
    // §12.12: one header line naming the anchor and the span it is showing.
    // A narrow frame drops the least load-bearing parts rather than truncating
    // the line, so `raw` — the fact that distinguishes this dialog from the log
    // behind it — survives to the smallest supported size.
    let anchor = format!("Anchor: #{}", dialog.anchor.sequence);
    let source = truncated(&dialog.anchor.source_id, 12);
    let span = format!(
        "records {}–{} of {}",
        page.start.saturating_add(1).min(page.total),
        page.start.saturating_add(page.rows.len()),
        page.total
    );
    let header = [
        format!("{anchor} · {source} · {span} · raw, unfiltered, ungrouped"),
        format!("{anchor} · {span} · raw, unfiltered, ungrouped"),
        format!("{anchor} · {span} · raw, unfiltered"),
        format!("{anchor} · {span} · raw"),
        format!("{anchor} · {span}"),
        anchor.clone(),
    ]
    .into_iter()
    .find(|line| UnicodeWidthStr::width(line.as_str()) <= usize::from(width))
    .unwrap_or(anchor);
    let content = DialogContent {
        header: 1,
        body: u16::try_from(page.rows.len().max(1)).unwrap_or(u16::MAX),
        message: message_rows(&sentence, width).max(1),
        help: 0,
        actions: packed_button_rows(width, &action_labels),
    };
    let title = format!("Raw context · {view_name}");
    let regions = dialog_frame(frame, app, area, DialogClass::XL, &title, &content, theme);
    if regions.header.height > 0 {
        frame.render_widget(
            Paragraph::new(truncated(&header, usize::from(regions.header.width)))
                .style(styles.description),
            regions.header,
        );
    }

    let body = regions.body;
    // §9: the list scrolls under a scrollbar rather than running to the border.
    let overflowing = page.total > page.rows.len();
    let viewport = Rect::new(
        body.x,
        body.y,
        body.width.saturating_sub(u16::from(overflowing)),
        body.height,
    );
    for (offset, row) in page.rows.iter().enumerate() {
        let y = viewport.y.saturating_add(offset as u16);
        if y >= viewport.bottom() {
            break;
        }
        let selected = row.id == dialog.anchor;
        let style = if selected {
            styles.selection
        } else {
            styles.description
        };
        let marker = if selected {
            if ascii { "> " } else { "› " }
        } else {
            "  "
        };
        let gutter = format!(
            "{marker}{:>width$}",
            row.id.sequence,
            width = usize::from(CONTEXT_SEQUENCE_WIDTH)
        );
        let gutter_width = (CONTEXT_SEQUENCE_WIDTH + 2).min(viewport.width);
        frame.render_widget(
            Paragraph::new(truncated(&gutter, usize::from(gutter_width))).style(style),
            Rect::new(viewport.x, y, gutter_width, 1),
        );
        let text_x = viewport
            .x
            .saturating_add(gutter_width)
            .saturating_add(FIELD_GUTTER);
        if text_x < viewport.right() {
            let text = row.text.replace(['\n', '\r', '\t'], " ");
            frame.render_widget(
                Paragraph::new(truncated(
                    &text,
                    usize::from(viewport.right().saturating_sub(text_x)),
                ))
                .style(style),
                Rect::new(text_x, y, viewport.right().saturating_sub(text_x), 1),
            );
        }
    }
    if overflowing && body.width > 0 {
        render_scrollbar(
            frame,
            Rect::new(body.right().saturating_sub(1), body.y, 1, body.height),
            page.start,
            page.total.saturating_sub(page.rows.len()),
            theme,
            ascii,
        );
    }

    if let Some(anchor_position) = page.anchor_position
        && let Some(dialog) = &mut app.context_dialog
    {
        dialog.offset = (page.start as isize).saturating_sub(anchor_position as isize);
    }
    render_message(frame, regions.message, state, &sentence, theme, ascii);
    // `g` remains the accelerator; §11 keeps it out of the body.
    for (_, rect) in render_action_row(frame, regions.actions, &action_labels, None, &[], theme) {
        app.hit_regions.context_actions.push(rect);
    }
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
        // whole stream is folded.
        let folding = match state.fold_summary.filter(|_| state.fold_enabled) {
            Some(summary) if summary.evicted_entries > 0 => format!(
                " | fold:{} runs, {} hidden, older runs uncounted",
                summary.folded_entries, summary.hidden_rows
            ),
            Some(summary) if summary.folded_entries > 0 => format!(
                " | fold:{} runs, {} hidden",
                summary.folded_entries, summary.hidden_rows
            ),
            Some(_) => " | fold:on".to_owned(),
            None if state.fold_enabled => " | fold:on".to_owned(),
            None => String::new(),
        };
        // Where the last gap jump landed. It sits before the constraint
        // indicators because it answers "what just happened", which is what a
        // user reads the status line for straight after pressing a key.
        let gap = state
            .gap_notice
            .as_ref()
            .map_or_else(String::new, |notice| format!(" | {notice}"));
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
            " {follow}{capture_time}{runtime} | {}-{}/{}{}{}{}{enrichment}{grouping}{folding}{gap} | ? help ",
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
        " NO VIEW | add or discover a source to begin | ? help ".into()
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
    let visible = app.visible_rows(provider);
    app.hit_regions.log_row_indices.clear();
    let mut screen_y = area.y.saturating_add(2);
    let rows = visible
        .into_iter()
        .enumerate()
        .map(|(offset, row)| {
            let style = if selected.as_ref() == Some(&row.id) {
                Style::default()
                    .fg(theme.selection_fg)
                    .bg(theme.selection_bg)
            } else if let Some(value) = color_field
                .as_ref()
                .and_then(|field| field_value(&row, field))
            {
                Style::default().fg(theme.value_color(value))
            } else if let Some(color) = theme.severity_color(&row.level) {
                Style::default().fg(color)
            } else {
                Style::default()
            };
            let selected_row = selected.as_ref() == Some(&row.id);
            let mut cells: Vec<Cell<'static>> =
                vec![row.timestamp.clone().into(), row.level.clone().into()];
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
            let event = if is_expanded {
                group_lines.join("\n")
            } else {
                row.text
            };
            let bookmark = app
                .bookmarks_for_view(app.active_view_id().unwrap_or(""))
                .iter()
                .any(|bookmark| bookmark.id == row.id)
                .then_some(if app.appearance.ascii { "* " } else { "★ " });
            let lines = styled_event_lines(
                &event,
                bookmark,
                horizontal,
                area.width as usize,
                style,
                selected_row,
                theme,
            );
            cells.push(Cell::from(Text::from(lines)));
            let height = if is_expanded {
                u16::try_from(group_lines.len()).unwrap_or(u16::MAX)
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
    let mut widths = vec![Constraint::Length(13), Constraint::Length(6)];
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
            let foreground = match &token.kind {
                JsonKind::Key(identity) => theme.value_color(identity),
                JsonKind::String => theme.json.string,
                JsonKind::Number => theme.json.number,
                JsonKind::Boolean => theme.json.boolean,
                JsonKind::Null => theme.json.null,
                JsonKind::Punctuation => theme.json.punctuation,
            };
            pieces.push((&text[token.bytes.clone()], row_style.fg(foreground)));
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
    let mut lines = Vec::new();
    if let Some(row) = row {
        lines.push(Line::from(vec![
            Span::styled("stable display id: ", styles.label),
            Span::styled(row.id.to_string(), styles.description),
        ]));
        lines.push(Line::from(vec![
            Span::styled("raw: ", styles.label),
            Span::styled(row.text, styles.description),
        ]));
        for (key, value) in row.fields.into_iter().chain(row.details) {
            let status = key == "command.status";
            let value_style = if status
                && value
                    .split_whitespace()
                    .next()
                    .is_some_and(|word| word.eq_ignore_ascii_case("pending"))
            {
                styles.pending
            } else if status {
                styles.applied
            } else {
                styles.description
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{key}: "), styles.label),
                Span::styled(value, value_style),
            ]));
        }
    } else {
        lines.push(Line::styled("No selected event", styles.unavailable));
    }
    let block = Block::default()
        .title(" Selected event details ")
        .borders(Borders::ALL)
        .style(Style::default().bg(theme.dialog_bg))
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
    let footer_height = u16::from(inner.height > 1);
    let content = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(footer_height),
    );
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let limit = paragraph
        .line_count(content.width)
        .saturating_sub(usize::from(content.height));
    let scroll = app.set_details_viewport(row_id, limit);
    frame.render_widget(
        paragraph.scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        content,
    );
    if footer_height > 0 {
        let footer = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
        frame.render_widget(
            Paragraph::new(crate::dialog_controls::action_line(
                &[("↑/↓", "scroll")],
                theme,
            )),
            footer,
        );
    }
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

fn class_l_content(
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
#[derive(Clone, Copy, Eq, PartialEq)]
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
        x = x.saturating_add(label_width).saturating_add(1);
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

/// §6.3 button roles: primary accent, focused selection, others base.
pub(crate) fn render_enrichment_button(
    frame: &mut Frame<'_>,
    rect: Rect,
    label: &str,
    focused: bool,
    primary: bool,
    theme: Theme,
) {
    if rect.is_empty() {
        return;
    }
    let styles = DialogStyles::new(theme);
    let style = if focused {
        styles.selection.add_modifier(Modifier::BOLD)
    } else if primary {
        styles.shortcut
    } else {
        styles.label
    };
    frame.render_widget(
        Paragraph::new(crate::dialog_controls::button_text(label)).style(style),
        rect,
    );
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
    let inner = popup.inner(ratatui::layout::Margin::new(1, 1));
    let footer = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
    frame.render_widget(
        Paragraph::new(action_line(&[("↑/↓", "select")], theme)),
        footer,
    );
    (popup, rows)
}

/// §12.17 Ask 🧠 — class L on the shared anatomy: title, an optional header
/// summary for a prepared task, the Kind/Request form, the Proposal and
/// Activity panes, one message row, help, and the actions last.
fn render_ask_ai(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    use crate::app::{AskAiStage as S, AskControl as C};
    use crate::dialog_layout::{DialogClass, DialogContent, PANE_INDENT, content_width};
    let styles = DialogStyles::new(theme);
    let ascii = app.appearance.ascii;
    let cursor = app.active_text_cursor();
    app.hit_regions.ask_controls.clear();
    app.hit_regions.ask_kind_choices.clear();
    let Some(mut dialog) = app.ask_ai_dialog.clone() else {
        return;
    };

    let editable = matches!(dialog.stage, S::Input | S::Error);
    // A prepared task has already decided the kind, so the dialog explains the
    // task in its header instead of offering an irrelevant choice.
    let task = dialog.task;
    let show_kind = task.is_none();
    let choosable_kind = show_kind && dialog.recipe.is_none() && dialog.stage == S::Input;

    let title = match (app.appearance.ascii, task) {
        (true, None) => "Ask Agent".to_owned(),
        (false, None) => "Ask 🧠".to_owned(),
        (true, Some(task)) => format!("Ask Agent · {}", task.object()),
        (false, Some(task)) => format!("Ask 🧠 · {}", task.object()),
    };
    let help = task.map(crate::app::AskTask::help).unwrap_or("");
    let (state_word, sentence) = ask_message(&dialog);

    let width = content_width(area, DialogClass::L);
    let labels: &[&str] = if show_kind {
        &["Kind", "Request"]
    } else {
        &["Request"]
    };
    let label_width = labels
        .iter()
        .map(|label| u16::try_from(UnicodeWidthStr::width(*label)).unwrap_or(0))
        .max()
        .unwrap_or(0)
        .min(18);
    // §4.2: below this the label no longer fits beside a usable field, so it
    // stacks above it.
    let stacked = width < label_width.saturating_add(FIELD_GUTTER).saturating_add(20);

    let field_width = if stacked {
        width
    } else {
        width
            .saturating_sub(label_width)
            .saturating_sub(FIELD_GUTTER)
            .max(1)
    };
    // §8.1: the Request field takes the rows its draft needs, up to three, and
    // scrolls internally beyond that. Sizing it to the content is what retires
    // the input-background slab painted over rows the prompt never reaches.
    // The internal scrollbar costs a column, which changes the wrap, so the
    // wrap is measured again once the overflow is known.
    let mut text_width = field_width.max(1);
    let mut wrapped = crate::text_edit::wrapped_text(&dialog.prompt, usize::from(text_width));
    let visible_rows = |wrapped: &crate::text_edit::WrappedText| {
        u16::try_from(wrapped.lines.len())
            .unwrap_or(ASK_REQUEST_ROWS)
            .clamp(1, ASK_REQUEST_ROWS)
    };
    let mut request_rows = visible_rows(&wrapped);
    if wrapped.lines.len() > usize::from(request_rows) && field_width > 1 {
        text_width = field_width.saturating_sub(1);
        wrapped = crate::text_edit::wrapped_text(&dialog.prompt, usize::from(text_width));
        request_rows = visible_rows(&wrapped);
    }
    let request_overflows = wrapped.lines.len() > usize::from(request_rows);

    // §7.4 caps the message at two rows, but a bridge diagnostic names what
    // failed *and* what to do about it. When it does not fit, the full text
    // becomes body content so the body's own scroll reaches it; truncating the
    // remedy away is not a diagnostic.
    let message_height = message_rows(&sentence, width);
    let pane_width = usize::from(width.saturating_sub(PANE_INDENT)).max(1);
    let diagnostic: Vec<PaneLine> = if wrap_sentence(
        &sentence,
        usize::from(width.saturating_sub(MESSAGE_SENTENCE_COLUMN)).max(1),
        usize::MAX,
    )
    .len()
        > usize::from(message_height)
    {
        wrap_sentence(&sentence, pane_width, usize::MAX)
            .into_iter()
            .map(|text| PaneLine {
                text,
                error: state_word == MessageState::Error,
            })
            .collect()
    } else {
        Vec::new()
    };
    let proposal = ask_proposal_lines(&dialog, pane_width);
    let activity = ask_activity_lines(&dialog, pane_width);
    let mut panes: Vec<(&str, Option<String>, &Vec<PaneLine>)> = Vec::new();
    if !diagnostic.is_empty() {
        panes.push(("Details", None, &diagnostic));
    }
    panes.push((
        "Proposal",
        dialog.expression.is_none().then(|| "none yet".to_owned()),
        &proposal,
    ));
    panes.push(("Activity", None, &activity));
    let pane_lines: Vec<usize> = panes.iter().map(|(_, _, lines)| lines.len()).collect();
    let kind_width = ask_kind_width(&dialog);
    let measured = ask_body_layout(
        Rect::new(0, 0, width, 1),
        show_kind,
        kind_width,
        stacked,
        label_width,
        request_rows,
        &pane_lines,
    );

    let action_labels = ask_action_labels(&dialog);
    let borrowed: Vec<&str> = action_labels.iter().map(String::as_str).collect();
    let content = DialogContent {
        header: u16::from(task.is_some()),
        body: measured.height,
        message: message_height,
        help: help_rows(help, width),
        actions: packed_button_rows(width, &borrowed),
    };
    let regions = dialog_frame(frame, app, area, DialogClass::L, &title, &content, theme);
    let inner = regions.content;

    if let Some(task) = task
        && regions.header.height > 0
    {
        frame.render_widget(
            Paragraph::new(truncated(task.summary(), usize::from(regions.header.width)))
                .style(styles.description),
            regions.header,
        );
    }

    let layout = ask_body_layout(
        Rect::new(inner.x, inner.y, inner.width, 1),
        show_kind,
        kind_width,
        stacked,
        label_width,
        request_rows,
        &pane_lines,
    );
    // §9: the body is the only scrolling region, and its scrollbar replaces the
    // retired `[ More ]` pseudo-button.
    let overflowing = layout.height > regions.body.height;
    let viewport = Rect::new(
        regions.body.x,
        regions.body.y,
        regions.body.width.saturating_sub(u16::from(overflowing)),
        regions.body.height,
    );
    let max_scroll = layout.height.saturating_sub(viewport.height);
    dialog.review_scroll_limit = max_scroll;
    dialog.review_scroll = dialog.review_scroll.min(max_scroll);
    if dialog.focus == C::More && max_scroll == 0 {
        dialog.focus = match dialog.stage {
            S::Proposal => C::Apply,
            S::Input | S::Error => C::Submit,
            _ => C::Cancel,
        };
    }
    let scroll = dialog.review_scroll;
    let project = |rect: Rect| -> Option<Rect> {
        let top = rect.y.max(scroll);
        let bottom = rect.bottom().min(scroll.saturating_add(viewport.height));
        (bottom > top && rect.width > 0).then(|| {
            Rect::new(
                viewport.x.saturating_add(rect.x),
                viewport.y.saturating_add(top - scroll),
                rect.width.min(viewport.width.saturating_sub(rect.x)),
                bottom - top,
            )
        })
    };

    if let Some((label, field)) = layout.kind {
        let focused = dialog.focus == C::Kind;
        if let Some(rect) = project(label) {
            frame.render_widget(
                Paragraph::new("Kind").style(if focused {
                    styles.shortcut
                } else {
                    styles.label
                }),
                rect,
            );
        }
        if let Some(rect) = project(field) {
            // §8.3: a dropdown is a field with a chevron in its last cell.
            let style = if focused {
                styles.selection
            } else {
                styles.input
            };
            InputSurface { style }.render(rect, frame.buffer_mut());
            frame.render_widget(
                Paragraph::new(truncated(
                    ask_kind_label(dialog.kind),
                    usize::from(rect.width.saturating_sub(2)),
                ))
                .style(style),
                rect,
            );
            if choosable_kind && rect.width > 0 {
                frame.render_widget(
                    Paragraph::new(if ascii { "v" } else { "▾" }).style(
                        Style::default().fg(theme.accent).bg(if focused {
                            theme.selection_bg
                        } else {
                            theme.input_bg
                        }),
                    ),
                    Rect::new(rect.right().saturating_sub(1), rect.y, 1, 1),
                );
                app.hit_regions.ask_controls.push((rect, C::Kind));
            }
        }
    }

    let request_focused = dialog.focus == C::Prompt;
    if let Some(rect) = project(layout.request_label) {
        frame.render_widget(
            Paragraph::new("Request").style(if request_focused {
                styles.shortcut
            } else {
                styles.label
            }),
            rect,
        );
    }
    let editing = request_focused && editable;
    let (caret_row, caret_column) = cursor.map_or((0, 0), |cursor| {
        let mut logical = crate::text_edit::TextCursor { char_index: cursor };
        crate::text_edit::wrapped_cursor(&dialog.prompt, &mut logical, usize::from(text_width))
    });
    let visible = usize::from(layout.request_field.height);
    let mut prompt_scroll =
        usize::from(dialog.prompt_scroll).min(wrapped.lines.len().saturating_sub(visible.max(1)));
    if editing {
        if caret_row < prompt_scroll {
            prompt_scroll = caret_row;
        }
        if caret_row >= prompt_scroll.saturating_add(visible.max(1)) {
            prompt_scroll = caret_row + 1 - visible.max(1);
        }
    }
    dialog.prompt_scroll = u16::try_from(prompt_scroll).unwrap_or(u16::MAX);
    dialog.prompt_width = text_width;
    for row in 0..layout.request_field.height {
        let logical = Rect::new(
            layout.request_field.x,
            layout.request_field.y.saturating_add(row),
            layout.request_field.width,
            1,
        );
        let Some(rect) = project(logical) else {
            continue;
        };
        let text_rect = Rect::new(rect.x, rect.y, text_width.min(rect.width), 1);
        InputSurface {
            style: styles.input,
        }
        .render(text_rect, frame.buffer_mut());
        let line = wrapped
            .lines
            .get(prompt_scroll.saturating_add(usize::from(row)))
            .cloned()
            .unwrap_or_default();
        frame.render_widget(
            Paragraph::new(truncated(&line, usize::from(text_rect.width))).style(styles.input),
            text_rect,
        );
        if editable {
            app.hit_regions.ask_controls.push((rect, C::Prompt));
        }
    }
    if dialog.prompt.is_empty()
        && let Some(rect) = project(Rect::new(
            layout.request_field.x,
            layout.request_field.y,
            text_width,
            1,
        ))
    {
        render_placeholder(frame, rect, ask_placeholder(&dialog), theme);
    }
    if request_overflows
        && let Some(rect) = project(Rect::new(
            layout.request_field.x.saturating_add(text_width),
            layout.request_field.y,
            1,
            layout.request_field.height,
        ))
    {
        render_scrollbar(
            frame,
            rect,
            prompt_scroll,
            wrapped.lines.len().saturating_sub(visible.max(1)),
            theme,
            ascii,
        );
    }
    if editing
        && let Some(rect) = project(Rect::new(
            layout.request_field.x,
            layout.request_field.y.saturating_add(
                u16::try_from(caret_row.saturating_sub(prompt_scroll)).unwrap_or(0),
            ),
            text_width,
            1,
        ))
        && rect.width > 0
    {
        let x = rect
            .x
            .saturating_add(u16::try_from(caret_column).unwrap_or(0))
            .min(rect.right().saturating_sub(1));
        frame.buffer_mut()[(x, rect.y)]
            .set_style(Style::default().bg(theme.cursor).fg(theme.input_fg));
        frame.set_cursor_position((x, rect.y));
    }

    let pane_focused = dialog.focus == C::More;
    for ((heading, count, lines), rows) in panes.into_iter().zip(layout.panes.iter().copied()) {
        if let Some(rect) = project(Rect::new(rows.x, rows.y, rows.width, 1)) {
            let style = if pane_focused {
                styles.shortcut.add_modifier(Modifier::BOLD)
            } else {
                styles.label.add_modifier(Modifier::BOLD)
            };
            let mut spans = vec![Span::styled(heading.to_owned(), style)];
            if let Some(count) = count {
                let used = UnicodeWidthStr::width(heading)
                    .saturating_add(UnicodeWidthStr::width(count.as_str()));
                spans.push(Span::raw(
                    " ".repeat(usize::from(rect.width).saturating_sub(used)),
                ));
                spans.push(Span::styled(count, styles.description));
            }
            frame.render_widget(Paragraph::new(Line::from(spans)), rect);
        }
        for (index, line) in lines.iter().enumerate() {
            let logical = Rect::new(
                rows.x.saturating_add(PANE_INDENT),
                rows.y
                    .saturating_add(1)
                    .saturating_add(u16::try_from(index).unwrap_or(u16::MAX)),
                rows.width.saturating_sub(PANE_INDENT),
                1,
            );
            if let Some(rect) = project(logical) {
                frame.render_widget(
                    Paragraph::new(line.text.clone()).style(if line.error {
                        styles.error
                    } else {
                        styles.description
                    }),
                    rect,
                );
            }
        }
    }

    if overflowing {
        render_scrollbar(
            frame,
            Rect::new(
                regions.body.right().saturating_sub(1),
                regions.body.y,
                1,
                regions.body.height,
            ),
            usize::from(scroll),
            usize::from(max_scroll),
            theme,
            ascii,
        );
        app.hit_regions.dialog_scroll = Some(regions.body);
    } else {
        app.hit_regions.dialog_scroll = None;
    }

    render_message(frame, regions.message, state_word, &sentence, theme, ascii);
    render_help_text(frame, regions.help, help, theme);
    // §3: the actions come last, after every field they act on.
    let action_controls = ask_action_controls(&dialog);
    let focused_action = action_controls
        .iter()
        .position(|control| *control == dialog.focus);
    for (index, rect) in render_action_row(
        frame,
        regions.actions,
        &borrowed,
        focused_action,
        &[],
        theme,
    ) {
        app.hit_regions
            .ask_controls
            .push((rect, action_controls[index]));
    }

    if dialog.kind_dropdown
        && let Some((_, field)) = layout.kind
        && let Some(anchor) = project(field)
    {
        render_ask_kind_dropdown(frame, app, area, anchor, dialog.kind_selected, theme);
    }
    if let Some(state) = &mut app.ask_ai_dialog {
        state.review_scroll_limit = dialog.review_scroll_limit;
        state.review_scroll = dialog.review_scroll;
        state.focus = dialog.focus;
        state.prompt_scroll = dialog.prompt_scroll;
        state.prompt_width = dialog.prompt_width;
    }
}

/// §8.1 visible-row cap for the Request field.
const ASK_REQUEST_ROWS: u16 = 3;

struct AskBodyLayout {
    kind: Option<(Rect, Rect)>,
    request_label: Rect,
    request_field: Rect,
    /// One rect per pane, heading row included, in the order supplied.
    panes: Vec<Rect>,
    height: u16,
}

/// §4.2/§4.3 body rows for Ask, in coordinates relative to `content`. Measuring
/// and drawing call this with the same arguments, so the scroll, the hitboxes
/// and the glyphs cannot disagree.
#[allow(clippy::too_many_arguments)]
fn ask_body_layout(
    content: Rect,
    show_kind: bool,
    kind_width: u16,
    stacked: bool,
    label_width: u16,
    request_rows: u16,
    pane_lines: &[usize],
) -> AskBodyLayout {
    let field_x = if stacked {
        0
    } else {
        label_width.saturating_add(FIELD_GUTTER)
    };
    let field_width = content.width.saturating_sub(field_x).max(1);
    let mut y = 0u16;
    let kind = show_kind.then(|| {
        let label = Rect::new(0, y, label_width.min(content.width), 1);
        let field = if stacked {
            Rect::new(0, y.saturating_add(1), kind_width.min(field_width), 1)
        } else {
            Rect::new(field_x, y, kind_width.min(field_width), 1)
        };
        y = y.saturating_add(1 + u16::from(stacked));
        (label, field)
    });
    let request_label = Rect::new(0, y, label_width.min(content.width), 1);
    let request_field = if stacked {
        Rect::new(0, y.saturating_add(1), field_width, request_rows)
    } else {
        Rect::new(field_x, y, field_width, request_rows)
    };
    y = y
        .saturating_add(u16::from(stacked))
        .saturating_add(request_rows);
    let panes = pane_lines
        .iter()
        .map(|lines| {
            // §4.3: one gap row before each pane, then its heading and rows.
            y = y.saturating_add(1);
            let rect = Rect::new(
                0,
                y,
                content.width,
                1u16.saturating_add(u16::try_from(*lines).unwrap_or(u16::MAX)),
            );
            y = y.saturating_add(rect.height);
            rect
        })
        .collect();
    AskBodyLayout {
        kind,
        request_label,
        request_field,
        panes,
        height: y,
    }
}

/// §4.2: a dropdown is `max(longest option) + 4` cells wide, minimum 12.
fn ask_kind_width(dialog: &crate::app::AskAiDialogState) -> u16 {
    let longest = if dialog.recipe.is_some() {
        UnicodeWidthStr::width(ask_kind_label(crate::app::AskAiKind::Recipe))
    } else {
        [
            crate::app::AskAiKind::Filter,
            crate::app::AskAiKind::Enrichment,
        ]
        .into_iter()
        .map(|kind| UnicodeWidthStr::width(ask_kind_label(kind)))
        .max()
        .unwrap_or(0)
    };
    u16::try_from(longest)
        .unwrap_or(8)
        .saturating_add(4)
        .max(12)
}

struct PaneLine {
    text: String,
    error: bool,
}

impl PaneLine {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: false,
        }
    }
}

fn ask_placeholder(dialog: &crate::app::AskAiDialogState) -> &'static str {
    match dialog.kind {
        crate::app::AskAiKind::Filter => "Show only errors from the worker service",
        crate::app::AskAiKind::Enrichment => "Derive a duration_ms field from the latency text",
        crate::app::AskAiKind::Recipe => "Adapt the suggested recipe to this source",
    }
}

fn ask_action_controls(dialog: &crate::app::AskAiDialogState) -> Vec<crate::app::AskControl> {
    use crate::app::{AskAiStage as S, AskControl as C};
    match dialog.stage {
        S::Input | S::Error => vec![C::Submit],
        S::Proposal => vec![C::Apply],
        S::Snapshot | S::StartingSession | S::Proposing => vec![C::Cancel],
    }
}

fn ask_action_labels(dialog: &crate::app::AskAiDialogState) -> Vec<String> {
    use crate::app::{AskAiStage as S, AskControl as C};
    ask_action_controls(dialog)
        .into_iter()
        .map(|control| match control {
            C::Submit if dialog.stage == S::Error => "Submit again".to_owned(),
            C::Submit => "Submit".to_owned(),
            C::Apply => "Apply".to_owned(),
            C::Cancel => "Cancel request".to_owned(),
            C::Kind | C::Prompt | C::More => String::new(),
        })
        .collect()
}

/// §7.4: one message row, one state word from the closed vocabulary.
fn ask_message(dialog: &crate::app::AskAiDialogState) -> (MessageState, String) {
    use crate::app::AskAiStage as S;
    let state = match dialog.stage {
        S::Input => MessageState::Ready,
        S::Error => MessageState::Error,
        S::Proposal => MessageState::Ready,
        S::Snapshot | S::StartingSession | S::Proposing => MessageState::Pending,
    };
    (state, dialog.progress.clone())
}

fn ask_proposal_lines(dialog: &crate::app::AskAiDialogState, width: usize) -> Vec<PaneLine> {
    let width = width.max(1);
    let mut lines = Vec::new();
    if let Some(expression) = &dialog.expression {
        lines.extend(
            wrap_sentence(expression, width, 6)
                .into_iter()
                .map(PaneLine::plain),
        );
    }
    if dialog.kind == crate::app::AskAiKind::Recipe
        && dialog.stage == crate::app::AskAiStage::Proposal
        && let Some(recipe) = &dialog.recipe
    {
        for (index, stage) in recipe.enrichments.iter().enumerate() {
            lines.push(PaneLine::plain(truncated(
                &format!("{}. [{}] {}", index + 1, stage.id.0, stage.source),
                width,
            )));
        }
        if recipe.enrichments.is_empty() && !recipe.enrichment.is_empty() {
            lines.push(PaneLine::plain(truncated(&recipe.enrichment, width)));
        }
        lines.extend(
            wrap_sentence(
                "Advanced filter and ordered enrichments may change; search, pins, colors, time and grouping are retained",
                width,
                3,
            )
            .into_iter()
            .map(PaneLine::plain),
        );
    }
    if let Some(explanation) = &dialog.explanation {
        lines.extend(
            wrap_sentence(explanation, width, 6)
                .into_iter()
                .map(PaneLine::plain),
        );
    }
    if lines.is_empty() {
        lines.extend(
            wrap_sentence(
                "A proposal appears here for review; nothing is applied until you accept it",
                width,
                2,
            )
            .into_iter()
            .map(PaneLine::plain),
        );
    }
    lines
}

fn ask_activity_lines(dialog: &crate::app::AskAiDialogState, width: usize) -> Vec<PaneLine> {
    let width = width.max(1);
    let mut lines = vec![PaneLine::plain(truncated(
        &format!(
            "{} · {} · thinking {}",
            dialog.provider, dialog.mode, dialog.thinking
        ),
        width,
    ))];
    if !matches!(
        dialog.stage,
        crate::app::AskAiStage::Input | crate::app::AskAiStage::Error
    ) {
        lines.push(PaneLine::plain(truncated(
            &format!("request: {}", dialog.prompt.replace('\n', " ")),
            width,
        )));
    }
    if let Some(session) = &dialog.session_id {
        lines.push(PaneLine::plain(truncated(
            &format!("session {session}"),
            width,
        )));
    }
    if let Some(directory) = &dialog.snapshot_dir {
        lines.push(PaneLine::plain(truncated(
            &format!("snapshot {directory}"),
            width,
        )));
    }
    lines
}

fn ask_kind_label(kind: crate::app::AskAiKind) -> &'static str {
    match kind {
        crate::app::AskAiKind::Filter => "Filter",
        crate::app::AskAiKind::Enrichment => "Enrichment",
        crate::app::AskAiKind::Recipe => "Recipe adaptation",
    }
}

/// §5.1 class A: the kind list is anchored to the field that opened it, with no
/// scrim and no breadcrumb, drawn last so it sits above the dialog.
fn render_ask_kind_dropdown(
    frame: &mut Frame<'_>,
    app: &mut App,
    area: Rect,
    anchor: Rect,
    selected: usize,
    theme: Theme,
) {
    let styles = DialogStyles::new(theme);
    let kinds = [
        crate::app::AskAiKind::Filter,
        crate::app::AskAiKind::Enrichment,
    ];
    let rect = crate::dialog_layout::anchored_rect(area, anchor, kinds.len(), anchor.width);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.active_border))
            .style(Style::default().fg(theme.base_fg).bg(theme.dialog_bg)),
        rect,
    );
    for (index, kind) in kinds
        .into_iter()
        .take(usize::from(rect.height.saturating_sub(2)))
        .enumerate()
    {
        let row = Rect::new(
            rect.x.saturating_add(1),
            rect.y.saturating_add(1 + index as u16),
            rect.width.saturating_sub(2),
            1,
        );
        app.hit_regions.ask_kind_choices.push((row, index));
        frame.render_widget(
            Paragraph::new(ask_kind_label(kind)).style(if index == selected {
                styles.selection
            } else {
                styles.label
            }),
            row,
        );
    }
}

/// §12.18: the question is a multiline field, deep enough to see a follow-up
/// without scrolling it.
const INVESTIGATION_QUESTION_ROWS: u16 = 3;
const INVESTIGATION_LABEL_WIDTH: u16 = 11;

/// The pane's heading, its count and its lines. Shared by sizing and drawing so
/// the rows the dialog asks for are the rows it then lays out.
fn investigation_pane(
    dialog: &crate::app::InvestigationDialogState,
    saved_mode: bool,
    help: &str,
    ascii: bool,
) -> (&'static str, String, Vec<String>) {
    if saved_mode {
        return (
            "Saved investigations",
            format!("{} saved", dialog.items.len()),
            dialog
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let marker = if index == dialog.selected {
                        if ascii { "> " } else { "› " }
                    } else {
                        "  "
                    };
                    format!("{marker}{} — {}", item.session_id, item.question)
                })
                .collect(),
        );
    }
    (
        "Transcript",
        format!(
            "{} message{}",
            dialog.messages.len(),
            if dialog.messages.len() == 1 { "" } else { "s" }
        ),
        if dialog.messages.is_empty() {
            vec![help.to_owned()]
        } else {
            dialog.messages.iter().cloned().collect()
        },
    )
}

/// A transcript line is prose and wraps; a saved row is a row and does not.
fn investigation_pane_rows(lines: &[String], saved_mode: bool, width: u16) -> usize {
    if saved_mode {
        return lines.len();
    }
    lines
        .iter()
        .map(|line| wrap_sentence(line, usize::from(width), usize::MAX).len())
        .sum()
}

fn render_investigation(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    use crate::app::InvestigationControl as C;
    use crate::app::InvestigationStage as Stage;
    use crate::dialog_layout::{DialogClass, DialogContent, content_width, pane};
    let styles = DialogStyles::new(theme);
    let ascii = app.appearance.ascii;
    let cursor = app.active_text_cursor();
    app.hit_regions.investigation_controls.clear();
    let Some(dialog) = app.investigation_dialog.clone() else {
        return;
    };
    let width = content_width(area, DialogClass::L);
    let editable = matches!(
        dialog.stage,
        Stage::Input | Stage::Conversation | Stage::Error
    );
    let saved_mode = dialog.saved_mode && !dialog.items.is_empty();

    // §7.4: the state word, and the progress line the agent is producing.
    let (state, sentence) = match dialog.stage {
        Stage::Input | Stage::Conversation => (MessageState::Ready, dialog.progress.clone()),
        Stage::Error => (MessageState::Error, dialog.progress.clone()),
        _ => (MessageState::Pending, dialog.progress.clone()),
    };
    let help = if dialog.session_id.is_some() {
        "Follow-ups reuse the fixed snapshot this session started from."
    } else {
        "Start an investigation on a fixed snapshot of this view; follow-ups reuse it."
    };
    // An empty transcript already shows this sentence as its own empty state;
    // §11 does not want it twice.
    let help_row = if dialog.messages.is_empty() && !dialog.saved_mode {
        ""
    } else {
        help
    };

    // Provenance: which session is answering, and the snapshot it is bound to.
    // One line, because it identifies the conversation rather than joining it.
    let provenance = match (&dialog.session_id, &dialog.snapshot_dir) {
        (Some(session), Some(snapshot)) => {
            Some(format!("Session: {session} · Snapshot: {snapshot}"))
        }
        (Some(session), None) => Some(format!("Session: {session}")),
        (None, Some(snapshot)) => Some(format!("Snapshot: {snapshot}")),
        (None, None) => None,
    };

    let primary = if dialog.stage == Stage::Conversation {
        "Send"
    } else if dialog.input.trim().is_empty() && !dialog.items.is_empty() {
        "Resume"
    } else {
        "Start"
    };
    let mut actions: Vec<(&str, C)> = Vec::new();
    if editable {
        actions.push((primary, C::Submit));
        if saved_mode {
            actions.push(("Open", C::Open));
        }
        if dialog.investigation_id.is_some() || dialog.session_id.is_some() {
            actions.push(("New snapshot", C::New));
        }
    }
    let action_labels = actions.iter().map(|(label, _)| *label).collect::<Vec<_>>();

    let segmented = !dialog.items.is_empty();
    let (heading, count, lines) = investigation_pane(&dialog, saved_mode, help, ascii);
    // Measure the pane at the width it will get, so the dialog asks for the
    // rows it will actually use rather than for the class maximum (§5.2).
    let pane_width = width
        .saturating_sub(crate::dialog_layout::PANE_INDENT)
        .max(1);
    let pane_rows = investigation_pane_rows(&lines, saved_mode, pane_width);
    let provenance_rows = u16::from(provenance.is_some());
    let content = DialogContent {
        header: u16::from(segmented),
        // Question, provenance, a blank row, the pane heading, its rows.
        body: INVESTIGATION_QUESTION_ROWS
            .saturating_add(provenance_rows)
            .saturating_add(2)
            .saturating_add(u16::try_from(pane_rows).unwrap_or(u16::MAX)),
        message: message_rows(&sentence, width).max(1),
        help: help_rows(help_row, width),
        actions: packed_button_rows(width, &action_labels),
    };
    let title = if ascii {
        "Investigation Agent"
    } else {
        "Investigation 🧠"
    };
    let regions = dialog_frame(frame, app, area, DialogClass::L, title, &content, theme);
    if segmented && regions.header.height > 0 {
        let labels = ["New", "Saved"];
        let active = usize::from(saved_mode);
        let focused = match dialog.focus {
            C::ModeNew => Some(0),
            C::Saved => Some(1),
            _ => None,
        };
        for (index, rect) in
            render_segmented_control(frame, regions.header, &labels, active, focused, theme)
                .into_iter()
                .enumerate()
        {
            app.hit_regions
                .investigation_controls
                .push((rect, if index == 0 { C::ModeNew } else { C::Saved }));
        }
    }

    let body = regions.body;
    if body.width == 0 || body.height == 0 {
        return;
    }
    let question_rows = INVESTIGATION_QUESTION_ROWS.min(body.height);
    let question = Rect::new(
        body.x.saturating_add(INVESTIGATION_LABEL_WIDTH),
        body.y,
        body.width.saturating_sub(INVESTIGATION_LABEL_WIDTH),
        question_rows,
    );
    frame.render_widget(
        Paragraph::new("Question").style(if dialog.focus == C::Prompt {
            styles.shortcut
        } else {
            styles.label
        }),
        Rect::new(body.x, body.y, INVESTIGATION_LABEL_WIDTH, 1),
    );
    if editable && question.width > 0 {
        InputSurface {
            style: styles.input,
        }
        .render(question, frame.buffer_mut());
        app.hit_regions
            .investigation_controls
            .push((question, C::Prompt));
    }
    // §8.1: a multiline field shows the line the caret is on, and the caret
    // sits where the next character will land.
    let wrapped = crate::text_edit::wrapped_text(&dialog.input, usize::from(question.width.max(1)));
    let (cursor_row, cursor_col) = cursor.map_or((0, 0), |cursor| {
        let mut value = crate::text_edit::TextCursor { char_index: cursor };
        crate::text_edit::wrapped_cursor(
            &dialog.input,
            &mut value,
            usize::from(question.width.max(1)),
        )
    });
    let top = cursor_row
        .saturating_add(1)
        .saturating_sub(usize::from(question.height.max(1)));
    frame.render_widget(
        Paragraph::new(wrapped.lines.get(top..).unwrap_or(&[]).join("\n")).style(if editable {
            styles.input
        } else {
            styles.description
        }),
        question,
    );
    if editable && dialog.focus == C::Prompt && question.width > 0 && question.height > 0 {
        let x = question.x + cursor_col.min(usize::from(question.width - 1)) as u16;
        let y = question.y
            + cursor_row
                .saturating_sub(top)
                .min(usize::from(question.height - 1)) as u16;
        frame.buffer_mut()[(x, y)].set_style(Style::default().bg(theme.cursor).fg(theme.input_fg));
        frame.set_cursor_position((x, y));
    }

    let mut cursor_y = body.y.saturating_add(question_rows);
    if let Some(provenance) = &provenance
        && cursor_y < body.bottom()
    {
        frame.render_widget(
            Paragraph::new(truncated(provenance, usize::from(body.width)))
                .style(styles.description),
            Rect::new(body.x, cursor_y, body.width, 1),
        );
        cursor_y = cursor_y.saturating_add(1);
    }
    // One blank row before the pane (§4.1), when there is one to spare.
    if cursor_y.saturating_add(2) < body.bottom() {
        cursor_y = cursor_y.saturating_add(1);
    }

    let pane_area = Rect::new(
        body.x,
        cursor_y,
        body.width,
        body.bottom().saturating_sub(cursor_y),
    );
    let rects = pane(
        pane_area,
        u16::try_from(UnicodeWidthStr::width(count.as_str())).unwrap_or(0),
        lines.len(),
    );
    if rects.heading.height > 0 {
        frame.render_widget(
            Paragraph::new(heading).style(if dialog.focus == C::More {
                styles.shortcut.add_modifier(Modifier::BOLD)
            } else {
                styles.label.add_modifier(Modifier::BOLD)
            }),
            rects.heading,
        );
        if rects.count.width > 0 {
            frame.render_widget(Paragraph::new(count).style(styles.description), rects.count);
        }
    }
    let wrapped_lines: Vec<(String, bool)> = lines
        .iter()
        .enumerate()
        .flat_map(|(index, line)| {
            let selected = saved_mode && index == dialog.selected;
            if saved_mode {
                vec![(truncated(line, usize::from(rects.viewport.width)), selected)]
            } else {
                wrap_sentence(line, usize::from(rects.viewport.width.max(1)), usize::MAX)
                    .into_iter()
                    .map(|part| (part, false))
                    .collect()
            }
        })
        .collect();
    let visible = usize::from(rects.viewport.height);
    let limit = wrapped_lines.len().saturating_sub(visible);
    let scroll = usize::from(dialog.review_scroll).min(limit);
    for (offset, (line, selected)) in wrapped_lines.iter().skip(scroll).take(visible).enumerate() {
        frame.render_widget(
            Paragraph::new(line.clone()).style(if *selected {
                styles.selection
            } else {
                styles.description
            }),
            Rect::new(
                rects.viewport.x,
                rects.viewport.y.saturating_add(offset as u16),
                rects.viewport.width,
                1,
            ),
        );
    }
    if let Some(bar) = rects.scrollbar {
        render_scrollbar(frame, bar, scroll, limit, theme, ascii);
    }
    app.hit_regions.dialog_scroll = Some(rects.viewport);
    if let Some(state) = &mut app.investigation_dialog {
        state.review_scroll_limit = u16::try_from(limit).unwrap_or(u16::MAX);
        state.review_scroll = u16::try_from(scroll).unwrap_or(0);
        if state.focus == C::More && limit == 0 {
            state.focus = if editable { C::Submit } else { C::Prompt };
        }
    }

    render_message(frame, regions.message, state, &sentence, theme, ascii);
    render_help_text(frame, regions.help, help_row, theme);
    let focused = actions
        .iter()
        .position(|(_, control)| *control == dialog.focus);
    for (index, rect) in
        render_action_row(frame, regions.actions, &action_labels, focused, &[], theme)
    {
        app.hit_regions
            .investigation_controls
            .push((rect, actions[index].1));
    }
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

/// §4.1 `gutter` between buttons.
pub(crate) const ACTION_GUTTER: u16 = 2;

/// §4.1 `gutter` between the label column and the field column.
pub(crate) const FIELD_GUTTER: u16 = 2;

/// §8.2 button row: primary first in `accent`, destructive last in `error`,
/// focused in the selection style. Returns the hitboxes actually drawn, which
/// are the same rects the mouse handler is given.
pub(crate) fn render_action_row(
    frame: &mut Frame<'_>,
    rect: Rect,
    labels: &[&str],
    focused: Option<usize>,
    destructive: &[usize],
    theme: Theme,
) -> Vec<(usize, Rect)> {
    if rect.height == 0 || rect.width == 0 {
        return Vec::new();
    }
    let styles = DialogStyles::new(theme);
    let mut placed = Vec::new();
    let mut x = rect.x;
    let mut y = rect.y;
    for (index, label) in labels.iter().enumerate() {
        let width = button_width(label).min(rect.width);
        if x > rect.x && x.saturating_add(width) > rect.right() {
            x = rect.x;
            y = y.saturating_add(1);
        }
        if y >= rect.bottom() {
            break;
        }
        let button = Rect::new(x, y, width, 1);
        let style = if focused == Some(index) {
            styles.selection.add_modifier(Modifier::BOLD)
        } else if destructive.contains(&index) {
            styles.error
        } else if index == 0 {
            styles.shortcut
        } else {
            styles.label
        };
        frame.render_widget(Paragraph::new(button_text(label)).style(style), button);
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
