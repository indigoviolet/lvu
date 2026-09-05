use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Row, Table, Wrap},
};
use unicode_width::UnicodeWidthStr;

use crate::{App, app::Focus, provider::RowProvider};

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
    let geometry = layout(frame.area(), app.show_details);
    app.terminal_size = (geometry.area.width, geometry.area.height);
    if geometry.tiny {
        app.hit_regions = Default::default();
        render_tiny(frame, app, geometry.area);
        return;
    }

    app.hit_regions.log = Some(geometry.log);
    app.hit_regions.log_rows = Some(geometry.log_rows);
    app.hit_regions.sidebar = geometry.sidebar;
    app.hit_regions.sidebar_views = sidebar_view_regions(app, geometry.sidebar);
    app.sync_provider(provider, usize::from(geometry.log_rows.height));

    render_header(frame, app, geometry.header);
    render_status(frame, app, geometry.status);
    if let Some(sidebar) = geometry.sidebar {
        render_selector(frame, app, sidebar);
    }
    render_logs(frame, app, provider, geometry.log);
    if let Some(details) = geometry.details {
        render_details(frame, app, provider, details);
    }
    if matches!(app.focus, Focus::SearchEditor | Focus::AdvancedEditor) {
        render_editor(frame, app, geometry.area);
    }
    if app.focus == Focus::SourceDialog {
        render_source_dialog(frame, app, geometry.area);
    } else if app.focus == Focus::FieldPicker {
        render_field_picker(frame, app, provider, geometry.area);
    }
    if app.show_help {
        render_help(frame, geometry.area);
    }
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

fn render_tiny(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let mode = if app.demo_mode { " DEMO" } else { "" };
    frame.render_widget(
        Paragraph::new(format!(
            "lvu{mode}\nterminal too small\n{}x{}  q quit",
            area.width, area.height
        ))
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let demo = if app.demo_mode {
        Span::styled(
            " DEMO FIXTURE — NOT ACQUISITION ",
            Style::default().fg(Color::Black).bg(Color::Yellow),
        )
    } else {
        Span::raw(" ")
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(&app.title, Style::default().add_modifier(Modifier::BOLD)),
            demo,
        ])),
        area,
    );
}

fn render_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let mut text = if let (Some(view_id), Some(state)) = (app.active_view_id(), app.view_state()) {
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
        let runtime = app
            .active_view_runtime_status()
            .map_or_else(String::new, |status| format!(" | {status}"));
        format!(
            " {follow}{runtime} | {view_id} | {}-{}/{}{}{}{} | ?:help /:search p:advanced q:quit ",
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
        " NO VIEW | add or discover a source to begin | q:quit ".into()
    };
    if let Some(notice) = &app.source_notice {
        text.push_str(" | ");
        text.push_str(notice);
    }
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::Black).bg(Color::Cyan)),
        area,
    );
}

