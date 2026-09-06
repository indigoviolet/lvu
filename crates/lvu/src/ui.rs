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
    json_spans::{JsonKind, JsonSpan, classify},
    provider::RowProvider,
    theme::Theme,
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
    use crate::app::{CommandEnrichmentField as Field, CommandEnrichmentRunState as RunState};
    let Some(dialog) = app.command_enrichment_dialog.clone() else {
        return;
    };
    let popup = centered(area, 86, 24);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    frame.render_widget(
        Block::default()
            .title(" Command enrichment · runs only when confirmed ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        popup,
    );
    let (footer, footer_text) = if dialog.run_state == RunState::SavingResults {
        (Rect::default(), String::new())
    } else {
        adaptive_footer(
            popup,
            "Ctrl-S save · Ctrl-R review run · Alt-Delete remove · Alt-N new line",
            "Ctrl-S save · Ctrl-R review · Alt-Delete remove · Alt-N new line",
            3,
        )
    };
    let body = dialog_body_with_footer(popup, footer.height);
    let rows = Layout::vertical([
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
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(help, Style::default().fg(theme.muted)),
            ]),
            Rect::new(row.x, row.y, row.width, 1),
        );
        let input = Rect::new(row.x, row.y.saturating_add(1), row.width, 1);
        InputSurface {
            style: Style::default().fg(theme.input_fg).bg(theme.input_bg),
        }
        .render(input, frame.buffer_mut());
        let final_line = value.rsplit('\n').next().unwrap_or("");
        frame.render_widget(
            Paragraph::new(input_tail(
                final_line,
                usize::from(input.width.saturating_sub(1)),
            ))
            .style(Style::default().fg(theme.input_fg).bg(theme.input_bg)),
            input,
        );
        if dialog.selected_field == *field
            && !app.dialog_scroll_focused
            && !matches!(
                dialog.run_state,
                RunState::Saving
                    | RunState::Preparing
                    | RunState::Running
                    | RunState::SavingResults
            )
        {
            place_input_cursor(frame, input, 0, 0, final_line, theme);
        }
    }
    let accepted =
        dialog
            .accepted
            .as_ref()
            .map_or("None · native steps still apply".into(), |stage| {
                let crate::app::CommandEnrichmentStage { definition, .. } = stage;
                match &definition.program {
                    lvu_core::CommandProgram::Exec { executable, args } => format!(
                        "After {} native step(s): {} ({} arguments)",
                        app.view_state().map_or(0, |state| state.enrichments.len()),
                        executable.display(),
                        args.len()
                    ),
                    lvu_core::CommandProgram::Shell { .. } => "Invalid saved command form".into(),
                }
            });
    frame.render_widget(
        Paragraph::new(format!("Accepted command step: {accepted}"))
            .style(Style::default().fg(theme.base_fg)),
        rows[4],
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
    status.push_str("\nResults appear in Details as command.<field>; command.status shows Ready or Pending. Native filters and field choices use only the native steps above.");
    let status_p = Paragraph::new(status)
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(
            if dialog.error.is_some() || dialog.run_state == RunState::Error {
                theme.severity.error
            } else {
                theme.muted
            },
        ));
    let status_block = Block::default()
        .title(" Status and review · ↑/↓ when focused ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if app.dialog_scroll_focused {
            theme.focused_input_border
        } else {
            theme.accent
        }));
    let status_inner = status_block.inner(rows[5]);
    app.dialog_scroll_limit = status_p
        .line_count(status_inner.width)
        .saturating_sub(usize::from(status_inner.height));
    app.dialog_scroll = app.dialog_scroll.min(app.dialog_scroll_limit);
    app.hit_regions.dialog_scroll = Some(rows[5]);
    frame.render_widget(
        status_p
            .scroll((app.dialog_scroll.min(u16::MAX as usize) as u16, 0))
            .block(status_block),
        rows[5],
    );
    if footer.height > 0 {
        render_action_footer(frame, footer, &footer_text, theme);
    }
}

fn render_bookmarks(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    app.hit_regions.bookmark_rows.clear();
    let mut hitboxes = Vec::new();
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
    let body = dialog_body(popup);
    if let Some(id) = &dialog.editing {
        let text = format!(
            "Record {id}\nNote (1024 bytes): {}\n\n{}",
            dialog.draft, dialog.status
        );
        frame.render_widget(Paragraph::new(text), body);
        place_input_cursor(frame, body, 1, 19, &dialog.draft, theme);
    } else {
        let count = usize::from(body.height.saturating_sub(2)).max(1);
        let first = dialog.selected.saturating_sub(count.saturating_sub(1));
        let mut lines = vec![Line::raw(format!(
            "{} / 128 bookmarks · {}",
            bookmarks.len(),
            dialog.status
        ))];
        for (index, bookmark) in bookmarks.iter().enumerate().skip(first).take(count) {
            let y = body
                .y
                .saturating_add(1)
                .saturating_add((index - first) as u16);
            if y < body.bottom() {
                hitboxes.push((Rect::new(body.x, y, body.width, 1), index));
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
                clipped_width(&text, usize::from(body.width)),
                if index == dialog.selected {
                    Style::default()
                        .fg(theme.selection_fg)
                        .bg(theme.selection_bg)
                } else {
                    Style::default().fg(theme.base_fg).bg(theme.dialog_bg)
                },
            ));
        }
        frame.render_widget(Paragraph::new(lines), body);
        render_dialog_footer(
            frame,
            popup,
            "↑/↓ select · raw context · Alt-E note · Alt-D remove",
            theme,
        );
    }
    app.hit_regions.bookmark_rows = hitboxes;
}

fn render_context<P: RowProvider>(
    frame: &mut Frame<'_>,
    app: &mut App,
    provider: &P,
    area: Rect,
    theme: Theme,
) {
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
    let body = dialog_body(popup);
    let len = usize::from(body.height.saturating_sub(2)).min(32);
    let page = provider.context_page(&dialog.view_id, &dialog.anchor, dialog.offset, len);
    let status = page.diagnostic.as_deref().unwrap_or(if page.pending {
        "loading raw context…"
    } else {
        "physical source records"
    });
    let mut lines = vec![
        Line::raw(clipped_width(
            &format!("anchor {} · {}", dialog.anchor, status),
            usize::from(body.width),
        )),
        Line::raw(format!(
            "{}–{} / {} · raw, unfiltered, ungrouped",
            page.start.saturating_add(1).min(page.total),
            page.start.saturating_add(page.rows.len()),
            page.total
        )),
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
                Style::default()
                    .fg(theme.selection_fg)
                    .bg(theme.selection_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.base_fg).bg(theme.dialog_bg)
            },
        ));
    }
    if let Some(anchor_position) = page.anchor_position
        && let Some(dialog) = &mut app.context_dialog
    {
        dialog.offset = (page.start as isize).saturating_sub(anchor_position as isize);
    }
    frame.render_widget(Paragraph::new(lines), body);
    render_dialog_footer(frame, popup, "↑/↓ scroll · g anchor", theme);
}

