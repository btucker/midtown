//! Board panel: channel list and coworker status table.

use std::collections::{BTreeMap, HashMap, HashSet};

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
};

use super::super::app::{App, KanbanTask, TaskStatus};
use super::Hyperlink;
use super::text::wrap_content;

/// Draw the board panel (left side) with channel list
pub fn draw_board_panel(f: &mut Frame, app: &mut App, area: Rect) -> Vec<Hyperlink> {
    // Split board area vertically: tasks at top, coworkers at bottom
    let active_coworker_count = app.coworkers.len();
    let coworker_section_height = if active_coworker_count > 0 {
        active_coworker_count as u16 + 3 // 1 header + N rows + 2 borders
    } else {
        0
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if coworker_section_height > 0 {
            vec![
                Constraint::Min(10),
                Constraint::Length(coworker_section_height),
            ]
        } else {
            vec![Constraint::Min(10)]
        })
        .split(area);

    let tasks_area = chunks[0];
    let mut lines = Vec::new();
    let hyperlinks = Vec::new();

    // Default channel matches the daemon's ChannelRouter default ("midtown")
    let main_channel = "midtown";

    let mut tasks_by_channel: BTreeMap<String, Vec<&KanbanTask>> = BTreeMap::new();
    let (pending, in_progress, _completed) = app.tasks_by_status();

    for task in in_progress.iter().chain(pending.iter()) {
        let channel_key = task.channel.as_deref().unwrap_or(main_channel).to_string();
        tasks_by_channel.entry(channel_key).or_default().push(task);
    }

    let wrap_width = area.width.saturating_sub(2).max(20) as usize;

    // Count active PRs per channel
    let mut prs_by_channel: HashMap<String, Vec<&super::super::app::KanbanPr>> = HashMap::new();
    for pr in &app.prs {
        if let Some(task_id) = midtown::tasks::extract_task_id_from_pr_title(&pr.title) {
            let task_id_str = task_id.to_string();
            if let Some(task) = app.tasks.iter().find(|t| t.id == task_id_str) {
                let channel_key = task.channel.as_deref().unwrap_or(main_channel).to_string();
                prs_by_channel.entry(channel_key).or_default().push(pr);
            }
        }
    }

    // Render each channel as a swimlane
    let mut first_channel = true;
    for (channel_name, tasks) in &tasks_by_channel {
        if !first_channel {
            lines.push(Line::from(""));
        }
        first_channel = false;

        render_channel_header(app, channel_name, tasks, &prs_by_channel, &mut lines);
        lines.push(Line::from("")); // Blank line after header

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
    let is_focused = app.focused_pane == super::super::app::FocusedPane::Board;
    let border_color = if is_focused {
        Color::Yellow
    } else {
        Color::White
    };

    // Render the tasks panel with focus-dependent border
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Board")
        .border_style(Style::default().fg(border_color));

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, tasks_area);

    if coworker_section_height > 0 {
        draw_coworker_status(f, app, chunks[1]);
    }

    hyperlinks
}

/// Render a channel header line with task count and unread count.
fn render_channel_header(
    app: &App,
    channel_name: &str,
    tasks: &[&KanbanTask],
    _prs_by_channel: &HashMap<String, Vec<&super::super::app::KanbanPr>>,
    lines: &mut Vec<Line<'static>>,
) {
    let task_count = tasks.len();
    let channel_header = if let Some(&unread_count) = app.channel_unread_counts.get(channel_name) {
        format!(
            "  #{} ({}) — {} tasks",
            channel_name, unread_count, task_count
        )
    } else {
        format!("  #{} — {} tasks", channel_name, task_count)
    };

    let is_selected = app.board_selection.as_ref().is_some_and(|sel| match sel {
        super::super::app::BoardSelection::Channel(ch) => ch == channel_name,
        _ => false,
    });

    let mut style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    if is_selected {
        style = style.bg(Color::DarkGray);
    }

    lines.push(Line::from(vec![Span::styled(channel_header, style)]));
}

/// Render a single task item with indentation and wrapping.
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

    // Determine bullet color based on task and PR status
    let (bullet_color, text_color) = match task_pr {
        // PR exists - check for conflicts or CI status
        Some(pr) => {
            if pr.has_conflicts {
                // Merge conflict takes priority - show red
                (Color::Red, Color::Red)
            } else {
                match pr.ci_status {
                    super::super::app::CiStatus::Passed => (Color::Green, Color::Green),
                    super::super::app::CiStatus::Failed => (Color::Red, Color::Red),
                    super::super::app::CiStatus::Running => (Color::Yellow, Color::Yellow),
                    super::super::app::CiStatus::Unknown => {
                        // PR exists but CI status unknown - treat as in-progress
                        (Color::Yellow, Color::Green)
                    }
                }
            }
        }
        // No PR - use task status
        None => {
            if task.status == TaskStatus::InProgress {
                (Color::Yellow, Color::Green)
            } else {
                (Color::DarkGray, Color::DarkGray)
            }
        }
    };

    let prefix = format!("!{} ", task.id);
    let prefix_width = task_indent.len() + 2 + prefix.len(); // indent + "● " + prefix
    let task_line = format!("{}{}", prefix, task.subject);

    let is_task_selected = app.board_selection.as_ref().is_some_and(|sel| match sel {
        super::super::app::BoardSelection::Task(ch, tid) => ch == channel_name && tid == &task.id,
        _ => false,
    });

    let wrapped_lines = wrap_content(&task_line, wrap_width);
    for (i, wrapped) in wrapped_lines.iter().enumerate() {
        if i == 0 {
            // First line: render indent + bullet + text as separate spans
            let bullet_span = Span::styled(
                format!("{}● ", task_indent),
                Style::default().fg(bullet_color),
            );
            let mut text_style = Style::default().fg(text_color);
            if is_task_selected {
                text_style = text_style.bg(Color::DarkGray);
            }
            let text_span = Span::styled(wrapped.to_string(), text_style);
            lines.push(Line::from(vec![bullet_span, text_span]));
        } else {
            // Continuation lines: indent without bullet
            let indent_width = prefix_width.saturating_sub(2);
            let text = format!(
                "{:width$}{}",
                "",
                wrapped.trim_start(),
                width = indent_width
            );
            let mut style = Style::default().fg(text_color);
            if is_task_selected {
                style = style.bg(Color::DarkGray);
            }
            lines.push(Line::from(vec![Span::styled(text, style)]));
        }
    }
}

