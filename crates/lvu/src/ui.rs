use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Row, Table, Widget, Wrap},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    App,
    app::{Focus, StorageCategory, format_storage_bytes},
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

    app.hit_regions.log = Some(geometry.log);
    app.hit_regions.log_rows = Some(geometry.log_rows);
    app.hit_regions.sidebar = geometry.sidebar;
    app.hit_regions.sidebar_views = sidebar_view_regions(app, geometry.sidebar);
    app.hit_regions.editor_completion_rows.clear();
    app.sync_provider(provider, usize::from(geometry.log_rows.height));

    render_header(frame, app, geometry.header, theme);
    if let Some((elapsed, config, activity)) =
        delight.filter(|(_, config, _)| config.enabled && geometry.status.width >= 60)
    {
        let width = geometry.status.width.min(18);
        let heart_area = Rect::new(geometry.status.x, geometry.status.y, width, 1);
        frame.render_widget(Clear, heart_area);
        crate::delight::FooterDelight::render_with_theme(
            frame, heart_area, elapsed, config, activity, theme,
        );
        render_status(
            frame,
            app,
            Rect::new(
                geometry.status.x + width,
                geometry.status.y,
                geometry.status.width - width,
                geometry.status.height,
            ),
            theme,
        );
    } else {
        render_status(frame, app, geometry.status, theme);
    }
    if let Some(sidebar) = geometry.sidebar {
        render_selector(frame, app, sidebar, theme);
    }
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
    if app.focus == Focus::Context {
        render_context(frame, app, provider, geometry.area, theme);
    }
    if app.show_help {
        render_help(frame, app, geometry.area, theme);
    }
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
    render_dialog_footer(
        frame,
        popup,
        "↑/↓ scroll · PgUp/PgDn page · g anchor · Esc close",
        theme,
    );
}

fn render_settings(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    let popup = centered(area, 104, 24);
    clear_themed(frame, popup, theme);
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
        "↑/↓ or Tab field · type/backspace edit · Space toggle/cycle · Enter save · Esc close",
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
    render_dialog_footer(
        frame,
        popup,
        "↑/↓ field · type edit · Space toggle · Enter save · Esc close",
        theme,
    );
}