fn render_settings(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let popup = centered(area, 104, 24);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    let Some(dialog) = &app.settings_dialog else {
        return;
    };
    let selected = dialog.selected;
    let values = &dialog.draft;
    let agent_label = if app.ascii { "Agent" } else { "🧠" };
    let rows = [
        (
            format!("{agent_label} provider/model"),
            values.provider.clone(),
        ),
        ("Agent mode".into(), values.mode.clone()),
        ("Thinking effort".into(), values.thinking.clone()),
        ("Theme".into(), values.theme.as_str().into()),
        ("Delight".into(), values.delight_enabled.to_string()),
        ("Reduced motion".into(), values.reduced_motion.to_string()),
        ("ASCII".into(), values.ascii.to_string()),
        ("Row cache MiB".into(), values.rows_mib.clone()),
        ("Membership MiB".into(), values.membership_mib.clone()),
        ("Derived total MiB".into(), values.disk_total_mib.clone()),
        (
            "Index/source MiB".into(),
            values.index_per_source_mib.clone(),
        ),
    ];
    let mut lines = Vec::new();
    for (index, (label, value)) in rows.into_iter().enumerate() {
        let marker = if index == selected { ">" } else { " " };
        lines.push(Line::styled(
            format!(
                "{marker} {label}{} {}",
                " ".repeat(22usize.saturating_sub(UnicodeWidthStr::width(label.as_str()))),
                clipped_width(&value, usize::from(popup.width.saturating_sub(27)))
            ),
            if index == selected {
                Style::default()
                    .fg(theme.selection_fg)
                    .bg(theme.selection_bg)
            } else {
                Style::default().fg(theme.base_fg).bg(theme.base_bg)
            },
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw(format!(
        "effective {agent_label}: {} [{}] · {} [{}] · {} [{}]",
        dialog.context.effective_provider,
        dialog.context.provider_source,
        dialog.context.effective_mode,
        dialog.context.mode_source,
        dialog.context.effective_thinking,
        dialog.context.thinking_source,
    )));
    lines.push(Line::raw(format!(
        "effective appearance: theme {} · delight {} [{}] · motion {} [{}] · ASCII {} [{}]",
        dialog.context.effective_theme.as_str(),
        dialog.context.effective_delight_enabled,
        dialog.context.delight_source,
        dialog.context.effective_reduced_motion,
        dialog.context.reduced_motion_source,
        dialog.context.effective_ascii,
        dialog.context.ascii_source,
    )));
    lines.push(Line::raw(format!(
        "startup-applied MiB: rows {} · membership {} · total derived {} · index/source {}",
        dialog.context.applied_rows_mib,
        dialog.context.applied_membership_mib,
        dialog.context.applied_disk_total_mib,
        dialog.context.applied_index_per_source_mib,
    )));
    lines.push(Line::raw(format!(
        "settings: {}",
        dialog.context.settings_path
    )));
    lines.push(Line::raw(format!(
        "data: {} · cache: {}",
        dialog.context.data_path, dialog.context.cache_path
    )));
    lines.push(Line::raw(format!(
        "capture: {}",
        dialog.context.capture_path
    )));
    lines.push(Line::styled(
        dialog.status.clone(),
        Style::default().fg(if dialog.status.contains("failed") {
            theme.severity.error
        } else {
            theme.muted
        }),
    ));
    lines.push(Line::raw(
        "↑/↓ field · type/backspace edit · Space toggle/cycle",
    ));
    let block = Block::default()
        .title(" Settings · global settings.toml ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent));
    frame.render_widget(block, popup);
    let body = dialog_body(popup);
    let visible = usize::from(body.height);
    let top = selected
        .saturating_add(1)
        .saturating_sub(visible)
        .min(lines.len().saturating_sub(visible));
    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .skip(top)
                .take(visible)
                .collect::<Vec<_>>(),
        ),
        body,
    );
    let editable = match crate::app::SettingsField::ALL[selected] {
        crate::app::SettingsField::Provider => Some(values.provider.as_str()),
        crate::app::SettingsField::Mode => Some(values.mode.as_str()),
        crate::app::SettingsField::Thinking => Some(values.thinking.as_str()),
        crate::app::SettingsField::RowCache => Some(values.rows_mib.as_str()),
        crate::app::SettingsField::Membership => Some(values.membership_mib.as_str()),
        crate::app::SettingsField::DiskTotal => Some(values.disk_total_mib.as_str()),
        crate::app::SettingsField::IndexPerSource => Some(values.index_per_source_mib.as_str()),
        crate::app::SettingsField::Theme
        | crate::app::SettingsField::Delight
        | crate::app::SettingsField::ReducedMotion
        | crate::app::SettingsField::Ascii => None,
    };
    if let Some(value) = editable {
        place_input_cursor(frame, body, selected.saturating_sub(top), 25, value, theme);
    }
    render_dialog_footer(frame, popup, "↑/↓ field · type edit · Space toggle", theme);
}

fn render_storage(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
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
    let (footer, footer_text) = adaptive_footer(
        popup,
        "↑/↓ active pane · r refresh · c preview/confirm cleanup",
        "↑/↓ pane · r refresh · c cleanup",
        2,
    );
    let inner = dialog_body_with_footer(popup, footer.height);
    frame.render_widget(block, popup);
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
    frame.render_widget(Paragraph::new(budget), header);
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
        .style(Style::default().fg(if errors.is_empty() {
            theme.muted
        } else {
            theme.severity.error
        }));
    let status_block = Block::default()
        .title(" Status · ↑/↓ when focused ")
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
    render_action_footer(frame, footer, &footer_text, theme);
}

fn render_time_editor(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    use crate::app::{TimeControl as C, TimeDropdown as D, TimeWindowChoice as W};
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
            .border_style(Style::default().fg(theme.accent)),
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
    let recognize = if app.ascii {
        "[ Agent Recognize timestamp ]"
    } else {
        "[ 🧠 Recognize timestamp ]"
    };
    let mut rows = vec![
        (Some(C::Basis), format!("Time basis: {basis} ▾")),
        (Some(C::Window), format!("Window: {window} ▾")),
        (
            Some(C::StartDate),
            format!("Start date: {}", dialog.start_date),
        ),
        (
            Some(C::StartClock),
            format!("Start time: {}", dialog.start_clock),
        ),
        (
            Some(C::StartZone),
            format!("Start timezone: {}", dialog.start_zone),
        ),
        (Some(C::EndDate), format!("End date: {}", dialog.end_date)),
        (Some(C::EndClock), format!("End time: {}", dialog.end_clock)),
        (
            Some(C::EndZone),
            format!("End timezone: {}", dialog.end_zone),
        ),
        (Some(C::Apply), "Apply".into()),
        (Some(C::Clear), "Clear".into()),
        (
            Some(C::Recognize),
            recognize.trim_matches(['[', ']']).trim().into(),
        ),
    ];
    let help_width = usize::from(inner.width.max(1));
    for line in wrap_time_text(&format!("Applied: {applied}"), help_width) {
        rows.push((None, line));
    }
    if updating {
        rows.push((None, "Updating: last applied window remains active".into()));
    }
    if let Some(error) = &state.time_error {
        for line in wrap_time_text(&format!("Error: {error}"), help_width) {
            rows.push((None, line));
        }
    }
    for line in wrap_time_text(reason, help_width) {
        rows.push((None, line));
    }
    let viewport = Rect::new(
        inner.x,
        inner.y.saturating_add(1),
        inner.width,
        inner.height.saturating_sub(2),
    );
    let visible = usize::from(viewport.height);
    let max_scroll = rows.len().saturating_sub(visible);
    let focus_row = rows
        .iter()
        .position(|(control, _)| *control == Some(dialog.focus))
        .unwrap_or_else(|| {
            if dialog.focus == C::ScrollDown {
                rows.len().saturating_sub(1)
            } else {
                0
            }
        });
    let mut scroll = dialog.scroll.min(max_scroll);
    if dialog.reveal_focus {
        if focus_row < scroll {
            scroll = focus_row;
        }
        if focus_row >= scroll.saturating_add(visible) {
            scroll = focus_row + 1 - visible;
        }
    }
    if let Some(current) = &mut app.time_dialog {
        current.scroll = scroll;
        current.reveal_focus = false;
    }
    for (row, (control, text)) in rows.iter().enumerate().skip(scroll).take(visible) {
        let rect = Rect::new(
            viewport.x,
            viewport.y + (row - scroll) as u16,
            viewport.width,
            1,
        );
        let focused = *control == Some(dialog.focus);
        let segment = control.and_then(|control| match control {
            C::StartDate => Some(("Start date", dialog.start_date.as_str())),
            C::StartClock => Some(("Start time", dialog.start_clock.as_str())),
            C::StartZone => Some(("Start timezone", dialog.start_zone.as_str())),
            C::EndDate => Some(("End date", dialog.end_date.as_str())),
            C::EndClock => Some(("End time", dialog.end_clock.as_str())),
            C::EndZone => Some(("End timezone", dialog.end_zone.as_str())),
            _ => None,
        });
        if let Some((label, value)) = segment {
            let label_width = (label.width() as u16 + 2).min(rect.width);
            let input_rect = Rect::new(
                rect.x + label_width,
                rect.y,
                rect.width.saturating_sub(label_width),
                1,
            );
            frame.render_widget(
                Paragraph::new(format!("{label}:")),
                Rect::new(rect.x, rect.y, label_width, 1),
            );
            let caret = if focused {
                dialog.segment_cursor.min(value.chars().count())
            } else {
                value.chars().count()
            };
            let (visible_value, caret_column) =
                time_input_window(value, caret, usize::from(input_rect.width));
            frame.render_widget(
                Paragraph::new(visible_value)
                    .style(Style::default().fg(theme.input_fg).bg(theme.input_bg)),
                input_rect,
            );
            if focused && input_rect.width > 0 {
                let x = input_rect
                    .x
                    .saturating_add(caret_column as u16)
                    .min(input_rect.right().saturating_sub(1));
                frame.render_widget(
                    Block::default().style(Style::default().bg(theme.cursor)),
                    Rect::new(x, rect.y, 1, 1),
                );
                frame.set_cursor_position((x, rect.y));
            }
            if let Some(control) = control {
                app.hit_regions.time_controls.push((input_rect, *control));
            }
            continue;
        }
        let style = if focused {
            Style::default()
                .fg(theme.input_fg)
                .bg(theme.input_bg)
                .add_modifier(Modifier::BOLD)
        } else if text.starts_with("Error:") {
            Style::default().fg(theme.severity.error)
        } else {
            Style::default().fg(theme.base_fg)
        };
        frame.render_widget(Paragraph::new(text.as_str()).style(style), rect);
        if let Some(control) = control {
            app.hit_regions.time_controls.push((rect, *control));
        }
    }
    for (control, rect, label, enabled) in [
        (
            C::ScrollUp,
            Rect::new(inner.x, inner.y, inner.width, 1),
            "▲ More",
            scroll > 0,
        ),
        (
            C::ScrollDown,
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
            "▼ More",
            scroll < max_scroll,
        ),
    ] {
        let style = if dialog.focus == control {
            Style::default()
                .fg(theme.input_fg)
                .bg(theme.input_bg)
                .add_modifier(Modifier::BOLD)
        } else if enabled {
            Style::default().fg(theme.accent)
        } else {
            Style::default().fg(theme.muted)
        };
        frame.render_widget(Paragraph::new(label).style(style), rect);
        app.hit_regions.time_controls.push((rect, control));
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
        };
        let selected = dialog.highlighted.min(choices.len().saturating_sub(1));
        let anchor_row: usize = if dropdown == D::Basis { 0 } else { 1 };
        let y = viewport.y + anchor_row.saturating_sub(scroll) as u16 + 1;
        let w = choices
            .iter()
            .map(|s| s.as_str().width())
            .max()
            .unwrap_or(1) as u16
            + 4;
        let dropdown_x = inner
            .x
            .saturating_add(12.min(inner.width.saturating_sub(1)));
        let box_area = Rect::new(
            dropdown_x,
            y.min(viewport.bottom()),
            w.min(inner.right().saturating_sub(dropdown_x)),
            (choices.len() as u16 + 2).min(viewport.bottom().saturating_sub(y)),
        );
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
                            Style::default()
                                .fg(theme.input_fg)
                                .bg(theme.input_bg)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(theme.base_fg).bg(theme.base_bg)
                        })
                    })
                    .collect::<Vec<_>>(),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.accent)),
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
    let popup = centered(area, 84, 20);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    let Some(dialog) = &app.recipe_dialog else {
        return;
    };
    let (footer, footer_text) = adaptive_footer(
        popup,
        "Alt-B browse · Alt-S save · Alt-I import · Alt-E export · Alt-H history · Alt-U update · Alt-G refresh · Alt-A adapt · x reject",
        "Alt-B browse · Alt-S save · Alt-I import · Alt-E export · Alt-H history · Alt-U update · Alt-G refresh · Alt-A adapt · x reject",
        5,
    );
    let body = dialog_body_with_footer(popup, footer.height);
    let mut lines = vec![format!("Mode: {:?}", dialog.mode)];
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
            "{label}: {}_",
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
    lines.push(format!("Status: {}", dialog.status));
    frame.render_widget(
        Block::default()
            .title(" Named recipes ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        popup,
    );
    frame.render_widget(
        Paragraph::new(lines.join("\n")).wrap(Wrap { trim: false }),
        body,
    );
    if dialog.mode.is_editable() {
        place_input_cursor(
            frame,
            body,
            1,
            if dialog.mode == crate::app::RecipeDialogMode::Save {
                6
            } else {
                11
            },
            &dialog.name,
            theme,
        );
    }
    render_action_footer(frame, footer, &footer_text, theme);
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
    let row = app.selected_row(provider);
    let row_id = row.as_ref().map(|row| row.id.clone());
    let mut lines = Vec::new();
    if let Some(row) = row {
        lines.push(Line::from(vec![
            Span::styled("stable display id: ", Style::default().fg(theme.muted)),
            Span::styled(row.id.to_string(), Style::default().fg(theme.accent)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("raw: ", Style::default().fg(theme.muted)),
            Span::styled(row.text, Style::default().fg(theme.base_fg)),
        ]));
        for (key, value) in row.fields.into_iter().chain(row.details) {
            let status = key == "command.status";
            let value_color = if status
                && value
                    .split_whitespace()
                    .next()
                    .is_some_and(|word| word.eq_ignore_ascii_case("pending"))
            {
                theme.severity.warn
            } else if status {
                theme.severity.info
            } else {
                theme.base_fg
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{key}: "),
                    Style::default().fg(if key.starts_with("command.") {
                        theme.accent
                    } else {
                        theme.muted
                    }),
                ),
                Span::styled(value, Style::default().fg(value_color)),
            ]));
        }
    } else {
        lines.push(Line::styled(
            "No selected event",
            Style::default().fg(theme.muted),
        ));
    }
    let block = Block::default()
        .title(" Selected event details ")
        .borders(Borders::ALL)
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
        let footer_text = "↑/↓ scroll";
        frame.render_widget(
            Paragraph::new(footer_text).style(
                Style::default()
                    .fg(theme.focused_input_border)
                    .bg(theme.input_bg)
                    .add_modifier(Modifier::BOLD),
            ),
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
    let popup = centered(area, 70, 16);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    app.hit_regions.field_picker_rows.clear();
    let inner = popup.inner(ratatui::layout::Margin::new(1, 1));
    frame.render_widget(
        Block::default()
            .title(" Event fields ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        popup,
    );
    let row = app.field_picker_row(provider);
    let has_anchor = app.field_picker_row_id().is_some();
    let footer_text = if row.as_ref().is_some_and(|row| !row.fields.is_empty()) {
        Some("↑/↓ select · Space pin · c Color rows by this field")
    } else if has_anchor {
        Some("o raw context")
    } else {
        None
    };
    let body = if let Some(footer_text) = footer_text {
        let footer_lines = wrap_actions(footer_text, usize::from(inner.width));
        let footer_height =
            (footer_lines.len().max(1) as u16).min(inner.height.saturating_sub(1).max(1));
        let footer = Rect::new(
            inner.x,
            inner.bottom().saturating_sub(footer_height),
            inner.width,
            footer_height,
        );
        render_action_footer(frame, footer, &footer_lines.join("\n"), theme);
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
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(theme.base_fg)),
            body,
        );
        return;
    };
    if row.fields.is_empty() {
        frame.render_widget(
            Paragraph::new("No fields found for this event")
                .style(Style::default().fg(theme.base_fg)),
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
                Style::default()
                    .fg(theme.selection_fg)
                    .bg(theme.selection_bg)
            } else {
                Style::default()
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
    if app.focus == Focus::EnrichmentEditor && dialog_body(popup).height >= 14 {
        render_enrichment_workspace(frame, app, provider, area, popup, theme);
        return;
    }
    let (title, guidance) = match app.focus {
        Focus::SearchEditor => (
            " Search ",
            r#"text · "field name": text · /regex/ims · \/literal · pl.col(...) predicate"#,
        ),
        Focus::AdvancedEditor => (
            " Advanced Polars filter ",
            "Sampled fields and string values are available as completion candidates.",
        ),
        Focus::EnrichmentEditor => (
            " Native enrichment ",
            "output_name = Polars expression; raw is original input; sampled source fields and values are available",
        ),
        Focus::GroupingEditor => (
            " Display-only multiline grouping ",
            r"Rust regex over raw bytes; ^ anchors to line start; applying empty disables",
        ),
        Focus::Selector
        | Focus::Logs
        | Focus::Details
        | Focus::SourceDialog
        | Focus::Help
        | Focus::ViewDialog
        | Focus::FieldPicker
        | Focus::AskAi
        | Focus::Investigation
        | Focus::CommandEnrichment => return,
        Focus::Recipes
        | Focus::TimeEditor
        | Focus::Storage
        | Focus::Settings
        | Focus::Context
        | Focus::Bookmarks => {
            return;
        }
    };
    let message = editor.error.as_deref().unwrap_or(guidance);
    let mut draft_row = 1;
    let mut text = format!("Draft:\n\n\napplied: {}\n{}", editor.applied, message);
    if app.focus == Focus::EnrichmentEditor {
        let state = app.view_state().expect("active enrichment view");
        let enrichments = state.enrichments.clone();
        let editing = state.enrichment_editing.clone();
        let selected = state
            .enrichment_selected
            .min(enrichments.len().saturating_sub(1));
        app.hit_regions.enrichment_rows.clear();
        let body = dialog_body(popup);
        let mut lines = Vec::new();
        if body.height >= 5 {
            lines
                .push("Accepted stages (ordered; later stages may use earlier fields):".to_owned());
            let visible = usize::from(body.height.saturating_sub(4)).clamp(1, 4);
            let top = selected.saturating_sub(visible.saturating_sub(1));
            for (position, (index, stage)) in enrichments
                .iter()
                .enumerate()
                .skip(top)
                .take(visible)
                .enumerate()
            {
                lines.push(format!(
                    "{} {}. {}",
                    if index == selected { ">" } else { " " },
                    index + 1,
                    clipped_width(&stage.source, usize::from(body.width.saturating_sub(6)))
                ));
                app.hit_regions.enrichment_rows.push((
                    Rect::new(body.x, body.y + 1 + position as u16, body.width, 1),
                    index,
                ));
            }
            if enrichments.is_empty() {
                lines.push("  (none yet)".into());
            }
            lines.push(format!(
                "Mode: {}",
                editing.as_ref().map_or("ADD", |_| "EDIT SELECTED")
            ));
        }
        lines.push("Draft — /regex (?P<name>...)/ or name = Polars Expr:".into());
        draft_row = lines.len();
        lines.push(String::new());
        lines.push(message.to_owned());
        text = lines.join("\n");
        text.push('\n');
        if let Some(row) = app.selected_row(provider) {
            text.push_str("Raw input before enrichment: ");
            text.push_str(&clipped_width(
                &row.text,
                usize::from(popup.width.saturating_sub(25)),
            ));
            let derived = row
                .details
                .iter()
                .filter(|(name, _)| name.starts_with("derived."))
                .collect::<Vec<_>>();
            if derived.is_empty() {
                text.push_str("\nDerived outputs after accepted stages: none");
            } else {
                for (name, value) in derived.iter().take(3) {
                    text.push('\n');
                    text.push_str(name);
                    text.push_str(": ");
                    text.push_str(&clipped_width(
                        value,
                        usize::from(popup.width.saturating_sub(name.len() as u16 + 4)),
                    ));
                }
                if derived.len() > 3 {
                    text.push_str(&format!("\n… {} more derived outputs", derived.len() - 3));
                }
            }
        } else {
            text.push_str("Raw input / named output preview: no selected record");
        }
        if !editor.draft.is_empty() {
            text.push_str("\nCandidate after: submit to evaluate");
        }
    }
    if app.focus == Focus::GroupingEditor {
        text.push_str(
            "\n\nExample preview (display only):\nRuntimeException: boom\n  at worker.rs:42\n=> RuntimeException: boom  [2 physical lines]",
        );
    }
    render_dialog_text(frame, popup, title, text, theme);
    if app.editor_completion.is_none() && !app.dialog_scroll_focused {
        place_input_cursor(
            frame,
            dialog_body(popup),
            draft_row,
            0,
            &editor.draft,
            theme,
        );
    }
    render_dialog_footer(
        frame,
        popup,
        if app.focus == Focus::EnrichmentEditor {
            "Alt-A add native · Alt-E edit native · Alt-R remove native · Alt-C command step · Alt-J/K select"
        } else if app.focus == Focus::SearchEditor {
            "Apply now"
        } else {
            "Apply · sampled fields/values available"
        },
        theme,
    );
    render_editor_completion(frame, app, area, theme);
}

fn render_simple_editor(
    frame: &mut Frame<'_>,
    app: &mut App,
    area: Rect,
    popup: Rect,
    editor: crate::app::EditorState,
    theme: Theme,
) {
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
    let (footer, footer_text) = (Rect::default(), String::new());
    let body = dialog_body_with_footer(popup, footer.height);
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
        Paragraph::new(if search { "" } else { "FILTER EXPRESSION" }).style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        rows[0],
    );
    InputSurface {
        style: Style::default().fg(theme.input_fg).bg(theme.input_bg),
    }
    .render(rows[1], frame.buffer_mut());
    frame.render_widget(
        Paragraph::new(input_tail(&editor.draft, usize::from(rows[1].width)))
            .style(Style::default().fg(theme.input_fg).bg(theme.input_bg)),
        rows[1],
    );
    if app.editor_completion.is_none() && !app.dialog_scroll_focused {
        place_input_cursor(frame, rows[1], 0, 0, &editor.draft, theme);
    }
    let help = if search {
        r#"Examples: text · "field name": text · /regex/ims · \/literal"#
    } else {
        "Use a native expression. Sampled fields and values are available as completions."
    };
    frame.render_widget(
        Paragraph::new(help)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(theme.base_fg)),
        rows[3],
    );
    let (label, value, color) = if let Some(error) = editor.error.as_deref() {
        ("Error", error, theme.severity.error)
    } else if editor.pending_generation.is_some() {
        (
            "Updating…",
            "Checking this draft; the last applied view remains visible.",
            theme.accent,
        )
    } else if !editor.applied.is_empty() {
        ("Applied", editor.applied.as_str(), theme.severity.info)
    } else {
        ("Status", "No filter applied.", theme.muted)
    };
    let mut status_lines = vec![Line::from(vec![
        Span::styled(
            format!("{label}  "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(value.to_owned(), Style::default().fg(theme.base_fg)),
    ])];
    if !editor.applied.is_empty() && editor.draft != editor.applied {
        status_lines.push(Line::from(vec![
            Span::styled(
                "Last accepted  ",
                Style::default()
                    .fg(theme.severity.info)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(editor.applied.clone(), Style::default().fg(theme.base_fg)),
        ]));
    }
    let status = Paragraph::new(status_lines).wrap(Wrap { trim: false });
    let mut status_area = rows[2];
    if status_area.height > 1 {
        frame.render_widget(
            Paragraph::new("Status · ↑/↓").style(Style::default().fg(
                if app.dialog_scroll_focused {
                    theme.focused_input_border
                } else {
                    theme.muted
                },
            )),
            Rect::new(status_area.x, status_area.y, status_area.width, 1),
        );
        status_area.y += 1;
        status_area.height -= 1;
    }
    app.hit_regions.dialog_scroll = Some(rows[2]);
    app.dialog_scroll_limit = status
        .line_count(status_area.width)
        .saturating_sub(usize::from(status_area.height));
    app.dialog_scroll = app.dialog_scroll.min(app.dialog_scroll_limit);
    frame.render_widget(
        status.scroll((app.dialog_scroll.min(u16::MAX as usize) as u16, 0)),
        status_area,
    );
    if footer.height > 0 {
        render_action_footer(frame, footer, &footer_text, theme);
    }
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
    let state = app.view_state().expect("enrichment view");
    let editor = state.enrichment.clone();
    let stages = state.enrichments.clone();
    let selected = state
        .enrichment_selected
        .min(stages.len().saturating_sub(1));
    let editing = state.enrichment_editing.is_some();
    render_dialog_text(
        frame,
        popup,
        " Native enrichment · extracted fields ",
        String::new(),
        theme,
    );
    let body = dialog_body(popup);
    let sections = Layout::vertical([
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
    let stage_area = sections[0];
    let stage_inner = panel("").inner(stage_area);
    let mut stage_lines = Vec::new();
    if stages.is_empty() {
        stage_lines.push(Line::from(
            "No extracted fields yet. Add an expression below.",
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
                Style::default()
                    .fg(theme.selection_fg)
                    .bg(theme.selection_bg)
            } else {
                Style::default().fg(theme.base_fg).bg(theme.dialog_bg)
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
    let input_area = input.inner(sections[1]);
    frame.render_widget(input, sections[1]);
    InputSurface {
        style: Style::default().fg(theme.input_fg).bg(theme.input_bg),
    }
    .render(input_area, frame.buffer_mut());
    let lines = expression_lines(&editor.draft, usize::from(input_area.width));
    let top = lines.len().saturating_sub(usize::from(input_area.height));
    let visible = lines[top..].join("\n");
    frame.render_widget(
        Paragraph::new(visible).style(Style::default().fg(theme.input_fg).bg(theme.input_bg)),
        input_area,
    );
    if app.editor_completion.is_none()
        && !app.dialog_scroll_focused
        && input_area.width > 0
        && input_area.height > 0
    {
        let x = input_area.x
            + UnicodeWidthStr::width(lines.last().unwrap().as_str())
                .min(usize::from(input_area.width - 1)) as u16;
        let y = input_area.y + (lines.len() - top - 1) as u16;
        frame.buffer_mut()[(x, y)].set_style(Style::default().bg(theme.cursor).fg(theme.input_fg));
        frame.set_cursor_position((x, y));
    }
    let message = editor.error.as_ref().map_or(
        "Applying this step keeps existing fields until a valid change succeeds.".to_owned(),
        |error| format!("Not applied — previous results retained. {error}"),
    );
    let status_area = sections[2];
    let status_inner = Rect::new(
        status_area.x,
        status_area.y.saturating_add(1),
        status_area.width,
        status_area.height.saturating_sub(1),
    );
    frame.render_widget(
        Paragraph::new("Status · ↑/↓").style(Style::default().fg(if app.dialog_scroll_focused {
            theme.focused_input_border
        } else {
            theme.muted
        })),
        Rect::new(status_area.x, status_area.y, status_area.width, 1),
    );
    let status = Paragraph::new(message)
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(if editor.error.is_some() {
            theme.severity.error
        } else {
            theme.muted
        }));
    app.dialog_scroll_limit = status
        .line_count(status_inner.width)
        .saturating_sub(usize::from(status_inner.height));
    app.dialog_scroll = app.dialog_scroll.min(app.dialog_scroll_limit);
    app.hit_regions.dialog_scroll = Some(status_area);
    frame.render_widget(
        status.scroll((app.dialog_scroll.min(u16::MAX as usize) as u16, 0)),
        status_inner,
    );
    let samples = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(sections[3]);
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
            .block(panel(" Raw input before enrichment: ")),
        samples[0],
    );
    frame.render_widget(
        Paragraph::new(derived)
            .wrap(Wrap { trim: false })
            .block(panel(" Accepted output · same record ")),
        samples[1],
    );
    render_dialog_footer(
        frame,
        popup,
        "Alt-A add native · Alt-E edit native · Alt-R remove native · Alt-C command step · Alt-J/K select",
        theme,
    );
    render_editor_completion(frame, app, area, theme);
}

fn expression_lines(value: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let width = width.max(1);
    let mut lines = vec![String::new()];
    let mut column = 0;
    for ch in value.chars() {
        if ch == '\n' {
            lines.push(String::new());
            column = 0;
            continue;
        }
        if ch.is_control() {
            continue;
        }
        let size = ch.width().unwrap_or(0);
        if column + size > width {
            lines.push(String::new());
            column = 0;
        }
        lines.last_mut().unwrap().push(ch);
        column += size;
    }
    // Reserve a real cursor cell after the last character.
    if column >= width {
        lines.push(String::new());
    }
    lines
}

fn render_editor_completion(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let Some(completion) = &app.editor_completion else {
        return;
    };
    let popup = centered(area, 76, 12);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    let inner = Block::default()
        .title(match completion.kind {
            crate::app::EditorCompletionKind::Field => " Complete field ",
            crate::app::EditorCompletionKind::SampledValue => " Complete sampled string value ",
        })
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent));
    let content = dialog_body(popup);
    frame.render_widget(inner, popup);
    if content.height == 0 {
        return;
    }
    let visible = usize::from(content.height.saturating_sub(1)).max(1);
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
                Style::default()
                    .fg(theme.selection_fg)
                    .bg(theme.selection_bg)
            } else {
                Style::default()
            },
        ));
        app.hit_regions.editor_completion_rows.push((
            Rect::new(content.x, content.y + offset as u16, content.width, 1),
            index,
        ));
    }
    if completion.items.is_empty() {
        lines.push(Line::from("(no sampled completions)"));
    }
    lines.push(Line::from(completion.status.clone()));
    frame.render_widget(Paragraph::new(lines), content);
    render_dialog_footer(frame, popup, "↑/↓ select · fields/values completion", theme);
}