/// Draw the coworker status section (bottom of board sidebar)
fn draw_coworker_status(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Header line
            Constraint::Min(0),    // Table rows
        ])
        .split(area);

    // Filter out idle coworkers - only show those actively working
    let active_coworkers: Vec<_> = app
        .coworkers
        .iter()
        .filter(|cw| {
            // Show coworker if they have a phase and it's not "idle"
            cw.phase.as_deref() != Some("idle") && cw.phase.is_some()
        })
        .collect();

    let active_count = active_coworkers.len();
    let header = format!("  Coworkers ({}/{})", active_count, app.max_coworkers);
    let header_paragraph = Paragraph::new(Line::from(vec![Span::styled(
        header,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    f.render_widget(header_paragraph, chunks[0]);

    let rows: Vec<Row> = active_coworkers
        .iter()
        .map(|cw| {
            let health_dot = "●";
            let health_color = match cw.health.as_str() {
                "green" => Color::Green,
                "yellow" => Color::Yellow,
                "red" => Color::Red,
                _ => Color::Green,
            };

            let mut cells = vec![
                Cell::from(health_dot).style(Style::default().fg(health_color)),
                Cell::from(cw.name.clone()),
            ];

            cells.push(Cell::from(
                cw.task_id.map(|id| format!("!{}", id)).unwrap_or_default(),
            ));
            cells.push(Cell::from(cw.phase.clone().unwrap_or_default()));
            cells.push(Cell::from(
                cw.pr_number
                    .map(|pr| format!("#{}", pr))
                    .unwrap_or_default(),
            ));
            // Progress bar column
            cells.push(Cell::from(
                cw.progress
                    .map(|p| {
                        let bar_width = 10;
                        let filled = (p as usize * bar_width) / 100;
                        let empty = bar_width - filled;
                        format!("[{}{}] {}%", "█".repeat(filled), "░".repeat(empty), p)
                    })
                    .unwrap_or_default(),
            ));

            Row::new(cells)
        })
        .collect();

    let widths = [
        Constraint::Length(2),
        Constraint::Min(10),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Length(5),
        Constraint::Length(17), // Progress bar: "[██████░░░░] 60%"
    ];

    let table = Table::new(rows, widths)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .style(Style::default().fg(Color::White)),
        )
        .column_spacing(1);

    f.render_widget(table, chunks[1]);
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
            owner: None,
            status: TaskStatus::Pending,
            modified_at: None,
            channel: None,
            blocked_by: blocked_by.into_iter().map(|s| s.to_string()).collect(),
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

        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 2);
        // Bullet span: no indent (level 0), just "● "
        assert_eq!(spans[0].content.as_ref(), "● ");
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

        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 2);
        // Bullet span includes indent: "  ● "
        assert_eq!(spans[0].content.as_ref(), "  ● ");
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

        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        // Bullet span includes 6-space indent: "      ● "
        assert_eq!(spans[0].content.as_ref(), "      ● ");
        assert_eq!(spans[1].content.as_ref(), "!99 Task 99");
    }

    #[test]
    fn test_render_task_item_pending_uses_dark_gray() {
        let app = test_app();
        let task = make_task("1", vec![]);
        let indentation = HashMap::from([("1".to_string(), 0)]);
        let mut lines = Vec::new();

        render_task_item(&app, &task, "midtown", &indentation, 80, &mut lines);

        // Pending task (no PR) should use DarkGray
        let bullet_style = lines[0].spans[0].style;
        assert_eq!(bullet_style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn test_render_task_item_in_progress_uses_yellow_bullet() {
        let app = test_app();
        let mut task = make_task("5", vec![]);
        task.status = TaskStatus::InProgress;
        let indentation = HashMap::from([("5".to_string(), 0)]);
        let mut lines = Vec::new();

        render_task_item(&app, &task, "midtown", &indentation, 80, &mut lines);

        // InProgress task (no PR) should use Yellow bullet, Green text
        let bullet_style = lines[0].spans[0].style;
        let text_style = lines[0].spans[1].style;
        assert_eq!(bullet_style.fg, Some(Color::Yellow));
        assert_eq!(text_style.fg, Some(Color::Green));
    }
}
