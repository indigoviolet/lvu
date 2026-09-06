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
        DialogStyles, action_line, button_layout, button_style, button_width, render_button,
    },
    json_spans::{JsonKind, JsonSpan, classify},
    provider::RowProvider,
    theme::{Theme, ThemeId},
};

const SIDEBAR_WIDTH: u16 = 22;

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
    let (sidebar, main) = if area.width >= 48 {
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
    let geometry = layout(frame.area(), app.show_details);
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
    if matches!(
        app.focus,
        Focus::SearchEditor
            | Focus::AdvancedEditor
            | Focus::EnrichmentEditor
            | Focus::GroupingEditor
    ) {
        render_editor(frame, app, provider, geometry.area, theme);
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
            .title(" External command · runs only when confirmed ")
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

fn render_settings(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let styles = DialogStyles::new(theme);
    let cursor = app.active_text_cursor();
    let popup = centered(area, 104, 30);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    app.hit_regions.settings_controls.clear();
    app.hit_regions.settings_theme_choices.clear();
    let Some(dialog) = app.settings_dialog.clone() else {
        return;
    };
    let values = &dialog.draft;
    let agent_label = if app.ascii { "Agent" } else { "🧠" };
    frame.render_widget(
        Block::default()
            .title(" Settings ")
            .borders(Borders::ALL)
            .border_style(styles.label),
        popup,
    );
    let body = dialog_body(popup);
    if body.height < 18 {
        render_compact_settings(frame, app, popup, body, &dialog, cursor, theme);
        return;
    }
    let wide = body.width >= 84;
    let rows = Layout::vertical([
        Constraint::Length(if wide { 3 } else { 4 }),
        Constraint::Length(4),
        Constraint::Length(if wide { 3 } else { 5 }),
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Min(1),
    ])
    .split(body);
    frame.render_widget(
        Paragraph::new(format!("{agent_label} configuration")).style(styles.label),
        Rect::new(rows[0].x, rows[0].y, rows[0].width, 1),
    );
    let agent_fields = [
        (
            crate::app::SettingsField::Provider,
            "Provider/model",
            values.provider.as_str(),
        ),
        (
            crate::app::SettingsField::Mode,
            "Mode",
            values.mode.as_str(),
        ),
        (
            crate::app::SettingsField::Thinking,
            "Thinking",
            values.thinking.as_str(),
        ),
    ];
    let agent_areas = settings_field_areas(rows[0], agent_fields.len(), wide);
    let mut theme_anchor = Rect::default();
    for ((field, label, value), rect) in agent_fields.into_iter().zip(agent_areas) {
        render_settings_field(
            frame,
            app,
            rect,
            field,
            label,
            value,
            &dialog,
            cursor,
            styles.label,
            styles.input,
            theme,
        );
    }
    frame.render_widget(
        Paragraph::new("Appearance").style(styles.label),
        Rect::new(rows[1].x, rows[1].y, rows[1].width, 1),
    );
    let appearance = [
        (
            crate::app::SettingsField::Theme,
            format!("Theme: {} ▾", values.theme.as_str()),
        ),
        (
            crate::app::SettingsField::Delight,
            format!("Delight: {}", on_off(values.delight_enabled)),
        ),
        (
            crate::app::SettingsField::ReducedMotion,
            format!("Reduced motion: {}", on_off(values.reduced_motion)),
        ),
        (
            crate::app::SettingsField::Ascii,
            format!("ASCII: {}", on_off(values.ascii)),
        ),
    ];
    let appearance_focus = appearance
        .iter()
        .position(|(field, _)| dialog.focus == crate::app::SettingsControl::Field(*field))
        .unwrap_or(0);
    let appearance_labels = appearance
        .iter()
        .map(|(_, label)| label.as_str())
        .collect::<Vec<_>>();
    let appearance_area = Rect::new(
        rows[1].x,
        rows[1].y.saturating_add(1),
        rows[1].width,
        rows[1].height.saturating_sub(1),
    );
    for (index, rect) in button_layout(appearance_area, &appearance_labels, Some(appearance_focus))
    {
        let (field, label) = &appearance[index];
        if *field == crate::app::SettingsField::Theme {
            theme_anchor = rect;
        }
        let control = crate::app::SettingsControl::Field(*field);
        app.hit_regions.settings_controls.push((rect, control));
        let selected = match field {
            crate::app::SettingsField::Delight => values.delight_enabled,
            crate::app::SettingsField::ReducedMotion => values.reduced_motion,
            crate::app::SettingsField::Ascii => values.ascii,
            _ => false,
        };
        render_button(frame, rect, label, dialog.focus == control, selected, theme);
    }
    frame.render_widget(
        Paragraph::new("Cache limits (MiB)").style(styles.label),
        Rect::new(rows[2].x, rows[2].y, rows[2].width, 1),
    );
    let cache_fields = [
        (
            crate::app::SettingsField::RowCache,
            "Rows",
            values.rows_mib.as_str(),
        ),
        (
            crate::app::SettingsField::Membership,
            "Membership",
            values.membership_mib.as_str(),
        ),
        (
            crate::app::SettingsField::DiskTotal,
            "Derived total",
            values.disk_total_mib.as_str(),
        ),
        (
            crate::app::SettingsField::IndexPerSource,
            "Per source",
            values.index_per_source_mib.as_str(),
        ),
    ];
    let cache_areas = settings_field_areas(rows[2], 2, wide);
    for (index, (field, label, value)) in cache_fields.into_iter().enumerate() {
        let column = if wide { index % 2 } else { 0 };
        let row = if wide { index / 2 } else { index };
        let base = cache_areas[column];
        let rect = Rect::new(base.x, base.y.saturating_add(row as u16), base.width, 1);
        render_settings_field(
            frame,
            app,
            rect,
            field,
            label,
            value,
            &dialog,
            cursor,
            styles.label,
            styles.input,
            theme,
        );
    }
    let details = vec![
        Line::raw(format!(
            "Effective {agent_label}: {} [{}] · {} [{}] · {} [{}]",
            dialog.context.effective_provider,
            dialog.context.provider_source,
            dialog.context.effective_mode,
            dialog.context.mode_source,
            dialog.context.effective_thinking,
            dialog.context.thinking_source,
        )),
        Line::raw(format!(
            "Effective appearance: theme {} · delight {} [{}] · motion {} [{}] · ASCII {} [{}]",
            dialog.context.effective_theme.as_str(),
            dialog.context.effective_delight_enabled,
            dialog.context.delight_source,
            dialog.context.effective_reduced_motion,
            dialog.context.reduced_motion_source,
            dialog.context.effective_ascii,
            dialog.context.ascii_source,
        )),
        Line::raw(format!(
            "Startup-applied MiB: rows {} · membership {} · total derived {} · index/source {}",
            dialog.context.applied_rows_mib,
            dialog.context.applied_membership_mib,
            dialog.context.applied_disk_total_mib,
            dialog.context.applied_index_per_source_mib,
        )),
        Line::raw(format!("Settings: {}", dialog.context.settings_path)),
        Line::raw(format!("Data: {}", dialog.context.data_path)),
        Line::raw(format!("Cache: {}", dialog.context.cache_path)),
        Line::raw(format!("Capture: {}", dialog.context.capture_path)),
        Line::raw(
            "Cache-limit changes take effect after restart; appearance previews immediately.",
        ),
    ];
    let details_p = Paragraph::new(details).wrap(Wrap { trim: false });
    let details_block = Block::default()
        .title(" Effective values and paths ")
        .borders(Borders::ALL)
        .border_style(
            Style::default().fg(if dialog.focus == crate::app::SettingsControl::More {
                theme.focused_input_border
            } else {
                theme.border
            }),
        );
    let details_inner = details_block.inner(rows[5]);
    let detail_limit = details_p
        .line_count(details_inner.width)
        .saturating_sub(usize::from(details_inner.height));
    if let Some(state) = &mut app.settings_dialog {
        state.details_scroll_limit = detail_limit;
        state.details_scroll = state.details_scroll.min(detail_limit);
        if detail_limit == 0 && state.focus == crate::app::SettingsControl::More {
            state.focus = crate::app::SettingsControl::Save;
        }
    }
    let save = crate::app::SettingsControl::Save;
    let save_label = if dialog.saving { "Saving…" } else { "Save" };
    let mut action_controls = vec![(save, save_label)];
    if detail_limit > 0 {
        action_controls.push((crate::app::SettingsControl::More, "More"));
    }
    let action_labels = action_controls
        .iter()
        .map(|(_, label)| *label)
        .collect::<Vec<_>>();
    let focused_action = action_controls
        .iter()
        .position(|(control, _)| *control == dialog.focus);
    for (index, rect) in button_layout(rows[3], &action_labels, focused_action) {
        let (control, label) = action_controls[index];
        app.hit_regions.settings_controls.push((rect, control));
        render_button(frame, rect, label, dialog.focus == control, false, theme);
    }
    let (status_label, status_style) = match dialog.status_kind {
        crate::app::SettingsStatus::Saved => ("Saved", styles.applied),
        crate::app::SettingsStatus::Pending => ("Pending", styles.pending),
        crate::app::SettingsStatus::Error => ("Error", styles.error),
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{status_label}: {}\n{}",
            settings_state_summary(&dialog),
            dialog.status
        ))
        .wrap(Wrap { trim: false })
        .style(status_style)
        .block(
            Block::default()
                .title(" State ")
                .borders(Borders::ALL)
                .border_style(status_style),
        ),
        rows[4],
    );
    frame.render_widget(
        details_p
            .scroll((dialog.details_scroll.min(u16::MAX as usize) as u16, 0))
            .style(styles.description)
            .block(details_block),
        rows[5],
    );
    if dialog.theme_dropdown && theme_anchor.width > 0 {
        render_settings_theme_dropdown(
            frame,
            app,
            popup,
            theme_anchor,
            dialog.theme_selected,
            theme,
        );
    }
}