fn render_help(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let popup = centered(area, 94, area.height.saturating_sub(2).min(30));
    clear_themed(frame, popup, theme);
    let agent = if app.ascii { "Agent" } else { "🧠" };
    frame.render_widget(
        Block::default()
            .title(" Help ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        popup,
    );
    let (footer, footer_text) =
        adaptive_footer(popup, "↑/↓ or j/k scroll · ? closes", "↑/↓ · j/k · ?", 2);
    let body = dialog_body_with_footer(popup, footer.height);
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
    render_action_footer(frame, footer, &footer_text, theme);
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
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));
        for (key, description) in &section.entries {
            let padding = " ".repeat(key_width.saturating_sub(UnicodeWidthStr::width(*key)) + 2);
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {key}"),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(padding),
                Span::styled(description.clone(), Style::default().fg(theme.base_fg)),
            ]));
        }
    }
    lines
}

fn render_ask_ai(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let popup = centered(area, 88, 17);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    let Some(dialog) = &mut app.ask_ai_dialog else {
        return;
    };
    let kind = match dialog.kind {
        crate::app::AskAiKind::Filter => "FILTER",
        crate::app::AskAiKind::Enrichment => "ENRICHMENT",
        crate::app::AskAiKind::Recipe => "RECIPE ADAPTATION",
    };
    let mut text = format!(
        "Kind: {kind}   provider: {}   mode: {}   thinking: {}\n\nRequest:\n{}_\n\nStatus: {}",
        dialog.provider,
        dialog.mode,
        dialog.thinking,
        clipped_width(&dialog.prompt, usize::from(popup.width.saturating_sub(2))),
        dialog.progress
    );
    if let Some(expression) = &dialog.expression {
        text.push_str("\n\nProposal:\n");
        text.push_str(expression);
    }
    if dialog.kind == crate::app::AskAiKind::Recipe
        && dialog.stage == crate::app::AskAiStage::Proposal
        && let Some(recipe) = &dialog.recipe
    {
        text.push_str(&format!(
            "\nOrdered enrichments ({} stages):",
            recipe.enrichments.len()
        ));
        for (index, stage) in recipe.enrichments.iter().enumerate() {
            text.push_str(&format!(
                "\n{}. [{}]\n{}",
                index + 1,
                stage.id.0,
                stage.source
            ));
        }
        if recipe.enrichments.is_empty() && !recipe.enrichment.is_empty() {
            text.push_str(&format!("\n{}", recipe.enrichment));
        }
    }
    if let Some(explanation) = &dialog.explanation {
        text.push_str("\nWhy: ");
        text.push_str(explanation);
    }
    if let Some(session) = &dialog.session_id {
        text.push_str("\nSession: ");
        text.push_str(session);
    }
    if let Some(directory) = &dialog.snapshot_dir {
        text.push_str("\nSnapshot: ");
        text.push_str(directory);
    }
    if dialog.kind == crate::app::AskAiKind::Recipe {
        text.push_str(
            "\nScope: advanced filter and ordered enrichments; search/pins/colors/time/grouping are retained.",
        );
    }
    render_dialog_text(
        frame,
        popup,
        if app.ascii {
            " Ask Agent "
        } else {
            " Ask 🧠 "
        },
        String::new(),
        theme,
    );
    let (footer, footer_text) = adaptive_footer(
        popup,
        "Alt-F filter · Alt-E enrichment · Alt-T timestamp · ↑/↓ review",
        "Alt-F filter · Alt-E enrich · Alt-T time · ↑/↓",
        3,
    );
    let body = dialog_body_with_footer(popup, footer.height);
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    dialog.review_scroll_limit = paragraph
        .line_count(body.width)
        .saturating_sub(usize::from(body.height))
        .min(usize::from(u16::MAX)) as u16;
    dialog.review_scroll = dialog.review_scroll.min(dialog.review_scroll_limit);
    app.hit_regions.dialog_scroll = Some(body);
    frame.render_widget(paragraph.scroll((dialog.review_scroll, 0)), body);
    if !app.dialog_scroll_focused
        && matches!(
            dialog.stage,
            crate::app::AskAiStage::Input | crate::app::AskAiStage::Error
        )
    {
        place_input_cursor(frame, body, 3, 0, &dialog.prompt, theme);
    }
    render_action_footer(frame, footer, &footer_text, theme);
}

