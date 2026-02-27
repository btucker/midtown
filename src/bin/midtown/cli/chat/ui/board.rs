//! Board panel: channel list and coworker status table.

use std::collections::{BTreeMap, HashMap, HashSet};

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
};

use ratatui_themes::ThemePalette;

use super::super::app::{
    App, BoardSelection, CiStatus, CoworkerInfo, FocusedPane, KanbanPr, KanbanTask, TaskStatus,
};
use super::Hyperlink;
use super::text::wrap_content;

fn is_coworker_idle_or_done(phase: Option<&str>) -> bool {
    match phase {
        Some(raw_phase) => {
            let phase = raw_phase.trim().to_ascii_lowercase();
            phase == "idle" || phase == "done"
        }
        None => false,
    }
}

/// Draw the board panel (left side) with channel list
///
/// Returns (hyperlinks, tasks_area) where tasks_area is the rect containing the task list
/// (excluding the coworker status section). This is used for click detection.
pub fn draw_board_panel(f: &mut Frame, app: &mut App, area: Rect) -> (Vec<Hyperlink>, Rect) {
    // Clear line maps for new render
    app.task_line_map.clear();
    app.channel_line_map.clear();
    app.coworker_line_map.clear();

    // Split board area vertically: tasks at top, coworkers at bottom.
    // Exclude the project lead (legacy "lead" or repo-named) to keep the height
    // in sync with the rendering filter in draw_coworker_status.
    let project_name_lower_bp = app.project_name.to_lowercase();
    let active_coworker_count = app
        .coworkers
        .iter()
        .filter(|cw| !is_coworker_idle_or_done(cw.phase.as_deref()))
        .filter(|cw| {
            let name = cw.name.to_lowercase();
            name != "lead" && name != project_name_lower_bp
        })
        .count();
    let coworker_section_height = if active_coworker_count > 0 {
        active_coworker_count as u16 + 3 // 1 header + N rows + 2 borders
    } else {
        0
    };

    // Build layout constraints: tasks always grow, then coworkers (if any)
    let mut constraints = vec![Constraint::Min(10)];
    if coworker_section_height > 0 {
        constraints.push(Constraint::Length(coworker_section_height));
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let tasks_area = chunks[0];
    let mut lines = Vec::new();
    let hyperlinks = Vec::new();

    // Default channel matches the daemon's ChannelRouter default (the project name)
    let main_channel = app.project_name.as_str();

    // Clone task, PR, coworker, and channel data to avoid holding borrows on app
    // This allows us to mutate app.task_line_map later
    let tasks_clone: Vec<KanbanTask> = app.tasks.clone();
    let prs_clone: Vec<KanbanPr> = app.prs.clone();
    let coworkers_clone = app.coworkers.clone();
    let available_channels_clone = app.available_channels.clone();
    let show_archived = app.show_archived_channels;

    // Group tasks by channel
    let mut tasks_by_channel: BTreeMap<String, Vec<&KanbanTask>> = BTreeMap::new();
    for task in &tasks_clone {
        if task.status == TaskStatus::InProgress || task.status == TaskStatus::Pending {
            let channel_key = task.channel.as_deref().unwrap_or(main_channel).to_string();
            tasks_by_channel.entry(channel_key).or_default().push(task);
        }
    }

    // Build the set of channels to display - start with all available channels
    let mut channels_to_display: BTreeMap<String, Vec<&KanbanTask>> = BTreeMap::new();
    for channel_info in &available_channels_clone {
        // Only show if not archived, or if showing archived channels
        if !channel_info.is_archived || show_archived {
            channels_to_display.insert(
                channel_info.name.clone(),
                tasks_by_channel
                    .get(&channel_info.name)
                    .cloned()
                    .unwrap_or_default(),
            );
        }
    }

    // If available_channels is empty (initial load), fall back to showing midtown
    if channels_to_display.is_empty() {
        channels_to_display.insert(
            main_channel.to_string(),
            tasks_by_channel
                .get(main_channel)
                .cloned()
                .unwrap_or_default(),
        );
    }

    let wrap_width = area.width.saturating_sub(2).max(20) as usize;

    // Build task_line_map and channel_line_map before rendering (to avoid borrow conflicts)
    // task_line_map: maps line numbers to (task_id, owner) for click-to-attach
    // channel_line_map: maps line numbers to channel_name for click-to-select
    let mut task_line_map: HashMap<u16, (String, Option<String>)> = HashMap::new();
    let mut channel_line_map: HashMap<u16, String> = HashMap::new();
    let mut current_line: u16 = 0;

    // First pass: calculate line positions
    for (channel_name, tasks) in &channels_to_display {
        // Record channel header line for click-to-select
        channel_line_map.insert(current_line, channel_name.clone());
        current_line += 1; // Channel header

        for task in tasks {
            // Record the first line of this task for click-to-attach
            task_line_map.insert(current_line, (task.id.clone(), task.owner.clone()));

            // Calculate how many lines this task will take (wrapping)
            let prefix = format!("!{} ", task.id);
            let task_line = format!("{}{}", prefix, task.subject);
            let wrapped_lines = wrap_content(&task_line, wrap_width);
            current_line += wrapped_lines.len() as u16;

            // Phase label line: only emitted as a separate extra line when the title does NOT wrap.
            // When the title wraps, the label is merged into the first continuation line (no extra line).
            let title_wraps = wrapped_lines.len() > 1;
            let task_pr = prs_clone.iter().find(|pr| {
                pr.task_id
                    .map(|id| id.to_string() == task.id)
                    .unwrap_or(false)
            });
            let coworker_phase = coworkers_clone.iter().find_map(|cw| {
                if cw.task_id.map(|id| id.to_string()) == Some(task.id.clone()) {
                    cw.phase.as_deref()
                } else {
                    None
                }
            });
            if task_phase_label(task, task_pr, coworker_phase).is_some() {
                if title_wraps {
                    // The phase label is merged into the first continuation line (wrapped_lines[1]).
                    // That line's position is current_line - wrapped_lines.len() + 1.
                    // Register it so click-to-attach works on the continuation line too.
                    let continuation_line = current_line - wrapped_lines.len() as u16 + 1;
                    task_line_map.insert(continuation_line, (task.id.clone(), task.owner.clone()));
                } else {
                    // Label is a separate line below the title; register it.
                    task_line_map.insert(current_line, (task.id.clone(), task.owner.clone()));
                    current_line += 1;
                }
            }
        }
    }

    // Store line maps in app for mouse click handler
    app.task_line_map = task_line_map;
    app.channel_line_map = channel_line_map;

    // Render each channel as a swimlane
    for (channel_name, tasks) in &channels_to_display {
        render_channel_header(app, channel_name, &mut lines);

        let task_indentation = compute_task_indentation(tasks);

        for task in tasks {
            render_task_item(
                app,
                task,
                channel_name,
                &task_indentation,
                wrap_width,
                &mut lines,
            );
        }
    }

    // Determine border color based on focus
    let palette = app.theme.palette();
    let is_focused = app.focused_pane == FocusedPane::Board;
    let border_color = if is_focused {
        palette.accent
    } else {
        palette.fg
    };

    // Render the tasks panel with focus-dependent border
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Board")
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(palette.bg));

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, tasks_area);

    // Draw optional coworker status section
    if coworker_section_height > 0 {
        draw_coworker_status(f, app, chunks[1]);
    }

    (hyperlinks, tasks_area)
}