fn render_compact_settings(
    frame: &mut Frame<'_>,
    app: &mut App,
    popup: Rect,
    body: Rect,
    dialog: &crate::app::SettingsDialogState,
    cursor: Option<usize>,
    theme: Theme,
) {
    use crate::app::{SettingsControl as Control, SettingsField as Field, SettingsStatus};
    let styles = DialogStyles::new(theme);
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Min(2),
    ])
    .split(body);
    let field = match dialog.focus {
        Control::Field(field) => field,
        _ => Field::ALL[dialog.selected],
    };
    let (label, value) = match field {
        Field::Provider => ("Provider/model", Some(dialog.draft.provider.as_str())),
        Field::Mode => ("Mode", Some(dialog.draft.mode.as_str())),
        Field::Thinking => ("Thinking", Some(dialog.draft.thinking.as_str())),
        Field::RowCache => ("Rows MiB", Some(dialog.draft.rows_mib.as_str())),
        Field::Membership => ("Membership MiB", Some(dialog.draft.membership_mib.as_str())),
        Field::DiskTotal => (
            "Derived total MiB",
            Some(dialog.draft.disk_total_mib.as_str()),
        ),
        Field::IndexPerSource => (
            "Per source MiB",
            Some(dialog.draft.index_per_source_mib.as_str()),
        ),
        Field::Theme => ("Theme", None),
        Field::Delight => ("Delight", None),
        Field::ReducedMotion => ("Reduced motion", None),
        Field::Ascii => ("ASCII", None),
    };
    frame.render_widget(
        Paragraph::new(label).style(styles.label),
        Rect::new(rows[0].x, rows[0].y, rows[0].width, 1),
    );
    let control_area = Rect::new(rows[0].x, rows[0].y.saturating_add(1), rows[0].width, 1);
    let mut theme_anchor = Rect::default();
    if let Some(value) = value {
        render_settings_field(
            frame,
            app,
            control_area,
            field,
            "Value",
            value,
            dialog,
            cursor,
            styles.label,
            styles.input,
            theme,
        );
    } else {
        let value = match field {
            Field::Theme => format!("Theme: {} ▾", dialog.draft.theme.as_str()),
            Field::Delight => format!("Delight: {}", on_off(dialog.draft.delight_enabled)),
            Field::ReducedMotion => {
                format!("Reduced motion: {}", on_off(dialog.draft.reduced_motion))
            }
            Field::Ascii => format!("ASCII: {}", on_off(dialog.draft.ascii)),
            _ => unreachable!(),
        };
        let rect = Rect::new(
            control_area.x,
            control_area.y,
            button_width(&value).min(control_area.width),
            1,
        );
        theme_anchor = if field == Field::Theme {
            rect
        } else {
            Rect::default()
        };
        app.hit_regions
            .settings_controls
            .push((rect, Control::Field(field)));
        render_button(
            frame,
            rect,
            &value,
            dialog.focus == Control::Field(field),
            matches!(
                field,
                Field::Delight if dialog.draft.delight_enabled
            ) || matches!(field, Field::ReducedMotion if dialog.draft.reduced_motion)
                || matches!(field, Field::Ascii if dialog.draft.ascii),
            theme,
        );
    }
    let detail_lines = settings_detail_lines(dialog, if app.ascii { "Agent" } else { "🧠" });
    let details_block = Block::default()
        .title(" Effective values and paths ")
        .borders(Borders::ALL);
    let details_inner = details_block.inner(rows[3]);
    let details_p = Paragraph::new(detail_lines).wrap(Wrap { trim: false });
    let limit = details_p
        .line_count(details_inner.width)
        .saturating_sub(usize::from(details_inner.height));
    if let Some(state) = &mut app.settings_dialog {
        state.details_scroll_limit = limit;
        state.details_scroll = state.details_scroll.min(limit);
        if limit == 0 && state.focus == Control::More {
            state.focus = Control::Save;
        }
    }
    let save = Control::Save;
    let save_label = if dialog.saving { "Saving…" } else { "Save" };
    let mut action_controls = vec![(save, save_label)];
    if limit > 0 {
        action_controls.push((Control::More, "More"));
    }
    let labels = action_controls
        .iter()
        .map(|(_, label)| *label)
        .collect::<Vec<_>>();
    let focused = action_controls
        .iter()
        .position(|(control, _)| *control == dialog.focus);
    for (index, rect) in button_layout(rows[1], &labels, focused) {
        let (control, label) = action_controls[index];
        app.hit_regions.settings_controls.push((rect, control));
        render_button(frame, rect, label, dialog.focus == control, false, theme);
    }
    let (label, status_style) = match dialog.status_kind {
        SettingsStatus::Saved => ("Saved", styles.applied),
        SettingsStatus::Pending => ("Pending", styles.pending),
        SettingsStatus::Error => ("Error", styles.error),
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{label}: {}\n{}",
            settings_state_summary(dialog),
            dialog.status
        ))
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
    frame.render_widget(
        details_p
            .scroll((dialog.details_scroll.min(u16::MAX as usize) as u16, 0))
            .style(styles.description)
            .block(details_block),
        rows[3],
    );
    if dialog.theme_dropdown && theme_anchor.width > 0 {
        render_settings_theme_dropdown(
            frame,
            app,
            popup,
            theme_anchor,
            dialog.theme_selected,
            theme,
        );
    }
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