fn render_investigation(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let popup = centered(area, 100, 22);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    let Some(dialog) = &app.investigation_dialog else {
        return;
    };
    let mut lines = vec![format!("Status: {}", dialog.progress)];
    if let Some(session) = &dialog.session_id {
        lines.push(format!("Session: {session}"));
    }
    if let Some(snapshot) = &dialog.snapshot_dir {
        lines.push(format!("Snapshot: {snapshot}"));
    }
    if dialog.stage == crate::app::InvestigationStage::Input && !dialog.items.is_empty() {
        lines.push("Saved investigations available when the question is empty:".into());
        let top = dialog.selected.saturating_sub(4);
        for (index, item) in dialog.items.iter().enumerate().skip(top).take(5) {
            let marker = if index == dialog.selected { ">" } else { " " };
            lines.push(clipped_width(
                &format!("{marker} {} — {}", item.session_id, item.question),
                usize::from(popup.width.saturating_sub(2)),
            ));
        }
    }
    let message_rows = usize::from(popup.height.saturating_sub(12)).max(2);
    if !dialog.messages.is_empty() {
        lines.push("Conversation:".into());
        let visible = dialog
            .messages
            .iter()
            .rev()
            .take(message_rows)
            .collect::<Vec<_>>();
        for message in visible.into_iter().rev() {
            lines.push(clipped_width(
                message,
                usize::from(popup.width.saturating_sub(2)),
            ));
        }
    }
    lines.push(format!(
        "Question/follow-up: {}_",
        clipped_width(&dialog.input, usize::from(popup.width.saturating_sub(24)))
    ));
    let input_row = lines.len().saturating_sub(1);
    render_dialog_text(
        frame,
        popup,
        " Investigate with local agent ",
        lines.join("\n"),
        theme,
    );
    if matches!(
        dialog.stage,
        crate::app::InvestigationStage::Input
            | crate::app::InvestigationStage::Conversation
            | crate::app::InvestigationStage::Error
    ) {
        place_input_cursor(
            frame,
            dialog_body(popup),
            input_row,
            UnicodeWidthStr::width("Question/follow-up: "),
            &dialog.input,
            theme,
        );
    }
    render_dialog_footer(frame, popup, "↑/↓ saved · Alt-N new", theme);
}