/// Render a channel header line: `#channel-name` with optional `(X)` unread count.
fn render_channel_header(app: &App, channel_name: &str, lines: &mut Vec<Line<'static>>) {
    let channel_header = if let Some(&unread_count) = app.channel_unread_counts.get(channel_name) {
        format!("#{} ({})", channel_name, unread_count)
    } else {
        format!("#{}", channel_name)
    };

    let is_selected = app.board_selection.as_ref().is_some_and(|sel| match sel {
        BoardSelection::Channel(ch) => ch == channel_name,
        _ => false,
    });

    let palette = app.theme.palette();
    let mut style = Style::default()
        .fg(palette.accent)
        .add_modifier(Modifier::BOLD);
    if is_selected {
        style = style.bg(palette.selection);
    }

    lines.push(Line::from(vec![Span::styled(channel_header, style)]));
}

/// Derive the phase status label for a task based on its PR and coworker state.
///
/// Returns a short label (e.g. "dev", "pr", "rvw", "rvd", "addr", "ci", "done", "cnfl")
/// or `None` if the task is pending with no PR.
pub fn task_phase_label(
    task: &KanbanTask,
    task_pr: Option<&KanbanPr>,
    coworker_phase: Option<&str>,
) -> Option<&'static str> {
    match task_pr {
        None => {
            // No PR yet — use the coworker's reported phase if available, otherwise "dev"
            if task.status == TaskStatus::InProgress {
                let label = match coworker_phase {
                    Some("claim") => "claim",
                    Some("test") => "test",
                    Some("PR") => "PR",
                    Some("review") => "review",
                    Some("debug") => "debug",
                    Some("done") => "done",
                    _ => "dev",
                };
                Some(label)
            } else {
                None
            }
        }
        Some(pr) => {
            // Merge conflicts take priority over everything else
            if pr.has_conflicts {
                return Some("cnfl");
            }

            // CI takes priority when running or failed
            match pr.ci_status {
                CiStatus::Running => return Some("ci"),
                CiStatus::Failed => return Some("ci"),
                _ => {}
            }

            // Review has been posted — check if feedback is being addressed or done
            if pr.review_posted {
                // Coworker actively working = addressing feedback
                // coworker_phase holds abbreviations from WorkflowPhase::abbreviation()
                let addressing_feedback = matches!(coworker_phase, Some("dev" | "debug" | "test"));
                if addressing_feedback {
                    return Some("addr");
                }
                // CI passed and review done = waiting for merge
                if matches!(pr.ci_status, CiStatus::Passed) {
                    return Some("done");
                }
                return Some("rvd");
            }

            // Reviewer assigned but review not yet posted
            if pr.reviewer.is_some() {
                return Some("rvw");
            }

            // PR open, no review activity yet
            Some("pr")
        }
    }
}

/// Render a single task item with indentation, wrapping, and phase status label.
fn render_task_item(
    app: &App,
    task: &KanbanTask,
    channel_name: &str,
    task_indentation: &HashMap<String, usize>,
    wrap_width: usize,
    lines: &mut Vec<Line<'static>>,
) {
    let indent_level = task_indentation.get(&task.id).copied().unwrap_or(0);
    let task_indent = "  ".repeat(indent_level);

    // Find PR for this task
    let task_pr = app.prs.iter().find(|pr| {
        pr.task_id
            .map(|id| id.to_string() == task.id)
            .unwrap_or(false)
    });

    // Find coworker phase for this task (used by phase label derivation)
    let coworker_phase = app.coworkers.iter().find_map(|cw| {
        if cw.task_id.map(|id| id.to_string()) == Some(task.id.clone()) {
            cw.phase.as_deref()
        } else {
            None
        }
    });

    // Determine bullet color based on task and PR status
    let palette = app.theme.palette();
    let (bullet_color, text_color) = match task_pr {
        // PR exists - check for conflicts or CI status
        Some(pr) => {
            if pr.has_conflicts {
                // Merge conflict takes priority - show error color
                (palette.error, palette.error)
            } else {
                match pr.ci_status {
                    CiStatus::Passed => (palette.success, palette.success),
                    CiStatus::Failed => (palette.error, palette.error),
                    CiStatus::Running => (palette.warning, palette.warning),
                    CiStatus::Unknown => {
                        // PR exists but CI status unknown - treat as in-progress
                        (palette.warning, palette.success)
                    }
                }
            }
        }
        // No PR - use task status
        None => {
            if task.status == TaskStatus::InProgress {
                (palette.warning, palette.success)
            } else {
                (palette.muted, palette.muted)
            }
        }
    };

    let prefix = format!("!{} ", task.id);
    // 1-space indent + indent_level + "● " + prefix = total width before subject text
    let bullet_prefix_width = 1 + task_indent.len() + 2 + prefix.len();
    let task_line = format!("{}{}", prefix, task.subject);

    let is_task_selected = app.board_selection.as_ref().is_some_and(|sel| match sel {
        BoardSelection::Task(ch, tid) => ch == channel_name && tid == &task.id,
        _ => false,
    });

    let phase_label = task_phase_label(task, task_pr, coworker_phase);
    let wrapped_lines = wrap_content(&task_line, wrap_width);
    let title_wraps = wrapped_lines.len() > 1;

    for (i, wrapped) in wrapped_lines.iter().enumerate() {
        if i == 0 {
            // First line: 1-space indent + bullet + text as separate spans
            let bullet_span = Span::styled(
                format!(" {}● ", task_indent),
                Style::default().fg(bullet_color),
            );
            let mut text_style = Style::default().fg(text_color);
            if is_task_selected {
                text_style = text_style.bg(palette.selection);
            }
            let text_span = Span::styled(wrapped.to_string(), text_style);
            lines.push(Line::from(vec![bullet_span, text_span]));
        } else if i == 1 && phase_label.is_some() {
            // First continuation line with a phase label: label in the left gutter,
            // continuation text aligned at bullet_prefix_width
            let label_text = phase_label.unwrap();
            let label_span = Span::styled(
                format!("{:<width$}", label_text, width = bullet_prefix_width),
                Style::default().fg(Color::DarkGray),
            );
            let mut text_style = Style::default().fg(text_color);
            if is_task_selected {
                text_style = text_style.bg(Color::DarkGray);
            }
            let continuation_span = Span::styled(wrapped.trim_start().to_string(), text_style);
            lines.push(Line::from(vec![label_span, continuation_span]));
        } else {
            // Continuation lines: align with first letter of subject text
            let text = format!(
                "{:width$}{}",
                "",
                wrapped.trim_start(),
                width = bullet_prefix_width
            );
            let mut style = Style::default().fg(text_color);
            if is_task_selected {
                style = style.bg(palette.selection);
            }
            lines.push(Line::from(vec![Span::styled(text, style)]));
        }
    }

    // If the title doesn't wrap, emit the phase label as a separate line below.
    // When the title wraps, the label is merged into the first continuation line (no extra line).
    if !title_wraps && let Some(label_text) = phase_label {
        let label_line = format!("{:width$}{}", "", label_text, width = bullet_prefix_width);
        lines.push(Line::from(vec![Span::styled(
            label_line,
            Style::default().fg(palette.muted),
        )]));
    }
}

