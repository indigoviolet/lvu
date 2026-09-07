use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Widget, Wrap},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    App,
    app::{Focus, StorageCategory, format_storage_bytes},
    dialog_controls::{
        DialogStyles, action_line, button_layout, button_style, button_text, button_width,
        render_button,
    },
    dialog_layout::MIN_BODY_ROWS,
    json_spans::{JsonKind, JsonSpan, classify},
    provider::RowProvider,
    theme::{Theme, ThemeId, ensure_contrast},
};

const SIDEBAR_WIDTH: u16 = 22;

/// True while a modal dialog covers the workspace. The scrim (§6.2) and the
/// compact backdrop rule (§5.5) both key off this, so they can never disagree
/// about whether a dialog is open.
pub fn dialog_is_open(app: &App) -> bool {
    app.show_help || !matches!(app.focus, Focus::Logs | Focus::Selector | Focus::Details)
}

/// The marker for a value whose head is scrolled out of a field (§8.1) and for
/// a truncated list cell (§9). ASCII terminals get the same glyph from
/// crossterm; only product labels have an ASCII fallback.
const ELLIPSIS: &str = "…";

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
    render_with_theme(frame, app, provider, app.theme_id.theme(), delight);
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
    app.terminal_size = (geometry.area.width, geometry.area.height);
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
    app.hit_regions.editor_completion_rows.clear();
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
    if matches!(
        app.focus,
        Focus::SearchEditor | Focus::AdvancedEditor | Focus::GroupingEditor
    ) {
        render_editor(frame, app, geometry.area, theme);
    }
    if app.focus == Focus::EnrichmentEditor {
        render_enrichment_steps(frame, app, geometry.area, theme);
    }
    if app.focus == Focus::EnrichmentStep {
        render_enrichment_step(frame, app, provider, geometry.area, theme);
    }
    if app.focus == Focus::CommandEnrichment {
        render_command_enrichment(frame, app, geometry.area, theme);
    }
    if app.focus == Focus::SourceDialog {
        render_source_dialog(frame, app, geometry.area, theme);
    } else if app.focus == Focus::ViewDialog {
        render_view_dialog(frame, app, geometry.area, theme);
    } else if app.focus == Focus::FieldPicker {
        render_field_picker(frame, app, provider, geometry.area, theme);
    } else if app.focus == Focus::AskAi {
        render_ask_ai(frame, app, geometry.area, theme);
    } else if app.focus == Focus::Investigation {
        render_investigation(frame, app, geometry.area, theme);
    } else if app.focus == Focus::Recipes {
        render_recipes(frame, app, geometry.area, theme);
    } else if app.focus == Focus::TimeEditor {
        render_time_editor(frame, app, geometry.area, theme);
    } else if app.focus == Focus::Storage {
        render_storage(frame, app, geometry.area, theme);
    } else if app.focus == Focus::Settings {
        render_settings(frame, app, geometry.area, theme);
    }
    if app.focus == Focus::Bookmarks {
        render_bookmarks(frame, app, geometry.area, theme);
    }
    if app.focus == Focus::Context {
        render_context(frame, app, provider, geometry.area, theme);
    }
    if app.show_help {
        render_help(frame, app, geometry.area, theme);
    }
}

fn render_command_enrichment(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    use crate::app::{
        CommandEnrichmentControl as Control, CommandEnrichmentField as Field,
        CommandEnrichmentRunState as RunState,
    };
    let cursor = app.active_text_cursor();
    let styles = DialogStyles::new(theme);
    let Some(dialog) = app.command_enrichment_dialog.clone() else {
        return;
    };
    let popup = centered(area, 86, 24);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    frame.render_widget(
        Block::default()
            // §7.1/§11: a title is a noun. The confirmation promise moved into
            // the status line, where it also survives a narrow terminal that
            // cannot render a 42-column title.
            .title(" External command ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        popup,
    );
    let body = dialog_body_with_footer(popup, 0);
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(3),
    ])
    .split(body);
    let specs = [
        (
            Field::Program,
            "Program",
            &dialog.program,
            "Executable path; no shell parsing",
        ),
        (
            Field::Arguments,
            "Arguments",
            &dialog.arguments,
            "One argument per line, e.g. --format then json",
        ),
        (
            Field::Cwd,
            "Working directory",
            &dialog.cwd,
            "Optional; defaults to this workspace directory",
        ),
        (
            Field::Environment,
            "Environment",
            &dialog.environment,
            "Optional KEY=value per line, e.g. LANG=C",
        ),
    ];
    for (index, (field, label, value, help)) in specs.iter().enumerate() {
        let row = rows[index];
        let help = if matches!(field, Field::Arguments | Field::Environment) {
            format!("{} line(s) · {help}", value.split('\n').count())
        } else {
            (*help).to_owned()
        };
        frame.render_widget(
            Line::from(vec![
                Span::styled(
                    format!("{label}: "),
                    styles.label.add_modifier(Modifier::BOLD),
                ),
                Span::styled(help, styles.description),
            ]),
            Rect::new(row.x, row.y, row.width, 1),
        );
        let input = Rect::new(row.x, row.y.saturating_add(1), row.width, 1);
        InputSurface {
            style: styles.input,
        }
        .render(input, frame.buffer_mut());
        let final_line = value.rsplit('\n').next().unwrap_or("");
        frame.render_widget(
            Paragraph::new(input_tail(
                final_line,
                usize::from(input.width.saturating_sub(1)),
            ))
            .style(styles.input),
            input,
        );
        if dialog.selected_field == *field
            && dialog.selected_control == Control::Field
            && !matches!(
                dialog.run_state,
                RunState::Saving
                    | RunState::Preparing
                    | RunState::Running
                    | RunState::SavingResults
            )
        {
            place_input_cursor_at(
                frame,
                input,
                0,
                0,
                value,
                cursor.unwrap_or_else(|| value.chars().count()),
                theme,
            );
        }
    }
    let accepted =
        dialog
            .accepted
            .as_ref()
            .map_or("None · enrichment steps still apply".into(), |stage| {
                let crate::app::CommandEnrichmentStage { definition, .. } = stage;
                match &definition.program {
                    lvu_core::CommandProgram::Exec { executable, args } => format!(
                        "After {} enrichment step(s): {} ({} arguments)",
                        app.view_state().map_or(0, |state| state.enrichments.len()),
                        executable.display(),
                        args.len()
                    ),
                    lvu_core::CommandProgram::Shell { .. } => "Invalid saved command form".into(),
                }
            });
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "Applied command step: ",
                styles.applied.add_modifier(Modifier::BOLD),
            ),
            Span::styled(accepted, styles.description),
        ])),
        rows[4],
    );
    app.hit_regions.command_enrichment_controls.clear();
    let command_controls = [
        (Control::NewLine, "New line (Alt-N)"),
        (Control::Save, "Save"),
        (Control::Review, "Review"),
        (Control::Remove, "Remove"),
    ];
    render_command_controls(
        frame,
        rows[5],
        &command_controls,
        dialog.selected_control,
        &mut app.hit_regions.command_enrichment_controls,
        theme,
    );
    let status_kind = if dialog.error.is_some() || dialog.run_state == RunState::Error {
        "Error"
    } else {
        match dialog.run_state {
            RunState::Unrun => "Unrun",
            RunState::Saving => "Saving",
            RunState::Preparing => "Preparing",
            RunState::Ready => "Ready",
            RunState::Running => "Running",
            RunState::SavingResults => "Saving results",
            RunState::Complete => "Complete",
            RunState::Error => "Error",
        }
    };
    let status_detail = dialog.error.as_deref().unwrap_or(&dialog.run_status);
    let detail_already_names_state = dialog.error.is_none()
        && status_detail
            .strip_prefix(status_kind)
            .is_some_and(|tail| tail.starts_with(" ·") || tail.starts_with('…'));
    let mut status = if detail_already_names_state {
        format!("Status: {status_detail}")
    } else {
        format!("Status: {status_kind} · {status_detail}")
    };
    if matches!(dialog.run_state, RunState::Unrun) && dialog.error.is_none() {
        status.push_str(" · runs only when confirmed");
    }
    if app
        .view_state()
        .is_some_and(|state| state.command_publication.is_some())
        && dialog.run_state != RunState::Complete
    {
        status.push_str(
            "\nPrevious published results retained; changed and new records remain pending.",
        );
    }
    if let Some(review) = &dialog.review {
        status.push_str(&format!("\n\nRun review\nFixed snapshot: {} records from {} sources\nLimit: 1,024 records / 4 MiB input; no sampling\nExecutable: {}\nArguments: {}\nWorking directory: {}\nEnvironment keys: {}",
            review.record_count, review.source_count, review.executable, review.arguments.join(" | "), review.cwd.as_deref().unwrap_or("current"),
            if review.environment_keys.is_empty() { "none".into() } else { review.environment_keys.join(", ") }));
    } else if dialog.run_state == RunState::Unrun {
        status.push_str("\nSaving or restoring never starts this command. New records remain pending until another explicit run.");
    }
    status.push_str("\nResults appear in Details as command.<field>; command.status shows Ready or Pending. Filters and field choices use the enrichment steps above.");
    let status_style = if dialog.error.is_some() || dialog.run_state == RunState::Error {
        styles.error
    } else if matches!(
        dialog.run_state,
        RunState::Saving | RunState::Preparing | RunState::Running | RunState::SavingResults
    ) {
        styles.pending
    } else if dialog.run_state == RunState::Complete {
        styles.applied
    } else {
        styles.description
    };
    let status_p = Paragraph::new(status)
        .wrap(Wrap { trim: false })
        .style(status_style);
    let bordered_inner = rows[6].inner(ratatui::layout::Margin::new(1, 1));
    let overflow = status_p
        .line_count(bordered_inner.width)
        .saturating_sub(usize::from(bordered_inner.height));
    let status_block = Block::default()
        .title(if overflow > 0 {
            " Status and review · ↑/↓ scroll "
        } else {
            " Status and review "
        })
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if app.dialog_scroll_focused {
            theme.focused_input_border
        } else {
            theme.accent
        }));
    let status_inner = status_block.inner(rows[6]);
    app.dialog_scroll_limit = status_p
        .line_count(status_inner.width)
        .saturating_sub(usize::from(status_inner.height));
    app.dialog_scroll = app.dialog_scroll.min(app.dialog_scroll_limit);
    app.hit_regions.dialog_scroll = (app.dialog_scroll_limit > 0).then_some(rows[6]);
    frame.render_widget(
        status_p
            .scroll((app.dialog_scroll.min(u16::MAX as usize) as u16, 0))
            .block(status_block),
        rows[6],
    );
}

fn render_command_controls(
    frame: &mut Frame<'_>,
    area: Rect,
    controls: &[(crate::app::CommandEnrichmentControl, &str)],
    focused: crate::app::CommandEnrichmentControl,
    hitboxes: &mut Vec<(Rect, crate::app::CommandEnrichmentControl)>,
    theme: Theme,
) {
    let focused_index = controls.iter().position(|(control, _)| *control == focused);
    let labels = controls.iter().map(|(_, label)| *label).collect::<Vec<_>>();
    for (index, rect) in button_layout(area, &labels, focused_index) {
        let (control, label) = controls[index];
        hitboxes.push((rect, control));
        render_button(frame, rect, label, control == focused, false, theme);
    }
}

#[doc(hidden)]
pub fn render_bookmarks(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let styles = DialogStyles::new(theme);
    let cursor = app.active_text_cursor();
    app.hit_regions.bookmark_rows.clear();
    app.hit_regions.bookmark_controls.clear();
    let mut row_hitboxes = Vec::new();
    let mut control_hitboxes = Vec::new();
    let Some(dialog) = &app.bookmark_dialog else {
        return;
    };
    let popup = centered(area, 100, 22);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    frame.render_widget(
        Block::default()
            .title(" Bookmarks / notes · this view ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        popup,
    );
    let bookmarks = app.bookmarks_for_view(&dialog.view_id);
    let body = dialog_body_with_footer(popup, 1);
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(if dialog.editing.is_some() { 2 } else { 0 }),
        Constraint::Length(1),
    ])
    .split(body);
    frame.render_widget(
        Paragraph::new(format!(
            "{} / 128 bookmarks · {}",
            bookmarks.len(),
            dialog.status
        ))
        .style(if dialog.status.contains("limit") {
            styles.error
        } else {
            styles.description
        }),
        rows[0],
    );
    if let Some(id) = &dialog.editing {
        frame.render_widget(
            Paragraph::new(format!("Record {id}")).style(styles.label),
            rows[1],
        );
        let input = Rect::new(rows[2].x, rows[2].y + 1, rows[2].width, 1);
        frame.render_widget(
            Paragraph::new("Note (1024 bytes)").style(styles.label),
            rows[2],
        );
        frame.render_widget(
            Paragraph::new(clipped_width(&dialog.draft, usize::from(input.width)))
                .style(styles.input),
            input,
        );
        place_input_cursor_at(
            frame,
            input,
            0,
            0,
            &dialog.draft,
            cursor.unwrap_or_else(|| dialog.draft.chars().count()),
            theme,
        );
    } else {
        let count = usize::from(rows[1].height).max(1);
        let first = dialog.selected.saturating_sub(count.saturating_sub(1));
        let mut lines = Vec::new();
        for (index, bookmark) in bookmarks.iter().enumerate().skip(first).take(count) {
            let y = rows[1].y.saturating_add((index - first) as u16);
            if y < rows[1].bottom() {
                row_hitboxes.push((Rect::new(rows[1].x, y, rows[1].width, 1), index));
            }

            let note = if bookmark.note.is_empty() {
                "(no note)"
            } else {
                &bookmark.note
            };
            let text = format!(
                "{} #{} {note}",
                if index == dialog.selected { ">" } else { " " },
                bookmark.id.sequence
            );
            lines.push(Line::styled(
                clipped_width(&text, usize::from(rows[1].width)),
                if index == dialog.selected {
                    styles.selection
                } else {
                    styles.description
                },
            ));
        }
        frame.render_widget(Paragraph::new(lines), rows[1]);
    }
    let controls: Vec<(crate::app::BookmarkDialogControl, &str)> = if dialog.editing.is_some() {
        vec![(crate::app::BookmarkDialogControl::Save, "Save note")]
    } else if bookmarks.is_empty() {
        Vec::new()
    } else {
        vec![
            (crate::app::BookmarkDialogControl::Context, "Raw context"),
            (crate::app::BookmarkDialogControl::Edit, "Edit note"),
            (crate::app::BookmarkDialogControl::Delete, "Remove"),
        ]
    };
    let labels = controls.iter().map(|(_, label)| *label).collect::<Vec<_>>();
    let focused = controls
        .iter()
        .position(|(control, _)| *control == dialog.control);
    for (index, rect) in button_layout(rows[3], &labels, focused) {
        let (control, label) = controls[index];
        control_hitboxes.push((rect, control));
        render_button(frame, rect, label, dialog.control == control, false, theme);
    }
    let inner = popup.inner(ratatui::layout::Margin::new(1, 1));
    if inner.height > 0 && inner.width > 0 {
        let footer = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
        frame.render_widget(
            Paragraph::new(action_line(&[("↑/↓", "select")], theme)),
            footer,
        );
    }
    app.hit_regions.bookmark_rows = row_hitboxes;
    app.hit_regions.bookmark_controls = control_hitboxes;
}

fn render_context<P: RowProvider>(
    frame: &mut Frame<'_>,
    app: &mut App,
    provider: &P,
    area: Rect,
    theme: Theme,
) {
    let styles = DialogStyles::new(theme);
    let Some(dialog) = &app.context_dialog else {
        return;
    };
    let popup = centered(area, 116, 26);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    frame.render_widget(
        Block::default()
            .title(" Raw context · filter unchanged ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        popup,
    );
    let footer_actions = &[("↑/↓", "scroll"), ("g", "anchor")];
    let footer_height = read_only_action_footer_height(popup, footer_actions, theme);
    let body = dialog_body_with_footer(popup, footer_height);
    let len = usize::from(body.height.saturating_sub(2)).min(32);
    let page = provider.context_page(&dialog.view_id, &dialog.anchor, dialog.offset, len);
    let status = page.diagnostic.as_deref().unwrap_or(if page.pending {
        "loading raw context…"
    } else {
        "physical source records"
    });
    let status_style = if page.diagnostic.is_some() {
        styles.unavailable
    } else if page.pending {
        styles.pending
    } else {
        styles.description
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Anchor: ", styles.label),
            Span::styled(dialog.anchor.to_string(), styles.description),
            Span::styled(" · ", styles.description),
            Span::styled(status.to_owned(), status_style),
        ]),
        Line::styled(
            format!(
                "{}–{} / {} · raw, unfiltered, ungrouped",
                page.start.saturating_add(1).min(page.total),
                page.start.saturating_add(page.rows.len()),
                page.total
            ),
            styles.description,
        ),
    ];
    for row in page.rows {
        let selected = row.id == dialog.anchor;
        let text = format!(
            "{} {:>6} {}",
            if selected { ">" } else { " " },
            row.id.sequence,
            row.text.replace(['\n', '\r', '\t'], " ")
        );
        lines.push(Line::styled(
            clipped_width(&text, usize::from(body.width)),
            if selected {
                styles.selection
            } else {
                styles.description
            },
        ));
    }
    if let Some(anchor_position) = page.anchor_position
        && let Some(dialog) = &mut app.context_dialog
    {
        dialog.offset = (page.start as isize).saturating_sub(anchor_position as isize);
    }
    frame.render_widget(Paragraph::new(lines), body);
    render_read_only_action_footer(frame, popup, footer_actions, theme);
}