fn render_view_dialog(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let popup = centered(area, 76, 10);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    let Some(dialog) = &app.view_dialog else {
        return;
    };
    app.hit_regions.view_source_rows.clear();
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
        let body = dialog_body(popup);
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
                    Style::default()
                        .fg(theme.selection_fg)
                        .bg(theme.selection_bg)
                } else {
                    Style::default().fg(theme.base_fg).bg(theme.dialog_bg)
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
        render_dialog_footer(
            frame,
            popup,
            "↑/↓ select · Space include · Alt-↑/↓ order",
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
            "Mode: {mode}\n\nName: {}_\n\n{message}",
            clipped_width(&dialog.draft, usize::from(popup.width.saturating_sub(9)))
        ),
        theme,
    );
    place_input_cursor(
        frame,
        dialog_body(popup),
        2,
        UnicodeWidthStr::width("Name: "),
        &dialog.draft,
        theme,
    );
    render_dialog_footer(
        frame,
        popup,
        "Alt-B blank · Alt-D clone · Alt-R rename · Alt-M sources",
        theme,
    );
}

fn render_source_dialog(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let popup = centered(area, 90, 18);
    clear_themed(frame, popup, theme);
    app.hit_regions.selection_modal = Some(popup.inner(ratatui::layout::Margin::new(1, 1)));
    let Some(dialog) = &app.source_dialog else {
        return;
    };
    if dialog.mode == crate::app::SourceDialogMode::Ai {
        let ai = &dialog.ai;
        let mut lines = vec![
            format!(
                "Request: {}_",
                clipped_width(&ai.instruction, usize::from(popup.width.saturating_sub(11)))
            ),
            String::new(),
            format!("Status: {}", ai.progress),
        ];
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
        render_dialog_text(
            frame,
            popup,
            if app.ascii {
                " Ask Agent for a source — preview never executes "
            } else {
                " Ask 🧠 for a source — preview never executes "
            },
            lines.join("\n"),
            theme,
        );
        if matches!(
            ai.stage,
            crate::app::SourceAiStage::Input | crate::app::SourceAiStage::Error
        ) {
            place_input_cursor(
                frame,
                dialog_body(popup),
                0,
                UnicodeWidthStr::width("Request: "),
                &ai.instruction,
                theme,
            );
        }
        render_dialog_footer(frame, popup, "Ctrl-A manual · Ctrl-D discover", theme);
        return;
    }
    if dialog.mode == crate::app::SourceDialogMode::Discovery {
        app.hit_regions.discovery_rows.clear();
        let indices = crate::app::filtered_discovery_indices(&dialog.discovery);
        let block = Block::default()
            .title(" Discover sources — selection never auto-starts ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent));
        let (footer, footer_text) = adaptive_footer(
            popup,
            "Ctrl-R Refresh · Ctrl-D Manual · ↑/↓ active pane",
            "Ctrl-R · Ctrl-D · ↑/↓ pane",
            2,
        );
        let inner = dialog_body_with_footer(popup, footer.height);
        frame.render_widget(block, popup);
        let search = Rect::new(inner.x, inner.y, inner.width, inner.height.min(1));
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
            inner.height.saturating_sub(2).min(5)
        } else {
            0
        };
        let list_height = inner.height.saturating_sub(1 + diagnostic_height);
        let list_area = Rect::new(inner.x, search.bottom(), inner.width, list_height);
        let diagnostic_area =
            Rect::new(inner.x, list_area.bottom(), inner.width, diagnostic_height);
        frame.render_widget(
            Paragraph::new(format!(
                "Search: {}_   {}/{} matches",
                clipped_width(
                    &dialog.discovery.query,
                    usize::from(search.width.saturating_sub(24))
                ),
                indices.len(),
                dialog.discovery.items.len()
            )),
            search,
        );
        place_input_cursor(
            frame,
            search,
            0,
            UnicodeWidthStr::width("Search: "),
            &dialog.discovery.query,
            theme,
        );
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
            .style(Style::default().fg(if dialog.error.is_some() {
                theme.severity.error
            } else {
                theme.muted
            }));
        let diagnostic_block = Block::default()
            .title(" Diagnostics · ↑/↓ when focused ")
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
        render_action_footer(frame, footer, &footer_text, theme);
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
        .unwrap_or("Provide a file path or command. Capture starts only after submission.");
    let empty = if app.views.is_empty() {
        "No view selected — add or discover a source.\n"
    } else {
        ""
    };
    let agent = if app.ascii { "Agent" } else { "🧠" };
    let footer_full = format!("Alt-F File · Alt-C Command · Ctrl-D Discover · Ctrl-A {agent}");
    let (footer, footer_text) =
        adaptive_footer(popup, &footer_full, "Alt-F · Alt-C · Ctrl-D · Ctrl-A", 3);
    let input_row = if app.views.is_empty() { 3usize } else { 2usize };
    let mut text = format!("{empty}{kind}\n\n\n{message}");
    if dialog.kind == crate::app::SourceKind::Command {
        text.push_str("\nCommand completion is disabled; command cwd is app cwd.");
    } else if dialog.path_completion.scanning {
        text.push_str("\nCompleting path…");
    } else if !dialog.path_completion.candidates.is_empty() {
        text.push_str("\nPath matches:");
        let available = usize::from(popup.height.saturating_sub(10)).max(1);
        let selected = dialog
            .path_completion
            .selected
            .min(dialog.path_completion.candidates.len().saturating_sub(1));
        let top = selected.saturating_sub(available.saturating_sub(1));
        for (position, candidate) in dialog
            .path_completion
            .candidates
            .iter()
            .skip(top)
            .take(available)
            .enumerate()
        {
            let marker = if top + position == selected { ">" } else { " " };
            text.push_str(&format!("\n{marker} {candidate}"));
        }
    }
    render_dialog_text(frame, popup, " Add source ", text, theme);
    let body = dialog_body_with_footer(popup, footer.height);
    let input_area = Rect::new(
        body.x,
        body.y.saturating_add(input_row as u16),
        body.width,
        u16::from(usize::from(body.height) > input_row),
    );
    InputSurface {
        style: Style::default().fg(theme.input_fg).bg(theme.input_bg),
    }
    .render(input_area, frame.buffer_mut());
    frame.render_widget(
        Paragraph::new(input_tail(&dialog.draft, usize::from(input_area.width)))
            .style(Style::default().fg(theme.input_fg).bg(theme.input_bg)),
        input_area,
    );
    place_input_cursor(frame, body, input_row, 0, &dialog.draft, theme);
    render_action_footer(frame, footer, &footer_text, theme);
}

fn place_input_cursor(
    frame: &mut Frame<'_>,
    area: Rect,
    first_row: usize,
    prefix_width: usize,
    value: &str,
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
    let visible = input_tail(
        value.lines().last().unwrap_or(""),
        usize::from(field.width - 1),
    );
    frame.render_widget(
        Paragraph::new(visible.as_str())
            .style(Style::default().fg(theme.input_fg).bg(theme.input_bg)),
        field,
    );
    let column = UnicodeWidthStr::width(visible.as_str());
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

fn adaptive_footer(popup: Rect, full: &str, compact: &str, maximum_height: u16) -> (Rect, String) {
    let inner = popup.inner(ratatui::layout::Margin::new(1, 1));
    let wrap = |text: &str| wrap_actions(text, usize::from(inner.width));
    let mut lines = wrap(full);
    if lines.len() > usize::from(maximum_height.max(1)) {
        lines = wrap(compact);
    }
    // A tiny terminal must not turn the entire dialog into shortcuts or
    // silently hide the end of the action list. Keep editable rows available
    // and tell the user how to reveal the complete controls.
    let body_reserve = 4.min(inner.height.saturating_sub(1));
    if lines.len() > usize::from(inner.height.saturating_sub(body_reserve)) {
        lines = wrap("Enlarge terminal");
    }
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .min(inner.height);
    let area = Rect::new(
        inner.x,
        inner.bottom().saturating_sub(height),
        inner.width,
        height,
    );
    (area, lines.join("\n"))
}

fn wrap_actions(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = vec![String::new()];
    for action in text.split(" · ") {
        let current = lines.last_mut().expect("footer line");
        let separator = if current.is_empty() { "" } else { " · " };
        if UnicodeWidthStr::width(current.as_str())
            + UnicodeWidthStr::width(separator)
            + UnicodeWidthStr::width(action)
            <= width
        {
            current.push_str(separator);
            current.push_str(action);
        } else {
            lines.push(clipped_width(action, width));
        }
    }
    lines
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

fn render_dialog_footer(frame: &mut Frame<'_>, popup: Rect, text: &str, theme: Theme) {
    let inner = popup.inner(ratatui::layout::Margin::new(1, 1));
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let area = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
    render_action_footer(frame, area, text, theme);
}

fn render_action_footer(frame: &mut Frame<'_>, area: Rect, text: &str, theme: Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(text).style(
            Style::default()
                .fg(theme.focused_input_border)
                .bg(theme.input_bg)
                .add_modifier(Modifier::BOLD),
        ),
        area,
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