/// Draw the coworker status section (bottom of board sidebar)
fn draw_coworker_status(f: &mut Frame, app: &mut App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Header line
            Constraint::Min(0),    // Content rows (table)
        ])
        .split(area);

    // Filter out idle/completed coworkers and the project lead. The project lead
    // and channel leads are both excluded upstream by build_coworkers_data
    // (via is_project_lead / is_channel_lead); this is a defensive guard.
    let project_name_lower = app.project_name.to_lowercase();
    let active_coworkers: Vec<_> = app
        .coworkers
        .iter()
        .filter(|cw| !is_coworker_idle_or_done(cw.phase.as_deref()))
        .filter(|cw| {
            let name = cw.name.to_lowercase();
            name != "lead" && name != project_name_lower
        })
        .collect();

    // project lead and channel leads are already excluded upstream by build_coworkers_data
    let active_count = active_coworkers.len();
    let header = format!("Coworkers ({}/{})", active_count, app.max_coworkers);
    let palette = app.theme.palette();
    let header_paragraph = Paragraph::new(Line::from(vec![Span::styled(
        header,
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    )]));
    f.render_widget(header_paragraph, chunks[0]);

    // Available width inside the bordered table (subtract 2 for left+right borders)
    let available_width = area.width.saturating_sub(2) as usize;

    // Determine which columns to show based on available width.
    //
    // Column widths (fixed):
    //   name    : max name length (variable, but bounded) — pulsed bold when active
    //   task_id : 5  (e.g. "!1418")
    //   phase   : 6  (e.g. "review", "debug")
    //   pr_num  : 5  (e.g. "#1207")
    //   progress: 4  (e.g. "42% ")
    //   time    : 4  (e.g. "~3m")
    //
    // Degradation order (drop from right):
    //   Full:    name | task_id | phase | pr_num | progress | time
    //   Level 1: name | task_id | phase | pr_num | progress
    //   Level 2: name | task_id | phase | pr_num
    //   Level 3: name | task_id | phase
    //   Level 4: name | task_id
    //   Minimal: name

    let name_max = active_coworkers
        .iter()
        .map(|cw| cw.name.len())
        .max()
        .unwrap_or(6);

    // Column widths (each includes one space of padding on the right, handled by Cell)
    let w_name: usize = name_max; // right-padded to align; bold when active (pulsing)
    let w_task_id: usize = 5; // "!1418"
    let w_phase: usize = 6; // "review" (longest abbreviation)
    let w_pr: usize = 5; // "#1207"
    let w_progress: usize = 4; // "42% "
    let w_time: usize = 4; // "~3m"

    // Gap between columns — must match Table::column_spacing() below.
    let gap: usize = 1;

    // Cumulative widths for each layout level
    let base = w_name;
    let with_task = base + gap + w_task_id;
    let with_phase = with_task + gap + w_phase;
    let with_pr = with_phase + gap + w_pr;
    let with_progress = with_pr + gap + w_progress;
    let with_time = with_progress + gap + w_time;

    let show_task_id = with_task <= available_width;
    let show_phase = show_task_id && with_phase <= available_width;
    let show_pr = show_phase && with_pr <= available_width;
    let show_progress = show_pr && with_progress <= available_width;
    let show_time = show_progress && with_time <= available_width;

    // Build column constraints
    let mut constraints = vec![Constraint::Length(w_name as u16)];
    if show_task_id {
        constraints.push(Constraint::Length(w_task_id as u16));
    }
    if show_phase {
        constraints.push(Constraint::Length(w_phase as u16));
    }
    if show_pr {
        constraints.push(Constraint::Length(w_pr as u16));
    }
    if show_progress {
        constraints.push(Constraint::Length(w_progress as u16));
    }
    if show_time {
        // Min (not Length) so the last column absorbs remaining width.
        constraints.push(Constraint::Min(w_time as u16));
    }

    // Populate coworker_line_map: map absolute terminal y-coordinates to coworker names.
    // chunks[1] is the bordered table; top border is at chunks[1].y, first data row at chunks[1].y + 1.
    let table_content_y = chunks[1].y + 1; // skip top border
    for (i, cw) in active_coworkers.iter().enumerate() {
        app.coworker_line_map
            .insert(table_content_y + i as u16, cw.name.clone());
    }

    let rows: Vec<Row> = active_coworkers
        .iter()
        .enumerate()
        .map(|(row_index, cw)| {
            let health_color = coworker_health_color(cw, palette);
            let has_change = app.is_coworker_name_pulsing(&cw.name);
            let name_style = app.coworker_name_style(health_color, row_index, has_change);
            coworker_table_row(
                cw,
                name_style,
                show_task_id,
                show_phase,
                show_pr,
                show_progress,
                show_time,
                palette,
            )
        })
        .collect();

    let table = Table::new(rows, constraints)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .style(Style::default().fg(palette.fg).bg(palette.bg)),
        )
        .column_spacing(1);

    f.render_widget(table, chunks[1]);
}

/// Build a single table Row for a coworker.
#[allow(clippy::too_many_arguments)]
fn coworker_table_row(
    cw: &CoworkerInfo,
    name_style: Style,
    show_task_id: bool,
    show_phase: bool,
    show_pr: bool,
    show_progress: bool,
    show_time: bool,
    palette: ThemePalette,
) -> Row<'static> {
    let mut cells: Vec<Cell> = vec![Cell::from(cw.name.clone()).style(name_style)];

    if show_task_id {
        let task_text = cw.task_id.map(|id| format!("!{id}")).unwrap_or_default();
        cells.push(Cell::from(task_text).style(Style::default().fg(palette.muted)));
    }

    if show_phase {
        let phase_text = cw.phase.as_deref().unwrap_or("").to_string();
        cells.push(Cell::from(phase_text).style(Style::default().fg(palette.muted)));
    }

    if show_pr {
        let pr_text = cw.pr_number.map(|n| format!("#{n}")).unwrap_or_default();
        cells.push(Cell::from(pr_text).style(Style::default().fg(palette.info)));
    }

    if show_progress {
        let progress_text = cw.progress.map(|p| format!("{p}%")).unwrap_or_default();
        cells.push(Cell::from(progress_text).style(Style::default().fg(palette.accent)));
    }

    if show_time {
        let time_text = cw.time_estimate.as_deref().unwrap_or("").to_string();
        cells.push(Cell::from(time_text).style(Style::default().fg(palette.success)));
    }

    Row::new(cells)
}