/// Hard-wrap on display width. `wrap_sentence` truncates a token that is wider
/// than the line, which would silently shorten a path; §9 only allows that for
/// list cells, never for a value the user has to read.
fn wrap_value(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut rows = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let head = clipped_width(rest, width);
        if head.is_empty() {
            break;
        }
        rest = &rest[head.len()..];
        rows.push(head);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// Logical body rows of the Settings form (§12.14). The form is always shown in
/// full; the body window follows the focused control, so no field is ever
/// hidden behind a paging button.
const SETTINGS_FORM_ROWS: u16 = 16;

fn settings_focus_row(focus: crate::app::SettingsControl) -> Option<u16> {
    use crate::app::{SettingsControl as Control, SettingsField as Field};
    Some(match focus {
        Control::Field(Field::Provider) => 1,
        Control::Field(Field::Mode) => 2,
        Control::Field(Field::Thinking) => 3,
        Control::Field(Field::Theme) => 6,
        Control::Field(Field::Delight)
        | Control::Field(Field::ReducedMotion)
        | Control::Field(Field::Ascii) => 7,
        Control::Field(Field::RowCache) => 10,
        Control::Field(Field::Membership) => 11,
        Control::Field(Field::DiskTotal) => 12,
        Control::Field(Field::IndexPerSource) => 13,
        Control::More => SETTINGS_FORM_ROWS.saturating_sub(1),
        Control::Save => return None,
    })
}

fn render_settings(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    use crate::app::{SettingsControl as Control, SettingsField as Field};
    use crate::dialog_layout::{DialogClass, DialogContent, content_width};

    let styles = DialogStyles::new(theme);
    let cursor = app.active_text_cursor();
    let ascii = app.ascii;
    app.hit_regions.settings_controls.clear();
    app.hit_regions.settings_theme_choices.clear();
    let Some(dialog) = app.settings_dialog.clone() else {
        return;
    };
    let values = dialog.draft.clone();
    let agent_label = if ascii { "Agent" } else { "🧠" };
    let width = content_width(area, DialogClass::L);

    // §7.4: one message row, one vocabulary, and a sentence that does not
    // repeat the state word.
    let (state, sentence) = match dialog.status_kind {
        crate::app::SettingsStatus::Saved if dialog.status.contains("restart") => (
            MessageState::Saved,
            "appearance applies now · cache limits apply after restart".to_owned(),
        ),
        crate::app::SettingsStatus::Saved => (MessageState::Saved, "saved and applied".to_owned()),
        crate::app::SettingsStatus::Pending if dialog.saving => {
            (MessageState::Pending, "saving settings".to_owned())
        }
        crate::app::SettingsStatus::Pending => {
            (MessageState::Pending, "changes are not saved".to_owned())
        }
        // The failure text is the sentence: an error the user cannot read is
        // not a diagnostic, and the pane it used to live in scrolls.
        crate::app::SettingsStatus::Error => (MessageState::Error, dialog.status.clone()),
    };
    // Wrap the effective values before measuring: a settings path is longer
    // than the pane at every terminal width, and it has to stay readable.
    let detail_width = usize::from(width.saturating_sub(crate::dialog_layout::PANE_INDENT)).max(1);
    let details: Vec<String> = settings_detail_lines(&dialog, agent_label)
        .iter()
        .flat_map(|line| wrap_value(&line.to_string(), detail_width))
        .collect();
    let natural_body = SETTINGS_FORM_ROWS.saturating_add(u16::try_from(details.len()).unwrap_or(0));

    let save_label = if dialog.saving { "Saving…" } else { "Save" };
    let content = DialogContent {
        header: 0,
        body: natural_body,
        message: message_rows(&sentence, width),
        help: 0,
        actions: packed_button_rows(width, &[save_label, "More"]),
    };
    let regions = dialog_frame(
        frame,
        app,
        area,
        DialogClass::L,
        "Settings",
        &content,
        theme,
    );
    let body = regions.body;
    if body.width == 0 || body.height == 0 {
        return;
    }

    // §8.8: the window follows focus, which is what keeps every field reachable
    // at 54x16 without a paging control or a change of information architecture.
    let visible = body.height;
    let max_offset = natural_body.saturating_sub(visible);
    let focus_row = settings_focus_row(dialog.focus).unwrap_or(0);
    let base_offset = focus_row
        .saturating_sub(visible.saturating_sub(1))
        .min(max_offset);
    // Focusing the effective-values pane hands it the arrow keys, so its own
    // scroll offset moves the body window further; otherwise the paths below
    // the pane heading would be unreachable.
    let pane_focused = dialog.focus == Control::More;
    let offset = if pane_focused {
        base_offset
            .saturating_add(u16::try_from(dialog.details_scroll).unwrap_or(u16::MAX))
            .min(max_offset)
    } else {
        base_offset
    };
    let overflows = natural_body > visible;
    let bar_width = u16::from(overflows);
    let form = Rect::new(
        body.x,
        body.y,
        body.width.saturating_sub(bar_width),
        body.height,
    );
    if overflows {
        render_scrollbar(
            frame,
            Rect::new(body.right().saturating_sub(1), body.y, 1, body.height),
            usize::from(offset),
            usize::from(max_offset),
            theme,
            ascii,
        );
        app.hit_regions.dialog_scroll = Some(body);
    }

    // Row index -> screen rect, or None when scrolled out of the window.
    let row_rect = |index: u16| -> Option<Rect> {
        (index >= offset && index < offset.saturating_add(visible))
            .then(|| Rect::new(form.x, form.y.saturating_add(index - offset), form.width, 1))
    };

    let label_width = u16::try_from(UnicodeWidthStr::width("Provider / model")).unwrap_or(16);
    let mut theme_anchor = Rect::default();

    let section = |frame: &mut Frame<'_>, rect: Option<Rect>, text: &str| {
        if let Some(rect) = rect {
            frame.render_widget(
                Paragraph::new(text.to_owned()).style(styles.label.add_modifier(Modifier::BOLD)),
                rect,
            );
        }
    };

    section(frame, row_rect(0), &format!("{agent_label} Agent"));
    section(frame, row_rect(5), "Appearance");
    section(frame, row_rect(9), "Cache limits (MiB)");

    for (index, field, label, value) in [
        (1u16, Field::Provider, "Provider / model", &values.provider),
        (2, Field::Mode, "Mode", &values.mode),
        (3, Field::Thinking, "Thinking", &values.thinking),
        (10, Field::RowCache, "Rows", &values.rows_mib),
        (11, Field::Membership, "Membership", &values.membership_mib),
        (
            12,
            Field::DiskTotal,
            "Derived total",
            &values.disk_total_mib,
        ),
        (
            13,
            Field::IndexPerSource,
            "Per source",
            &values.index_per_source_mib,
        ),
    ] {
        let Some(rect) = row_rect(index) else {
            continue;
        };
        render_labelled_field(
            frame,
            app,
            rect,
            label_width,
            label,
            value,
            Control::Field(field),
            dialog.focus == Control::Field(field),
            cursor,
            theme,
        );
    }

    // §8.3: the theme is a dropdown field, drawn in the field column like the
    // text fields rather than as a button with its label inside.
    if let Some(rect) = row_rect(6) {
        let control = Control::Field(Field::Theme);
        theme_anchor = render_dropdown_field(
            frame,
            app,
            rect,
            label_width,
            "Theme",
            values.theme.as_str(),
            control,
            dialog.focus == control,
            ascii,
            theme,
        );
    }

    // §8.4: toggles are checkboxes sharing a row, not buttons with state in the
    // label.
    if let Some(rect) = row_rect(7) {
        let mut x = rect.x;
        for (field, label, on) in [
            (Field::Delight, "Delight", values.delight_enabled),
            (
                Field::ReducedMotion,
                "Reduced motion",
                values.reduced_motion,
            ),
            (Field::Ascii, "ASCII", values.ascii),
        ] {
            let text = format!("[{}] {label}", if on { "x" } else { " " });
            let text_width = u16::try_from(UnicodeWidthStr::width(text.as_str())).unwrap_or(0);
            if x.saturating_add(text_width) > rect.right() {
                break;
            }
            let control = Control::Field(field);
            let cell = Rect::new(x, rect.y, text_width, 1);
            frame.render_widget(
                Paragraph::new(text).style(if dialog.focus == control {
                    styles.selection.add_modifier(Modifier::BOLD)
                } else {
                    styles.label
                }),
                cell,
            );
            app.hit_regions.settings_controls.push((cell, control));
            x = x
                .saturating_add(text_width)
                .saturating_add(ACTION_GUTTER + 1);
        }
    }

    // §8.7: the effective values are a pane — a bold heading and indented rows,
    // no border, and they scroll with the rest of the body.
    if let Some(rect) = row_rect(SETTINGS_FORM_ROWS.saturating_sub(1)) {
        frame.render_widget(
            Paragraph::new("Effective values and paths")
                .style(styles.label.add_modifier(Modifier::BOLD)),
            rect,
        );
    }
    for (index, line) in details.iter().enumerate() {
        let row = SETTINGS_FORM_ROWS.saturating_add(u16::try_from(index).unwrap_or(0));
        let Some(rect) = row_rect(row) else { continue };
        let indent = crate::dialog_layout::PANE_INDENT.min(rect.width);
        frame.render_widget(
            Paragraph::new(line.clone()).style(styles.description),
            Rect::new(
                rect.x.saturating_add(indent),
                rect.y,
                rect.width.saturating_sub(indent),
                1,
            ),
        );
    }

    // `More` no longer pages the form; it exists only while the body genuinely
    // overflows, and it moves focus into the scrolled region.
    if let Some(state) = &mut app.settings_dialog {
        state.details_scroll_limit = usize::from(max_offset.saturating_sub(base_offset));
        state.details_scroll = state.details_scroll.min(state.details_scroll_limit);
        if !overflows && state.focus == Control::More {
            state.focus = Control::Save;
        }
    }
    let mut controls = vec![(Control::Save, save_label)];
    if overflows {
        controls.push((Control::More, "More"));
    }
    let labels: Vec<&str> = controls.iter().map(|(_, label)| *label).collect();
    let focused = controls
        .iter()
        .position(|(control, _)| *control == dialog.focus);
    for (index, rect) in render_action_row(frame, regions.actions, &labels, focused, &[], theme) {
        app.hit_regions
            .settings_controls
            .push((rect, controls[index].0));
    }

    render_message(frame, regions.message, state, &sentence, theme, ascii);

    if dialog.theme_dropdown && theme_anchor.width > 0 {
        render_settings_theme_dropdown(
            frame,
            app,
            regions.popup,
            theme_anchor,
            dialog.theme_selected,
            theme,
        );
    }
}

/// §4.2: label column, then the field column at a fixed x. The painted input
/// rect is exactly the field.
#[allow(clippy::too_many_arguments)]
fn render_labelled_field(
    frame: &mut Frame<'_>,
    app: &mut App,
    row: Rect,
    label_width: u16,
    label: &str,
    value: &str,
    control: crate::app::SettingsControl,
    focused: bool,
    cursor: Option<usize>,
    theme: Theme,
) {
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
        return;
    }
    let field = Rect::new(field_x, row.y, row.right().saturating_sub(field_x), 1);
    app.hit_regions.settings_controls.push((field, control));
    if focused {
        place_input_cursor_at(
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
        // §11: an unfocused value is identified by its head, so it truncates at
        // the end. Showing the tail rendered `fixture/provider` as
        // `xture/provider`.
        frame.render_widget(
            Paragraph::new(truncated(value, usize::from(field.width))).style(styles.input),
            field,
        );
    }
}

/// §8.3: a dropdown is a field with a chevron in its last cell. Returns the
/// field rect so the popup can anchor to it.
#[allow(clippy::too_many_arguments)]
fn render_dropdown_field(
    frame: &mut Frame<'_>,
    app: &mut App,
    row: Rect,
    label_width: u16,
    label: &str,
    value: &str,
    control: crate::app::SettingsControl,
    focused: bool,
    ascii: bool,
    theme: Theme,
) -> Rect {
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
        return Rect::default();
    }
    let longest = ThemeId::ALL
        .iter()
        .map(|id| UnicodeWidthStr::width(id.as_str()))
        .max()
        .unwrap_or(12);
    let field_width = u16::try_from(longest + 4)
        .unwrap_or(16)
        .max(12)
        .min(row.right().saturating_sub(field_x));
    let field = Rect::new(field_x, row.y, field_width, 1);
    app.hit_regions.settings_controls.push((field, control));
    InputSurface {
        style: if focused {
            styles.selection
        } else {
            styles.input
        },
    }
    .render(field, frame.buffer_mut());
    frame.render_widget(
        Paragraph::new(truncated(value, usize::from(field.width.saturating_sub(2)))).style(
            if focused {
                styles.selection
            } else {
                styles.input
            },
        ),
        field,
    );
    frame.render_widget(
        Paragraph::new(if ascii { "v" } else { "▾" }).style(Style::default().fg(theme.accent).bg(
            if focused {
                theme.selection_bg
            } else {
                theme.input_bg
            },
        )),
        Rect::new(field.right().saturating_sub(1), field.y, 1, 1),
    );
    field
}

fn settings_detail_lines(
    dialog: &crate::app::SettingsDialogState,
    agent_label: &str,
) -> Vec<Line<'static>> {
    vec![
        Line::raw(format!("State detail: {}", dialog.status)),
        Line::raw(format!(
            "Effective {agent_label}: {} [{}] · {} [{}] · {} [{}]",
            dialog.context.effective_provider,
            dialog.context.provider_source,
            dialog.context.effective_mode,
            dialog.context.mode_source,
            dialog.context.effective_thinking,
            dialog.context.thinking_source
        )),
        Line::raw(format!(
            "Effective appearance: theme {} · delight {} [{}] · motion {} [{}] · ASCII {} [{}]",
            dialog.context.effective_theme.as_str(),
            dialog.context.effective_delight_enabled,
            dialog.context.delight_source,
            dialog.context.effective_reduced_motion,
            dialog.context.reduced_motion_source,
            dialog.context.effective_ascii,
            dialog.context.ascii_source
        )),
        Line::raw(format!("Settings: {}", dialog.context.settings_path)),
        Line::raw(format!("Data: {}", dialog.context.data_path)),
        Line::raw(format!("Cache: {}", dialog.context.cache_path)),
        Line::raw(format!("Capture: {}", dialog.context.capture_path)),
        Line::raw(
            "Cache-limit changes take effect after restart; appearance previews immediately.",
        ),
    ]
}

fn render_settings_theme_dropdown(
    frame: &mut Frame<'_>,
    app: &mut App,
    popup: Rect,
    anchor: Rect,
    selected: usize,
    theme: Theme,
) {
    let styles = DialogStyles::new(theme);
    let width = ThemeId::ALL
        .iter()
        .map(|value| UnicodeWidthStr::width(value.as_str()))
        .max()
        .unwrap_or(1) as u16
        + 2;
    let height = ThemeId::ALL
        .len()
        .min(usize::from(popup.height.saturating_sub(4))) as u16
        + 2;
    let x = anchor
        .x
        .min(popup.right().saturating_sub(width).saturating_sub(1));
    let y = anchor
        .bottom()
        .min(popup.bottom().saturating_sub(height).saturating_sub(1));
    let area = Rect::new(x, y, width.min(popup.width.saturating_sub(2)), height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(styles.label),
        area,
    );
    let choice_height = usize::from(area.height.saturating_sub(2));
    let selected = selected.min(ThemeId::ALL.len().saturating_sub(1));
    let choice_scroll = selected.saturating_add(1).saturating_sub(choice_height);
    for (offset, (index, value)) in ThemeId::ALL
        .iter()
        .enumerate()
        .skip(choice_scroll)
        .take(choice_height)
        .enumerate()
    {
        let rect = Rect::new(
            area.x + 1,
            area.y + 1 + offset as u16,
            area.width.saturating_sub(2),
            1,
        );
        app.hit_regions.settings_theme_choices.push((rect, index));
        frame.render_widget(
            Paragraph::new(value.as_str()).style(if index == selected {
                styles.selection
            } else {
                button_style(theme, false, false)
            }),
            rect,
        );
    }
}

fn render_storage(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let styles = DialogStyles::new(theme);
    let popup = centered(area, 88, 20);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    app.hit_regions.storage_rows.clear();
    let Some(dialog) = &app.storage_dialog else {
        return;
    };
    let snapshot = &dialog.snapshot;
    let title = format!(
        " Storage usage — total {} / unused derived {} ",
        format_storage_bytes(snapshot.total_bytes),
        format_storage_bytes(snapshot.reclaimable_bytes)
    );
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent));
    let footer_height = read_only_action_footer_height(
        popup,
        &[
            ("↑/↓", "active pane"),
            ("r", "refresh"),
            ("c", "preview/confirm cleanup"),
        ],
        theme,
    );
    let inner = dialog_body_with_footer(popup, footer_height);
    frame.render_widget(block, popup);
    render_read_only_action_footer(
        frame,
        popup,
        &[
            ("↑/↓", "active pane"),
            ("r", "refresh"),
            ("c", "preview/confirm cleanup"),
        ],
        theme,
    );
    if inner.height < 5 {
        return;
    }
    let header = Rect::new(inner.x, inner.y, inner.width, 4.min(inner.height));
    let budget = format!(
        "row cache {} / {}   query membership {} / {}\nderived disk cap/source {} · global {}\nmanaged budgets; not a process RSS limit",
        format_storage_bytes(snapshot.row_cache_bytes),
        format_storage_bytes(snapshot.row_cache_limit),
        format_storage_bytes(snapshot.query_index_bytes),
        format_storage_bytes(snapshot.query_index_limit),
        format_storage_bytes(snapshot.derived_index_limit_per_source),
        format_storage_bytes(snapshot.derived_index_limit_total),
    );
    frame.render_widget(Paragraph::new(budget).style(styles.description), header);
    let status_height = 3.min(inner.height.saturating_sub(header.height + 1));
    let rows = Rect::new(
        inner.x,
        header.bottom(),
        inner.width,
        inner.height.saturating_sub(header.height + status_height),
    );
    let visible = usize::from(rows.height);
    let start = dialog.selected.saturating_sub(visible.saturating_sub(1));
    let items = snapshot
        .entries
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(index, entry)| {
            let category = match entry.category {
                StorageCategory::Capture => "capture",
                StorageCategory::Derived => "derived",
                StorageCategory::Workspace => "workspace",
                StorageCategory::Investigation => "exports",
            };
            let marker = if index == dialog.selected { ">" } else { " " };
            let reclaim = if entry.reclaimable > 0 {
                " reclaimable"
            } else {
                ""
            };
            ListItem::new(format!(
                "{marker} {category:<9} {:>9} {} — {}{reclaim}",
                format_storage_bytes(entry.bytes),
                entry.label,
                entry.status
            ))
            .style(if index == dialog.selected {
                styles.selection
            } else {
                styles.description
            })
        })
        .collect::<Vec<_>>();
    for (offset, index) in (start..start + items.len()).enumerate() {
        app.hit_regions.storage_rows.push((
            Rect::new(rows.x, rows.y + offset as u16, rows.width, 1),
            index,
        ));
    }
    frame.render_widget(List::new(items), rows);
    let status_area = Rect::new(inner.x, rows.bottom(), inner.width, status_height);
    let errors = snapshot.errors.join("\nError: ");
    let status_text = if errors.is_empty() {
        format!("Status: {}", dialog.status)
    } else {
        format!("Status: {}\nError: {errors}", dialog.status)
    };
    let status = Paragraph::new(status_text)
        .wrap(Wrap { trim: false })
        .style(if errors.is_empty() {
            styles.applied
        } else {
            styles.error
        });
    let status_block = Block::default()
        .title(Span::styled(" Status ", styles.label))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if app.dialog_scroll_focused {
            theme.focused_input_border
        } else {
            theme.border
        }));
    let status_inner = status_block.inner(status_area);
    let limit = status
        .line_count(status_inner.width)
        .saturating_sub(usize::from(status_inner.height));
    app.dialog_scroll_limit = limit;
    app.dialog_scroll = app.dialog_scroll.min(limit);
    app.hit_regions.dialog_scroll = Some(status_area);
    frame.render_widget(
        status
            .scroll((app.dialog_scroll.min(u16::MAX as usize) as u16, 0))
            .block(status_block),
        status_area,
    );
}