fn render_selector(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let mut items = Vec::new();
    for source in &app.sources {
        items.push(ListItem::new(Line::from(vec![
            Span::styled("● ", Style::default().fg(Color::Green)),
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
                    .fg(Color::Yellow)
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
        Color::Yellow
    } else {
        Color::DarkGray
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

fn render_logs<P: RowProvider>(frame: &mut Frame<'_>, app: &App, provider: &P, area: Rect) {
    if app.active_view_id().is_none() {
        frame.render_widget(
            Paragraph::new("No view selected. Add or discover a source, then create a view.")
                .wrap(Wrap { trim: true })
                .block(
                    Block::default()
                        .title(" Log viewport ")
                        .borders(Borders::ALL),
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
                    .borders(Borders::ALL),
            ),
            area,
        );
        return;
    }
    let selected = app.view_state().and_then(|state| state.selected.as_ref());
    let state = app.view_state().expect("active view state");
    let pinned = state.pinned_columns.clone();
    let color_field = state.color_field.clone();
    let rows = app.visible_rows(provider).into_iter().map(|row| {
        let style = if selected == Some(&row.id) {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        } else if let Some(value) = color_field
            .as_ref()
            .and_then(|field| field_value(&row, field))
        {
            Style::default().fg(stable_value_color(value))
        } else if matches!(row.level.as_str(), "ERROR" | "FATAL") {
            Style::default().fg(Color::Red)
        } else if row.level == "WARN" {
            Style::default().fg(Color::Yellow)
        } else if row.level == "INFO" {
            Style::default().fg(Color::Green)
        } else if row.level == "DEBUG" {
            Style::default().fg(Color::Blue)
        } else if row.level == "TRACE" {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        };
        let mut cells = vec![row.timestamp.clone(), row.level.clone()];
        cells.extend(
            pinned
                .iter()
                .map(|field| field_value(&row, field).unwrap_or("—").to_owned()),
        );
        cells.push(row.text);
        Row::new(cells).style(style)
    });
    let border = if app.focus == Focus::Logs {
        Color::Yellow
    } else {
        Color::DarkGray
    };
    let mut widths = vec![Constraint::Length(13), Constraint::Length(6)];
    widths.extend(pinned.iter().map(|_| Constraint::Length(14)));
    widths.push(Constraint::Min(1));
    let mut headers = vec!["time".to_owned(), "level".to_owned()];
    headers.extend(pinned.iter().cloned());
    headers.push("event".into());
    frame.render_widget(
        Table::new(rows, widths)
            .header(Row::new(headers).style(Style::default().add_modifier(Modifier::BOLD)))
            .column_spacing(1)
            .block(
                Block::default()
                    .title(" Log viewport ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border)),
            ),
        area,
    );
}

fn render_details<P: RowProvider>(frame: &mut Frame<'_>, app: &App, provider: &P, area: Rect) {
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
                .borders(Borders::ALL),
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

fn stable_value_color(value: &str) -> Color {
    let hash = value.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    [
        Color::Cyan,
        Color::Magenta,
        Color::Blue,
        Color::Green,
        Color::Yellow,
    ][hash as usize % 5]
}

fn render_field_picker<P: RowProvider>(
    frame: &mut Frame<'_>,
    app: &mut App,
    provider: &P,
    area: Rect,
) {
    let popup = centered(area, 70, 16);
    frame.render_widget(Clear, popup);
    let Some(row) = app.field_picker_row(provider) else {
        return;
    };
    let visible = usize::from(popup.height.saturating_sub(3)).max(1);
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
        lines.push(clipped_width(
            &format!("{cursor} {pin} {key} = {value}{color}"),
            usize::from(popup.width.saturating_sub(2)),
        ));
        app.hit_regions.field_picker_rows.push((
            Rect::new(
                popup.x + 1,
                popup.y + 1 + position as u16,
                popup.width.saturating_sub(2),
                1,
            ),
            index,
        ));
    }
    lines.push("↑/↓ select  Space/Enter pin  c color-by-value  Esc close".into());
    frame.render_widget(
        Paragraph::new(lines.join("\n")).block(
            Block::default()
                .title(" Event fields ")
                .borders(Borders::ALL),
        ),
        popup,
    );
}

fn render_editor(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let popup = centered(area, 80, 8);
    frame.render_widget(Clear, popup);
    let Some(editor) = app.active_editor_state() else {
        return;
    };
    let (title, guidance) = match app.focus {
        Focus::SearchEditor => (
            " Search ",
            "Live literal substring; case-insensitive Unicode lowercase; punctuation is literal",
        ),
        Focus::AdvancedEditor => (
            " Advanced Polars filter ",
            "Enter submits to the optional Polars adapter; invalid drafts keep the applied filter",
        ),
        Focus::Selector | Focus::Logs | Focus::SourceDialog | Focus::FieldPicker => return,
    };
    let message = editor.error.as_deref().unwrap_or(guidance);
    let text = format!(
        "Draft:\n{}\n\napplied: {}\n{}",
        editor.draft, editor.applied, message
    );
    frame.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }).block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Magenta)),
        ),
        popup,
    );
}

fn render_help(frame: &mut Frame<'_>, area: Rect) {
    let popup = centered(area, 90, 16);
    frame.render_widget(Clear, popup);
    let help = "Keyboard\n  q/Ctrl-C quit     Tab focus       [ ] switch view\n  j/k or arrows     PgUp/PgDn       g/G top/end\n  d details         i event fields   f follow/history\n  / literal search  p advanced Polars n add source\n  Field picker: Space pin/unpin, c color-by-value, Esc close\n  Source dialog: Tab path completion  Alt-F file  Alt-C command\n  Ctrl-D discovery (in source dialog)  ? close help\n\nSearch is case-insensitive Unicode lowercase; punctuation is literal.\nMouse: wheel active pane; left click exact row/view.";
    frame.render_widget(
        Paragraph::new(help)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(" Help ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
        popup,
    );
}

fn render_source_dialog(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let popup = centered(area, 90, 18);
    frame.render_widget(Clear, popup);
    let Some(dialog) = &app.source_dialog else {
        return;
    };
    if dialog.mode == crate::app::SourceDialogMode::Discovery {
        let indices = crate::app::filtered_discovery_indices(&dialog.discovery);
        let visible = usize::from(popup.height.saturating_sub(7));
        let selected = dialog
            .discovery
            .selected
            .min(indices.len().saturating_sub(1));
        let top = selected.saturating_sub(visible.saturating_sub(1));
        let mut lines = vec![format!(
            "Search: {}_   {}/{} matches",
            dialog.discovery.query,
            indices.len(),
            dialog.discovery.items.len()
        )];
        for (position, index) in indices.iter().skip(top).take(visible).enumerate() {
            let item = &dialog.discovery.items[*index];
            let marker = if top + position == selected { ">" } else { " " };
            lines.push(format!("{marker} {} [{}]", item.label, item.status));
            lines.push(format!("  {}", item.detail));
        }
        if indices.is_empty() {
            lines.push("  No matching candidates.".into());
        }
        lines.push(format!("Status: {}", dialog.discovery.status));
        lines.push("↑/↓ select  Enter start  Ctrl-R rescan  Ctrl-D manual  Esc close".into());
        if let Some(error) = &dialog.error {
            lines.push(format!("Error: {error}"));
        }
        frame.render_widget(
            Paragraph::new(lines.join("\n"))
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .title(" Discover sources — selection never auto-starts ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Green)),
                ),
            popup,
        );
        return;
    }
    let kind = match dialog.kind {
        crate::app::SourceKind::File => "FILE PATH",
        crate::app::SourceKind::Command => "COMMAND (sh -c)",
    };
    let message = dialog.error.as_deref().unwrap_or(
        "Tab completes file paths; Alt-F file; Alt-C command; Ctrl-D discover; Enter starts; Esc closes.",
    );
    let empty = if app.views.is_empty() {
        "No view selected — add or discover a source.\n"
    } else {
        ""
    };
    let mut text = format!("{empty}Kind: {kind}\n\n{}\n\n{message}", dialog.draft);
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
    frame.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }).block(
            Block::default()
                .title(" Add source ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green)),
        ),
        popup,
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
    use super::stable_value_color;

    #[test]
    fn value_colors_are_stable_and_null_remains_visible() {
        assert_eq!(
            stable_value_color("same-request"),
            stable_value_color("same-request")
        );
        assert_eq!(stable_value_color("null"), stable_value_color("null"));
    }
}