fn settings_state_summary(dialog: &crate::app::SettingsDialogState) -> &'static str {
    match dialog.status_kind {
        crate::app::SettingsStatus::Saved if dialog.status.contains("restart") => {
            "Saved; restart required for cache-limit changes"
        }
        crate::app::SettingsStatus::Saved => "Saved and applied",
        crate::app::SettingsStatus::Pending if dialog.saving => "Saving settings…",
        crate::app::SettingsStatus::Pending => "Changes are not saved",
        crate::app::SettingsStatus::Error => "Save failed; details below",
    }
}

fn on_off(value: bool) -> &'static str {
    if value { "On" } else { "Off" }
}

fn settings_field_areas(area: Rect, count: usize, wide: bool) -> Vec<Rect> {
    let y = area.y.saturating_add(1);
    if wide {
        Layout::horizontal(vec![Constraint::Ratio(1, count as u32); count])
            .split(Rect::new(area.x, y, area.width, 1))
            .to_vec()
    } else {
        (0..count)
            .map(|index| Rect::new(area.x, y.saturating_add(index as u16), area.width, 1))
            .collect()
    }
}

#[allow(clippy::too_many_arguments)]
fn render_settings_field(
    frame: &mut Frame<'_>,
    app: &mut App,
    area: Rect,
    field: crate::app::SettingsField,
    label: &str,
    value: &str,
    dialog: &crate::app::SettingsDialogState,
    cursor: Option<usize>,
    label_style: Style,
    input_style: Style,
    theme: Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let label_text = format!("{label}: ");
    let label_width = (UnicodeWidthStr::width(label_text.as_str()) as u16).min(area.width);
    frame.render_widget(
        Paragraph::new(label_text).style(label_style),
        Rect::new(area.x, area.y, label_width, 1),
    );
    let input = Rect::new(
        area.x.saturating_add(label_width),
        area.y,
        area.width.saturating_sub(label_width).saturating_sub(1),
        1,
    );
    if input.width == 0 {
        return;
    }
    app.hit_regions
        .settings_controls
        .push((input, crate::app::SettingsControl::Field(field)));
    InputSurface { style: input_style }.render(input, frame.buffer_mut());
    frame.render_widget(
        Paragraph::new(input_tail(
            value,
            usize::from(input.width.saturating_sub(1)),
        ))
        .style(input_style),
        input,
    );
    if dialog.focus == crate::app::SettingsControl::Field(field) {
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

fn render_editor<P: RowProvider>(
    frame: &mut Frame<'_>,
    app: &mut App,
    provider: &P,
    area: Rect,
    theme: Theme,
) {
    let active_cursor = app.active_text_cursor();
    let popup_height = if app.focus == Focus::EnrichmentEditor {
        26
    } else if app.focus == Focus::GroupingEditor {
        13
    } else {
        11
    };
    let popup = centered(area, 80, popup_height);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    let Some(editor) = app.active_editor_state().cloned() else {
        return;
    };
    if matches!(app.focus, Focus::SearchEditor | Focus::AdvancedEditor) {
        render_simple_editor(frame, app, area, popup, editor, theme);
        return;
    }
    if app.focus == Focus::GroupingEditor {
        render_shared_compact_grouping(frame, app, popup, editor, active_cursor, theme);
        return;
    }
    if app.focus == Focus::EnrichmentEditor && dialog_body(popup).height >= 14 {
        render_enrichment_workspace(frame, app, provider, area, popup, theme);
        return;
    }
    debug_assert_eq!(app.focus, Focus::EnrichmentEditor);
    render_compact_enrichment_fallback(frame, app, provider, popup, editor, active_cursor, theme);
    render_editor_completion(frame, app, area, theme);
}

fn render_compact_enrichment_fallback<P: RowProvider>(
    frame: &mut Frame<'_>,
    app: &mut App,
    provider: &P,
    popup: Rect,
    editor: crate::app::EditorState,
    cursor: Option<usize>,
    theme: Theme,
) {
    let styles = DialogStyles::new(theme);
    frame.render_widget(
        Block::default()
            .title(" Enrichment ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        popup,
    );
    let body = dialog_body(popup);
    if body.height == 0 {
        return;
    }
    let state = app.view_state().expect("active enrichment view");
    let stages = state.enrichments.clone();
    let selected = state
        .enrichment_selected
        .min(stages.len().saturating_sub(1));
    let focused = state.enrichment_control;
    let editing = state.enrichment_editing.is_some();

    let controls_height = body.height.min(2);
    let content = Rect::new(
        body.x,
        body.y,
        body.width,
        body.height.saturating_sub(controls_height),
    );
    let summary_height = if content.height >= 6 {
        content.height.saturating_sub(3).min(3)
    } else {
        0
    };
    let rows = Layout::vertical([
        Constraint::Length(summary_height),
        Constraint::Length(content.height.saturating_sub(summary_height).min(1)),
        Constraint::Length(content.height.saturating_sub(summary_height + 1).min(1)),
        Constraint::Min(0),
    ])
    .split(content);

    app.hit_regions.enrichment_rows.clear();
    if summary_height > 0 {
        let mut lines = vec![Line::styled("Applied steps", styles.applied)];
        if stages.is_empty() {
            lines.push(Line::styled("  None yet", styles.description));
        } else {
            let visible = usize::from(summary_height.saturating_sub(1));
            let top = selected.saturating_sub(visible.saturating_sub(1));
            for (position, (index, stage)) in stages
                .iter()
                .enumerate()
                .skip(top)
                .take(visible)
                .enumerate()
            {
                let rect = Rect::new(rows[0].x, rows[0].y + 1 + position as u16, rows[0].width, 1);
                app.hit_regions.enrichment_rows.push((rect, index));
                lines.push(Line::styled(
                    format!(
                        "{} {}. {}",
                        if index == selected { "›" } else { " " },
                        index + 1,
                        clipped_width(&stage.source, usize::from(rect.width.saturating_sub(6)))
                    ),
                    if index == selected {
                        styles.selection
                    } else {
                        styles.description
                    },
                ));
            }
        }
        frame.render_widget(Paragraph::new(lines), rows[0]);
    }

    if rows[1].height > 0 {
        frame.render_widget(
            Paragraph::new(if editing {
                "Edit selected step"
            } else {
                "Add step: name = expression or named-capture regex"
            })
            .style(styles.label),
            rows[1],
        );
    }
    if rows[2].height > 0 {
        InputSurface {
            style: styles.input,
        }
        .render(rows[2], frame.buffer_mut());
        frame.render_widget(
            Paragraph::new(input_tail(&editor.draft, usize::from(rows[2].width)))
                .style(styles.input),
            rows[2],
        );
        if focused == crate::app::EnrichmentControl::Editor
            && app.editor_completion.is_none()
            && !app.dialog_scroll_focused
        {
            place_input_cursor_at(
                frame,
                rows[2],
                0,
                0,
                &editor.draft,
                cursor.unwrap_or_else(|| editor.draft.chars().count()),
                theme,
            );
        }
    }

    let (status_label, status_detail, status_role) = if let Some(error) = &editor.error {
        (
            "Error",
            format!("Previous applied steps retained. {error}"),
            styles.error,
        )
    } else if editor.pending_generation.is_some() {
        (
            "Updating",
            "Checking the draft; previous applied steps remain active.".to_owned(),
            styles.pending,
        )
    } else {
        (
            "Applied",
            if stages.is_empty() {
                "No enrichment steps yet.".to_owned()
            } else {
                format!("{} ordered step(s) active.", stages.len())
            },
            styles.applied,
        )
    };
    let raw = app.selected_row(provider).map_or_else(
        || "No selected raw record.".to_owned(),
        |row| {
            format!(
                "Raw: {}",
                clipped_width(&row.text, usize::from(rows[3].width.saturating_sub(5)))
            )
        },
    );
    let mut status_lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{status_label}: "),
                status_role.add_modifier(Modifier::BOLD),
            ),
            Span::styled(status_detail, styles.description),
        ]),
        Line::styled(raw, styles.description),
        Line::styled(
            "Fields and sampled values remain available through completion.",
            styles.description,
        ),
    ];
    let base_status = Paragraph::new(status_lines.clone()).wrap(Wrap { trim: false });
    if base_status.line_count(rows[3].width) > usize::from(rows[3].height) {
        status_lines[0]
            .spans
            .push(Span::styled(" · ↑/↓ scroll", styles.shortcut));
    }
    let status = Paragraph::new(status_lines).wrap(Wrap { trim: false });
    app.dialog_scroll_limit = status
        .line_count(rows[3].width)
        .saturating_sub(usize::from(rows[3].height));
    app.dialog_scroll = app.dialog_scroll.min(app.dialog_scroll_limit);
    app.hit_regions.dialog_scroll =
        (app.dialog_scroll_limit > 0 && !rows[3].is_empty()).then_some(rows[3]);
    frame.render_widget(
        status.scroll((app.dialog_scroll.min(u16::MAX as usize) as u16, 0)),
        rows[3],
    );
    render_compact_enrichment_controls(frame, app, body, focused, theme);
}