struct TimeFieldLayout<'a> {
    control: crate::app::TimeControl,
    label: Rect,
    input: Rect,
    label_text: &'static str,
    value: &'a str,
}

struct TimeButtonLayout<'a> {
    control: crate::app::TimeControl,
    rect: Rect,
    label: &'a str,
}

struct TimeEditorLayout<'a> {
    fields: Vec<TimeFieldLayout<'a>>,
    buttons: Vec<TimeButtonLayout<'a>>,
    controls: Vec<(Rect, crate::app::TimeControl)>,
    status: Rect,
    help: Rect,
    height: u16,
}

fn time_status_row(label: &str, line: Option<&str>, width: u16, bottom: bool) -> String {
    let width = usize::from(width);
    if width == 0 {
        return String::new();
    }
    if let Some(line) = line {
        if width < 2 {
            return clipped_width(line, width);
        }
        let text = clipped_width(line, width.saturating_sub(2));
        return format!("│{text:<fill$}│", fill = width.saturating_sub(2));
    }
    if bottom {
        return if width == 1 {
            "└".into()
        } else {
            format!("└{}┘", "─".repeat(width.saturating_sub(2)))
        };
    }
    let title = format!(" {label} ");
    if width == 1 {
        return "┌".into();
    }
    let title = clipped_width(&title, width.saturating_sub(2));
    format!(
        "┌{title}{}┐",
        "─".repeat(width.saturating_sub(2 + title.width()))
    )
}

fn time_editor_layout<'a>(
    area: Rect,
    dialog: &'a crate::app::TimeDialogState,
    status_height: u16,
    help_height: u16,
    basis_label: &'a str,
    window_label: &'a str,
    recognize: &'a str,
) -> TimeEditorLayout<'a> {
    use crate::app::TimeControl as C;
    let width = area.width.max(1);
    let mut fields = Vec::new();
    let mut buttons = Vec::new();
    let mut controls = Vec::new();
    let mut y = 0;
    for (control, label) in [(C::Basis, basis_label), (C::Window, window_label)] {
        let rect = Rect::new(0, y, button_width(label).min(width), 1);
        buttons.push(TimeButtonLayout {
            control,
            rect,
            label,
        });
        controls.push((rect, control));
        y += 1;
    }
    y += 1;
    let wide = width >= 56;
    for (
        row_label,
        date,
        clock,
        zone,
        zone_menu,
        zone_custom,
        date_value,
        clock_value,
        zone_value,
    ) in [
        (
            "Start",
            C::StartDate,
            C::StartClock,
            C::StartZone,
            C::StartZoneMenu,
            dialog.start_zone_custom,
            dialog.start_date.as_str(),
            dialog.start_clock.as_str(),
            dialog.start_zone.as_str(),
        ),
        (
            "End",
            C::EndDate,
            C::EndClock,
            C::EndZone,
            C::EndZoneMenu,
            dialog.end_zone_custom,
            dialog.end_date.as_str(),
            dialog.end_clock.as_str(),
            dialog.end_zone.as_str(),
        ),
    ] {
        if wide {
            let row_label_width = 6;
            let date_width = 10;
            let zone_width = 14.min(width.saturating_sub(row_label_width + date_width + 2));
            let clock_width = width
                .saturating_sub(row_label_width + date_width + zone_width + 2)
                .max(12);
            let label_rect = Rect::new(0, y, row_label_width, 1);
            fields.push(TimeFieldLayout {
                control: date,
                label: label_rect,
                input: Rect::new(row_label_width, y, date_width, 1),
                label_text: row_label,
                value: date_value,
            });
            fields.push(TimeFieldLayout {
                control: clock,
                label: Rect::new(row_label_width + date_width, y, 1, 1),
                input: Rect::new(row_label_width + date_width + 1, y, clock_width, 1),
                label_text: "",
                value: clock_value,
            });
            fields.push(TimeFieldLayout {
                control: zone,
                label: Rect::new(row_label_width + date_width + clock_width + 1, y, 1, 1),
                input: Rect::new(
                    row_label_width + date_width + clock_width + 2,
                    y,
                    zone_width.saturating_sub(5),
                    1,
                ),
                label_text: "",
                value: zone_value,
            });
            let menu_width = button_width("▾").min(width);
            let menu_rect = Rect::new(width.saturating_sub(menu_width), y, menu_width, 1);
            buttons.push(TimeButtonLayout {
                control: zone_menu,
                rect: menu_rect,
                label: "▾",
            });
            controls.push((menu_rect, zone_menu));
            if !zone_custom {
                controls.retain(|(_, control)| *control != zone);
            }
            y += 1;
        } else {
            let date_label = if row_label == "Start" {
                "Start date"
            } else {
                "End date"
            };
            for (index, (control, label, value)) in [
                (date, date_label, date_value),
                (clock, "time", clock_value),
                (zone, "zone", zone_value),
            ]
            .into_iter()
            .enumerate()
            {
                let label_width = if index == 0 {
                    (date_label.width() as u16 + 2).min(width)
                } else {
                    6.min(width)
                };
                fields.push(TimeFieldLayout {
                    control,
                    label: Rect::new(0, y, label_width, 1),
                    input: Rect::new(
                        label_width,
                        y,
                        width.saturating_sub(label_width + if control == zone { 5 } else { 0 }),
                        1,
                    ),
                    label_text: label,
                    value,
                });
                if control == zone {
                    let menu_width = button_width("▾").min(width);
                    let menu_rect = Rect::new(width.saturating_sub(menu_width), y, menu_width, 1);
                    buttons.push(TimeButtonLayout {
                        control: zone_menu,
                        rect: menu_rect,
                        label: "▾",
                    });
                    controls.push((menu_rect, zone_menu));
                    if !zone_custom {
                        controls.retain(|(_, saved)| *saved != zone);
                    }
                }
                y += 1;
            }
        }
    }
    for field in &fields {
        let editable = match field.control {
            C::StartZone => dialog.start_zone_custom,
            C::EndZone => dialog.end_zone_custom,
            _ => true,
        };
        if editable {
            controls.push((field.input, field.control));
        }
    }
    y += 1;
    let action_specs = [
        (C::Apply, "Apply", button_width("Apply")),
        (C::Clear, "Clear", button_width("Clear")),
        (C::Recognize, recognize, button_width(recognize)),
    ];
    let mut x: u16 = 0;
    for (control, fallback, button_width) in action_specs {
        let label = fallback;
        let actual_width = button_width.min(width);
        if x > 0 && x.saturating_add(actual_width) > width {
            y += 1;
            x = 0;
        }
        let rect = Rect::new(x, y, actual_width, 1);
        buttons.push(TimeButtonLayout {
            control,
            rect,
            label,
        });
        controls.push((rect, control));
        x = x.saturating_add(actual_width + 1);
    }
    y += 2;
    let status = Rect::new(0, y, width, status_height.max(1).saturating_add(2));
    y += status.height + 1;
    let help = Rect::new(0, y, width, help_height.max(1));
    y += help.height;
    TimeEditorLayout {
        fields,
        buttons,
        controls,
        status,
        help,
        height: y,
    }
}

fn render_time_editor(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    use crate::app::{TimeControl as C, TimeDropdown as D, TimeWindowChoice as W};
    let styles = DialogStyles::new(theme);
    let popup = centered(area, 88, 22);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    app.hit_regions.time_controls.clear();
    app.hit_regions.time_choices.clear();
    let (Some(dialog), Some(state)) = (app.time_dialog.clone(), app.view_state().cloned()) else {
        return;
    };
    let basis = match dialog.basis {
        crate::TimeBasis::Capture => "Capture",
        crate::TimeBasis::Extracted => "Extracted timestamp_utc (UTC RFC3339)",
        crate::TimeBasis::Event => "Recognized event (RFC3339 normalized to UTC)",
    };
    let applied = match state.applied_capture_time_policy {
        Some(crate::CaptureTimePolicy::Recent { seconds }) => {
            format!("rolling last {}", crate::format_capture_duration(seconds))
        }
        Some(crate::CaptureTimePolicy::Absolute(_)) => state.applied_capture_time.map_or_else(
            || "absolute pending".into(),
            |w| {
                format!(
                    "absolute {} .. {}",
                    crate::format_utc_nanos(w.start_unix_nanos),
                    crate::format_utc_nanos(w.end_unix_nanos)
                )
            },
        ),
        None => "all times".into(),
    };
    frame.render_widget(
        Block::default()
            .title(" Time window ")
            .borders(Borders::ALL)
            .border_style(styles.label),
        popup,
    );
    let inner = popup.inner(ratatui::layout::Margin::new(2, 1));
    let window = match dialog.window {
        W::All => "All time".into(),
        W::Absolute => "Absolute".into(),
        W::Recent(s) if matches!(s, 300 | 900 | 3600) => {
            format!("Last {}", crate::format_capture_duration(s))
        }
        W::Recent(s) => format!("Custom last {}", crate::format_capture_duration(s)),
        W::AroundSelected => "Around selected".into(),
    };
    let updating = app.time_update_pending();
    let missing = match dialog.basis {
        crate::TimeBasis::Capture => dialog.anchored_capture_nanos.is_none(),
        crate::TimeBasis::Event => dialog.anchored_event_nanos.is_none(),
        crate::TimeBasis::Extracted => dialog.anchored_extracted_nanos.is_none(),
    };
    let reason = if dialog.window == W::AroundSelected && missing {
        "Around selected is disabled: the opening record has no timestamp in the chosen basis."
    } else {
        "Bounds are half-open. UTC and numeric offsets are normalized to UTC; named zones are not supported."
    };
    let help_width = usize::from(inner.width.max(1));
    let (status_label, status_text, status_style) = if let Some(error) = &state.time_error {
        ("Error", error.clone(), styles.error)
    } else if updating {
        (
            "Updating",
            "Last applied window remains active".into(),
            styles.pending,
        )
    } else {
        ("Applied", applied, styles.applied)
    };
    let status_lines = wrap_time_text(
        &format!("{status_label}: {status_text}"),
        help_width.saturating_sub(2),
    );
    let help_lines = wrap_time_text(reason, help_width);
    let recognize = if app.ascii {
        "Agent Recognize timestamp"
    } else {
        "🧠 Recognize timestamp"
    };
    let basis_label = format!("Time basis: {basis} ▾");
    let window_label = format!("Window: {window} ▾");
    let time_layout = time_editor_layout(
        inner,
        &dialog,
        status_lines.len() as u16,
        help_lines.len() as u16,
        &basis_label,
        &window_label,
        recognize,
    );
    let viewport = Rect::new(
        inner.x,
        inner.y.saturating_add(1),
        inner.width,
        inner.height.saturating_sub(2),
    );
    let max_scroll = usize::from(time_layout.height.saturating_sub(viewport.height));
    let focus_row = time_layout
        .controls
        .iter()
        .find(|(_, control)| *control == dialog.focus)
        .map(|(rect, _)| usize::from(rect.y));
    let mut scroll = dialog.scroll.min(max_scroll);
    if dialog.reveal_focus
        && let Some(focus_row) = focus_row
    {
        if focus_row < scroll {
            scroll = focus_row;
        }
        if focus_row >= scroll.saturating_add(usize::from(viewport.height)) {
            scroll = focus_row + 1 - usize::from(viewport.height);
        }
    }
    if let Some(current) = &mut app.time_dialog {
        current.scroll = scroll;
        current.reveal_focus = false;
        current.has_overflow = max_scroll > 0;
    }
    let project = |rect: Rect| {
        let y = usize::from(rect.y);
        (y >= scroll && y < scroll + usize::from(viewport.height)).then(|| {
            Rect::new(
                viewport.x + rect.x,
                viewport.y + (y - scroll) as u16,
                rect.width.min(viewport.width.saturating_sub(rect.x)),
                1,
            )
        })
    };
    for field in &time_layout.fields {
        let Some(label_rect) = project(field.label) else {
            continue;
        };
        let Some(input_rect) = project(field.input) else {
            continue;
        };
        frame.render_widget(
            Paragraph::new(field.label_text).style(styles.label),
            label_rect,
        );
        let focused = dialog.focus == field.control;
        let is_custom_zone = match field.control {
            C::StartZone => dialog.start_zone_custom,
            C::EndZone => dialog.end_zone_custom,
            _ => true,
        };
        let display = if is_custom_zone {
            field.value.to_owned()
        } else if field.value == "Z" {
            "UTC".into()
        } else {
            format!("UTC{}", field.value)
        };
        let caret = if focused {
            dialog.segment_cursor.min(field.value.chars().count())
        } else {
            field.value.chars().count()
        };
        let (visible_value, caret_column) = if focused {
            time_input_window(&display, caret, usize::from(input_rect.width))
        } else {
            (clipped_width(&display, usize::from(input_rect.width)), 0)
        };
        let style = if is_custom_zone {
            styles.input
        } else {
            styles.description
        };
        frame.render_widget(Paragraph::new(visible_value).style(style), input_rect);
        if focused && is_custom_zone && dialog.dropdown.is_none() && input_rect.width > 0 {
            let x = input_rect
                .x
                .saturating_add(caret_column as u16)
                .min(input_rect.right().saturating_sub(1));
            frame.render_widget(
                Block::default().style(Style::default().bg(theme.cursor)),
                Rect::new(x, input_rect.y, 1, 1),
            );
            frame.set_cursor_position((x, input_rect.y));
        }
        if is_custom_zone {
            app.hit_regions
                .time_controls
                .push((input_rect, field.control));
        }
    }
    for button in &time_layout.buttons {
        let Some(rect) = project(button.rect) else {
            continue;
        };
        let focused = dialog.focus == button.control;
        render_button(frame, rect, button.label, focused, false, theme);
        app.hit_regions.time_controls.push((rect, button.control));
    }
    for row in 0..time_layout.status.height {
        let logical = Rect::new(
            time_layout.status.x,
            time_layout.status.y + row,
            time_layout.status.width,
            1,
        );
        if let Some(rect) = project(logical) {
            let line = if row == 0 {
                time_status_row(status_label, None, rect.width, false)
            } else if row + 1 == time_layout.status.height {
                time_status_row(status_label, None, rect.width, true)
            } else {
                time_status_row(
                    status_label,
                    status_lines.get(usize::from(row - 1)).map(String::as_str),
                    rect.width,
                    false,
                )
            };
            frame.render_widget(Paragraph::new(line).style(status_style), rect);
        }
    }
    for (index, line) in help_lines.iter().enumerate() {
        let rect = Rect::new(
            time_layout.help.x,
            time_layout.help.y + index as u16,
            time_layout.help.width,
            1,
        );
        if let Some(rect) = project(rect) {
            frame.render_widget(
                Paragraph::new(line.as_str()).style(styles.description),
                rect,
            );
        }
    }
    if max_scroll > 0 {
        for (control, rect, label) in [
            (
                C::ScrollUp,
                Rect::new(inner.x, inner.y, inner.width, 1),
                "▲ Scroll up",
            ),
            (
                C::ScrollDown,
                Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
                "▼ Scroll down",
            ),
        ] {
            render_button(frame, rect, label, dialog.focus == control, false, theme);
            app.hit_regions.time_controls.push((rect, control));
        }
    }
    if let Some(dropdown) = dialog.dropdown {
        let choices: Vec<String> = match dropdown {
            D::Basis => vec!["Capture".into(), "Recognized".into(), "Extracted".into()],
            D::Window => dialog
                .window_choices
                .iter()
                .map(|choice| match choice {
                    W::All => "All time".into(),
                    W::Absolute => "Absolute".into(),
                    W::Recent(s) if matches!(s, 300 | 900 | 3600) => {
                        format!("Last {}", crate::format_capture_duration(*s))
                    }
                    W::Recent(s) => format!("Custom last {}", crate::format_capture_duration(*s)),
                    W::AroundSelected => "Around selected".into(),
                })
                .collect(),
            D::StartZone | D::EndZone => crate::app::time_zone_choices()
                .iter()
                .map(|(label, _)| (*label).into())
                .chain(std::iter::once("Custom offset…".into()))
                .collect(),
        };
        let selected = dialog.highlighted.min(choices.len().saturating_sub(1));
        let anchor = time_layout
            .controls
            .iter()
            .find(|(_, control)| {
                *control
                    == match dropdown {
                        D::Basis => C::Basis,
                        D::Window => C::Window,
                        D::StartZone => C::StartZoneMenu,
                        D::EndZone => C::EndZoneMenu,
                    }
            })
            .map_or(Rect::default(), |(rect, _)| *rect);
        let anchor_y = viewport.y + usize::from(anchor.y).saturating_sub(scroll) as u16;
        let w = choices
            .iter()
            .map(|s| s.as_str().width())
            .max()
            .unwrap_or(1) as u16
            + 4;
        let box_width = w.min(viewport.width).max(3.min(viewport.width));
        let dropdown_x = viewport
            .x
            .saturating_add(anchor.x)
            .min(viewport.right().saturating_sub(box_width));
        let below = viewport.bottom().saturating_sub(anchor_y.saturating_add(1));
        let above = anchor_y.saturating_sub(viewport.y);
        let desired_height = (choices.len() as u16 + 2).min(viewport.height);
        let place_below = below >= desired_height || below >= above;
        let available = if place_below { below } else { above };
        let mut box_height = desired_height.min(available);
        let mut dropdown_y = if place_below {
            anchor_y.saturating_add(1)
        } else {
            anchor_y.saturating_sub(box_height)
        };
        if box_height < 3 && viewport.height >= 3 {
            box_height = desired_height.min(viewport.height).max(3);
            dropdown_y = anchor_y
                .saturating_add(1)
                .min(viewport.bottom().saturating_sub(box_height))
                .max(viewport.y);
        }
        let box_area = Rect::new(dropdown_x, dropdown_y, box_width, box_height);
        if box_area.width < 3 || box_area.height < 3 {
            return;
        }
        frame.render_widget(Clear, box_area);
        let choice_height = usize::from(box_area.height - 2);
        let choice_scroll = selected.saturating_add(1).saturating_sub(choice_height);
        frame.render_widget(
            List::new(
                choices
                    .iter()
                    .enumerate()
                    .skip(choice_scroll)
                    .map(|(index, value)| {
                        ListItem::new(value.as_str()).style(if index == selected {
                            styles.selection
                        } else {
                            button_style(theme, false, false)
                        })
                    })
                    .collect::<Vec<_>>(),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(styles.label),
            ),
            box_area,
        );
        for (offset, i) in (choice_scroll..choices.len())
            .take(choice_height)
            .enumerate()
        {
            app.hit_regions.time_choices.push((
                Rect::new(
                    box_area.x + 1,
                    box_area.y + 1 + offset as u16,
                    box_area.width.saturating_sub(2),
                    1,
                ),
                i,
            ));
        }
    }
}