fn render_storage(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let popup = centered(area, 88, 20);
    clear_themed(frame, popup, theme);
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
    let inner = dialog_body(popup);
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
    let footer_height = 3.min(inner.height.saturating_sub(header.height));
    let rows = Rect::new(
        inner.x,
        header.bottom(),
        inner.width,
        inner.height.saturating_sub(header.height + footer_height),
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
    let footer = Rect::new(inner.x, rows.bottom(), inner.width, footer_height);
    let errors = snapshot.errors.first().map_or("", String::as_str);
    frame.render_widget(
        Paragraph::new(format!(
            "{}{}\n↑↓ select  r refresh  c preview/confirm unused derived  Esc close",
            dialog.status,
            if errors.is_empty() {
                String::new()
            } else {
                format!(" | error: {errors}")
            }
        ))
        .style(
            Style::default()
                .fg(theme.focused_input_border)
                .bg(theme.input_bg),
        ),
        footer,
    );
}

fn render_time_editor(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    let popup = centered(area, 86, 12);
    clear_themed(frame, popup, theme);
    let (Some(dialog), Some(state)) = (&app.time_dialog, app.view_state()) else {
        return;
    };
    let basis = match dialog.basis {
        crate::TimeBasis::Capture => "Capture",
        crate::TimeBasis::Extracted => "Extracted timestamp_utc (UTC RFC3339)",
        crate::TimeBasis::Event => "Recognized event (RFC3339 normalized to UTC)",
    };
    let applied = match state.applied_capture_time_policy {
        Some(crate::CaptureTimePolicy::Recent { seconds }) => {
            format!(
                "rolling last {} (resolved membership refreshes while idle)",
                crate::format_capture_duration(seconds)
            )
        }
        Some(crate::CaptureTimePolicy::Absolute(_)) => state.applied_capture_time.map_or_else(
            || "absolute pending".into(),
            |w| format!("absolute {} .. {}", w.start_unix_nanos, w.end_unix_nanos),
        ),
        None => "all times".into(),
    };
    let lines = format!(
        "Time basis: {basis}  (Alt-P capture / Alt-E raw event / Alt-U extracted)\n{} Start: {}_\n{} End:   {}_\nRolling presets: Alt-5 last 5m  Alt-M last 15m  Alt-H last 1h\nAlt-T Recognize timestamp — propose a UTC timestamp enrichment\nApplied: {}\n{}\nEnter absolute  Tab field  Alt-A ±30s selected  Alt-C clear  Esc cancel",
        if !dialog.editing_end { ">" } else { " " },
        state.time_start_draft,
        if dialog.editing_end { ">" } else { " " },
        state.time_end_draft,
        applied,
        state.time_error.as_deref().unwrap_or(
            "Half-open [start, end); event offsets normalize to UTC, missing/invalid do not match."
        )
    );
    render_dialog_text(frame, popup, " Time window ", lines, theme);
    let inner = dialog_body(popup);
    let (row, draft) = if dialog.editing_end {
        (2, state.time_end_draft.as_str())
    } else {
        (1, state.time_start_draft.as_str())
    };
    place_input_cursor(frame, inner, row, 9, draft, theme);
    render_dialog_footer(
        frame,
        popup,
        "Enter apply · Alt-U extracted · Alt-T help · Tab field · Alt-A around · Alt-C clear · Esc",
        theme,
    );
}

fn render_recipes(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    let popup = centered(area, 84, 20);
    clear_themed(frame, popup, theme);
    let Some(dialog) = &app.recipe_dialog else {
        return;
    };
    let mut lines = vec![format!(
        "Mode: {:?}   Alt-S save  Alt-I import TOML  Alt-B browse",
        dialog.mode
    )];
    if dialog.mode != crate::app::RecipeDialogMode::Browse {
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
        } else {
            "Import installs a canonical copy for preview; Apply is a separate action.".into()
        });
    } else {
        let first = dialog.selected.saturating_sub(11);
        for (index, item) in dialog.items.iter().enumerate().skip(first).take(12) {
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
    lines.push("Enter apply  Alt-G refresh suggestions  Alt-A adapt  x reject  Esc close".into());
    render_dialog_text(frame, popup, " Named recipes ", lines.join("\n"), theme);
    if dialog.mode != crate::app::RecipeDialogMode::Browse {
        place_input_cursor(
            frame,
            dialog_body(popup),
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
    render_dialog_footer(
        frame,
        popup,
        "Enter apply · Alt-G suggestions · Alt-A adapt · x reject · Esc close",
        theme,
    );
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
            " {follow}{capture_time}{runtime} | {}-{}/{}{}{}{}{enrichment}{grouping} | Ctrl-P commands ?:help /:search p:advanced e:enrich m:group t:time q:quit ",
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
        " NO VIEW | add or discover a source to begin | Ctrl-P commands q:quit ".into()
    };
    if let Some(notice) = &app.source_control_notice {
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

fn render_selector(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
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
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(" Sources / views ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border)),
        ),
        area,
    );
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
            Paragraph::new("No view selected. Add or discover a source, then create a view.")
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
            let mut cells = vec![row.timestamp.clone(), row.level.clone()];
            cells.extend(
                pinned
                    .iter()
                    .map(|field| field_value(&row, field).unwrap_or("—").to_owned()),
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
            cells.push(
                event
                    .lines()
                    .map(|line| {
                        crate::horizontal::scroll_columns(line, horizontal, area.width as usize)
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
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
    widths.extend(pinned.iter().map(|_| Constraint::Length(14)));
    widths.push(Constraint::Min(1));
    let title = if horizontal == 0 {
        " Log viewport ".to_owned()
    } else {
        format!(" Log viewport · x={horizontal} ")
    };
    let mut headers = vec!["time".to_owned(), "level".to_owned()];
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

fn render_details<P: RowProvider>(
    frame: &mut Frame<'_>,
    app: &App,
    provider: &P,
    area: Rect,
    theme: Theme,
) {
    let text = app.selected_row(provider).map_or_else(
        || "No selected event".into(),
        |row| {
            let mut result = format!("stable display id: {}\nraw: {}\n", row.id, row.text);
            for (key, value) in row.fields {
                result.push_str(&format!("{key}: {value}\n"));
            }
            for (key, value) in row.details {
                result.push_str(&format!("{key}: {value}\n"));
            }
            result
        },
    );
    frame.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }).block(
            Block::default()
                .title(" Selected event details ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border)),
        ),
        area,
    );
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
    let Some(row) = app.field_picker_row(provider) else {
        return;
    };
    let body = dialog_body(popup);
    let visible = usize::from(body.height).max(1);
    app.set_field_picker_viewport(visible);
    let Some(state) = app.view_state() else {
        return;
    };
    let selected = state.field_picker_selected;
    let top = state.field_picker_top;
    let pinned = state.pinned_columns.clone();
    let color_field = state.color_field.clone();
    let mut lines = Vec::new();
    app.hit_regions.field_picker_rows.clear();
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
    frame.render_widget(
        Block::default()
            .title(" Event fields ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        popup,
    );
    frame.render_widget(Paragraph::new(lines), body);
    render_dialog_footer(
        frame,
        popup,
        "↑/↓ select · Space/Enter pin · c color · Esc close",
        theme,
    );
}

fn render_editor<P: RowProvider>(
    frame: &mut Frame<'_>,
    app: &mut App,
    provider: &P,
    area: Rect,
    theme: Theme,
) {
    let popup_height = if app.focus == Focus::EnrichmentEditor {
        17
    } else if app.focus == Focus::GroupingEditor {
        13
    } else {
        8
    };
    let popup = centered(area, 80, popup_height);
    clear_themed(frame, popup, theme);
    let Some(editor) = app.active_editor_state().cloned() else {
        return;
    };
    let (title, guidance) = match app.focus {
        Focus::SearchEditor => (
            " Search ",
            r#"text · "field name": text · /regex/ims · \/literal · pl.col(...) predicate"#,
        ),
        Focus::AdvancedEditor => (
            " Advanced Polars filter ",
            "Tab completes sampled fields/string values; Enter submits only after popup closes",
        ),
        Focus::EnrichmentEditor => (
            " Native enrichment ",
            "output_name = Python Polars Expr; raw is original input; Tab shows raw and sampled source field names/string values",
        ),
        Focus::GroupingEditor => (
            " Display-only multiline grouping ",
            r"Rust regex over raw bytes; ^ anchors to line start; Enter applies, empty disables",
        ),
        Focus::Selector
        | Focus::Logs
        | Focus::SourceDialog
        | Focus::ViewDialog
        | Focus::FieldPicker
        | Focus::AskAi
        | Focus::Investigation => return,
        Focus::Recipes | Focus::TimeEditor | Focus::Storage | Focus::Settings | Focus::Context => {
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
    if app.editor_completion.is_none() {
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
            "Enter apply · Alt-A add · Alt-E edit · Alt-R remove · Alt-J/K select · Tab complete"
        } else if app.focus == Focus::SearchEditor {
            "300ms live search · Enter apply now · Esc close"
        } else {
            "Enter apply · Tab sampled fields/values · Esc close"
        },
        theme,
    );
    render_editor_completion(frame, app, area, theme);
}

fn render_editor_completion(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let Some(completion) = &app.editor_completion else {
        return;
    };
    let popup = centered(area, 76, 12);
    clear_themed(frame, popup, theme);
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
    render_dialog_footer(
        frame,
        popup,
        "Tab fields/values · ↑/↓ select · Enter insert · Esc close",
        theme,
    );
}

fn render_help(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    let popup = centered(area, 90, 20);
    clear_themed(frame, popup, theme);
    let agent = if app.ascii { "Agent" } else { "🧠" };
    let help = format!(
        "Keyboard\n  Ctrl-P command palette            , settings\n  q/Ctrl-C quit     Tab focus       [ ] switch view\n  j/k or arrows     PgUp/PgDn       g/G top/end\n  d details         i fields         f follow/history\n  / search          p advanced       e enrichment\n  Editor: Tab sampled field/value completion; Enter inserts\n  m grouping (display-only)          S storage usage\n  A Ask {agent} Alt-F/E; I investigate Enter/resume Alt-N new\n  n source          v source views  r recipes  t capture time\n  o raw context · Alt-S stop capture  Alt-R restart source (logs/sidebar)\n  View: Alt-B blank  Alt-D clone  Alt-R rename\n  Fields: Space pin, c color   Source: Tab path completion\n  Source: Alt-F file Alt-C command Ctrl-D discovery Ctrl-A Ask {agent}\n\n{agent} proposals are local and require explicit review/apply.\nMouse: left click exact row/view; wheel active pane."
    );
    render_dialog_text(frame, popup, " Help ", help, theme);
    render_dialog_footer(
        frame,
        popup,
        "←/→ scroll event · 0 reset · Esc or ? close",
        theme,
    );
}

fn render_ask_ai(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    let popup = centered(area, 88, 17);
    clear_themed(frame, popup, theme);
    let Some(dialog) = &app.ask_ai_dialog else {
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
            "\nScope: advanced filter only; recipe search/enrichment/pins/colors and current time/grouping are retained.",
        );
    }
    text.push_str(
        "\n\nAlt-F filter  Alt-E enrichment  Alt-T timestamp  Enter request/apply  Esc cancel",
    );
    render_dialog_text(
        frame,
        popup,
        if app.ascii {
            " Ask Agent "
        } else {
            " Ask 🧠 "
        },
        text,
        theme,
    );
    if matches!(
        dialog.stage,
        crate::app::AskAiStage::Input | crate::app::AskAiStage::Error
    ) {
        place_input_cursor(frame, dialog_body(popup), 3, 0, &dialog.prompt, theme);
    }
    render_dialog_footer(
        frame,
        popup,
        "Alt-F filter · Alt-E enrichment · Enter request/apply · Esc cancel",
        theme,
    );
}

fn render_investigation(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    let popup = centered(area, 100, 22);
    clear_themed(frame, popup, theme);
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
        lines.push("Saved investigations (empty input + Enter resumes):".into());
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
    lines.push("Enter send/resume  ↑/↓ saved  Alt-N new snapshot  Esc cancel/close".into());
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
    render_dialog_footer(
        frame,
        popup,
        "Enter send/resume · ↑/↓ saved · Alt-N new · Esc cancel/close",
        theme,
    );
}

fn render_view_dialog(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    let popup = centered(area, 76, 10);
    clear_themed(frame, popup, theme);
    let Some(dialog) = &app.view_dialog else {
        return;
    };
    let mode = match dialog.mode {
        crate::app::ViewDialogMode::Blank => "NEW BLANK",
        crate::app::ViewDialogMode::Clone => "CLONE SETTINGS",
        crate::app::ViewDialogMode::Rename => "RENAME",
    };
    let message = dialog
        .error
        .as_deref()
        .unwrap_or("Alt-B blank  Alt-D clone  Alt-R rename  Enter save  Esc close");
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
        "Alt-B blank · Alt-D clone · Alt-R rename · Enter save · Esc close",
        theme,
    );
}

fn render_source_dialog(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let popup = centered(area, 90, 18);
    clear_themed(frame, popup, theme);
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
        lines.push(
            "↑/↓ review  Enter requests/applies; Ctrl-A manual; Ctrl-D discovery; Esc cancel"
                .into(),
        );
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
        render_dialog_footer(
            frame,
            popup,
            "Enter request/apply · Ctrl-A manual · Ctrl-D discover · Esc cancel",
            theme,
        );
        return;
    }
    if dialog.mode == crate::app::SourceDialogMode::Discovery {
        app.hit_regions.discovery_rows.clear();
        let indices = crate::app::filtered_discovery_indices(&dialog.discovery);
        let block = Block::default()
            .title(" Discover sources — selection never auto-starts ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent));
        let inner = dialog_body(popup);
        frame.render_widget(block, popup);
        let search = Rect::new(inner.x, inner.y, inner.width, inner.height.min(1));
        let footer_height = inner.height.saturating_sub(1).min(2);
        let details_height = inner.height.saturating_sub(1 + footer_height).min(3);
        let list_height = inner
            .height
            .saturating_sub(1 + details_height + footer_height);
        let list_area = Rect::new(inner.x, search.bottom(), inner.width, list_height);
        let details_area = Rect::new(inner.x, list_area.bottom(), inner.width, details_height);
        let footer = Rect::new(inner.x, details_area.bottom(), inner.width, footer_height);
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
        frame.render_widget(
            Paragraph::new(detail).wrap(Wrap { trim: false }),
            details_area,
        );
        let error = dialog
            .error
            .as_ref()
            .map_or(String::new(), |error| format!(" · Error: {error}"));
        frame.render_widget(
            Paragraph::new(format!(
                "Status: {}{error}\n↑/↓ or wheel select · click row · Enter start · Ctrl-R rescan · Ctrl-D manual · Esc close",
                dialog.discovery.status
            ))
            .style(Style::default().fg(theme.focused_input_border).bg(theme.input_bg)),
            footer,
        );
        return;
    }
    let kind = match dialog.kind {
        crate::app::SourceKind::File => "FILE PATH",
        crate::app::SourceKind::Command => "COMMAND (sh -c)",
    };
    let message = dialog.error.as_deref().unwrap_or(
        "Tab completes paths; Alt-F file; Alt-C command; Ctrl-D discover; Ctrl-A Ask 🧠; Enter starts.",
    );
    let empty = if app.views.is_empty() {
        "No view selected — add or discover a source.\n"
    } else {
        ""
    };
    let mut text = format!(
        "{empty}Kind: {kind}\n\n{}\n\n{message}",
        clipped_width(&dialog.draft, usize::from(popup.width.saturating_sub(2)))
    );
    if dialog.kind == crate::app::SourceKind::Command {
        text.push_str("\nCommand completion is disabled; command cwd is app cwd.");
    } else if dialog.path_completion.scanning {
        text.push_str("\nCompleting path…");
    } else if !dialog.path_completion.candidates.is_empty() {
        text.push_str("\nChoices (↑/↓ then Tab):");
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
    place_input_cursor(
        frame,
        dialog_body(popup),
        if app.views.is_empty() { 3 } else { 2 },
        0,
        &dialog.draft,
        theme,
    );
    render_dialog_footer(
        frame,
        popup,
        "Tab complete · Alt-F file · Alt-C command · Ctrl-D discover · Enter start",
        theme,
    );
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

fn dialog_body(popup: Rect) -> Rect {
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
        inner.height.saturating_sub(1),
    )
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
    frame.render_widget(
        Paragraph::new(clipped_width(text, usize::from(area.width))).style(
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
    use super::input_tail;
    use crate::theme::Theme;
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
}