fn render_shared_compact_grouping(
    frame: &mut Frame<'_>,
    app: &mut App,
    popup: Rect,
    editor: crate::app::EditorState,
    cursor: Option<usize>,
    theme: Theme,
) {
    let styles = DialogStyles::new(theme);
    frame.render_widget(
        Block::default()
            .title(" Display-only multiline grouping ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        popup,
    );
    let body = dialog_body(popup);
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(2),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(body);
    frame.render_widget(
        Paragraph::new("Continuation regex over raw bytes").style(styles.label),
        rows[0],
    );
    InputSurface {
        style: styles.input,
    }
    .render(rows[1], frame.buffer_mut());
    frame.render_widget(
        Paragraph::new(input_tail(&editor.draft, usize::from(rows[1].width))).style(styles.input),
        rows[1],
    );
    if !app.dialog_scroll_focused {
        place_input_cursor_at(
            frame,
            rows[1],
            0,
            0,
            &editor.draft,
            cursor.unwrap_or_else(|| editor.draft.chars().count()),
            theme,
        );
    }
    let (label, detail, role) = if let Some(error) = editor.error.as_deref() {
        ("Error", error, styles.error)
    } else if editor.pending_generation.is_some() {
        (
            "Updating",
            "Checking this draft; the last applied grouping remains active.",
            styles.pending,
        )
    } else if editor.applied.is_empty() {
        ("Applied", "Grouping disabled.", styles.applied)
    } else {
        ("Applied", editor.applied.as_str(), styles.applied)
    };
    let status = Paragraph::new(Line::from(vec![
        Span::styled(format!("{label}: "), role.add_modifier(Modifier::BOLD)),
        Span::styled(detail.to_owned(), styles.description),
    ]))
    .wrap(Wrap { trim: false });
    app.dialog_scroll_limit = status
        .line_count(rows[2].width)
        .saturating_sub(usize::from(rows[2].height));
    app.dialog_scroll = app.dialog_scroll.min(app.dialog_scroll_limit);
    app.hit_regions.dialog_scroll = (app.dialog_scroll_limit > 0).then_some(rows[2]);
    frame.render_widget(
        status.scroll((app.dialog_scroll.min(u16::MAX as usize) as u16, 0)),
        rows[2],
    );
    frame.render_widget(
        Paragraph::new(
            "Preview (display only):\nRuntimeException: boom\n  at worker.rs:42  → 2 physical lines",
        )
        .style(styles.description),
        rows[3],
    );
    frame.render_widget(
        Paragraph::new(action_line(&[("", "Empty draft disables grouping")], theme)),
        rows[4],
    );
}

fn render_compact_enrichment_controls(
    frame: &mut Frame<'_>,
    app: &mut App,
    body: Rect,
    focused: crate::app::EnrichmentControl,
    theme: Theme,
) {
    if body.height == 0 {
        return;
    }
    app.hit_regions.enrichment_controls.clear();
    let area = Rect::new(
        body.x,
        body.bottom().saturating_sub(2),
        body.width,
        body.height.min(2),
    );
    let controls = [
        (crate::app::EnrichmentControl::Steps, "Steps"),
        (crate::app::EnrichmentControl::Editor, "Editor"),
        (crate::app::EnrichmentControl::Add, "Add"),
        (crate::app::EnrichmentControl::Edit, "Edit"),
        (crate::app::EnrichmentControl::Remove, "Remove"),
        (
            crate::app::EnrichmentControl::ExternalCommand,
            "External command",
        ),
    ];
    frame.render_widget(Clear, area);
    let focused_index = controls
        .iter()
        .position(|(control, _)| *control == focused)
        .unwrap_or(0);
    let labels = controls.iter().map(|(_, label)| *label).collect::<Vec<_>>();
    for (index, hit) in button_layout(area, &labels, Some(focused_index)) {
        let (control, label) = controls[index];
        app.hit_regions.enrichment_controls.push((hit, control));
        render_button(frame, hit, label, control == focused, false, theme);
    }
}

fn render_simple_editor(
    frame: &mut Frame<'_>,
    app: &mut App,
    area: Rect,
    popup: Rect,
    editor: crate::app::EditorState,
    theme: Theme,
) {
    let styles = DialogStyles::new(theme);
    let cursor = app
        .active_text_cursor()
        .unwrap_or_else(|| editor.draft.chars().count());
    let search = app.focus == Focus::SearchEditor;
    let title = if search {
        " Search "
    } else {
        " Advanced filter "
    };
    frame.render_widget(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        popup,
    );
    let body = dialog_body(popup);
    if body.height == 0 {
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(if search { 0 } else { 1 }),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(body);
    frame.render_widget(
        Paragraph::new(if search { "" } else { "FILTER EXPRESSION" })
            .style(styles.label.add_modifier(Modifier::BOLD)),
        rows[0],
    );
    InputSurface {
        style: styles.input,
    }
    .render(rows[1], frame.buffer_mut());
    frame.render_widget(
        Paragraph::new(input_tail(&editor.draft, usize::from(rows[1].width))).style(styles.input),
        rows[1],
    );
    if app.editor_completion.is_none() && !app.dialog_scroll_focused {
        place_input_cursor_at(frame, rows[1], 0, 0, &editor.draft, cursor, theme);
    }
    let help = if search {
        r#"Examples: text · "field name": text · /regex/ims · \/literal"#
    } else {
        "Use a Polars expression. Fields and static sampled literals are available as completions."
    };
    frame.render_widget(
        Paragraph::new(help)
            .wrap(Wrap { trim: false })
            .style(styles.description),
        rows[3],
    );
    let (label, value, status_style) = if let Some(error) = editor.error.as_deref() {
        ("Error", error, styles.error)
    } else if editor.pending_generation.is_some() {
        (
            "Updating",
            "Checking this draft; the last applied view remains visible.",
            styles.pending,
        )
    } else if !editor.applied.is_empty() {
        ("Applied", editor.applied.as_str(), styles.applied)
    } else {
        ("Applied", "No filter applied.", styles.applied)
    };
    let mut status_lines = vec![Line::from(vec![
        Span::styled(
            format!("{label}  "),
            status_style.add_modifier(Modifier::BOLD),
        ),
        Span::styled(value.to_owned(), styles.description),
    ])];
    if !editor.applied.is_empty() && editor.draft != editor.applied {
        status_lines.push(Line::from(vec![
            Span::styled(
                "Last accepted  ",
                styles.applied.add_modifier(Modifier::BOLD),
            ),
            Span::styled(editor.applied.clone(), styles.description),
        ]));
    }
    let status = Paragraph::new(status_lines).wrap(Wrap { trim: false });
    let mut status_area = rows[2];
    let overflow = status
        .line_count(status_area.width)
        .saturating_sub(usize::from(status_area.height));
    if overflow > 0 && status_area.height > 1 {
        frame.render_widget(
            Paragraph::new(action_line(&[("↑/↓", "Scroll status")], theme)),
            Rect::new(status_area.x, status_area.y, status_area.width, 1),
        );
        status_area.y += 1;
        status_area.height -= 1;
    }
    app.dialog_scroll_limit = status
        .line_count(status_area.width)
        .saturating_sub(usize::from(status_area.height));
    app.hit_regions.dialog_scroll = (app.dialog_scroll_limit > 0).then_some(rows[2]);
    app.dialog_scroll = app.dialog_scroll.min(app.dialog_scroll_limit);
    frame.render_widget(
        status.scroll((app.dialog_scroll.min(u16::MAX as usize) as u16, 0)),
        status_area,
    );
    render_editor_completion(frame, app, area, theme);
}

fn render_enrichment_workspace<P: RowProvider>(
    frame: &mut Frame<'_>,
    app: &mut App,
    provider: &P,
    area: Rect,
    popup: Rect,
    theme: Theme,
) {
    let cursor = app.active_text_cursor();
    let styles = DialogStyles::new(theme);
    let state = app.view_state().expect("enrichment view");
    let editor = state.enrichment.clone();
    let stages = state.enrichments.clone();
    let selected = state
        .enrichment_selected
        .min(stages.len().saturating_sub(1));
    let editing = state.enrichment_editing.is_some();
    let focused_control = state.enrichment_control;
    render_dialog_text(frame, popup, " Enrichment ", String::new(), theme);
    let body = dialog_body(popup);
    let sections = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(stages.len().clamp(1, 3) as u16 + 2),
        Constraint::Length(6),
        Constraint::Length(3),
        Constraint::Min(4),
    ])
    .split(body);
    let panel = |title: &str| {
        Block::default()
            .title(title.to_owned())
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent))
    };
    app.hit_regions.enrichment_rows.clear();
    app.hit_regions.enrichment_controls.clear();
    render_compact_enrichment_controls(frame, app, sections[0], focused_control, theme);
    let stage_area = sections[1];
    let stage_inner = panel("").inner(stage_area);
    let mut stage_lines = Vec::new();
    if stages.is_empty() {
        stage_lines.push(Line::styled(
            "No extracted fields yet. Add an expression below.",
            styles.description,
        ));
    } else {
        let top = selected.saturating_sub(usize::from(stage_inner.height.saturating_sub(1)));
        for (position, (index, stage)) in stages
            .iter()
            .enumerate()
            .skip(top)
            .take(usize::from(stage_inner.height))
            .enumerate()
        {
            let hit = Rect::new(
                stage_inner.x,
                stage_inner.y + position as u16,
                stage_inner.width,
                1,
            );
            app.hit_regions.enrichment_rows.push((hit, index));
            let style = if index == selected {
                styles.selection
            } else {
                styles.label.bg(theme.dialog_bg)
            };
            stage_lines.push(Line::styled(
                format!("{}. {}", index + 1, stage.source),
                style,
            ));
        }
    }
    frame.render_widget(
        Paragraph::new(stage_lines).block(panel(" Saved steps · kept when you add ")),
        stage_area,
    );

    let input_title = if editing {
        " Edit selected step · name = expression "
    } else {
        " Add step · name = expression OR /regex with named groups/ "
    };
    let input = panel(input_title).border_style(Style::default().fg(theme.focused_input_border));
    let input_area = input.inner(sections[2]);
    frame.render_widget(input, sections[2]);
    InputSurface {
        style: styles.input,
    }
    .render(input_area, frame.buffer_mut());
    let wrapped = crate::text_edit::wrapped_text(&editor.draft, usize::from(input_area.width));
    let lines = wrapped.lines;
    let (cursor_row, cursor_column) = cursor.map_or((0, 0), |cursor| {
        let mut logical_cursor = crate::text_edit::TextCursor { char_index: cursor };
        crate::text_edit::wrapped_cursor(
            &editor.draft,
            &mut logical_cursor,
            usize::from(input_area.width),
        )
    });
    let top = cursor_row
        .saturating_add(1)
        .saturating_sub(usize::from(input_area.height));
    let visible = lines.get(top..).unwrap_or(&[]).join("\n");
    frame.render_widget(Paragraph::new(visible).style(styles.input), input_area);
    if cursor.is_some()
        && app.editor_completion.is_none()
        && !app.dialog_scroll_focused
        && input_area.width > 0
        && input_area.height > 0
    {
        let x = input_area.x + cursor_column.min(usize::from(input_area.width - 1)) as u16;
        let y = input_area.y + cursor_row.saturating_sub(top) as u16;
        frame.buffer_mut()[(x, y)].set_style(Style::default().bg(theme.cursor).fg(theme.input_fg));
        frame.set_cursor_position((x, y));
    }
    let (status_label, message, status_style) = if let Some(error) = &editor.error {
        (
            "Error",
            format!("Previous results retained. {error}"),
            styles.error,
        )
    } else if editor.pending_generation.is_some() {
        (
            "Updating",
            "Checking this draft; the complete last applied chain remains visible.".to_owned(),
            styles.pending,
        )
    } else {
        (
            "Applied",
            "Accepted steps are active; a new draft changes nothing until it succeeds.".to_owned(),
            styles.applied,
        )
    };
    let status_area = sections[3];
    let status_inner = Rect::new(
        status_area.x,
        status_area.y.saturating_add(1),
        status_area.width,
        status_area.height.saturating_sub(1),
    );
    let status = Paragraph::new(Line::from(vec![
        Span::styled(
            format!("{status_label}: "),
            status_style.add_modifier(Modifier::BOLD),
        ),
        Span::styled(message, styles.description),
    ]))
    .wrap(Wrap { trim: false })
    .style(styles.description);
    app.dialog_scroll_limit = status
        .line_count(status_inner.width)
        .saturating_sub(usize::from(status_inner.height));
    if app.dialog_scroll_limit > 0 {
        frame.render_widget(
            Paragraph::new(action_line(&[("↑/↓", "Scroll status")], theme)),
            Rect::new(status_area.x, status_area.y, status_area.width, 1),
        );
    } else {
        frame.render_widget(
            Paragraph::new("Status").style(styles.label),
            Rect::new(status_area.x, status_area.y, status_area.width, 1),
        );
    }
    app.dialog_scroll = app.dialog_scroll.min(app.dialog_scroll_limit);
    app.hit_regions.dialog_scroll = (app.dialog_scroll_limit > 0).then_some(status_area);
    frame.render_widget(
        status.scroll((app.dialog_scroll.min(u16::MAX as usize) as u16, 0)),
        status_inner,
    );
    let samples = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(sections[4]);
    let (raw, derived) = if let Some(row) = app.selected_row(provider) {
        let mut input = row.text;
        if !row.fields.is_empty() {
            input.push_str("\nAvailable fields: ");
            input.push_str(
                &row.fields
                    .iter()
                    .map(|field| field.0.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        let output = row
            .details
            .iter()
            .filter(|(name, _)| name.starts_with("derived."))
            .map(|(name, value)| format!("{name}: {value}"))
            .collect::<Vec<_>>()
            .join("\n");
        (
            input,
            if output.is_empty() {
                "No accepted outputs yet.\nApply a valid step to see its values here.".into()
            } else {
                output
            },
        )
    } else {
        (
            "Select a log record to inspect its input.".into(),
            "No record selected.".into(),
        )
    };
    frame.render_widget(
        Paragraph::new(raw)
            .wrap(Wrap { trim: false })
            .style(styles.description)
            .block(panel(" Raw input before enrichment: ")),
        samples[0],
    );
    frame.render_widget(
        Paragraph::new(derived)
            .wrap(Wrap { trim: false })
            .style(styles.description)
            .block(panel(" Accepted output · same record ")),
        samples[1],
    );
    render_editor_completion(frame, app, area, theme);
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
                ("e", "Open ordered enrichments".into()),
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
    let styles = DialogStyles::new(theme);
    let cursor = app.active_text_cursor();
    let popup = centered(area, 76, 10);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    let Some(dialog) = &app.view_dialog else {
        return;
    };
    app.hit_regions.view_source_rows.clear();
    app.hit_regions.view_dialog_controls.clear();
    if dialog.mode == crate::app::ViewDialogMode::Sources {
        let popup = centered(area, 94, 22);
        clear_themed(frame, popup, theme);
        app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
        frame.render_widget(
            Block::default()
                .title(" View sources · explicit source order ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.accent)),
            popup,
        );
        let body = dialog_body_with_footer(popup, 3);
        let heading_rows = if body.height >= 4 { 2 } else { 0 };
        let count = usize::from(body.height.saturating_sub(heading_rows)).max(1);
        let first = dialog
            .selected_source
            .saturating_sub(count.saturating_sub(1));
        let mut lines = vec![
            Line::raw("Order: source position, then record sequence (not clock order)."),
            Line::raw(
                dialog
                    .error
                    .as_deref()
                    .unwrap_or("The owning source remains included; captures are shared."),
            ),
        ];
        if heading_rows == 0 {
            lines.clear();
        }
        for (index, source) in app.sources.iter().enumerate().skip(first).take(count) {
            let order = dialog.source_ids.iter().position(|id| id == &source.id);
            let text = format!(
                "[{}] {:>2} {}",
                if order.is_some() { "x" } else { " " },
                order.map(|i| (i + 1).to_string()).unwrap_or_default(),
                source.name
            );
            lines.push(Line::styled(
                clipped_width(&text, body.width as usize),
                if index == dialog.selected_source {
                    styles.selection
                } else {
                    styles.description
                },
            ));
            let y = body.y.saturating_add(heading_rows + (index - first) as u16);
            if y < body.bottom() {
                app.hit_regions
                    .view_source_rows
                    .push((Rect::new(body.x, y, body.width, 1), index));
            }
        }
        frame.render_widget(Paragraph::new(lines), body);
        let controls = view_dialog_button_controls(dialog.mode);
        render_view_dialog_buttons(
            frame,
            app,
            popup,
            dialog.control,
            dialog.mode,
            &controls,
            theme,
        );
        return;
    }
    let mode = match dialog.mode {
        crate::app::ViewDialogMode::Sources => unreachable!(),
        crate::app::ViewDialogMode::Blank => "NEW BLANK",
        crate::app::ViewDialogMode::Clone => "CLONE SETTINGS",
        crate::app::ViewDialogMode::Rename => "RENAME",
    };
    let message = dialog
        .error
        .as_deref()
        .unwrap_or("Name the view. Creating, cloning, and renaming preserve the source capture.");
    render_dialog_text(
        frame,
        popup,
        " Source view ",
        format!(
            "Mode: {mode}\n\nName: {}\n\n{message}",
            clipped_width(&dialog.draft, usize::from(popup.width.saturating_sub(9)))
        ),
        theme,
    );
    let input = Rect::new(
        dialog_body(popup).x + 6,
        dialog_body(popup).y + 2,
        dialog_body(popup).width.saturating_sub(6),
        1,
    );
    frame.render_widget(
        Paragraph::new(clipped_width(&dialog.draft, usize::from(input.width))).style(styles.input),
        input,
    );
    place_input_cursor_at(
        frame,
        dialog_body(popup),
        2,
        UnicodeWidthStr::width("Name: "),
        &dialog.draft,
        cursor.unwrap_or_else(|| dialog.draft.chars().count()),
        theme,
    );
    let controls = view_dialog_button_controls(dialog.mode);
    render_view_dialog_buttons(
        frame,
        app,
        popup,
        dialog.control,
        dialog.mode,
        &controls,
        theme,
    );
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

fn render_view_dialog_buttons(
    frame: &mut Frame<'_>,
    app: &mut App,
    popup: Rect,
    focused: crate::app::ViewDialogControl,
    mode: crate::app::ViewDialogMode,
    controls: &[(crate::app::ViewDialogControl, &str)],
    theme: Theme,
) {
    let area = Rect::new(
        popup.x + 1,
        popup.bottom().saturating_sub(4),
        popup.width.saturating_sub(2),
        3,
    );
    let labels = controls.iter().map(|(_, label)| *label).collect::<Vec<_>>();
    let focused_index = controls.iter().position(|(control, _)| *control == focused);
    for (index, rect) in button_layout(area, &labels, focused_index) {
        let (control, label) = controls[index];
        app.hit_regions.view_dialog_controls.push((rect, control));
        let selected =
            matches!(control, crate::app::ViewDialogControl::Mode(value) if value == mode);
        render_button(frame, rect, label, control == focused, selected, theme);
    }
}

fn render_source_dialog(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let cursor = app.active_text_cursor();
    app.hit_regions.path_completion_rows.clear();
    let popup = centered(area, 90, 18);
    clear_themed(frame, popup, theme);
    app.hit_regions.source_controls.clear();
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    let Some(dialog) = &app.source_dialog else {
        return;
    };
    if dialog.mode == crate::app::SourceDialogMode::Ai {
        let ai = &dialog.ai;
        let content = source_content_popup(popup);
        let body = dialog_body(content);
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(2),
            Constraint::Length(1),
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
        frame.render_widget(
            Paragraph::new(format!("{state_label}: {}", ai.progress))
                .wrap(Wrap { trim: false })
                .style(state_style)
                .block(
                    Block::default()
                        .title(" State ")
                        .borders(Borders::ALL)
                        .border_style(state_style),
                ),
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
            lines.push(String::new());
            lines.extend(review.into_iter().skip(ai.preview_scroll).take(8));
        }
        if let Some(session) = &ai.session_id {
            lines.push(format!("Local session: {session}"));
        }
        frame.render_widget(
            Paragraph::new(lines.join("\n"))
                .wrap(Wrap { trim: false })
                .style(styles.description)
                .block(Block::default().title(" Preview ").borders(Borders::ALL)),
            rows[3],
        );
        frame.render_widget(
            Paragraph::new("Describe a source; review is required before capture starts.")
                .style(styles.description),
            rows[4],
        );
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
        return;
    }
    if dialog.mode == crate::app::SourceDialogMode::Discovery {
        app.hit_regions.discovery_rows.clear();
        let indices = crate::app::filtered_discovery_indices(&dialog.discovery);
        let block = Block::default()
            .title(" Discover sources — selection never auto-starts ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent));
        let inner = dialog_body(source_content_popup(popup));
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
    let body = dialog_body(content_popup);
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

fn source_content_popup(mut popup: Rect) -> Rect {
    popup.height = popup.height.saturating_sub(2);
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
        crate::app::SourceDialogMode::Ai => {}
    }
    let area = Rect::new(
        popup.x.saturating_add(2),
        popup.bottom().saturating_sub(2),
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
    let visible = format!("{before}{after}");
    frame.render_widget(
        Paragraph::new(visible.as_str())
            .style(Style::default().fg(theme.input_fg).bg(theme.input_bg)),
        field,
    );
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

fn render_dialog_text(frame: &mut Frame<'_>, popup: Rect, title: &str, text: String, theme: Theme) {
    frame.render_widget(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        popup,
    );
    frame.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }),
        dialog_body(popup),
    );
}

struct InputSurface {
    style: Style,
}

impl Widget for InputSurface {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                buffer[(x, y)].set_style(self.style);
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