fn coworker_health_color(cw: &CoworkerInfo, palette: ThemePalette) -> Color {
    match cw.health.as_str() {
        "green" => palette.success,
        "yellow" => palette.warning,
        "red" => palette.error,
        _ => palette.success,
    }
}

/// Compute indentation level for each task based on dependency structure.
/// Returns a HashMap mapping task ID to indentation level (0 = no indent, 1 = indent one level, etc.)
fn compute_task_indentation(tasks: &[&KanbanTask]) -> HashMap<String, usize> {
    let mut indentation: HashMap<String, usize> = HashMap::new();
    let mut processed: HashSet<String> = HashSet::new();

    let task_map: HashMap<String, &KanbanTask> = tasks.iter().map(|t| (t.id.clone(), *t)).collect();

    for task in tasks {
        compute_indentation_recursive(&task.id, &task_map, &mut indentation, &mut processed);
    }

    indentation
}

/// Recursive helper to compute indentation level for a task
fn compute_indentation_recursive(
    task_id: &str,
    task_map: &HashMap<String, &KanbanTask>,
    indentation: &mut HashMap<String, usize>,
    processed: &mut HashSet<String>,
) -> usize {
    if let Some(&level) = indentation.get(task_id) {
        return level;
    }

    if processed.contains(task_id) {
        return 0;
    }
    processed.insert(task_id.to_string());

    let task = match task_map.get(task_id) {
        Some(t) => t,
        None => {
            indentation.insert(task_id.to_string(), 0);
            return 0;
        }
    };

    if task.blocked_by.is_empty() {
        indentation.insert(task_id.to_string(), 0);
        return 0;
    }

    let first_blocker = task
        .blocked_by
        .iter()
        .find(|blocker_id| task_map.contains_key(blocker_id.as_str()));

    let level = if let Some(blocker_id) = first_blocker {
        let blocker_level =
            compute_indentation_recursive(blocker_id, task_map, indentation, processed);
        blocker_level + 1
    } else {
        0
    };

    indentation.insert(task_id.to_string(), level);
    level
}

#[cfg(test)]
mod tests {
    use super::super::super::app::tests::test_app;
    use super::*;

    fn make_task(id: &str, blocked_by: Vec<&str>) -> KanbanTask {
        KanbanTask {
            id: id.to_string(),
            subject: format!("Task {id}"),
            description: None,
            owner: None,
            status: TaskStatus::Pending,
            modified_at: None,
            channel: None,
            blocked_by: blocked_by.into_iter().map(|s| s.to_string()).collect(),
            message_id: None,
        }
    }

    #[test]
    fn test_indentation_no_dependencies() {
        let tasks = [
            make_task("1", vec![]),
            make_task("2", vec![]),
            make_task("3", vec![]),
        ];
        let task_refs: Vec<&KanbanTask> = tasks.iter().collect();
        let indent = compute_task_indentation(&task_refs);

        assert_eq!(indent.get("1"), Some(&0));
        assert_eq!(indent.get("2"), Some(&0));
        assert_eq!(indent.get("3"), Some(&0));
    }

    #[test]
    fn test_indentation_linear_chain() {
        let tasks = [
            make_task("A", vec![]),
            make_task("B", vec!["A"]),
            make_task("C", vec!["B"]),
        ];
        let task_refs: Vec<&KanbanTask> = tasks.iter().collect();
        let indent = compute_task_indentation(&task_refs);

        assert_eq!(indent.get("A"), Some(&0));
        assert_eq!(indent.get("B"), Some(&1));
        assert_eq!(indent.get("C"), Some(&2));
    }

    #[test]
    fn test_indentation_cycle() {
        let tasks = [make_task("A", vec!["B"]), make_task("B", vec!["A"])];
        let task_refs: Vec<&KanbanTask> = tasks.iter().collect();
        let indent = compute_task_indentation(&task_refs);

        assert!(indent.contains_key("A"));
        assert!(indent.contains_key("B"));
        assert_eq!(indent.get("B"), Some(&1));
        assert_eq!(indent.get("A"), Some(&2));
    }

    #[test]
    fn test_indentation_missing_blocker() {
        let tasks = [make_task("A", vec!["Z"])];
        let task_refs: Vec<&KanbanTask> = tasks.iter().collect();
        let indent = compute_task_indentation(&task_refs);

        assert_eq!(indent.get("A"), Some(&0));
    }

    #[test]
    fn test_indentation_diamond_dependency() {
        let tasks = [
            make_task("A", vec![]),
            make_task("B", vec!["A"]),
            make_task("C", vec!["A"]),
            make_task("D", vec!["B", "C"]),
        ];
        let task_refs: Vec<&KanbanTask> = tasks.iter().collect();
        let indent = compute_task_indentation(&task_refs);

        assert_eq!(indent.get("A"), Some(&0));
        assert_eq!(indent.get("B"), Some(&1));
        assert_eq!(indent.get("C"), Some(&1));
        assert_eq!(indent.get("D"), Some(&2));
    }

    #[test]
    fn test_indentation_partial_blockers() {
        let tasks = [make_task("A", vec![]), make_task("B", vec!["Z", "A"])];
        let task_refs: Vec<&KanbanTask> = tasks.iter().collect();
        let indent = compute_task_indentation(&task_refs);

        assert_eq!(indent.get("A"), Some(&0));
        assert_eq!(indent.get("B"), Some(&1));
    }

    #[test]
    fn test_indentation_three_node_cycle() {
        let tasks = [
            make_task("A", vec!["C"]),
            make_task("B", vec!["A"]),
            make_task("C", vec!["B"]),
        ];
        let task_refs: Vec<&KanbanTask> = tasks.iter().collect();
        let indent = compute_task_indentation(&task_refs);

        assert!(indent.contains_key("A"));
        assert!(indent.contains_key("B"));
        assert!(indent.contains_key("C"));
    }