fn render_recipes(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let styles = DialogStyles::new(theme);
    let cursor = app.active_text_cursor();
    let popup = centered(area, 84, 20);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    let Some(dialog) = &app.recipe_dialog else {
        return;
    };
    app.hit_regions.recipe_controls.clear();
    let footer = Rect::new(
        popup.x + 1,
        popup.bottom().saturating_sub(4),
        popup.width.saturating_sub(2),
        3,
    );
    let body = dialog_body_with_footer(popup, footer.height);
    let mut lines = Vec::new();
    if dialog.mode == crate::app::RecipeDialogMode::Update {
        if let Some(item) = dialog.items.get(dialog.selected) {
            lines.push(format!("Update {} @ {}", item.name, item.revision));
        }
        lines
            .push("Saving creates a NEW revision from the active view’s accepted settings.".into());
        lines
            .push("Old revisions remain. A concurrent update is rejected; reload to retry.".into());
        lines.push("Recipe name, source and identity remain unchanged.".into());
    } else if dialog.mode.is_editable() {
        let label = if dialog.mode == crate::app::RecipeDialogMode::Save {
            "Name"
        } else {
            "TOML path"
        };
        lines.push(format!(
            "{label}: {}",
            clipped_width(&dialog.name, usize::from(popup.width.saturating_sub(14)))
        ));
        lines.push(if dialog.mode == crate::app::RecipeDialogMode::Save {
            "Only accepted settings are saved; unfinished drafts are excluded.".into()
        } else if dialog.mode == crate::app::RecipeDialogMode::Export {
            "Exports selected revision, including source paths/environment; never overwrites."
                .into()
        } else {
            "Import installs a canonical copy for preview; Apply is a separate action.".into()
        });
        if dialog.mode == crate::app::RecipeDialogMode::Export
            && let Some(item) = dialog.items.get(dialog.selected)
        {
            lines.push(format!("Selected: {} · {}", item.name, item.revision));
        }
    } else {
        let visible = usize::from(body.height.saturating_sub(6)).clamp(1, 12);
        let first = dialog.selected.saturating_sub(visible.saturating_sub(1));
        for (index, item) in dialog.items.iter().enumerate().skip(first).take(visible) {
            let suggested = dialog
                .suggestions
                .iter()
                .any(|value| value.recipe_id == item.id);
            lines.push(format!(
                "{} {}{} @ {}",
                if index == dialog.selected { ">" } else { " " },
                if suggested { "★ " } else { "" },
                item.name,
                &item.revision[..item.revision.len().min(8)]
            ));
        }
        if dialog.items.is_empty() {
            lines.push("(no saved recipes)".into());
        }
        if let Some(item) = dialog.items.get(dialog.selected) {
            if let Some(suggestion) = dialog
                .suggestions
                .iter()
                .find(|value| value.recipe_id == item.id)
            {
                lines.push(format!(
                    "Suggested because: {}",
                    suggestion.evidence.join("; ")
                ));
                if !suggestion.missing_fields.is_empty() {
                    lines.push(format!(
                        "Required fields not observed in sampled visible rows: {}",
                        suggestion.missing_fields.join(", ")
                    ));
                }
            } else if dialog.suggestions.is_empty() {
                lines.push(
                    "No applicable similar-source suggestions; all recipes remain browsable."
                        .into(),
                );
            }
            lines.push(format!(
                "Preview search={:?} advanced={} enrichment={} pins={} color={} capture-time={}",
                item.config.search,
                !item.config.advanced.is_empty(),
                !item.config.enrichment.is_empty(),
                item.config.pinned_columns.join(","),
                item.config.color_field.as_deref().unwrap_or("none"),
                match item.config.capture_time_policy {
                    Some(crate::CaptureTimePolicy::Recent { .. }) => "rolling recent",
                    Some(crate::CaptureTimePolicy::Absolute(_)) => "fixed UTC",
                    None => "all",
                }
            ));
            if let Some(error) = &item.incompatibility {
                lines.push(format!("Cannot apply: {error}"));
            }
        }
    }
    lines.push(format!(
        "{}: {}",
        if dialog.loading {
            "Updating"
        } else if dialog.status.contains("fail") || dialog.status.contains("full") {
            "Error"
        } else {
            "Applied"
        },
        dialog.status
    ));
    frame.render_widget(
        Block::default()
            .title(" Named recipes ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        popup,
    );
    frame.render_widget(
        Paragraph::new(lines.join("\n"))
            .wrap(Wrap { trim: false })
            .style(styles.description),
        body,
    );
    if dialog.mode.is_editable() {
        let input_row = if dialog.mode == crate::app::RecipeDialogMode::Update {
            1
        } else {
            0
        };
        let input_x = if dialog.mode == crate::app::RecipeDialogMode::Save {
            6
        } else {
            11
        };
        let input = Rect::new(
            body.x + input_x,
            body.y + input_row as u16,
            body.width.saturating_sub(input_x),
            1,
        );
        frame.render_widget(
            Paragraph::new(clipped_width(&dialog.name, usize::from(input.width)))
                .style(styles.input),
            input,
        );
        place_input_cursor_at(
            frame,
            body,
            input_row,
            input_x as usize,
            &dialog.name,
            cursor.unwrap_or_else(|| dialog.name.chars().count()),
            theme,
        );
    }
    let mut controls = crate::app::RecipeDialogMode::ALL
        .iter()
        .copied()
        .map(|mode| {
            (
                crate::app::RecipeDialogControl::Mode(mode),
                match mode {
                    crate::app::RecipeDialogMode::Browse => "Browse",
                    crate::app::RecipeDialogMode::Save => "Save",
                    crate::app::RecipeDialogMode::Import => "Import",
                    crate::app::RecipeDialogMode::Export => "Export",
                    crate::app::RecipeDialogMode::History => "History",
                    crate::app::RecipeDialogMode::Update => "Update",
                },
            )
        })
        .collect::<Vec<_>>();
    if dialog.mode.is_editable() {
        controls.push((
            crate::app::RecipeDialogControl::Apply,
            match dialog.mode {
                crate::app::RecipeDialogMode::Save | crate::app::RecipeDialogMode::Update => {
                    "Save revision"
                }
                crate::app::RecipeDialogMode::Import => "Review import",
                crate::app::RecipeDialogMode::Export => "Export revision",
                _ => "Apply",
            },
        ));
    } else {
        if dialog.mode == crate::app::RecipeDialogMode::Browse {
            controls.push((crate::app::RecipeDialogControl::Refresh, "Refresh"));
        }
        let suggested = dialog
            .items
            .get(dialog.selected)
            .is_some_and(|item| dialog.suggestions.iter().any(|s| s.recipe_id == item.id));
        if suggested {
            controls.extend([
                (crate::app::RecipeDialogControl::Adapt, "Adapt"),
                (crate::app::RecipeDialogControl::Reject, "Reject"),
            ]);
        }
        controls.push((crate::app::RecipeDialogControl::Apply, "Apply revision"));
    }
    let labels = controls.iter().map(|(_, label)| *label).collect::<Vec<_>>();
    let focused = controls
        .iter()
        .position(|(control, _)| *control == dialog.control);
    let mut hitboxes = Vec::new();
    for (index, rect) in button_layout(footer, &labels, focused) {
        let (control, label) = controls[index];
        hitboxes.push((rect, control));
        let selected =
            matches!(control, crate::app::RecipeDialogControl::Mode(mode) if mode == dialog.mode);
        render_button(
            frame,
            rect,
            label,
            control == dialog.control,
            selected,
            theme,
        );
    }
    app.hit_regions.recipe_controls = hitboxes;
}

fn sidebar_view_regions(app: &App, area: Option<Rect>) -> Vec<(Rect, usize)> {
    let Some(area) = area else { return Vec::new() };
    let mut y = area.y.saturating_add(1);
    let mut regions = Vec::new();
    for source in &app.sources {
        y = y.saturating_add(2);
        for (index, _) in app
            .views
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
            " {follow}{capture_time}{runtime} | {}-{}/{}{}{}{}{enrichment}{grouping} | ? help ",
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
            .views
            .iter()
            .enumerate()
            .filter(|(_, view)| view.source_id == source.id)
        {
            let marker = if index == app.selected_view {
                "›"
            } else {
                " "
            };
            let style = if index == app.selected_view {
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
                .then_some(if app.ascii { "* " } else { "★ " });
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

fn render_field_picker<P: RowProvider>(
    frame: &mut Frame<'_>,
    app: &mut App,
    provider: &P,
    area: Rect,
    theme: Theme,
) {
    let styles = DialogStyles::new(theme);
    let popup = centered(area, 70, 16);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    app.hit_regions.field_picker_rows.clear();
    frame.render_widget(
        Block::default()
            .title(" Event fields ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        popup,
    );
    let row = app.field_picker_row(provider);
    let has_anchor = app.field_picker_row_id().is_some();
    let footer_actions: Option<&[(&str, &str)]> =
        if row.as_ref().is_some_and(|row| !row.fields.is_empty()) {
            Some(&[
                ("↑/↓", "select"),
                ("Space", "pin"),
                ("c", "Color rows by this field"),
            ])
        } else if has_anchor {
            Some(&[("o", "raw context")])
        } else {
            None
        };
    let body = if let Some(actions) = footer_actions {
        let footer_height = read_only_action_footer_height(popup, actions, theme);
        render_read_only_action_footer(frame, popup, actions, theme);
        dialog_body_with_footer(popup, footer_height)
    } else {
        popup.inner(ratatui::layout::Margin::new(2, 1))
    };
    let Some(row) = row else {
        let message = if has_anchor {
            "Field data is not available yet."
        } else {
            "No event selected."
        };
        frame.render_widget(Paragraph::new(message).style(styles.unavailable), body);
        return;
    };
    if row.fields.is_empty() {
        frame.render_widget(
            Paragraph::new("No fields found for this event").style(styles.unavailable),
            body,
        );
        return;
    }
    let visible = usize::from(body.height);
    app.set_field_picker_viewport(visible);
    let Some(state) = app.view_state() else {
        return;
    };
    let selected = state.field_picker_selected;
    let top = state.field_picker_top;
    let pinned = state.pinned_columns.clone();
    let color_field = state.color_field.clone();
    let mut lines = Vec::new();
    for (position, (index, (key, value))) in row
        .fields
        .iter()
        .enumerate()
        .skip(top)
        .take(visible)
        .enumerate()
    {
        let cursor = if index == selected { ">" } else { " " };
        let pin = if pinned.contains(key) { "[x]" } else { "[ ]" };
        let color = if color_field.as_deref() == Some(key) {
            " color"
        } else {
            ""
        };
        let text = clipped_width(
            &format!("{cursor} {pin} {key} = {value}{color}"),
            usize::from(body.width),
        );
        lines.push(Line::styled(
            text,
            if index == selected {
                styles.selection
            } else {
                styles.description
            },
        ));
        app.hit_regions.field_picker_rows.push((
            Rect::new(body.x, body.y + position as u16, body.width, 1),
            index,
        ));
    }
    frame.render_widget(Paragraph::new(lines), body);
}

/// §6.3: a placeholder marks an empty field without pretending to be a value.
fn render_placeholder(frame: &mut Frame<'_>, field: Rect, text: &str, theme: Theme) {
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

fn render_editor(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let active_cursor = app.active_text_cursor();
    let Some(editor) = app.active_editor_state().cloned() else {
        return;
    };
    if app.focus == Focus::GroupingEditor {
        render_shared_compact_grouping(frame, app, area, editor, active_cursor, theme);
        return;
    }
    render_simple_editor(frame, app, area, editor, theme);
}

/// §12.3. The preview pane is fixed content, so it is measured, not guessed.
const GROUPING_PREVIEW: [&str; 2] = ["RuntimeException: boom", "  at worker.rs:42"];

fn render_shared_compact_grouping(
    frame: &mut Frame<'_>,
    app: &mut App,
    area: Rect,
    editor: crate::app::EditorState,
    cursor: Option<usize>,
    theme: Theme,
) {
    use crate::dialog_layout::{DialogClass, DialogContent, content_width, pane};

    let styles = DialogStyles::new(theme);
    let ascii = app.ascii;
    let width = content_width(area, DialogClass::S);

    let (state, sentence) = if let Some(error) = editor.error.as_deref() {
        (MessageState::Error, error.to_owned())
    } else if editor.pending_generation.is_some() {
        (
            MessageState::Updating,
            "checking this draft · the last applied grouping stays active".to_owned(),
        )
    } else if editor.applied.is_empty() {
        (
            MessageState::Disabled,
            "an empty draft turns grouping off".to_owned(),
        )
    } else {
        (MessageState::Applied, editor.applied.clone())
    };
    let help = "Continuation lines match this regex over raw bytes; grouping is display only.";
    // §3: the action row is part of the anatomy, not an afterthought. Without
    // it this dialog rendered no way to apply at all and relied on the user
    // knowing that Enter works.
    let labels = ["Apply"];

    let preview_rows = u16::try_from(GROUPING_PREVIEW.len()).unwrap_or(2);
    let content = DialogContent {
        header: 0,
        // input row, gap, preview pane heading, preview rows
        body: 3u16.saturating_add(preview_rows),
        message: message_rows(&sentence, width),
        help: help_rows(help, width),
        actions: packed_button_rows(width, &labels),
    };
    let regions = dialog_frame(
        frame,
        app,
        area,
        DialogClass::S,
        "Multiline grouping",
        &content,
        theme,
    );
    if regions.body.width == 0 || regions.body.height == 0 {
        return;
    }

    let field = Rect::new(regions.body.x, regions.body.y, regions.body.width, 1);
    if !app.dialog_scroll_focused {
        place_input_cursor_at(
            frame,
            field,
            0,
            0,
            &editor.draft,
            cursor.unwrap_or_else(|| editor.draft.chars().count()),
            theme,
        );
    } else {
        InputSurface {
            style: styles.input,
        }
        .render(field, frame.buffer_mut());
        frame.render_widget(
            Paragraph::new(truncated(&editor.draft, usize::from(field.width))).style(styles.input),
            field,
        );
    }

    // §8.7: the preview is a pane, not three loose rows under a colon label.
    let preview_area = Rect::new(
        regions.body.x,
        regions.body.y.saturating_add(2),
        regions.body.width,
        regions.body.height.saturating_sub(2),
    );
    if preview_area.height > 0 {
        let rects = pane(preview_area, 0, GROUPING_PREVIEW.len());
        if rects.heading.height > 0 {
            frame.render_widget(
                Paragraph::new("Preview").style(styles.label.add_modifier(Modifier::BOLD)),
                rects.heading,
            );
        }
        for (index, line) in GROUPING_PREVIEW.iter().enumerate() {
            let Some(y) = u16::try_from(index)
                .ok()
                .map(|offset| rects.viewport.y.saturating_add(offset))
                .filter(|y| *y < rects.viewport.bottom())
            else {
                continue;
            };
            frame.render_widget(
                Paragraph::new(truncated(line, usize::from(rects.viewport.width)))
                    .style(styles.description),
                Rect::new(rects.viewport.x, y, rects.viewport.width, 1),
            );
        }
    }

    // The status is one line now, so nothing overflows and no scroll
    // affordance is claimed (§9).
    app.dialog_scroll_limit = 0;
    app.hit_regions.dialog_scroll = None;

    render_message(frame, regions.message, state, &sentence, theme, ascii);
    render_help_text(frame, regions.help, help, theme);
    render_action_row(frame, regions.actions, &labels, None, &[], theme);
}

fn render_simple_editor(
    frame: &mut Frame<'_>,
    app: &mut App,
    area: Rect,
    editor: crate::app::EditorState,
    theme: Theme,
) {
    use crate::dialog_layout::{DialogClass, DialogContent, content_width};

    let styles = DialogStyles::new(theme);
    let ascii = app.ascii;
    let cursor = app
        .active_text_cursor()
        .unwrap_or_else(|| editor.draft.chars().count());
    let search = app.focus == Focus::SearchEditor;
    let title = if search { "Search" } else { "Advanced filter" };
    let help = if search {
        r#"Examples: text · "field name": text · /regex/ims · \/literal"#
    } else {
        "Use a Polars expression. Fields and sampled literals complete with Tab."
    };
    let width = content_width(area, DialogClass::S);

    // §7.4: one message row. The last accepted value stays in the sentence, so
    // a failing draft never hides the filter that is actually applied.
    let (state, mut sentence) = if let Some(error) = editor.error.as_deref() {
        (MessageState::Error, error.to_owned())
    } else if editor.pending_generation.is_some() {
        (
            MessageState::Updating,
            "checking this draft · the last applied view stays visible".to_owned(),
        )
    } else if editor.applied.is_empty() {
        (MessageState::NoFilter, "every record is shown".to_owned())
    } else {
        (MessageState::Applied, editor.applied.clone())
    };
    if !editor.applied.is_empty() && editor.draft != editor.applied {
        sentence.push_str(" · last accepted ");
        sentence.push_str(&editor.applied);
    }

    // §7.4 caps the message row at two rows, but a rejected expression can carry
    // a long diagnostic and AGENTS.md requires it to stay reachable. When the
    // sentence does not fit, the state stays in the message row and the full
    // text moves into a scrollable pane (§9) instead of being clipped away.
    let message_width = usize::from(width.saturating_sub(MESSAGE_SENTENCE_COLUMN)).max(1);
    let wrapped = wrap_sentence(&sentence, message_width, usize::MAX);
    let overflows = wrapped.len() > usize::from(message_rows(&sentence, width));
    let diagnostic_rows = if overflows {
        u16::try_from(wrapped.len()).unwrap_or(u16::MAX).min(8)
    } else {
        0
    };

    let content = DialogContent {
        header: 0,
        body: if overflows {
            2u16.saturating_add(diagnostic_rows)
        } else {
            1
        },
        message: message_rows(&sentence, width),
        help: help_rows(help, width),
        actions: 0,
    };
    let regions = dialog_frame(frame, app, area, DialogClass::S, title, &content, theme);
    if regions.body.width == 0 || regions.body.height == 0 {
        return;
    }

    let field = Rect::new(regions.body.x, regions.body.y, regions.body.width, 1);
    if app.editor_completion.is_none() && !app.dialog_scroll_focused {
        place_input_cursor_at(frame, field, 0, 0, &editor.draft, cursor, theme);
        if editor.draft.is_empty() {
            let placeholder = if search {
                "Type to filter…"
            } else {
                r#"Polars expression, e.g. col("level") == "ERROR""#
            };
            render_placeholder(
                frame,
                Rect::new(
                    field.x.saturating_add(1),
                    field.y,
                    field.width.saturating_sub(1),
                    1,
                ),
                placeholder,
                theme,
            );
        }
    } else {
        InputSurface {
            style: styles.input,
        }
        .render(field, frame.buffer_mut());
        frame.render_widget(
            Paragraph::new(truncated(&editor.draft, usize::from(field.width))).style(styles.input),
            field,
        );
    }

    if overflows && regions.body.height > 1 {
        let pane_area = Rect::new(
            regions.body.x,
            regions.body.y.saturating_add(1),
            regions.body.width,
            regions.body.height.saturating_sub(1),
        );
        let rects = crate::dialog_layout::pane(pane_area, 0, wrapped.len());
        if rects.heading.height > 0 {
            frame.render_widget(
                Paragraph::new("Diagnostics").style(styles.label.add_modifier(Modifier::BOLD)),
                rects.heading,
            );
        }
        let visible = usize::from(rects.viewport.height);
        let limit = wrapped.len().saturating_sub(visible);
        app.dialog_scroll_limit = limit;
        app.dialog_scroll = app.dialog_scroll.min(limit);
        app.hit_regions.dialog_scroll = (limit > 0).then_some(rects.viewport);
        for (offset, line) in wrapped
            .iter()
            .skip(app.dialog_scroll)
            .take(visible)
            .enumerate()
        {
            frame.render_widget(
                Paragraph::new(line.clone()).style(styles.description),
                Rect::new(
                    rects.viewport.x,
                    rects.viewport.y.saturating_add(offset as u16),
                    rects.viewport.width,
                    1,
                ),
            );
        }
        if let Some(bar) = rects.scrollbar {
            render_scrollbar(frame, bar, app.dialog_scroll, limit, theme, ascii);
        }
    } else {
        // A status that fits claims no affordance (§9).
        app.dialog_scroll_limit = 0;
        app.hit_regions.dialog_scroll = None;
    }

    render_message(frame, regions.message, state, &sentence, theme, ascii);
    render_help_text(frame, regions.help, help, theme);
    render_editor_completion(frame, app, area, theme);
}

// The enrichment work landed private copies of these while dialog_layout.rs did
// not exist yet. They are now thin adapters over the shared primitives so there
// is one implementation of the spec, and its call sites did not have to move.
fn dialog_compact(area: Rect) -> bool {
    crate::dialog_layout::is_compact(area)
}

fn class_l_width(area: Rect) -> u16 {
    crate::dialog_layout::DialogClass::L.width(area)
}

type DialogRegions = crate::dialog_layout::DialogRegions;

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

fn class_l_popup(area: Rect, body_rows: u16, message: u16, help: u16, actions: u16) -> Rect {
    crate::dialog_layout::dialog_rect(
        area,
        crate::dialog_layout::DialogClass::L,
        &class_l_content(body_rows, message, help, actions),
    )
}

fn dialog_regions(popup: Rect, message: u16, help: u16, actions: u16) -> DialogRegions {
    // These callers size their own popup first and then take whatever the body
    // has left, so the body they "want" is only the floor that decides whether
    // the layout is squeezed enough to shed help and padding.
    crate::dialog_layout::regions(
        popup,
        &class_l_content(MIN_BODY_ROWS, message, help, actions),
    )
}

fn scrim(frame: &mut Frame<'_>, area: Rect, theme: Theme) {
    crate::dialog_layout::scrim(frame.buffer_mut(), area, theme);
}

/// §6.3/§7.4 message row: glyph, padded state word, one sentence. The
/// vocabulary is closed by the spec; `Scanned` and `Unrun` belong to Storage
/// and External command, which are not on the anatomy yet.
#[derive(Clone, Copy, Eq, PartialEq)]
#[allow(dead_code)]
enum MessageState {
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
fn wrap_sentence(sentence: &str, width: usize, rows: usize) -> Vec<String> {
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
const MESSAGE_SENTENCE_COLUMN: u16 = 12;

fn message_rows(sentence: &str, content_width: u16) -> u16 {
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
struct PaneRects {
    viewport: Rect,
    scrollbar: Option<Rect>,
}

fn render_message(
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

fn render_pane_heading(
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
fn render_scrollbar(
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
fn truncated(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width || width == 0 {
        return text.to_owned();
    }
    let mut result = clipped_width(text, width.saturating_sub(1));
    result.push('…');
    result
}

fn step_summary(source: &str, width: usize) -> String {
    truncated(&source.replace('\n', " ⏎ "), width)
}

/// Packs button labels the way `button_layout` does, returning the row count.
fn packed_button_rows(width: u16, labels: &[&str]) -> u16 {
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

fn render_dialog_frame(
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

/// Layer one: the ordered enrichment steps and their actions only.
fn render_enrichment_steps(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    // §6.2: the workspace behind an open dialog is inactive, not hidden.
    scrim(frame, area, theme);
    render_enrichment_step_list(frame, app, area, theme, true);
}

fn render_enrichment_step_list(
    frame: &mut Frame<'_>,
    app: &mut App,
    area: Rect,
    theme: Theme,
    active: bool,
) {
    use crate::app::EnrichmentControl as Control;

    let styles = DialogStyles::new(theme);
    let Some(state) = app.view_state() else {
        return;
    };
    let stages = state.enrichments.clone();
    let selected = state
        .enrichment_selected
        .min(stages.len().saturating_sub(1));
    let focused = state.enrichment_control;
    let editor = state.enrichment.clone();
    let command = state.command_enrichment.clone();

    let labels = ["Add", "Edit", "Remove", "External command…"];
    let controls = [
        Control::Add,
        Control::Edit,
        Control::Remove,
        Control::ExternalCommand,
    ];
    let (message_state, mut sentence) = if let Some(error) = &editor.error {
        (
            MessageState::Error,
            format!("{error} · every accepted step is retained"),
        )
    } else if editor.pending_generation.is_some() {
        (
            MessageState::Updating,
            "checking a step · the accepted chain stays active".to_owned(),
        )
    } else if stages.is_empty() {
        (
            MessageState::Ready,
            "no steps yet · Add creates one".to_owned(),
        )
    } else {
        (
            MessageState::Applied,
            format!("{} steps active", stages.len()),
        )
    };
    if !editor.draft.trim().is_empty() {
        sentence.push_str(" · unsaved draft kept");
    }

    // §5.2: measure the natural body before choosing the popup height.
    let probe_width = class_l_width(area).saturating_sub(4).max(1);
    let action_rows = packed_button_rows(probe_width, &labels);
    let message_rows = message_rows(&sentence, probe_width);
    let steps_rows = 1 + stages.len().clamp(1, 8) as u16;
    let command_text = command.as_ref().map_or_else(
        || "Not configured".to_owned(),
        |stage| match &stage.definition.program {
            lvu_core::CommandProgram::Exec { executable, args } => {
                format!("{} · {} argument(s)", executable.display(), args.len())
            }
            lvu_core::CommandProgram::Shell { .. } => "Invalid saved command form".to_owned(),
        },
    );
    let help = "Later steps can use fields from earlier steps · the external command runs after all of them";
    let help_rows = Paragraph::new(help)
        .wrap(Wrap { trim: true })
        .line_count(probe_width)
        .clamp(1, 2) as u16;
    let natural_body = steps_rows + 1 + 2;
    let popup = class_l_popup(area, natural_body, message_rows, help_rows, action_rows);
    if popup.width < 20 || popup.height < 5 {
        return;
    }
    clear_themed(frame, popup, theme);
    render_dialog_frame(frame, popup, " Enrichment ".to_owned(), active, theme);
    let regions = dialog_regions(popup, message_rows, help_rows, action_rows);
    app.hit_regions.selection_modal = Some(regions.interior);
    app.hit_regions.enrichment_rows.clear();
    app.hit_regions.enrichment_controls.clear();
    if !active {
        return;
    }
    app.hit_regions.enrichment_step_controls.clear();

    // Body: the steps list, then the external-command summary.
    let body = regions.body;
    if body.height > 0 {
        // §5.4: the external-command pane keeps its heading and its one row
        // before the steps list is allowed to grow, and the gap goes first.
        let summary_rows = 2u16.min(body.height);
        let gap = u16::from(body.height > steps_rows.saturating_add(summary_rows));
        let list_height = body
            .height
            .saturating_sub(summary_rows.saturating_add(gap))
            .min(steps_rows)
            .max(1);
        let list_area = Rect::new(body.x, body.y, body.width, list_height);
        let count = (!stages.is_empty()).then(|| format!("{} of {}", selected + 1, stages.len()));
        let pane =
            render_pane_heading(frame, list_area, "Steps", count, stages.len().max(1), theme);
        let visible = usize::from(pane.viewport.height).max(1);
        let top = selected.saturating_sub(visible.saturating_sub(1));
        let mut rows = Vec::new();
        if stages.is_empty() {
            rows.push(Line::styled(
                "No steps yet · Add creates one",
                styles.description,
            ));
        } else {
            for (position, (index, stage)) in stages
                .iter()
                .enumerate()
                .skip(top)
                .take(visible)
                .enumerate()
            {
                let hit = Rect::new(
                    list_area.x,
                    pane.viewport.y + position as u16,
                    list_area.width,
                    1,
                );
                app.hit_regions.enrichment_rows.push((hit, index));
                let marker = if index == selected {
                    if app.ascii { "> " } else { "› " }
                } else {
                    "  "
                };
                let prefix = format!("{marker}{}  ", index + 1);
                rows.push(Line::styled(
                    format!(
                        "{prefix}{}",
                        step_summary(
                            &stage.source,
                            usize::from(pane.viewport.width)
                                .saturating_sub(UnicodeWidthStr::width(prefix.as_str())),
                        )
                    ),
                    if index == selected && focused == Control::Steps {
                        styles.selection
                    } else if index == selected {
                        styles.label.add_modifier(Modifier::BOLD)
                    } else {
                        styles.label
                    },
                ));
            }
        }
        frame.render_widget(Paragraph::new(rows), pane.viewport);
        if let Some(bar) = pane.scrollbar {
            render_scrollbar(
                frame,
                bar,
                top,
                stages.len().saturating_sub(visible),
                theme,
                app.ascii,
            );
        }
        app.hit_regions.dialog_scroll = None;

        let summary_y = list_area.bottom().saturating_add(gap);
        if summary_y < body.bottom() {
            let summary_area = Rect::new(
                body.x,
                summary_y,
                body.width,
                body.bottom().saturating_sub(summary_y),
            );
            let pane = render_pane_heading(frame, summary_area, "External command", None, 1, theme);
            frame.render_widget(
                Paragraph::new(command_text)
                    .wrap(Wrap { trim: true })
                    .style(styles.description),
                pane.viewport,
            );
        }
    }

    // Message, help and actions.
    render_message(
        frame,
        regions.message,
        message_state,
        &sentence,
        theme,
        app.ascii,
    );
    if regions.help.height > 0 {
        frame.render_widget(
            Paragraph::new(help)
                .wrap(Wrap { trim: true })
                .style(styles.description),
            regions.help,
        );
    }
    if regions.actions.height > 0 {
        let focused_index = controls.iter().position(|control| *control == focused);
        for (index, hit) in button_layout(regions.actions, &labels, focused_index) {
            app.hit_regions
                .enrichment_controls
                .push((hit, controls[index]));
            render_enrichment_button(
                frame,
                hit,
                labels[index],
                controls[index] == focused,
                index == 0,
                theme,
            );
        }
    }
    let _ = regions.content;
}

/// §6.3 button roles: primary accent, focused selection, others base.
fn render_enrichment_button(
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

/// Layer two: one step's expression, the record it reads and what it produces.
fn render_enrichment_step<P: RowProvider>(
    frame: &mut Frame<'_>,
    app: &mut App,
    provider: &P,
    area: Rect,
    theme: Theme,
) {
    use crate::app::EnrichmentStepControl as Control;

    let styles = DialogStyles::new(theme);
    let cursor = app.active_text_cursor();
    let Some(dialog) = app.enrichment_step.clone() else {
        return;
    };
    let Some(state) = app.view_state() else {
        return;
    };
    let editor = state.enrichment.clone();
    let editing_index = dialog
        .editing
        .as_ref()
        .and_then(|id| state.enrichments.iter().position(|stage| &stage.id == id));
    let accepted_source = editing_index
        .and_then(|index| state.enrichments.get(index))
        .map(|stage| stage.source.clone());
    let unsaved_draft = accepted_source.as_ref().map_or_else(
        || !editor.draft.trim().is_empty(),
        |source| *source != editor.draft,
    );

    // §10: the parent stays visible and scrimmed behind a non-compact child.
    let compact = dialog_compact(area);
    if !compact {
        render_enrichment_step_list(frame, app, area, theme, false);
    }
    scrim(frame, area, theme);

    let title = if editing_index.is_some() {
        " Enrichment › Edit step ".to_owned()
    } else {
        " Enrichment › New step ".to_owned()
    };
    let labels: Vec<&str> = if editing_index.is_some() {
        vec!["Save", "Remove"]
    } else {
        vec!["Save"]
    };
    let controls: Vec<Control> = if editing_index.is_some() {
        vec![Control::Save, Control::Remove]
    } else {
        vec![Control::Save]
    };

    let (message_state, sentence) = if let Some(error) = &editor.error {
        (
            MessageState::Error,
            format!("{error} · every accepted step is retained"),
        )
    } else if editor.pending_generation.is_some() {
        (
            MessageState::Updating,
            "checking this step · the accepted chain stays active".to_owned(),
        )
    } else if editing_index.is_some() {
        (
            MessageState::Ready,
            "saving replaces this accepted step".to_owned(),
        )
    } else {
        (
            MessageState::Ready,
            "saving appends this step after the accepted ones".to_owned(),
        )
    };

    // §5.2: measure the real content — the previewed record and its outputs —
    // before choosing a height, so the dialog never pads itself to a shape.
    let page = provider.page(
        &dialog.view_id,
        crate::provider::ViewportRequest {
            start: dialog.sample,
            len: 1,
        },
    );
    let total = page.total;
    let sample = page.rows.into_iter().next();
    let mut input_lines: Vec<String> = Vec::new();
    let mut output_lines: Vec<String> = Vec::new();
    match &sample {
        Some(row) => {
            input_lines.push(row.text.clone());
            if !row.fields.is_empty() {
                input_lines.push(format!(
                    "Fields  {}",
                    row.fields
                        .iter()
                        .map(|field| field.0.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            for (name, value) in row
                .details
                .iter()
                .filter(|(name, _)| name.starts_with("derived."))
            {
                output_lines.push(format!("{}  {value}", name.trim_start_matches("derived.")));
            }
            if output_lines.is_empty() {
                output_lines.push("No accepted outputs yet".to_owned());
            }
            if unsaved_draft {
                output_lines.push("Save this step to evaluate the draft above".to_owned());
            }
        }
        None => {
            input_lines.push("No record available to preview".to_owned());
            output_lines.push("No output to show".to_owned());
        }
    }

    let probe_width = class_l_width(area).saturating_sub(4).max(1);
    let action_rows = packed_button_rows(probe_width, &labels);
    let message_rows = message_rows(&sentence, probe_width);
    let side_by_side = probe_width >= 72;
    // The field grows with the wrapped draft up to its cap (§8.1); measure it
    // exactly the way the input renders it.
    let expression_rows = crate::text_edit::wrapped_text(
        &editor.draft,
        usize::from(probe_width.saturating_sub(12).max(1)),
    )
    .lines
    .len()
    .clamp(1, 3) as u16;
    let input_rows = 1 + input_lines.len().clamp(1, 4) as u16;
    let output_rows = 1 + output_lines.len().clamp(1, 4) as u16;
    let preview_rows = if side_by_side {
        input_rows.max(output_rows)
    } else {
        input_rows + output_rows
    };
    let help = "name = expression  or  /regex with (?P<name>…) groups/";
    let help_rows = Paragraph::new(help)
        .wrap(Wrap { trim: true })
        .line_count(probe_width)
        .clamp(1, 2) as u16;
    let natural_body = expression_rows + 1 + preview_rows;
    let mut popup = class_l_popup(area, natural_body, message_rows, help_rows, action_rows);
    if !compact {
        // §10: a child never covers its parent's frame completely.
        let parent = class_l_popup(area, 0, message_rows, help_rows, action_rows);
        let width = popup.width.min(parent.width.saturating_sub(4)).max(20);
        let height = popup.height.min(area.height.saturating_sub(2));
        popup = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );
    }
    if popup.width < 20 || popup.height < 5 {
        return;
    }
    clear_themed(frame, popup, theme);
    render_dialog_frame(frame, popup, title, true, theme);
    let regions = dialog_regions(popup, message_rows, help_rows, action_rows);
    app.hit_regions.selection_modal = Some(regions.interior);
    app.hit_regions.enrichment_rows.clear();
    app.hit_regions.enrichment_controls.clear();
    app.hit_regions.enrichment_step_controls.clear();
    app.hit_regions.dialog_scroll = None;

    let body = regions.body;
    if body.height == 0 || body.width == 0 {
        return;
    }

    // §4.2 two-column form row for the expression.
    let label_w = UnicodeWidthStr::width("Expression") as u16;
    let stacked = body.width < label_w + 2 + 20;
    let (label_rect, field_rect) = if stacked {
        (
            Rect::new(body.x, body.y, body.width, 1),
            Rect::new(
                body.x,
                body.y.saturating_add(1),
                body.width,
                body.height.saturating_sub(1).min(expression_rows).max(1),
            ),
        )
    } else {
        (
            Rect::new(body.x, body.y, label_w, 1),
            Rect::new(
                body.x.saturating_add(label_w).saturating_add(2),
                body.y,
                body.width.saturating_sub(label_w + 2),
                body.height.min(expression_rows).max(1),
            ),
        )
    };
    let focused_expression = dialog.control == Control::Expression;
    frame.render_widget(
        Paragraph::new("Expression").style(if focused_expression {
            styles.shortcut
        } else {
            styles.label
        }),
        label_rect,
    );
    if field_rect.width > 0 && field_rect.height > 0 {
        InputSurface {
            style: styles.input,
        }
        .render(field_rect, frame.buffer_mut());
        app.hit_regions
            .enrichment_step_controls
            .push((field_rect, Control::Expression));
        let wrapped = crate::text_edit::wrapped_text(&editor.draft, usize::from(field_rect.width));
        let (cursor_row, cursor_column) = cursor.map_or((0, 0), |cursor| {
            let mut logical = crate::text_edit::TextCursor { char_index: cursor };
            crate::text_edit::wrapped_cursor(
                &editor.draft,
                &mut logical,
                usize::from(field_rect.width),
            )
        });
        let top = cursor_row
            .saturating_add(1)
            .saturating_sub(usize::from(field_rect.height));
        let visible = wrapped.lines.get(top..).unwrap_or(&[]).join("\n");
        frame.render_widget(Paragraph::new(visible).style(styles.input), field_rect);
        if focused_expression && app.editor_completion.is_none() {
            let x = field_rect.x + cursor_column.min(usize::from(field_rect.width - 1)) as u16;
            let y = field_rect.y + cursor_row.saturating_sub(top) as u16;
            frame.buffer_mut()[(x, y)]
                .set_style(Style::default().bg(theme.cursor).fg(theme.input_fg));
            frame.set_cursor_position((x, y));
        }
    }

    // Input record and accepted output panes.
    let preview_y = field_rect.bottom().saturating_add(1);
    if preview_y >= body.bottom() {
        render_enrichment_step_tail(
            frame,
            app,
            &regions,
            message_state,
            sentence,
            &labels,
            &controls,
            dialog.control,
            help,
            theme,
        );
        return;
    }
    let preview = Rect::new(
        body.x,
        preview_y,
        body.width,
        body.bottom().saturating_sub(preview_y),
    );
    let (input_area, output_area) = if side_by_side && preview.width >= 72 {
        let split = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .spacing(2)
            .split(preview);
        (split[0], split[1])
    } else {
        let top = input_rows
            .min(preview.height.saturating_sub(2))
            .max(preview.height.min(2));
        (
            Rect::new(preview.x, preview.y, preview.width, top),
            Rect::new(
                preview.x,
                preview.y.saturating_add(top),
                preview.width,
                preview.height.saturating_sub(top),
            ),
        )
    };

    let input_pane = render_pane_heading(
        frame,
        input_area,
        "Input record",
        (total > 0).then(|| format!("{} of {total}", dialog.sample.saturating_add(1).min(total))),
        input_lines.len(),
        theme,
    );
    if !input_pane.viewport.is_empty() {
        app.hit_regions
            .enrichment_step_controls
            .push((input_area, Control::Input));
        let focused = dialog.control == Control::Input;
        frame.render_widget(
            Paragraph::new(
                input_lines
                    .iter()
                    .map(|line| {
                        Line::styled(
                            truncated(line, usize::from(input_pane.viewport.width)),
                            if focused {
                                styles.selection
                            } else {
                                styles.description
                            },
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
            input_pane.viewport,
        );
        // The heading count is the list affordance; a scrollbar only earns its
        // column when it can show arrows and a thumb.
        if let Some(bar) = input_pane.scrollbar.filter(|bar| bar.height >= 3) {
            render_scrollbar(
                frame,
                bar,
                dialog.sample,
                total.saturating_sub(1),
                theme,
                app.ascii,
            );
        }
    }

    let output_pane = render_pane_heading(
        frame,
        output_area,
        "Accepted output",
        None,
        output_lines.len(),
        theme,
    );
    if !output_pane.viewport.is_empty() {
        app.hit_regions
            .enrichment_step_controls
            .push((output_area, Control::Output));
        let visible = usize::from(output_pane.viewport.height);
        let limit = output_lines.len().saturating_sub(visible);
        app.dialog_scroll_limit = limit;
        app.dialog_scroll = app.dialog_scroll.min(limit);
        app.hit_regions.dialog_scroll = (limit > 0).then_some(output_area);
        frame.render_widget(
            Paragraph::new(
                output_lines
                    .iter()
                    .skip(app.dialog_scroll)
                    .map(|line| {
                        Line::styled(
                            truncated(line, usize::from(output_pane.viewport.width)),
                            styles.description,
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
            output_pane.viewport,
        );
        if let Some(bar) = output_pane.scrollbar {
            render_scrollbar(frame, bar, app.dialog_scroll, limit, theme, app.ascii);
        }
    }

    render_enrichment_step_tail(
        frame,
        app,
        &regions,
        message_state,
        sentence,
        &labels,
        &controls,
        dialog.control,
        help,
        theme,
    );
    render_editor_completion(frame, app, area, theme);
}

#[allow(clippy::too_many_arguments)]
fn render_enrichment_step_tail(
    frame: &mut Frame<'_>,
    app: &mut App,
    regions: &DialogRegions,
    message_state: MessageState,
    sentence: String,
    labels: &[&str],
    controls: &[crate::app::EnrichmentStepControl],
    focused: crate::app::EnrichmentStepControl,
    help: &str,
    theme: Theme,
) {
    let styles = DialogStyles::new(theme);
    render_message(
        frame,
        regions.message,
        message_state,
        &sentence,
        theme,
        app.ascii,
    );
    if regions.help.height > 0 {
        frame.render_widget(
            Paragraph::new(help.to_owned())
                .wrap(Wrap { trim: true })
                .style(styles.description),
            regions.help,
        );
    }
    if regions.actions.height > 0 {
        let focused_index = controls.iter().position(|control| *control == focused);
        for (index, hit) in button_layout(regions.actions, labels, focused_index) {
            app.hit_regions
                .enrichment_step_controls
                .push((hit, controls[index]));
            render_enrichment_button(
                frame,
                hit,
                labels[index],
                controls[index] == focused,
                index == 0,
                theme,
            );
        }
    }
}

fn render_editor_completion(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let Some(completion) = &app.editor_completion else {
        return;
    };
    let popup = centered(area, 76, 12);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
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
        return;
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
        app.hit_regions.editor_completion_rows.push((
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
}

fn render_help(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let popup = centered(area, 94, area.height.saturating_sub(2).min(30));
    clear_themed(frame, popup, theme);
    let agent = if app.ascii { "Agent" } else { "🧠" };
    frame.render_widget(
        Block::default()
            .title(" Help ")
            .borders(Borders::ALL)
            .style(Style::default().bg(theme.dialog_bg))
            .border_style(Style::default().fg(theme.accent)),
        popup,
    );
    let footer_actions = &[("↑/↓ or j/k", "scroll"), ("?", "close")];
    let footer_height = read_only_action_footer_height(popup, footer_actions, theme);
    let body = dialog_body_with_footer(popup, footer_height);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    let sections = help_sections(agent);
    let columns = if body.width >= 92 { 2 } else { 1 };
    let areas = if columns == 2 {
        Layout::horizontal([
            Constraint::Percentage(50),
            Constraint::Length(2),
            Constraint::Percentage(50),
        ])
        .split(body)
    } else {
        Layout::horizontal([Constraint::Percentage(100)]).split(body)
    };
    let mut paragraphs = Vec::new();
    if columns == 2 {
        paragraphs.push((help_lines(&sections[..3], theme), areas[0]));
        paragraphs.push((help_lines(&sections[3..], theme), areas[2]));
    } else {
        paragraphs.push((help_lines(&sections, theme), areas[0]));
    }
    let mut content_height = 0usize;
    for (lines, column) in &paragraphs {
        let paragraph = Paragraph::new(lines.clone()).wrap(Wrap { trim: false });
        content_height = content_height.max(paragraph.line_count(column.width));
    }
    app.help_scroll_limit = content_height.saturating_sub(usize::from(body.height));
    app.help_scroll = app.help_scroll.min(app.help_scroll_limit);
    for (lines, column) in paragraphs {
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((app.help_scroll.min(u16::MAX as usize) as u16, 0)),
            column,
        );
    }
    render_read_only_action_footer(frame, popup, footer_actions, theme);
}

struct HelpSection<'a> {
    title: &'a str,
    entries: Vec<(&'a str, String)>,
}

fn help_sections(agent: &str) -> Vec<HelpSection<'_>> {
    vec![
        HelpSection {
            title: "EVERYWHERE",
            entries: vec![
                ("Ctrl-P", "Open the command palette".into()),
                ("?", "Open or close this help".into()),
                ("Ctrl-L", "Redraw the terminal".into()),
                (",", "Open settings".into()),
                ("q / Ctrl-C", "Quit".into()),
            ],
        },
        HelpSection {
            title: "LOGS & VIEWS",
            entries: vec![
                ("g / G", "Jump to first / last record".into()),
                ("←/→ · 0", "Pan the selected event / reset pan".into()),
                ("[ / ]", "Previous or next view".into()),
                ("f", "Toggle follow / history".into()),
                ("d", "Toggle selected-record details".into()),
                ("o", "Open raw context".into()),
                ("b", "Toggle a bookmark".into()),
                ("B", "Open bookmarks and notes".into()),
                ("Alt-S", "Stop the selected source".into()),
                ("Alt-R", "Restart the selected source".into()),
            ],
        },
        HelpSection {
            title: "FILTER & SHAPE",
            entries: vec![
                ("/", "Literal or field-aware search".into()),
                ("p", "Open the advanced filter".into()),
                ("e", "Open the ordered enrichment steps".into()),
                (
                    "Alt-C in Enrichment",
                    "Add, edit, remove, or explicitly run the terminal command step".into(),
                ),
                ("m", "Open display-only grouping".into()),
                ("i", "Inspect fields; Space pins, c colors".into()),
                ("t", "Choose capture or event time window".into()),
                ("S", "Review derived storage usage".into()),
            ],
        },
        HelpSection {
            title: "SOURCES",
            entries: vec![
                ("n", "Add a source".into()),
                (
                    "Alt-F / Alt-C",
                    "Choose file / command in Add source".into(),
                ),
                (
                    "Ctrl-D",
                    "Discover sources; selection never auto-starts".into(),
                ),
                (
                    "Ctrl-A",
                    format!("Ask {agent} to draft a source for review"),
                ),
                ("v", "Open view actions".into()),
                ("Alt-M", "Edit source membership in View actions".into()),
            ],
        },
        HelpSection {
            title: "VIEWS & RECIPES",
            entries: vec![
                ("Alt-B", "Create a blank view".into()),
                ("Alt-D", "Clone the current view".into()),
                ("Alt-R", "Rename the current view".into()),
                ("r", "Browse named recipes".into()),
            ],
        },
        HelpSection {
            title: "ASSISTANCE",
            entries: vec![
                (
                    "A",
                    format!("Ask {agent} for a filter or enrichment proposal"),
                ),
                ("I", format!("Open a local {agent} investigation")),
                ("Alt-N", "Start a new investigation snapshot".into()),
            ],
        },
    ]
}

fn help_lines(sections: &[HelpSection<'_>], theme: Theme) -> Vec<Line<'static>> {
    let styles = DialogStyles::new(theme);
    let mut lines = Vec::new();
    let key_width = sections
        .iter()
        .flat_map(|section| section.entries.iter())
        .map(|(key, _)| UnicodeWidthStr::width(*key))
        .max()
        .unwrap_or(0);
    for (section_index, section) in sections.iter().enumerate() {
        if section_index > 0 {
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::styled(
            section.title.to_owned(),
            styles.label.add_modifier(Modifier::BOLD),
        )));
        for (key, description) in &section.entries {
            let padding = " ".repeat(key_width.saturating_sub(UnicodeWidthStr::width(*key)) + 2);
            lines.push(Line::from(vec![
                Span::styled(format!("  {key}"), styles.shortcut),
                Span::styled(padding, styles.description),
                Span::styled(description.clone(), styles.description),
            ]));
        }
    }
    lines
}

fn render_ask_ai(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let cursor = app.active_text_cursor();
    let popup = centered(area, 94, 24);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    app.hit_regions.ask_controls.clear();
    app.hit_regions.ask_kind_choices.clear();
    let Some(mut dialog) = app.ask_ai_dialog.clone() else {
        return;
    };
    let title = if app.ascii {
        " Ask Agent "
    } else {
        " Ask 🧠 "
    };
    frame.render_widget(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        popup,
    );
    let body = dialog_body(popup);
    let compact = body.width < 58;
    let short = body.height < 14;
    let rows = Layout::vertical([
        Constraint::Length(if compact { 2 } else { 1 }),
        Constraint::Length(if short {
            3
        } else if body.height >= 18 {
            6
        } else {
            4
        }),
        Constraint::Length(if short { 3 } else { 4 }),
        Constraint::Min(2),
    ])
    .split(body);
    let details_block = Block::default()
        .title(" Proposal and activity ")
        .borders(Borders::ALL);
    let details_inner = details_block.inner(rows[3]);
    let status_capacity = usize::from(rows[2].width.saturating_sub(2))
        .saturating_mul(usize::from(rows[2].height.saturating_sub(2)));
    let include_activity_detail =
        UnicodeWidthStr::width(dialog.progress.as_str()) > status_capacity;
    let details = Paragraph::new(ask_detail_lines(&dialog, include_activity_detail))
        .wrap(Wrap { trim: false });
    let limit = details
        .line_count(details_inner.width)
        .saturating_sub(usize::from(details_inner.height))
        .min(usize::from(u16::MAX)) as u16;
    dialog.review_scroll_limit = limit;
    dialog.review_scroll = dialog.review_scroll.min(limit);
    if dialog.focus == crate::app::AskControl::More && limit == 0 {
        dialog.focus = match dialog.stage {
            crate::app::AskAiStage::Proposal => crate::app::AskControl::Apply,
            crate::app::AskAiStage::Input | crate::app::AskAiStage::Error => {
                crate::app::AskControl::Submit
            }
            _ => crate::app::AskControl::Prompt,
        };
    }
    if let Some(state) = &mut app.ask_ai_dialog {
        state.review_scroll_limit = dialog.review_scroll_limit;
        state.review_scroll = dialog.review_scroll;
        state.focus = dialog.focus;
    }

    let kind_label = if dialog.recipe.is_some() {
        "Kind: Recipe adaptation".to_owned()
    } else {
        format!("Kind: {} ▾", ask_kind_label(dialog.kind))
    };
    let kind_width = crate::dialog_controls::button_width(&kind_label).min(rows[0].width);
    let kind_rect = Rect::new(rows[0].x, rows[0].y, kind_width, 1);
    if dialog.recipe.is_none() && dialog.stage == crate::app::AskAiStage::Input {
        app.hit_regions
            .ask_controls
            .push((kind_rect, crate::app::AskControl::Kind));
    }
    render_button(
        frame,
        kind_rect,
        &kind_label,
        dialog.focus == crate::app::AskControl::Kind,
        false,
        theme,
    );

    let action = match dialog.stage {
        crate::app::AskAiStage::Input | crate::app::AskAiStage::Error => {
            Some((crate::app::AskControl::Submit, "Submit"))
        }
        crate::app::AskAiStage::Proposal => Some((crate::app::AskControl::Apply, "Apply")),
        _ => None,
    };
    if let Some((control, text)) = action {
        let x = if compact {
            rows[0].x
        } else {
            kind_rect.right().saturating_add(1)
        };
        let y = rows[0].y + u16::from(compact);
        let rect = Rect::new(
            x,
            y,
            crate::dialog_controls::button_width(text).min(rows[0].right().saturating_sub(x)),
            1,
        );
        app.hit_regions.ask_controls.push((rect, control));
        render_button(frame, rect, text, dialog.focus == control, false, theme);
    }

    let editable = matches!(
        dialog.stage,
        crate::app::AskAiStage::Input | crate::app::AskAiStage::Error
    );
    let prompt_block = Block::default()
        .title(" Request ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(
            if dialog.focus == crate::app::AskControl::Prompt && editable {
                theme.focused_input_border
            } else {
                theme.border
            },
        ));
    let prompt_area = prompt_block.inner(rows[1]);
    frame.render_widget(prompt_block, rows[1]);
    let editing = dialog.focus == crate::app::AskControl::Prompt && editable;
    if editable {
        app.hit_regions
            .ask_controls
            .push((prompt_area, crate::app::AskControl::Prompt));
        InputSurface {
            style: Style::default().fg(theme.input_fg).bg(theme.input_bg),
        }
        .render(prompt_area, frame.buffer_mut());
    }
    let wrapped = crate::text_edit::wrapped_text(&dialog.prompt, usize::from(prompt_area.width));
    let (cursor_row, cursor_column) = cursor.map_or((0, 0), |cursor| {
        let mut logical = crate::text_edit::TextCursor { char_index: cursor };
        crate::text_edit::wrapped_cursor(
            &dialog.prompt,
            &mut logical,
            usize::from(prompt_area.width),
        )
    });
    let prompt_top = cursor_row
        .saturating_add(1)
        .saturating_sub(usize::from(prompt_area.height));
    frame.render_widget(
        Paragraph::new(wrapped.lines.get(prompt_top..).unwrap_or(&[]).join("\n")).style(
            if editing {
                Style::default().fg(theme.input_fg).bg(theme.input_bg)
            } else {
                Style::default().fg(theme.base_fg)
            },
        ),
        prompt_area,
    );
    if editing && prompt_area.width > 0 && prompt_area.height > 0 {
        let x = prompt_area.x + cursor_column.min(usize::from(prompt_area.width - 1)) as u16;
        let y = prompt_area.y + cursor_row.saturating_sub(prompt_top) as u16;
        frame.buffer_mut()[(x, y)].set_style(Style::default().bg(theme.cursor).fg(theme.input_fg));
        frame.set_cursor_position((x, y));
    }

    let (status_label, status_style) = ask_status(dialog.stage, theme);
    frame.render_widget(
        Paragraph::new(format!("{status_label}: {}", dialog.progress))
            .wrap(Wrap { trim: false })
            .style(status_style)
            .block(
                Block::default()
                    .title(" State ")
                    .borders(Borders::ALL)
                    .border_style(status_style),
            ),
        rows[2],
    );

    if limit > 0 {
        let text = "More";
        let width = crate::dialog_controls::button_width(text);
        let rect = Rect::new(
            rows[0].right().saturating_sub(width),
            rows[0].y,
            width.min(rows[0].width),
            1,
        );
        app.hit_regions
            .ask_controls
            .push((rect, crate::app::AskControl::More));
        render_button(
            frame,
            rect,
            text,
            dialog.focus == crate::app::AskControl::More,
            false,
            theme,
        );
    }
    app.hit_regions.dialog_scroll = Some(details_inner);
    frame.render_widget(
        details
            .scroll((dialog.review_scroll, 0))
            .style(Style::default().fg(theme.base_fg))
            .block(details_block.border_style(Style::default().fg(
                if dialog.focus == crate::app::AskControl::More {
                    theme.focused_input_border
                } else {
                    theme.border
                },
            ))),
        rows[3],
    );
    if dialog.kind_dropdown {
        render_ask_kind_dropdown(frame, app, popup, kind_rect, dialog.kind_selected, theme);
    }
}

fn ask_kind_label(kind: crate::app::AskAiKind) -> &'static str {
    match kind {
        crate::app::AskAiKind::Filter => "Filter",
        crate::app::AskAiKind::Enrichment => "Enrichment",
        crate::app::AskAiKind::Recipe => "Recipe adaptation",
    }
}

fn ask_status(stage: crate::app::AskAiStage, theme: Theme) -> (&'static str, Style) {
    let styles = DialogStyles::new(theme);
    match stage {
        crate::app::AskAiStage::Input => ("Ready", styles.applied),
        crate::app::AskAiStage::Error => ("Error", styles.error),
        crate::app::AskAiStage::Proposal => ("Proposal", styles.applied),
        crate::app::AskAiStage::Snapshot
        | crate::app::AskAiStage::StartingSession
        | crate::app::AskAiStage::Proposing => ("Pending", styles.pending),
    }
}

fn ask_detail_lines(
    dialog: &crate::app::AskAiDialogState,
    include_activity_detail: bool,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if include_activity_detail {
        lines.push(Line::raw(format!("Activity detail: {}", dialog.progress)));
    }
    lines.push(Line::raw(format!(
        "Agent: {} · mode {} · thinking {}",
        dialog.provider, dialog.mode, dialog.thinking
    )));
    if !matches!(
        dialog.stage,
        crate::app::AskAiStage::Input | crate::app::AskAiStage::Error
    ) {
        lines.push(Line::raw(format!("Submitted request: {}", dialog.prompt)));
    }
    if let Some(expression) = &dialog.expression {
        lines.push(Line::raw(format!("Proposal: {expression}")));
    }
    if dialog.kind == crate::app::AskAiKind::Recipe
        && dialog.stage == crate::app::AskAiStage::Proposal
        && let Some(recipe) = &dialog.recipe
    {
        lines.push(Line::raw(format!(
            "Ordered enrichments ({} steps):",
            recipe.enrichments.len()
        )));
        for (index, stage) in recipe.enrichments.iter().enumerate() {
            lines.push(Line::raw(format!(
                "{}. [{}] {}",
                index + 1,
                stage.id.0,
                stage.source
            )));
        }
        if recipe.enrichments.is_empty() && !recipe.enrichment.is_empty() {
            lines.push(Line::raw(recipe.enrichment.clone()));
        }
    }
    if let Some(explanation) = &dialog.explanation {
        lines.push(Line::raw(format!("Explanation: {explanation}")));
    }
    if let Some(session) = &dialog.session_id {
        lines.push(Line::raw(format!("Session: {session}")));
    }
    if let Some(directory) = &dialog.snapshot_dir {
        lines.push(Line::raw(format!("Snapshot: {directory}")));
    }
    if dialog.kind == crate::app::AskAiKind::Recipe {
        lines.push(Line::raw(
            "Scope: advanced filter and ordered enrichments may change; search, pins, colors, time and grouping are retained.",
        ));
    }
    lines
}

fn render_ask_kind_dropdown(
    frame: &mut Frame<'_>,
    app: &mut App,
    popup: Rect,
    anchor: Rect,
    selected: usize,
    theme: Theme,
) {
    let width = 16_u16.min(popup.width.saturating_sub(2));
    let height = 4_u16.min(popup.height.saturating_sub(2));
    let area = Rect::new(
        anchor.x.min(popup.right().saturating_sub(width + 1)),
        anchor
            .bottom()
            .min(popup.bottom().saturating_sub(height + 1)),
        width,
        height,
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        area,
    );
    for (index, kind) in [
        crate::app::AskAiKind::Filter,
        crate::app::AskAiKind::Enrichment,
    ]
    .into_iter()
    .take(usize::from(area.height.saturating_sub(2)))
    .enumerate()
    {
        let rect = Rect::new(
            area.x.saturating_add(1),
            area.y.saturating_add(1 + index as u16),
            area.width.saturating_sub(2),
            1,
        );
        app.hit_regions.ask_kind_choices.push((rect, index));
        let styles = DialogStyles::new(theme);
        frame.render_widget(
            Paragraph::new(ask_kind_label(kind)).style(if index == selected {
                styles.selection
            } else {
                styles.description
            }),
            rect,
        );
    }
}

fn render_investigation(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let cursor = app.active_text_cursor();
    let popup = centered(area, 94, 24);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    app.hit_regions.investigation_controls.clear();
    let Some(mut dialog) = app.investigation_dialog.clone() else {
        return;
    };
    frame.render_widget(
        Block::default()
            .title(if app.ascii {
                " Investigation Agent "
            } else {
                " Investigation 🧠 "
            })
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        popup,
    );
    let body = dialog_body(popup);
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(body.height.clamp(2, 5)),
        Constraint::Length(3),
        Constraint::Min(2),
    ])
    .split(body);
    let editable = matches!(
        dialog.stage,
        crate::app::InvestigationStage::Input
            | crate::app::InvestigationStage::Conversation
            | crate::app::InvestigationStage::Error
    );
    let mut detail_lines = Vec::new();
    if let Some(session) = &dialog.session_id {
        detail_lines.push(Line::raw(format!("Session: {session}")));
    }
    if let Some(snapshot) = &dialog.snapshot_dir {
        detail_lines.push(Line::raw(format!("Snapshot: {snapshot}")));
    }
    if !dialog.items.is_empty() {
        detail_lines.push(Line::raw("Saved investigations:"));
        for (index, item) in dialog.items.iter().enumerate() {
            let marker = if index == dialog.selected { ">" } else { " " };
            detail_lines.push(Line::raw(format!(
                "{marker} {} — {}",
                item.session_id, item.question
            )));
        }
    }
    if !dialog.messages.is_empty() {
        detail_lines.push(Line::raw("Conversation:"));
        for message in &dialog.messages {
            detail_lines.push(Line::raw(message.clone()));
        }
    }
    let detail_block = Block::default()
        .title(" Activity and saved investigations ")
        .borders(Borders::ALL);
    let detail_inner = detail_block.inner(rows[3]);
    let details = Paragraph::new(detail_lines).wrap(Wrap { trim: false });
    let limit = details
        .line_count(detail_inner.width)
        .saturating_sub(usize::from(detail_inner.height))
        .min(u16::MAX as usize) as u16;
    dialog.review_scroll_limit = limit;
    dialog.review_scroll = dialog.review_scroll.min(limit);
    if dialog.focus == crate::app::InvestigationControl::More && limit == 0 {
        dialog.focus = if editable {
            crate::app::InvestigationControl::Submit
        } else {
            crate::app::InvestigationControl::Prompt
        };
    }
    if let Some(state) = &mut app.investigation_dialog {
        state.focus = dialog.focus;
        state.review_scroll = dialog.review_scroll;
        state.review_scroll_limit = limit;
    }
    use crate::app::InvestigationControl as Control;
    let submit = if dialog.stage == crate::app::InvestigationStage::Conversation {
        "Send"
    } else if dialog.input.trim().is_empty() && !dialog.items.is_empty() {
        "Resume"
    } else {
        "Start"
    };
    let mut controls = Vec::new();
    if dialog.stage == crate::app::InvestigationStage::Input && !dialog.items.is_empty() {
        controls.push((Control::Saved, "Saved"));
    }
    if editable {
        controls.push((Control::Submit, submit));
    }
    if dialog.investigation_id.is_some() || dialog.session_id.is_some() {
        controls.push((Control::New, "New"));
    }
    if limit > 0 {
        controls.push((Control::More, "More"));
    }
    let labels: Vec<_> = controls.iter().map(|(_, label)| *label).collect();
    let focused = controls
        .iter()
        .position(|(control, _)| *control == dialog.focus);
    for (index, rect) in button_layout(rows[0], &labels, focused) {
        let (control, label) = controls[index];
        app.hit_regions.investigation_controls.push((rect, control));
        render_button(frame, rect, label, dialog.focus == control, false, theme);
    }
    let prompt_block = Block::default()
        .title(" Question or follow-up ")
        .borders(Borders::ALL);
    let prompt = prompt_block.inner(rows[1]);
    frame.render_widget(prompt_block, rows[1]);
    if editable {
        app.hit_regions
            .investigation_controls
            .push((prompt, Control::Prompt));
        InputSurface {
            style: DialogStyles::new(theme).input,
        }
        .render(prompt, frame.buffer_mut());
    }
    let editing = editable && dialog.focus == Control::Prompt;
    let wrapped = crate::text_edit::wrapped_text(&dialog.input, usize::from(prompt.width));
    let (cursor_row, cursor_col) = cursor.map_or((0, 0), |cursor| {
        let mut value = crate::text_edit::TextCursor { char_index: cursor };
        crate::text_edit::wrapped_cursor(&dialog.input, &mut value, usize::from(prompt.width))
    });
    let top = cursor_row
        .saturating_add(1)
        .saturating_sub(usize::from(prompt.height));
    frame.render_widget(
        Paragraph::new(wrapped.lines.get(top..).unwrap_or(&[]).join("\n")).style(if editable {
            DialogStyles::new(theme).input
        } else {
            DialogStyles::new(theme).description
        }),
        prompt,
    );
    if editing && prompt.width > 0 && prompt.height > 0 {
        let x = prompt.x + cursor_col.min(usize::from(prompt.width - 1)) as u16;
        let y = prompt.y
            + cursor_row
                .saturating_sub(top)
                .min(usize::from(prompt.height - 1)) as u16;
        frame.buffer_mut()[(x, y)].set_style(Style::default().bg(theme.cursor).fg(theme.input_fg));
        frame.set_cursor_position((x, y));
    }
    let styles = DialogStyles::new(theme);
    let (label, style) = match dialog.stage {
        crate::app::InvestigationStage::Input | crate::app::InvestigationStage::Conversation => {
            ("Ready", styles.applied)
        }
        crate::app::InvestigationStage::Error => ("Error", styles.error),
        _ => ("Pending", styles.pending),
    };
    frame.render_widget(
        Paragraph::new(format!("{label}: {}", dialog.progress))
            .wrap(Wrap { trim: false })
            .style(style)
            .block(
                Block::default()
                    .title(" State ")
                    .borders(Borders::ALL)
                    .border_style(style),
            ),
        rows[2],
    );
    app.hit_regions.dialog_scroll = Some(detail_inner);
    frame.render_widget(
        details
            .scroll((dialog.review_scroll, 0))
            .style(styles.description)
            .block(detail_block.border_style(if dialog.focus == Control::More {
                styles.selection
            } else {
                styles.description
            })),
        rows[3],
    );
}

fn render_view_dialog(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    use crate::dialog_layout::{DialogClass, DialogContent, content_width, pane};

    let styles = DialogStyles::new(theme);
    let cursor = app.active_text_cursor();
    let ascii = app.ascii;
    let Some(dialog) = app.view_dialog.clone() else {
        return;
    };
    app.hit_regions.view_source_rows.clear();
    app.hit_regions.view_dialog_controls.clear();

    let sources_mode = dialog.mode == crate::app::ViewDialogMode::Sources;
    let controls = view_dialog_button_controls(dialog.mode);
    let labels: Vec<&str> = controls.iter().map(|(_, label)| *label).collect();
    let width = content_width(area, DialogClass::M);

    let (state, sentence) = match dialog.error.as_deref() {
        Some(error) => (MessageState::Error, error.to_owned()),
        None if sources_mode => (
            MessageState::Ready,
            "changing membership keeps every capture".to_owned(),
        ),
        None => (
            MessageState::Ready,
            "creating, cloning and renaming keep the capture".to_owned(),
        ),
    };
    let help = if sources_mode && app.sources.len() > 1 {
        "Sources are ordered by position, then by record sequence, not by clock time."
    } else {
        ""
    };

    // §5.2: the body asks for exactly the rows its content needs. The Sources
    // list is a pane (heading + one row per source) capped at 12.
    let body_rows = if sources_mode {
        1 + u16::try_from(app.sources.len().clamp(1, 12)).unwrap_or(1)
    } else {
        1
    };
    let content = DialogContent {
        header: 0,
        body: body_rows,
        message: message_rows(&sentence, width),
        help: help_rows(help, width),
        actions: packed_button_rows(width, &labels),
    };

    let view_name = app
        .active_view_id()
        .and_then(|id| app.views.iter().find(|view| view.id == id))
        .map(|view| view.name.clone());
    let title = match view_name {
        Some(name) => format!("View · {name}"),
        None => "View".to_owned(),
    };
    let regions = dialog_frame(frame, app, area, DialogClass::M, &title, &content, theme);
    if regions.content.width == 0 {
        return;
    }

    if sources_mode {
        // §8.5/§8.7: a list is a pane — heading with a count, indented rows,
        // and a scrollbar only when the rows do not fit.
        let total = app.sources.len();
        let rects = pane(regions.body, 12, total);
        frame.render_widget(
            Paragraph::new("Sources").style(styles.label.add_modifier(Modifier::BOLD)),
            rects.heading,
        );
        if rects.count.width > 0 {
            frame.render_widget(
                Paragraph::new(Line::from(format!(
                    "{} of {total}",
                    dialog.selected_source.saturating_add(1).min(total.max(1))
                )))
                .style(styles.description)
                .right_aligned(),
                rects.count,
            );
        }
        let visible = usize::from(rects.viewport.height);
        // §9: the viewport windows on the selection so the cursor is always
        // drawn, and the same window feeds the row hitboxes below.
        let first = dialog
            .selected_source
            .saturating_sub(visible.saturating_sub(1))
            .min(total.saturating_sub(visible.min(total)));
        for (offset, (index, source)) in app
            .sources
            .iter()
            .enumerate()
            .skip(first)
            .take(visible)
            .enumerate()
        {
            let order = dialog.source_ids.iter().position(|id| id == &source.id);
            let selected = index == dialog.selected_source;
            let checkbox = if order.is_some() { "[x]" } else { "[ ]" };
            let marker = if selected {
                if ascii { "> " } else { "› " }
            } else {
                "  "
            };
            let position = order
                .map(|value| format!("{:>2} ", value + 1))
                .unwrap_or_else(|| "   ".to_owned());
            let text = format!("{marker}{checkbox} {position}{}", source.name);
            let row = Rect::new(
                rects.viewport.x,
                rects.viewport.y.saturating_add(offset as u16),
                rects.viewport.width,
                1,
            );
            frame.render_widget(
                Paragraph::new(clipped_width(&text, usize::from(row.width))).style(if selected {
                    styles.selection
                } else {
                    styles.description
                }),
                row,
            );
            app.hit_regions.view_source_rows.push((row, index));
        }
        if let Some(bar) = rects.scrollbar {
            render_scrollbar(
                frame,
                bar,
                first,
                total.saturating_sub(visible),
                theme,
                ascii,
            );
        }
    } else {
        // §4.2: one labelled row. The field rect is exactly what gets painted,
        // and the caret is placed inside it.
        let label_width = u16::try_from(UnicodeWidthStr::width("Name")).unwrap_or(4);
        let field_x = regions
            .content
            .x
            .saturating_add(label_width)
            .saturating_add(FIELD_GUTTER);
        let row = Rect::new(regions.content.x, regions.body.y, regions.content.width, 1);
        frame.render_widget(Paragraph::new("Name").style(styles.label), row);
        let field = Rect::new(
            field_x.min(regions.content.right()),
            row.y,
            regions.content.right().saturating_sub(field_x),
            1,
        );
        if field.width > 0 {
            place_input_cursor_at(
                frame,
                field,
                0,
                0,
                &dialog.draft,
                cursor.unwrap_or_else(|| dialog.draft.chars().count()),
                theme,
            );
        }
    }

    render_message(frame, regions.message, state, &sentence, theme, ascii);
    render_help_text(frame, regions.help, help, theme);

    // §3: actions live in their own rect, so they can no longer be drawn into
    // the help text the way the old fixed-offset button block was.
    let focused = controls
        .iter()
        .position(|(control, _)| *control == dialog.control);
    for (index, rect) in render_action_row(frame, regions.actions, &labels, focused, &[], theme) {
        let (control, label) = controls[index];
        let selected =
            matches!(control, crate::app::ViewDialogControl::Mode(value) if value == dialog.mode);
        if selected && focused != Some(index) {
            frame.render_widget(
                Paragraph::new(button_text(label)).style(
                    styles
                        .applied
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                ),
                rect,
            );
        }
        app.hit_regions.view_dialog_controls.push((rect, control));
    }
}

fn view_dialog_button_controls(
    mode: crate::app::ViewDialogMode,
) -> Vec<(crate::app::ViewDialogControl, &'static str)> {
    let mut controls = vec![
        (
            crate::app::ViewDialogControl::Mode(crate::app::ViewDialogMode::Blank),
            "New blank",
        ),
        (
            crate::app::ViewDialogControl::Mode(crate::app::ViewDialogMode::Clone),
            "Clone",
        ),
        (
            crate::app::ViewDialogControl::Mode(crate::app::ViewDialogMode::Rename),
            "Rename",
        ),
        (
            crate::app::ViewDialogControl::Mode(crate::app::ViewDialogMode::Sources),
            "Sources",
        ),
    ];
    controls.push((
        crate::app::ViewDialogControl::Apply,
        if mode == crate::app::ViewDialogMode::Sources {
            "Apply membership"
        } else {
            "Apply"
        },
    ));
    controls
}

fn render_source_dialog(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let cursor = app.active_text_cursor();
    app.hit_regions.path_completion_rows.clear();
    app.hit_regions.dialog_scroll = None;
    let popup = centered(area, 104, 24);
    clear_themed(frame, popup, theme);
    app.hit_regions.source_controls.clear();
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    let Some(dialog) = &app.source_dialog else {
        return;
    };
    if dialog.mode == crate::app::SourceDialogMode::Ai {
        let ai = &dialog.ai;
        let content = source_content_popup(popup);
        let body = dialog_body_with_footer(content, 3);
        // A short terminal cannot afford decoration around the review the user
        // must read before an irreversible launch. Keep the labelled status and
        // its semantic color, but spend the border and input help on preview
        // lines instead.
        let compact = body.height < 14;
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(if compact { 1 } else { 3 }),
            Constraint::Min(2),
            Constraint::Length(u16::from(!compact)),
        ])
        .split(body);
        let styles = DialogStyles::new(theme);
        frame.render_widget(
            Block::default()
                .title(if app.ascii {
                    " Ask Agent for a source — preview never executes "
                } else {
                    " Ask 🧠 for a source — preview never executes "
                })
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.accent)),
            content,
        );
        frame.render_widget(Paragraph::new("Request").style(styles.label), rows[0]);
        InputSurface {
            style: styles.input,
        }
        .render(rows[1], frame.buffer_mut());
        frame.render_widget(
            Paragraph::new(input_tail(&ai.instruction, usize::from(rows[1].width)))
                .style(styles.input),
            rows[1],
        );
        let (state_label, state_style) = source_ai_status(ai.stage, theme);
        let state = Paragraph::new(format!("{state_label}: {}", ai.progress))
            .wrap(Wrap { trim: false })
            .style(state_style);
        frame.render_widget(
            if compact {
                state
            } else {
                state.block(
                    Block::default()
                        .title(" State ")
                        .borders(Borders::ALL)
                        .border_style(state_style),
                )
            },
            rows[2],
        );
        let mut lines = Vec::new();
        if let Some(preview) = &ai.preview {
            let mut review = vec![
                format!("Name: {}", preview.name),
                format!("Kind: {}", preview.kind),
                format!("Launch: {}", preview.launch),
                format!("Effective path/cwd: {}", preview.effective_path_or_cwd),
                format!("Restart: {}", preview.restart),
            ];
            review.extend(
                preview
                    .environment
                    .iter()
                    .map(|value| format!("Env: {value}")),
            );
            if preview.environment.is_empty() {
                review.push("Env: (none)".into());
            }
            review.push(format!("Why: {}", preview.explanation));
            lines.extend(review);
        }
        if let Some(session) = &ai.session_id {
            lines.push(format!("Local session: {session}"));
        }
        let preview_block = Block::default().borders(Borders::ALL);
        let preview_inner = preview_block.inner(rows[3]);
        let preview = Paragraph::new(lines.join("\n"))
            .wrap(Wrap { trim: false })
            .style(styles.description);
        let preview_scroll_limit = preview
            .line_count(preview_inner.width)
            .saturating_sub(usize::from(preview_inner.height));
        let preview_scroll = ai.preview_scroll.min(preview_scroll_limit);
        let preview_title = if preview_scroll_limit > 0 {
            format!(
                " Preview · lines {}–{} of {} · ↑/↓ ",
                preview_scroll.saturating_add(1),
                preview_scroll
                    .saturating_add(usize::from(preview_inner.height))
                    .min(preview_scroll_limit.saturating_add(usize::from(preview_inner.height))),
                preview_scroll_limit.saturating_add(usize::from(preview_inner.height))
            )
        } else {
            " Preview ".into()
        };
        frame.render_widget(
            preview
                .scroll((preview_scroll.min(u16::MAX as usize) as u16, 0))
                .block(
                    preview_block
                        .title(preview_title)
                        .border_style(Style::default().fg(if app.dialog_scroll_focused {
                            theme.focused_input_border
                        } else {
                            theme.border
                        })),
                ),
            rows[3],
        );
        app.hit_regions.dialog_scroll = (preview_scroll_limit > 0).then_some(rows[3]);
        if !compact {
            frame.render_widget(
                Paragraph::new("Describe a source; review is required before capture starts.")
                    .style(styles.description),
                rows[4],
            );
        }
        if matches!(
            ai.stage,
            crate::app::SourceAiStage::Input | crate::app::SourceAiStage::Error
        ) && !dialog.controls_focused
        {
            place_input_cursor_at(
                frame,
                rows[1],
                0,
                0,
                &ai.instruction,
                cursor.unwrap_or_else(|| ai.instruction.chars().count()),
                theme,
            );
        }
        render_source_controls(frame, app, popup, theme);
        if let Some(dialog) = &mut app.source_dialog {
            dialog.ai.preview_scroll_limit = preview_scroll_limit;
            dialog.ai.preview_scroll = preview_scroll;
        }
        return;
    }
    if dialog.mode == crate::app::SourceDialogMode::Discovery {
        app.hit_regions.discovery_rows.clear();
        let indices = crate::app::filtered_discovery_indices(&dialog.discovery);
        let block = Block::default()
            .title(" Discover sources — selection never auto-starts ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent));
        let inner = dialog_body_with_footer(source_content_popup(popup), 3);
        frame.render_widget(block, popup);
        let search_rows = inner.height.min(2);
        let search = Rect::new(inner.x, inner.y, inner.width, search_rows.min(1));
        let search_help = Rect::new(
            inner.x,
            search.bottom(),
            inner.width,
            search_rows.saturating_sub(1),
        );
        let status_text = dialog.error.as_ref().map_or_else(
            || {
                format!(
                    "{}: {}",
                    if dialog.discovery.scanning {
                        "UPDATING"
                    } else {
                        "SCAN SUMMARY"
                    },
                    dialog.discovery.status
                )
            },
            |error| format!("ERROR: {error}\nSCAN SUMMARY: {}", dialog.discovery.status),
        );
        let diagnostic_height = if inner.height >= 5 {
            inner.height.saturating_sub(search_rows + 1).min(5)
        } else {
            0
        };
        let list_height = inner.height.saturating_sub(search_rows + diagnostic_height);
        let list_area = Rect::new(inner.x, search_help.bottom(), inner.width, list_height);
        let diagnostic_area =
            Rect::new(inner.x, list_area.bottom(), inner.width, diagnostic_height);
        let search_label = Rect::new(search.x, search.y, search.width.min(7), search.height);
        let search_input = Rect::new(
            search_label.right(),
            search.y,
            search.width.saturating_sub(search_label.width),
            search.height,
        );
        frame.render_widget(
            Paragraph::new("Search").style(DialogStyles::new(theme).label),
            search_label,
        );
        InputSurface {
            style: DialogStyles::new(theme).input,
        }
        .render(search_input, frame.buffer_mut());
        frame.render_widget(
            Paragraph::new(clipped_width(
                &dialog.discovery.query,
                usize::from(search_input.width),
            ))
            .style(DialogStyles::new(theme).input),
            search_input,
        );
        frame.render_widget(
            Paragraph::new(format!(
                "{}/{} matches · selection never starts capture",
                indices.len(),
                dialog.discovery.items.len()
            ))
            .style(DialogStyles::new(theme).description),
            search_help,
        );
        if !dialog.controls_focused {
            place_input_cursor_at(
                frame,
                search_input,
                0,
                0,
                &dialog.discovery.query,
                cursor.unwrap_or_else(|| dialog.discovery.query.chars().count()),
                theme,
            );
        }
        let visible = usize::from(list_height);
        let selected = dialog
            .discovery
            .selected
            .min(indices.len().saturating_sub(1));
        let top = selected.saturating_sub(visible.saturating_sub(1));
        let mut rows = Vec::new();
        for (position, index) in indices.iter().skip(top).take(visible).enumerate() {
            let item = &dialog.discovery.items[*index];
            let marker = if top + position == selected { ">" } else { " " };
            let text = clipped_width(
                &format!("{marker} {} [{}]", item.label, item.status),
                usize::from(list_area.width),
            );
            rows.push(ListItem::new(text).style(if top + position == selected {
                Style::default()
                    .fg(theme.selection_fg)
                    .bg(theme.selection_bg)
            } else {
                Style::default().fg(theme.base_fg).bg(theme.base_bg)
            }));
            app.hit_regions.discovery_rows.push((
                Rect::new(
                    list_area.x,
                    list_area.y.saturating_add(position as u16),
                    list_area.width,
                    1,
                ),
                top + position,
            ));
        }
        if indices.is_empty() {
            rows.push(ListItem::new("  No matching candidates."));
        }
        frame.render_widget(List::new(rows), list_area);
        let detail = indices
            .get(selected)
            .and_then(|index| dialog.discovery.items.get(*index))
            .map_or_else(
                || "No candidate selected.".to_owned(),
                |item| format!("Selected: {}\nEvidence/path: {}", item.label, item.detail),
            );
        let status = Paragraph::new(format!("{detail}\n{status_text}"))
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(theme.base_fg).bg(theme.base_bg));
        let diagnostic_block = Block::default()
            .title(" Diagnostics ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if app.dialog_scroll_focused {
                theme.focused_input_border
            } else {
                theme.border
            }));
        let diagnostic_inner = diagnostic_block.inner(diagnostic_area);
        let status_scroll_limit = status
            .line_count(diagnostic_inner.width)
            .saturating_sub(usize::from(diagnostic_inner.height));
        let status_scroll = dialog.discovery.status_scroll.min(status_scroll_limit);
        app.hit_regions.dialog_scroll = Some(diagnostic_area);
        frame.render_widget(
            status
                .scroll((status_scroll.min(u16::MAX as usize) as u16, 0))
                .block(diagnostic_block),
            diagnostic_area,
        );
        render_source_controls(frame, app, popup, theme);
        if let Some(dialog) = &mut app.source_dialog {
            dialog.discovery.status_scroll_limit = status_scroll_limit;
            dialog.discovery.status_scroll = status_scroll;
        }
        return;
    }
    let kind = match dialog.kind {
        crate::app::SourceKind::File => "FILE PATH",
        crate::app::SourceKind::Command => "COMMAND (sh -c)",
    };
    let message = dialog
        .error
        .as_deref()
        .unwrap_or("Ready: provide a file path or command. Capture starts only after submission.");
    let empty = if app.views.is_empty() {
        "No view selected — add or discover a source.\n"
    } else {
        ""
    };
    let input_row = if app.views.is_empty() { 3usize } else { 2usize };
    let content_popup = source_content_popup(popup);
    let body = dialog_body_with_footer(content_popup, 3);
    let input_area = Rect::new(
        body.x,
        body.y.saturating_add(input_row as u16),
        body.width,
        u16::from(usize::from(body.height) > input_row),
    );
    let status_area = Rect::new(
        body.x,
        body.bottom().saturating_sub(3),
        body.width,
        body.height.min(3),
    );
    let details_y = input_area.bottom().saturating_add(1);
    let details_area = Rect::new(
        body.x,
        details_y,
        body.width,
        status_area.y.saturating_sub(details_y),
    );
    let mut details = String::new();
    let mut completion_top = 0;
    let mut completion_count = 0;
    if dialog.kind == crate::app::SourceKind::Command {
        details.push_str("Command completion is disabled; command cwd is app cwd.");
    } else if dialog.path_completion.scanning {
        details.push_str("Completing path…");
    } else if !dialog.path_completion.candidates.is_empty() {
        details.push_str("Path matches:");
        let available = usize::from(details_area.height.saturating_sub(1)).max(1);
        let selected = dialog
            .path_completion
            .selected
            .min(dialog.path_completion.candidates.len().saturating_sub(1));
        let top = selected.saturating_sub(available.saturating_sub(1));
        completion_top = top;
        for (position, candidate) in dialog
            .path_completion
            .candidates
            .iter()
            .skip(top)
            .take(available)
            .enumerate()
        {
            let marker = if top + position == selected { ">" } else { " " };
            details.push_str(&format!("\n{marker} {candidate}"));
            completion_count += 1;
        }
    }
    frame.render_widget(
        Block::default()
            .title(" Add source ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        content_popup,
    );
    frame.render_widget(
        Paragraph::new(format!("{empty}{kind}")).style(DialogStyles::new(theme).label),
        Rect::new(body.x, body.y, body.width, input_row as u16),
    );
    InputSurface {
        style: DialogStyles::new(theme).input,
    }
    .render(input_area, frame.buffer_mut());
    frame.render_widget(
        Paragraph::new(input_tail(&dialog.draft, usize::from(input_area.width)))
            .style(DialogStyles::new(theme).input),
        input_area,
    );
    for position in 0..completion_count.min(usize::from(details_area.height.saturating_sub(1))) {
        app.hit_regions.path_completion_rows.push((
            Rect::new(
                details_area.x,
                details_area.y.saturating_add(1 + position as u16),
                details_area.width,
                1,
            ),
            completion_top + position,
        ));
    }
    let styles = DialogStyles::new(theme);
    frame.render_widget(
        Paragraph::new(details)
            .wrap(Wrap { trim: false })
            .style(styles.description),
        details_area,
    );
    let status_style = if dialog.error.is_some() {
        styles.error
    } else if dialog.path_completion.scanning {
        styles.pending
    } else {
        styles.applied
    };
    frame.render_widget(
        Paragraph::new(message)
            .wrap(Wrap { trim: false })
            .style(status_style)
            .block(
                Block::default()
                    .title(" State ")
                    .borders(Borders::ALL)
                    .border_style(status_style),
            ),
        status_area,
    );
    if !dialog.controls_focused {
        place_input_cursor_at(
            frame,
            body,
            input_row,
            0,
            &dialog.draft,
            cursor.unwrap_or_else(|| dialog.draft.chars().count()),
            theme,
        );
    }
    render_source_controls(frame, app, popup, theme);
}

fn source_ai_status(stage: crate::app::SourceAiStage, theme: Theme) -> (&'static str, Style) {
    let styles = DialogStyles::new(theme);
    match stage {
        crate::app::SourceAiStage::Input => ("Ready", styles.applied),
        crate::app::SourceAiStage::Error => ("Error", styles.error),
        crate::app::SourceAiStage::Proposal => ("Proposal", styles.applied),
        crate::app::SourceAiStage::Preparing
        | crate::app::SourceAiStage::Starting
        | crate::app::SourceAiStage::Proposing => ("Updating", styles.pending),
    }
}

fn source_content_popup(popup: Rect) -> Rect {
    popup
}

fn render_source_controls(frame: &mut Frame<'_>, app: &mut App, popup: Rect, theme: Theme) {
    let Some(dialog) = app.source_dialog.as_ref() else {
        return;
    };
    let assist = if app.ascii { "Agent" } else { "🧠" };
    use crate::app::SourceControl as Control;
    let mut controls = vec![
        (Control::Manual, "Manual"),
        (Control::Discovery, "Discover"),
        (Control::Agent, assist),
    ];
    match dialog.mode {
        crate::app::SourceDialogMode::Manual => {
            controls.extend([(Control::File, "File"), (Control::Command, "Command")]);
        }
        crate::app::SourceDialogMode::Discovery => controls.push((Control::Refresh, "Refresh")),
        crate::app::SourceDialogMode::Ai => controls.insert(
            0,
            (
                Control::Input,
                match dialog.ai.stage {
                    crate::app::SourceAiStage::Input | crate::app::SourceAiStage::Error => {
                        "Request"
                    }
                    crate::app::SourceAiStage::Proposal => "Start reviewed",
                    crate::app::SourceAiStage::Preparing
                    | crate::app::SourceAiStage::Starting
                    | crate::app::SourceAiStage::Proposing => "Working…",
                },
            ),
        ),
    }
    let area = Rect::new(
        popup.x.saturating_add(2),
        popup.bottom().saturating_sub(3),
        popup.width.saturating_sub(4),
        2,
    );
    frame.render_widget(Clear, area);
    let focused_index = controls
        .iter()
        .position(|(control, _)| *control == dialog.control);
    let labels: Vec<_> = controls.iter().map(|(_, label)| *label).collect();
    for (index, rect) in button_layout(area, &labels, focused_index) {
        let (control, label) = controls[index];
        app.hit_regions.source_controls.push((rect, control));
        let selected = matches!(
            (control, dialog.mode),
            (Control::Manual, crate::app::SourceDialogMode::Manual)
                | (Control::Discovery, crate::app::SourceDialogMode::Discovery)
                | (Control::Agent, crate::app::SourceDialogMode::Ai)
        ) || matches!(
            (control, dialog.kind),
            (Control::File, crate::app::SourceKind::File)
                | (Control::Command, crate::app::SourceKind::Command)
        );
        render_button(
            frame,
            rect,
            label,
            control == dialog.control,
            selected,
            theme,
        );
    }
}

fn place_input_cursor_at(
    frame: &mut Frame<'_>,
    area: Rect,
    first_row: usize,
    prefix_width: usize,
    value: &str,
    cursor: usize,
    theme: Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
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
        return;
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
}

fn input_tail(value: &str, maximum_width: usize) -> String {
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

fn time_input_window(value: &str, caret: usize, maximum_width: usize) -> (String, usize) {
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

fn wrap_time_text(value: &str, maximum_width: usize) -> Vec<String> {
    let maximum_width = maximum_width.max(1);
    let mut lines = vec![String::new()];
    let mut width = 0usize;
    for ch in value.chars() {
        let char_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width > 0 && width.saturating_add(char_width) > maximum_width {
            lines.push(String::new());
            width = 0;
        }
        lines.last_mut().expect("line").push(ch);
        width = width.saturating_add(char_width);
    }
    lines
}

/// Rows a help sentence needs (§3 `help_h`, capped at 2).
fn help_rows(help: &str, width: u16) -> u16 {
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

fn render_help_text(frame: &mut Frame<'_>, rect: Rect, help: &str, theme: Theme) {
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
const ACTION_GUTTER: u16 = 2;

/// §4.1 `gutter` between the label column and the field column.
const FIELD_GUTTER: u16 = 2;

/// §8.2 button row: primary first in `accent`, destructive last in `error`,
/// focused in the selection style. Returns the hitboxes actually drawn, which
/// are the same rects the mouse handler is given.
fn render_action_row(
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
    let regions = crate::dialog_layout::regions(popup, content);
    app.hit_regions.selection_modal = Some(regions.interior);
    regions
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

fn read_only_action_footer_height(popup: Rect, actions: &[(&str, &str)], theme: Theme) -> u16 {
    let inner = popup.inner(ratatui::layout::Margin::new(1, 1));
    if actions.is_empty() || inner.width == 0 || inner.height <= 1 {
        return 0;
    }
    let paragraph = Paragraph::new(crate::dialog_controls::action_line(actions, theme))
        .wrap(Wrap { trim: false });
    u16::try_from(paragraph.line_count(inner.width))
        .unwrap_or(u16::MAX)
        .max(1)
        .min(inner.height.saturating_sub(1))
}

fn render_read_only_action_footer(
    frame: &mut Frame<'_>,
    popup: Rect,
    actions: &[(&str, &str)],
    theme: Theme,
) {
    let height = read_only_action_footer_height(popup, actions, theme);
    if height == 0 {
        return;
    }
    let inner = popup.inner(ratatui::layout::Margin::new(1, 1));
    let footer = Rect::new(
        inner.x,
        inner.bottom().saturating_sub(height),
        inner.width,
        height,
    );
    frame.render_widget(
        Paragraph::new(crate::dialog_controls::action_line(actions, theme))
            .wrap(Wrap { trim: false }),
        footer,
    );
}

struct InputSurface {
    style: Style,
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

fn clear_themed(frame: &mut Frame<'_>, area: Rect, theme: Theme) {
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