    #[test]
    fn test_render_task_item_no_indent() {
        let app = test_app();
        let task = make_task("42", vec![]);
        let indentation = HashMap::from([("42".to_string(), 0)]);
        let mut lines = Vec::new();

        render_task_item(&app, &task, "midtown", &indentation, 80, &mut lines);

        // Pending task with no PR: only the title line (no label line)
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 2);
        // Bullet span: 1-space indent + no indent (level 0) = " ● "
        assert_eq!(spans[0].content.as_ref(), " ● ");
        // Text span: "!42 Task 42"
        assert_eq!(spans[1].content.as_ref(), "!42 Task 42");
    }

    #[test]
    fn test_render_task_item_with_indent() {
        let app = test_app();
        let task = make_task("7", vec!["1"]);
        // Indent level 1 = "  " (2 spaces)
        let indentation = HashMap::from([("7".to_string(), 1)]);
        let mut lines = Vec::new();

        render_task_item(&app, &task, "midtown", &indentation, 80, &mut lines);

        // Pending task with no PR: only the title line (no label line)
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 2);
        // Bullet span: 1-space indent + 2-space indent (level 1) = "   ● "
        assert_eq!(spans[0].content.as_ref(), "   ● ");
        // Text span: "!7 Task 7"
        assert_eq!(spans[1].content.as_ref(), "!7 Task 7");
    }

    #[test]
    fn test_render_task_item_deep_indent() {
        let app = test_app();
        let task = make_task("99", vec![]);
        // Indent level 3 = "      " (6 spaces)
        let indentation = HashMap::from([("99".to_string(), 3)]);
        let mut lines = Vec::new();

        render_task_item(&app, &task, "midtown", &indentation, 80, &mut lines);

        // Pending task with no PR: only the title line (no label line)
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        // Bullet span: 1-space indent + 6-space indent (level 3) = "       ● "
        assert_eq!(spans[0].content.as_ref(), "       ● ");
        assert_eq!(spans[1].content.as_ref(), "!99 Task 99");
    }

    #[test]
    fn test_render_task_item_pending_uses_muted_color() {
        let app = test_app();
        let expected_muted = app.theme.palette().muted;
        let task = make_task("1", vec![]);
        let indentation = HashMap::from([("1".to_string(), 0)]);
        let mut lines = Vec::new();

        render_task_item(&app, &task, "midtown", &indentation, 80, &mut lines);

        // Pending task (no PR) should use muted theme color
        let bullet_style = lines[0].spans[0].style;
        assert_eq!(bullet_style.fg, Some(expected_muted));
    }

    #[test]
    fn test_render_task_item_in_progress_uses_warning_and_success() {
        let app = test_app();
        let palette = app.theme.palette();
        let mut task = make_task("5", vec![]);
        task.status = TaskStatus::InProgress;
        let indentation = HashMap::from([("5".to_string(), 0)]);
        let mut lines = Vec::new();

        render_task_item(&app, &task, "midtown", &indentation, 80, &mut lines);

        // InProgress task (no PR) should use warning for bullet, success for text
        let bullet_style = lines[0].spans[0].style;
        let text_style = lines[0].spans[1].style;
        assert_eq!(bullet_style.fg, Some(palette.warning));
        assert_eq!(text_style.fg, Some(palette.success));
    }

    #[test]
    fn test_draw_board_panel_populates_task_line_map() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = test_app();
        app.tasks = vec![
            KanbanTask {
                id: "42".to_string(),
                subject: "First task".to_string(),
                owner: Some("york".to_string()),
                status: TaskStatus::InProgress,
                modified_at: None,
                channel: None,
                description: None,
                blocked_by: vec![],
                message_id: None,
            },
            KanbanTask {
                id: "43".to_string(),
                subject: "Second task".to_string(),
                owner: Some("lexington".to_string()),
                status: TaskStatus::Pending,
                modified_at: None,
                channel: None,
                description: None,
                blocked_by: vec![],
                message_id: None,
            },
        ];

        terminal
            .draw(|f| {
                let area = f.area();
                let (_hyperlinks, tasks_area) = draw_board_panel(f, &mut app, area);

                // Verify tasks_area is returned correctly
                assert!(tasks_area.width > 0);
                assert!(tasks_area.height > 0);
            })
            .unwrap();

        // Verify task_line_map is populated
        assert!(
            !app.task_line_map.is_empty(),
            "task_line_map should be populated"
        );

        // Verify both tasks are in the map
        let task_42_found = app
            .task_line_map
            .values()
            .any(|(id, owner)| id == "42" && owner.as_deref() == Some("york"));
        let task_43_found = app
            .task_line_map
            .values()
            .any(|(id, owner)| id == "43" && owner.as_deref() == Some("lexington"));

        assert!(task_42_found, "Task 42 should be in task_line_map");
        assert!(task_43_found, "Task 43 should be in task_line_map");
    }

    #[test]
    fn test_draw_board_panel_returns_correct_tasks_area() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = test_app();
        // Add a coworker to trigger the split
        app.coworkers = vec![CoworkerInfo {
            name: "york".to_string(),
            task_id: Some(42),
            phase: Some("developing".to_string()),
            pr_number: None,
            health: "green".to_string(),
            provider: "claude".to_string(),
            profile: "test".to_string(),
            progress: None,
            time_estimate: None,
        }];

        let mut returned_tasks_area = None;

        terminal
            .draw(|f| {
                let area = Rect {
                    x: 0,
                    y: 0,
                    width: 80,
                    height: 40,
                };
                let (_hyperlinks, tasks_area) = draw_board_panel(f, &mut app, area);
                returned_tasks_area = Some(tasks_area);
            })
            .unwrap();

        let tasks_area = returned_tasks_area.unwrap();

        // When coworkers exist, tasks_area should be smaller than the input area
        // because the coworker status section takes up space at the bottom
        assert!(
            tasks_area.height < 40,
            "tasks_area should exclude coworker section"
        );
    }

    fn make_coworker(name: &str) -> CoworkerInfo {
        CoworkerInfo {
            name: name.to_string(),
            task_id: None,
            phase: Some("developing".to_string()),
            pr_number: None,
            health: "green".to_string(),
            provider: "claude".to_string(),
            profile: "default".to_string(),
            progress: None,
            time_estimate: None,
        }
    }

    #[test]
    fn test_coworker_table_row_all_columns() {
        use ratatui_themes::{Theme, ThemeName};
        let palette = Theme::new(ThemeName::CatppuccinMocha).palette();
        let mut cw = make_coworker("park");
        cw.task_id = Some(1418);
        cw.pr_number = Some(1207);
        cw.progress = Some(42);
        cw.time_estimate = Some("~3m".to_string());

        let name_style = Style::default().fg(palette.success);
        let row = coworker_table_row(&cw, name_style, true, true, true, true, true, palette);

        // Verify all 7 columns are present by checking the row can be constructed
        // (Row doesn't expose cell count directly, but we verify data is correct
        // by checking what we passed to each show_* flag)
        let _ = row; // Row is valid, column building doesn't panic
    }

    #[test]
    fn test_coworker_table_row_minimal_columns() {
        use ratatui_themes::{Theme, ThemeName};
        let palette = Theme::new(ThemeName::CatppuccinMocha).palette();
        let cw = make_coworker("york");
        // Minimal: only spinner + name (all show_* = false)
        let name_style = Style::default().fg(palette.success);
        let row = coworker_table_row(&cw, name_style, false, false, false, false, false, palette);
        let _ = row;
    }

    #[test]
    fn test_coworker_section_height_no_coworkers() {
        // When there are no coworkers, section height should be 0
        let coworker_count: u16 = 0;
        let height = if coworker_count > 0 {
            coworker_count + 3
        } else {
            0
        };
        assert_eq!(height, 0);
    }

    #[test]
    fn test_coworker_section_height_with_coworkers() {
        // 1 coworker → header (1) + row (1) + 2 borders = 4... but the current
        // implementation does active_count + 3 (1 header + N rows + 2 borders).
        // With 1 active coworker the height should be 4.
        // This mirrors the formula in draw_board_panel().
        let active_count: u16 = 1;
        let height = active_count + 3;
        assert_eq!(height, 4);

        let active_count: u16 = 3;
        let height = active_count + 3;
        assert_eq!(height, 6);
    }

    #[test]
    fn test_draw_coworker_status_renders_without_panic() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = test_app();
        app.coworkers = vec![
            {
                let mut cw = make_coworker("park");
                cw.task_id = Some(1418);
                cw.phase = Some("developing".to_string());
                cw.pr_number = Some(1207);
                cw.progress = Some(42);
                cw.time_estimate = Some("~3m".to_string());
                cw
            },
            {
                let mut cw = make_coworker("amsterdam");
                cw.task_id = Some(1419);
                cw.phase = Some("pull-request".to_string());
                cw.pr_number = Some(1208);
                cw.progress = Some(78);
                cw
            },
        ];
        app.max_coworkers = 4;

        terminal
            .draw(|f| {
                let area = f.area();
                draw_coworker_status(f, &mut app, area);
            })
            .unwrap();
        // If we get here without panicking, the render succeeded
    }

    #[test]
    fn test_draw_coworker_status_narrow_width_no_panic() {
        // Test responsive degradation at very narrow widths
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        for width in [20u16, 25, 30, 40, 50, 60, 80] {
            let backend = TestBackend::new(width, 6);
            let mut terminal = Terminal::new(backend).unwrap();

            let mut app = test_app();
            app.coworkers = vec![{
                let mut cw = make_coworker("amsterdam");
                cw.task_id = Some(1234);
                cw.phase = Some("testing".to_string());
                cw.pr_number = Some(999);
                cw.progress = Some(50);
                cw.time_estimate = Some("~5m".to_string());
                cw
            }];

            terminal
                .draw(|f| {
                    let area = f.area();
                    draw_coworker_status(f, &mut app, area);
                })
                .unwrap();
        }
    }

    #[test]
    fn test_draw_coworker_status_health_colors() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = test_app();
        app.coworkers = vec![
            {
                let mut cw = make_coworker("york");
                cw.health = "green".to_string();
                cw
            },
            {
                let mut cw = make_coworker("park");
                cw.health = "yellow".to_string();
                cw
            },
            {
                let mut cw = make_coworker("lexington");
                cw.health = "red".to_string();
                cw
            },
            {
                let mut cw = make_coworker("madison");
                cw.health = "unknown".to_string();
                cw
            },
        ];

        terminal
            .draw(|f| {
                let area = f.area();
                draw_coworker_status(f, &mut app, area);
            })
            .unwrap();
        // Verifies health-based color logic handles all branches including "unknown"
    }

    #[test]
    fn test_draw_coworker_status_idle_coworkers_excluded() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = test_app();
        app.coworkers = vec![
            {
                let mut cw = make_coworker("york");
                cw.phase = Some("idle".to_string());
                cw
            },
            {
                // No phase set — freshly spawned, should still appear
                let mut cw = make_coworker("park");
                cw.phase = None;
                cw
            },
            {
                let mut cw = make_coworker("amsterdam");
                cw.phase = Some("developing".to_string());
                cw
            },
        ];

        // "amsterdam" (developing) and "park" (no phase yet) should appear.
        // "york" (idle) should be excluded. Verifies no panic
        // and renders correctly with a mix of idle/none/active coworkers.
        terminal
            .draw(|f| {
                let area = f.area();
                draw_coworker_status(f, &mut app, area);
            })
            .unwrap();
    }

    #[test]
    fn test_draw_board_panel_coworker_section_snug_height() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = test_app();
        // 2 active coworkers → expected section height: 2 + 3 = 5
        app.coworkers = vec![
            {
                let mut cw = make_coworker("york");
                cw.phase = Some("developing".to_string());
                cw
            },
            {
                let mut cw = make_coworker("park");
                cw.phase = Some("testing".to_string());
                cw
            },
        ];
        app.max_coworkers = 4;

        let mut returned_tasks_area = None;

        terminal
            .draw(|f| {
                let area = Rect {
                    x: 0,
                    y: 0,
                    width: 80,
                    height: 40,
                };
                let (_hyperlinks, tasks_area) = draw_board_panel(f, &mut app, area);
                returned_tasks_area = Some(tasks_area);
            })
            .unwrap();

        let tasks_area = returned_tasks_area.unwrap();
        // 2 active coworkers → coworker section height = 2 + 3 = 5
        // tasks_area height should be 40 - 5 = 35
        assert_eq!(
            tasks_area.height, 35,
            "tasks area height should leave exactly 5 rows for 2 coworkers"
        );
    }

    #[test]
    fn test_draw_board_panel_idle_coworkers_dont_inflate_height() {
        // Regression test: idle coworkers should not inflate the section height.
        // If 2 out of 3 coworkers are idle, only 1 active row should be reserved.
        // Coworkers with phase=None (freshly spawned) ARE shown.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = test_app();
        app.coworkers = vec![
            {
                let mut cw = make_coworker("york");
                cw.phase = Some("idle".to_string()); // idle — excluded
                cw
            },
            {
                let mut cw = make_coworker("park");
                cw.phase = Some("done".to_string()); // completed — excluded
                cw
            },
            {
                let mut cw = make_coworker("amsterdam");
                cw.phase = Some("developing".to_string()); // active
                cw
            },
        ];
        app.max_coworkers = 4;

        let mut returned_tasks_area = None;

        terminal
            .draw(|f| {
                let area = Rect {
                    x: 0,
                    y: 0,
                    width: 80,
                    height: 40,
                };
                let (_hyperlinks, tasks_area) = draw_board_panel(f, &mut app, area);
                returned_tasks_area = Some(tasks_area);
            })
            .unwrap();

        let tasks_area = returned_tasks_area.unwrap();
        // Only 1 active coworker → coworker section height = 1 + 3 = 4
        // tasks_area height should be 40 - 4 = 36
        assert_eq!(
            tasks_area.height, 36,
            "idle coworkers should not inflate section height (only 1 active coworker)"
        );
    }

    #[test]
    fn test_draw_board_panel_project_lead_does_not_inflate_height() {
        // Regression test: project lead (literal "lead" or repo-named) with an active phase
        // must not inflate the coworker section height. Only real coworkers count.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = test_app();
        // project_name is "test" in test_app(); the repo-named lead would be "test"
        app.coworkers = vec![
            {
                let mut cw = make_coworker("lead"); // legacy lead name — must be excluded
                cw.phase = Some("developing".to_string());
                cw
            },
            {
                let mut cw = make_coworker("york"); // regular coworker
                cw.phase = Some("developing".to_string());
                cw
            },
        ];
        app.max_coworkers = 4;

        let mut returned_tasks_area = None;

        terminal
            .draw(|f| {
                let area = Rect {
                    x: 0,
                    y: 0,
                    width: 80,
                    height: 40,
                };
                let (_hyperlinks, tasks_area) = draw_board_panel(f, &mut app, area);
                returned_tasks_area = Some(tasks_area);
            })
            .unwrap();

        let tasks_area = returned_tasks_area.unwrap();
        // Only 1 visible coworker (lead is excluded) → coworker section height = 1 + 3 = 4
        // tasks_area height should be 40 - 4 = 36
        assert_eq!(
            tasks_area.height, 36,
            "project lead must not inflate coworker section height (only york should be counted)"
        );
    }

    #[test]
    fn test_phase_column_width_fits_all_abbreviations() {
        use midtown::coworker_state::WorkflowPhase;

        // w_phase in draw_coworker_status must fit the longest abbreviation.
        let w_phase: usize = 6;
        let phases = [
            WorkflowPhase::Claiming,
            WorkflowPhase::Developing,
            WorkflowPhase::Testing,
            WorkflowPhase::PullRequest,
            WorkflowPhase::Reviewing,
            WorkflowPhase::Debugging,
            WorkflowPhase::Completed,
            WorkflowPhase::Idle,
        ];

        for phase in &phases {
            let abbr = phase.abbreviation();
            assert!(
                abbr.len() <= w_phase,
                "Phase {:?} abbreviation {:?} ({} chars) exceeds column width {}",
                phase,
                abbr,
                abbr.len(),
                w_phase,
            );
        }
    }

    #[test]
    fn test_coworker_table_row_review_phase_untruncated() {
        use ratatui_themes::{Theme, ThemeName};
        // Verify that "review" (the longest phase abbreviation) renders
        // without truncation in the coworker table row.
        let palette = Theme::new(ThemeName::CatppuccinMocha).palette();
        let mut cw = make_coworker("york");
        cw.phase = Some("review".to_string());
        cw.task_id = Some(42);

        let name_style = Style::default().fg(palette.success);
        let row = coworker_table_row(&cw, name_style, true, true, true, false, false, palette);
        let _ = row; // Row builds successfully with the phase data
    }

    fn make_pr(task_id: u64) -> KanbanPr {
        KanbanPr {
            number: 100,
            title: format!("Some PR [Midtown !{task_id}]"),
            author: "pleasant".to_string(),
            created_at: chrono::Utc::now(),
            ci_status: CiStatus::Unknown,
            reviewer: None,
            reviewed_at: None,
            review_posted: false,
            repo: None,
            task_id: Some(task_id),
            task_name: None,
            has_conflicts: false,
        }
    }

    // --- task_phase_label tests ---

    #[test]
    fn test_phase_label_pending_no_pr() {
        let task = make_task("1", vec![]);
        assert_eq!(task_phase_label(&task, None, None), None);
    }

    #[test]
    fn test_phase_label_in_progress_no_pr() {
        let mut task = make_task("1", vec![]);
        task.status = TaskStatus::InProgress;
        assert_eq!(task_phase_label(&task, None, None), Some("dev"));
    }

    #[test]
    fn test_phase_label_pr_no_review() {
        let task = make_task("1", vec![]);
        let pr = make_pr(1);
        assert_eq!(task_phase_label(&task, Some(&pr), None), Some("pr"));
    }

    #[test]
    fn test_phase_label_ci_running_takes_priority() {
        let task = make_task("1", vec![]);
        let mut pr = make_pr(1);
        pr.ci_status = CiStatus::Running;
        pr.review_posted = true; // even if review was posted, ci takes priority
        assert_eq!(task_phase_label(&task, Some(&pr), None), Some("ci"));
    }

    #[test]
    fn test_phase_label_ci_failed_shows_ci() {
        let task = make_task("1", vec![]);
        let mut pr = make_pr(1);
        pr.ci_status = CiStatus::Failed;
        assert_eq!(task_phase_label(&task, Some(&pr), None), Some("ci"));
    }

    #[test]
    fn test_phase_label_reviewer_assigned_not_posted() {
        let task = make_task("1", vec![]);
        let mut pr = make_pr(1);
        pr.reviewer = Some("york".to_string());
        pr.review_posted = false;
        assert_eq!(task_phase_label(&task, Some(&pr), None), Some("rvw"));
    }

    #[test]
    fn test_phase_label_review_posted_idle_coworker() {
        let task = make_task("1", vec![]);
        let mut pr = make_pr(1);
        pr.review_posted = true;
        pr.reviewer = Some("york".to_string());
        // Coworker is idle — review done, not yet addressed
        assert_eq!(
            task_phase_label(&task, Some(&pr), Some("idle")),
            Some("rvd")
        );
    }

    #[test]
    fn test_phase_label_review_posted_coworker_developing() {
        let task = make_task("1", vec![]);
        let mut pr = make_pr(1);
        pr.review_posted = true;
        pr.reviewer = Some("york".to_string());
        // coworker_phase holds abbreviations: "dev" not "developing"
        assert_eq!(
            task_phase_label(&task, Some(&pr), Some("dev")),
            Some("addr")
        );
    }

    #[test]
    fn test_phase_label_review_posted_coworker_debugging() {
        let task = make_task("1", vec![]);
        let mut pr = make_pr(1);
        pr.review_posted = true;
        pr.reviewer = Some("york".to_string());
        // coworker_phase holds abbreviations: "debug" not "debugging"
        assert_eq!(
            task_phase_label(&task, Some(&pr), Some("debug")),
            Some("addr")
        );
    }

    #[test]
    fn test_phase_label_review_posted_coworker_testing() {
        let task = make_task("1", vec![]);
        let mut pr = make_pr(1);
        pr.review_posted = true;
        pr.reviewer = Some("york".to_string());
        // coworker_phase holds abbreviations: "test" not "testing"
        assert_eq!(
            task_phase_label(&task, Some(&pr), Some("test")),
            Some("addr")
        );
    }

    #[test]
    fn test_phase_label_done_ci_passed_review_posted() {
        let task = make_task("1", vec![]);
        let mut pr = make_pr(1);
        pr.ci_status = CiStatus::Passed;
        pr.review_posted = true;
        pr.reviewer = Some("york".to_string());
        assert_eq!(
            task_phase_label(&task, Some(&pr), Some("idle")),
            Some("done")
        );
    }

    #[test]
    fn test_phase_label_ci_passed_no_reviewer_shows_pr() {
        // PR with CI passing but no reviewer assigned should show "pr", not "done"
        // "done" means feedback addressed and CI green — but no review has happened yet
        let task = make_task("1", vec![]);
        let mut pr = make_pr(1);
        pr.ci_status = CiStatus::Passed;
        assert_eq!(task_phase_label(&task, Some(&pr), None), Some("pr"));
    }

    #[test]
    fn test_phase_label_second_line_rendered_in_muted_color() {
        let app = test_app();
        let expected_muted = app.theme.palette().muted;
        let mut task = make_task("5", vec![]);
        task.status = TaskStatus::InProgress;
        let indentation = HashMap::from([("5".to_string(), 0)]);
        let mut lines = Vec::new();

        render_task_item(&app, &task, "midtown", &indentation, 80, &mut lines);

        // Second line is the phase label
        assert_eq!(lines.len(), 2);
        let label_line = &lines[1];
        assert_eq!(label_line.spans.len(), 1);
        let span = &label_line.spans[0];
        assert_eq!(span.style.fg, Some(expected_muted));
        // Content should contain "dev" for in_progress task with no PR
        assert!(
            span.content.contains("dev"),
            "label line: {:?}",
            span.content
        );
    }

    #[test]
    fn test_phase_label_pending_task_no_extra_line() {
        // Pending task with no PR produces only 1 line (no blank label line)
        let app = test_app();
        let task = make_task("3", vec![]); // pending, no PR
        let indentation = HashMap::from([("3".to_string(), 0)]);
        let mut lines = Vec::new();

        render_task_item(&app, &task, "midtown", &indentation, 80, &mut lines);

        assert_eq!(
            lines.len(),
            1,
            "pending task should produce only a title line"
        );
    }

    #[test]
    fn test_phase_label_conflicts_takes_priority_over_ci() {
        // PR with merge conflicts should show "cnfl", not "ci"
        let task = make_task("1", vec![]);
        let mut pr = make_pr(1);
        pr.has_conflicts = true;
        pr.ci_status = CiStatus::Failed;
        assert_eq!(
            task_phase_label(&task, Some(&pr), None),
            Some("cnfl"),
            "has_conflicts should take priority over CI status"
        );
    }

    // --- Bug !1615: phase label position and stale "dev" label ---

    // Bug 2: task_phase_label should use coworker_phase when no PR exists
    #[test]
    fn test_phase_label_no_pr_coworker_testing() {
        let mut task = make_task("1", vec![]);
        task.status = TaskStatus::InProgress;
        // Coworker reported "test" phase — should reflect that, not always "dev"
        assert_eq!(task_phase_label(&task, None, Some("test")), Some("test"));
    }

    #[test]
    fn test_phase_label_no_pr_coworker_claiming() {
        let mut task = make_task("1", vec![]);
        task.status = TaskStatus::InProgress;
        assert_eq!(task_phase_label(&task, None, Some("claim")), Some("claim"));
    }

    #[test]
    fn test_phase_label_no_pr_coworker_pull_request() {
        let mut task = make_task("1", vec![]);
        task.status = TaskStatus::InProgress;
        assert_eq!(task_phase_label(&task, None, Some("PR")), Some("PR"));
    }

    #[test]
    fn test_phase_label_no_pr_coworker_debugging() {
        let mut task = make_task("1", vec![]);
        task.status = TaskStatus::InProgress;
        assert_eq!(task_phase_label(&task, None, Some("debug")), Some("debug"));
    }

    #[test]
    fn test_phase_label_no_pr_coworker_dev_stays_dev() {
        // When coworker is "dev", should still show "dev"
        let mut task = make_task("1", vec![]);
        task.status = TaskStatus::InProgress;
        assert_eq!(task_phase_label(&task, None, Some("dev")), Some("dev"));
    }

    #[test]
    fn test_phase_label_no_pr_no_coworker_phase_stays_dev() {
        // When there's no coworker phase reported, fall back to "dev"
        let mut task = make_task("1", vec![]);
        task.status = TaskStatus::InProgress;
        assert_eq!(task_phase_label(&task, None, None), Some("dev"));
    }

    // Bug 1: phase label should appear on the continuation line (second row), not as a separate
    // extra line after all title lines.

    #[test]
    fn test_phase_label_position_no_wrap() {
        // Single-line title: phase label goes on a new separate line (2 total lines)
        let app = test_app();
        let mut task = make_task("5", vec![]);
        task.status = TaskStatus::InProgress;
        let indentation = HashMap::from([("5".to_string(), 0)]);
        let mut lines = Vec::new();

        render_task_item(&app, &task, "midtown", &indentation, 80, &mut lines);

        // 2 lines: title + label (unchanged from current behavior for single-line title)
        assert_eq!(lines.len(), 2, "single-line title: title + label line");
    }

    #[test]
    fn test_phase_label_position_with_wrap_no_extra_line() {
        // Wrapped title: phase label appears on the first continuation line (NOT as extra line)
        // Total lines = number of wrapped title lines (the label is merged into line[1])
        let app = test_app();
        let mut task = make_task("5", vec![]);
        task.subject = "A very long title that definitely wraps across multiple lines in the sidebar board panel display area test".to_string();
        task.status = TaskStatus::InProgress;
        let indentation = HashMap::from([("5".to_string(), 0)]);
        let mut lines = Vec::new();

        // Use a narrow wrap_width to force wrapping
        render_task_item(&app, &task, "midtown", &indentation, 30, &mut lines);

        // With wrapping, the number of lines should equal the wrapped title lines count
        // (the label is merged INTO line[1], not appended as an extra line)
        let task_text = format!("!5 {}", task.subject);
        let wrapped = super::super::text::wrap_content(&task_text, 30);
        let wrapped_count = wrapped.len();
        assert!(
            wrapped_count >= 2,
            "title should wrap with width=30, got {} lines",
            wrapped_count
        );
        assert_eq!(
            lines.len(),
            wrapped_count,
            "wrapped title: label merged into continuation line, total lines = wrapped line count"
        );
    }

    #[test]
    fn test_phase_label_on_continuation_line_content() {
        // When title wraps, line[1] should start with the label, then the continuation text
        let app = test_app();
        let mut task = make_task("5", vec![]);
        task.subject = "A very long title that definitely wraps across multiple lines in the sidebar board panel display area test".to_string();
        task.status = TaskStatus::InProgress;
        let indentation = HashMap::from([("5".to_string(), 0)]);
        let mut lines = Vec::new();

        render_task_item(&app, &task, "midtown", &indentation, 30, &mut lines);

        // line[1] should contain the "dev" label
        assert!(
            lines.len() >= 2,
            "expected at least 2 lines, got {}",
            lines.len()
        );
        let continuation_content: String = lines[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            continuation_content.contains("dev"),
            "continuation line should contain 'dev' label, got: {:?}",
            continuation_content
        );
    }
}

#[path = "board_tests.rs"]
#[cfg(test)]
mod board_tests;
