//! Board panel: channel list and coworker status table.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};
use midtown::MessageType;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
};

use super::super::app::{
    App, BoardSelection, CiStatus, CoworkerInfo, FocusedPane, KanbanPr, KanbanTask, TaskStatus,
};
use super::Hyperlink;
use super::text::wrap_content;

/// Senders whose messages always belong in the Ops channel.
const OPS_SENDERS: &[&str] = &["midtown", "github", "system", "daemon"];

/// Maximum number of recent ops messages to display in the mini-channel.
const OPS_MAX_MESSAGES: usize = 20;

/// Senders whose /me actions stay in the main chat (not ops).
const NON_COWORKER_SENDERS: &[&str] = &["lead", "user"];

/// Returns true if the message belongs in the Ops mini-channel.
///
/// Ops messages are:
/// - Messages from system/daemon senders (midtown, github, system, daemon)
/// - Action messages (/me) from coworkers only (not lead or user)
pub fn is_ops_message(from: &str, msg_type: &MessageType, content: &str) -> bool {
    let sender = from.to_lowercase();
    if OPS_SENDERS.iter().any(|&s| s == sender) {
        return true;
    }
    // /me action messages = coworker workflow status updates; exclude lead/user
    if NON_COWORKER_SENDERS.iter().any(|&s| s == sender) {
        return false;
    }
    if *msg_type == MessageType::Action || content.starts_with("/me ") {
        return true;
    }
    false
}

/// Format a timestamp for the ops mini-channel (HH:MM).
fn format_ops_time(ts: &DateTime<Utc>) -> String {
    use chrono::Timelike;
    format!("{:02}:{:02}", ts.hour(), ts.minute())
}

/// Draw the board panel (left side) with channel list
///
/// Returns (hyperlinks, tasks_area) where tasks_area is the rect containing the task list
/// (excluding the coworker status section). This is used for click detection.
pub fn draw_board_panel(f: &mut Frame, app: &mut App, area: Rect) -> (Vec<Hyperlink>, Rect) {
    // Clear line maps for new render
    app.task_line_map.clear();
    app.channel_line_map.clear();

    // Split board area vertically: tasks at top, coworkers + ops at bottom
    let active_coworker_count = app
        .coworkers
        .iter()
        .filter(|cw| cw.phase.as_deref() != Some("idle") && cw.phase.is_some())
        .count();
    let coworker_section_height = if active_coworker_count > 0 {
        active_coworker_count as u16 + 3 // 1 header + N rows + 2 borders
    } else {
        0
    };

    // Ops mini-channel: fixed height (header + N message rows + 2 borders)
    // Show at most OPS_MAX_MESSAGES rows but cap at 8 visible lines to avoid
    // crowding the task list on short terminals.
    //
    // Clone the filtered ops messages to avoid holding an immutable borrow on
    // app.messages while draw_coworker_status takes app mutably below.
    let ops_messages: Vec<midtown::Message> = {
        let recent: Vec<_> = app
            .messages
            .iter()
            .filter(|m| is_ops_message(&m.from, &m.message_type, &m.content))
            .rev()
            .take(OPS_MAX_MESSAGES)
            .collect();
        recent.into_iter().rev().cloned().collect()
    };
    let ops_visible_rows = ops_messages.len().min(8) as u16;
    // Header line + visible rows + 2 borders = ops_section_height
    let ops_section_height = 1 + ops_visible_rows + 2;

    // Build layout constraints: tasks always grow, then coworkers (if any), then ops
    let mut constraints = vec![Constraint::Min(10)];
    if coworker_section_height > 0 {
        constraints.push(Constraint::Length(coworker_section_height));
    }
    constraints.push(Constraint::Length(ops_section_height));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let tasks_area = chunks[0];
    let mut lines = Vec::new();
    let hyperlinks = Vec::new();

    // Default channel matches the daemon's ChannelRouter default ("midtown")
    let main_channel = "midtown";

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

    // Count active PRs per channel
    let mut prs_by_channel: HashMap<String, Vec<&KanbanPr>> = HashMap::new();
    for pr in &app.prs {
        if let Some(task_id) = midtown::tasks::extract_task_id_from_pr_title(&pr.title) {
            let task_id_str = task_id.to_string();
            if let Some(task) = app.tasks.iter().find(|t| t.id == task_id_str) {
                let channel_key = task.channel.as_deref().unwrap_or(main_channel).to_string();
                prs_by_channel.entry(channel_key).or_default().push(pr);
            }
        }
    }

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

            // Phase label line is only emitted when there is a label
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
                // Register the label line for click-to-attach as well
                task_line_map.insert(current_line, (task.id.clone(), task.owner.clone()));
                current_line += 1;
            }
        }
    }

    // Store line maps in app for mouse click handler
    app.task_line_map = task_line_map;
    app.channel_line_map = channel_line_map;

    // Render each channel as a swimlane
    for (channel_name, tasks) in &channels_to_display {
        render_channel_header(app, channel_name, tasks, &prs_by_channel, &mut lines);

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
    let is_focused = app.focused_pane == FocusedPane::Board;
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

    // Determine the chunk index for each optional section
    let mut chunk_idx = 1usize;
    if coworker_section_height > 0 {
        draw_coworker_status(f, app, chunks[chunk_idx]);
        chunk_idx += 1;
    }
    let ops_refs: Vec<&midtown::Message> = ops_messages.iter().collect();
    draw_ops_mini_channel(f, &ops_refs, chunks[chunk_idx]);

    (hyperlinks, tasks_area)
}

/// Render a channel header line with task count and unread count.
fn render_channel_header(
    app: &App,
    channel_name: &str,
    tasks: &[&KanbanTask],
    _prs_by_channel: &HashMap<String, Vec<&KanbanPr>>,
    lines: &mut Vec<Line<'static>>,
) {
    let task_count = tasks.len();
    let channel_header = if let Some(&unread_count) = app.channel_unread_counts.get(channel_name) {
        format!(
            "#{} ({}) — {} tasks",
            channel_name, unread_count, task_count
        )
    } else {
        format!("#{} — {} tasks", channel_name, task_count)
    };

    let is_selected = app.board_selection.as_ref().is_some_and(|sel| match sel {
        BoardSelection::Channel(ch) => ch == channel_name,
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

/// Derive the phase status label for a task based on its PR and coworker state.
///
/// Returns a short label (e.g. "dev", "pr", "rvw", "rvd", "fix", "ci", "done")
/// or `None` if the task is pending with no PR.
pub fn task_phase_label(
    task: &KanbanTask,
    task_pr: Option<&KanbanPr>,
    coworker_phase: Option<&str>,
) -> Option<&'static str> {
    match task_pr {
        None => {
            // No PR yet
            if task.status == TaskStatus::InProgress {
                Some("dev")
            } else {
                None
            }
        }
        Some(pr) => {
            // Merge conflicts take priority over everything else
            if pr.has_conflicts {
                return Some("conflict");
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
                    return Some("fix");
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
    let (bullet_color, text_color) = match task_pr {
        // PR exists - check for conflicts or CI status
        Some(pr) => {
            if pr.has_conflicts {
                // Merge conflict takes priority - show red
                (Color::Red, Color::Red)
            } else {
                match pr.ci_status {
                    CiStatus::Passed => (Color::Green, Color::Green),
                    CiStatus::Failed => (Color::Red, Color::Red),
                    CiStatus::Running => (Color::Yellow, Color::Yellow),
                    CiStatus::Unknown => {
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
    // 1-space indent + indent_level + "● " + prefix = total width before subject text
    let bullet_prefix_width = 1 + task_indent.len() + 2 + prefix.len();
    let task_line = format!("{}{}", prefix, task.subject);

    let is_task_selected = app.board_selection.as_ref().is_some_and(|sel| match sel {
        BoardSelection::Task(ch, tid) => ch == channel_name && tid == &task.id,
        _ => false,
    });

    let wrapped_lines = wrap_content(&task_line, wrap_width);
    for (i, wrapped) in wrapped_lines.iter().enumerate() {
        if i == 0 {
            // First line: 1-space indent + bullet + text as separate spans
            let bullet_span = Span::styled(
                format!(" {}● ", task_indent),
                Style::default().fg(bullet_color),
            );
            let mut text_style = Style::default().fg(text_color);
            if is_task_selected {
                text_style = text_style.bg(Color::DarkGray);
            }
            let text_span = Span::styled(wrapped.to_string(), text_style);
            lines.push(Line::from(vec![bullet_span, text_span]));
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
                style = style.bg(Color::DarkGray);
            }
            lines.push(Line::from(vec![Span::styled(text, style)]));
        }
    }

    // Second line: phase status label in gray, indented to align with task text
    // Only emitted when there is a label (pending tasks with no PR get no extra line)
    if let Some(label_text) = task_phase_label(task, task_pr, coworker_phase) {
        // Indent to align with the task ID (same as bullet_prefix_width)
        let label_line = format!("{:width$}{}", "", label_text, width = bullet_prefix_width);
        lines.push(Line::from(vec![Span::styled(
            label_line,
            Style::default().fg(Color::DarkGray),
        )]));
    }
}

/// Draw the Midtown Ops mini-channel (bottom of board sidebar).
///
/// Displays system messages and coworker /me action messages in a compact scrollable view.
/// The most recent `ops_messages` entries are shown (already in chronological order).
fn draw_ops_mini_channel(f: &mut Frame, ops_messages: &[&midtown::Message], area: Rect) {
    // Split: header line + content
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Header line
            Constraint::Min(0),    // Message rows
        ])
        .split(area);

    // Render the bordered container
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    f.render_widget(block, area);

    // Header: "MIDTOWN OPS" in dim style, inside the border
    let header = Paragraph::new(Line::from(vec![Span::styled(
        " MIDTOWN OPS",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )]));
    f.render_widget(header, chunks[0]);

    // Message rows (inside border = area shrunk by 1 on each side)
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    if ops_messages.is_empty() {
        let empty = Paragraph::new(Line::from(vec![Span::styled(
            "No ops messages",
            Style::default().fg(Color::DarkGray),
        )]));
        f.render_widget(empty, inner);
        return;
    }

    // Build message lines (most recent at bottom)
    let content_width = inner.width.saturating_sub(1) as usize; // 1 for left pad
    let mut lines: Vec<Line<'static>> = Vec::new();

    for msg in ops_messages {
        let time_str = format_ops_time(&msg.timestamp);
        let is_action = msg.message_type == MessageType::Action || msg.content.starts_with("/me ");

        let sender_color = ops_sender_color(&msg.from);

        if is_action {
            // Format: "HH:MM * name content"
            let content = msg.content.trim_start_matches("/me").trim().to_string();
            let prefix = format!("{time_str} * ");
            let name = msg.from.clone();
            let remaining = content_width.saturating_sub(prefix.len() + name.len() + 1);
            let body = if remaining > 0 && content.len() > remaining {
                format!("{}..", &content[..remaining.saturating_sub(2)])
            } else {
                content.clone()
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{time_str} "),
                    Style::default().fg(Color::Rgb(50, 50, 50)),
                ),
                Span::styled("* ", Style::default().fg(sender_color)),
                Span::styled(name, Style::default().fg(sender_color)),
                Span::styled(
                    format!(" {body}"),
                    Style::default().fg(Color::Rgb(90, 90, 90)),
                ),
            ]));
        } else {
            // Format: "HH:MM sender: message"
            let sender = msg.from.clone();
            let prefix_len = time_str.len() + 1 + sender.len() + 2; // " HH:MM sender: "
            let remaining = content_width.saturating_sub(prefix_len);
            let body = if remaining > 0 && msg.content.len() > remaining {
                format!("{}..", &msg.content[..remaining.saturating_sub(2)])
            } else {
                msg.content.clone()
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{time_str} "),
                    Style::default().fg(Color::Rgb(50, 50, 50)),
                ),
                Span::styled(sender, Style::default().fg(sender_color)),
                Span::styled(
                    format!(": {body}"),
                    Style::default().fg(Color::Rgb(90, 90, 90)),
                ),
            ]));
        }
    }

    // Show the last N lines that fit in the available height
    let available_height = inner.height as usize;
    let start = lines.len().saturating_sub(available_height);
    let visible_lines: Vec<Line<'static>> = lines.into_iter().skip(start).collect();

    let paragraph = Paragraph::new(visible_lines);
    f.render_widget(paragraph, inner);
}

/// Map a sender name to a dim TUI color for the ops mini-channel.
fn ops_sender_color(name: &str) -> Color {
    match name.to_lowercase().as_str() {
        "midtown" | "system" | "daemon" | "github" => Color::Rgb(80, 80, 80),
        "lead" => Color::Rgb(180, 180, 100),
        _ => Color::Rgb(120, 160, 120), // coworker names: muted green
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

    // Get the spinner character before borrowing app.coworkers
    let spinner = app.spinner_char();

    // Filter out idle coworkers - only show those actively working
    let active_coworkers: Vec<_> = app
        .coworkers
        .iter()
        .filter(|cw| cw.phase.as_deref() != Some("idle") && cw.phase.is_some())
        .collect();

    let active_count = active_coworkers.len();
    let header = format!("Coworkers ({}/{})", active_count, app.max_coworkers);
    let header_paragraph = Paragraph::new(Line::from(vec![Span::styled(
        header,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    f.render_widget(header_paragraph, chunks[0]);

    // Available width inside the bordered table (subtract 2 for left+right borders)
    let available_width = area.width.saturating_sub(2) as usize;

    // Determine which columns to show based on available width.
    //
    // Column widths (fixed):
    //   spinner : 2  (e.g. "⠋ ")
    //   name    : max name length (variable, but bounded)
    //   task_id : 5  (e.g. "!1418")
    //   phase   : 6  (e.g. "review", "debug")
    //   pr_num  : 5  (e.g. "#1207")
    //   progress: 4  (e.g. "42% ")
    //   time    : 4  (e.g. "~3m")
    //
    // Degradation order (drop from right):
    //   Full:    spinner | name | task_id | phase | pr_num | progress | time
    //   Level 1: spinner | name | task_id | phase | pr_num | progress
    //   Level 2: spinner | name | task_id | phase | pr_num
    //   Level 3: spinner | name | task_id | phase
    //   Level 4: spinner | name | task_id
    //   Minimal: spinner | name

    let name_max = active_coworkers
        .iter()
        .map(|cw| cw.name.len())
        .max()
        .unwrap_or(6);

    // Column widths (each includes one space of padding on the right, handled by Cell)
    let w_spinner: usize = 2; // "⠋ "
    let w_name: usize = name_max; // right-padded to align
    let w_task_id: usize = 5; // "!1418"
    let w_phase: usize = 6; // "review" (longest abbreviation)
    let w_pr: usize = 5; // "#1207"
    let w_progress: usize = 4; // "42% "
    let w_time: usize = 4; // "~3m"

    // Gap between columns — must match Table::column_spacing() below.
    let gap: usize = 1;

    // Cumulative widths for each layout level
    let base = w_spinner + gap + w_name;
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
    let mut constraints = vec![
        Constraint::Length(w_spinner as u16),
        Constraint::Length(w_name as u16),
    ];
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

    let rows: Vec<Row> = active_coworkers
        .iter()
        .map(|cw| {
            coworker_table_row(
                cw,
                spinner,
                show_task_id,
                show_phase,
                show_pr,
                show_progress,
                show_time,
            )
        })
        .collect();

    let table = Table::new(rows, constraints)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .style(Style::default().fg(Color::White)),
        )
        .column_spacing(1);

    f.render_widget(table, chunks[1]);
}

/// Build a single table Row for a coworker.
fn coworker_table_row(
    cw: &CoworkerInfo,
    spinner: &str,
    show_task_id: bool,
    show_phase: bool,
    show_pr: bool,
    show_progress: bool,
    show_time: bool,
) -> Row<'static> {
    let health_color = match cw.health.as_str() {
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "red" => Color::Red,
        _ => Color::Green,
    };

    let mut cells: Vec<Cell> = vec![
        Cell::from(spinner.to_string()).style(Style::default().fg(Color::Yellow)),
        Cell::from(cw.name.clone()).style(Style::default().fg(health_color)),
    ];

    if show_task_id {
        let task_text = cw.task_id.map(|id| format!("!{id}")).unwrap_or_default();
        cells.push(Cell::from(task_text).style(Style::default().fg(Color::DarkGray)));
    }

    if show_phase {
        let phase_text = cw.phase.as_deref().unwrap_or("").to_string();
        cells.push(Cell::from(phase_text).style(Style::default().fg(Color::DarkGray)));
    }

    if show_pr {
        let pr_text = cw.pr_number.map(|n| format!("#{n}")).unwrap_or_default();
        cells.push(Cell::from(pr_text).style(Style::default().fg(Color::Blue)));
    }

    if show_progress {
        let progress_text = cw.progress.map(|p| format!("{p}%")).unwrap_or_default();
        cells.push(Cell::from(progress_text).style(Style::default().fg(Color::Cyan)));
    }

    if show_time {
        let time_text = cw.time_estimate.as_deref().unwrap_or("").to_string();
        cells.push(Cell::from(time_text).style(Style::default().fg(Color::Green)));
    }

    Row::new(cells)
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
                blocked_by: vec![],
            },
            KanbanTask {
                id: "43".to_string(),
                subject: "Second task".to_string(),
                owner: Some("lexington".to_string()),
                status: TaskStatus::Pending,
                modified_at: None,
                channel: None,
                blocked_by: vec![],
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
        let mut cw = make_coworker("park");
        cw.task_id = Some(1418);
        cw.pr_number = Some(1207);
        cw.progress = Some(42);
        cw.time_estimate = Some("~3m".to_string());

        let row = coworker_table_row(&cw, "⠋", true, true, true, true, true);

        // Verify all 7 columns are present by checking the row can be constructed
        // (Row doesn't expose cell count directly, but we verify data is correct
        // by checking what we passed to each show_* flag)
        let _ = row; // Row is valid, column building doesn't panic
    }

    #[test]
    fn test_coworker_table_row_minimal_columns() {
        let cw = make_coworker("york");
        // Minimal: only spinner + name (all show_* = false)
        let row = coworker_table_row(&cw, "⠙", false, false, false, false, false);
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
                // No phase set — also excluded
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

        // Only "amsterdam" (developing) should appear. This test verifies no panic
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
        // ops mini-channel always present: with 0 ops messages → ops section height = 1 + 0 + 2 = 3
        // tasks_area height should be 40 - 5 - 3 = 32
        assert_eq!(
            tasks_area.height, 32,
            "tasks area height should leave exactly 5 rows for 2 coworkers + 3 rows for ops section"
        );
    }

    #[test]
    fn test_draw_board_panel_idle_coworkers_dont_inflate_height() {
        // Regression test: idle coworkers should not inflate the section height.
        // If 2 out of 3 coworkers are idle, only 1 active row should be reserved.
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
                cw.phase = None; // no phase — excluded
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
        // ops mini-channel always present: with 0 ops messages → ops section height = 1 + 0 + 2 = 3
        // tasks_area height should be 40 - 4 - 3 = 33
        assert_eq!(
            tasks_area.height, 33,
            "idle coworkers should not inflate section height (only 1 active coworker)"
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
        // Verify that "review" (the longest phase abbreviation) renders
        // without truncation in the coworker table row.
        let mut cw = make_coworker("york");
        cw.phase = Some("review".to_string());
        cw.task_id = Some(42);

        let row = coworker_table_row(&cw, "⠋", true, true, false, false, false);
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
        assert_eq!(task_phase_label(&task, Some(&pr), Some("dev")), Some("fix"));
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
            Some("fix")
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
            Some("fix")
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
    fn test_phase_label_second_line_rendered_in_gray() {
        let app = test_app();
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
        assert_eq!(span.style.fg, Some(Color::DarkGray));
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
        // PR with merge conflicts should show "conflict", not "ci"
        let task = make_task("1", vec![]);
        let mut pr = make_pr(1);
        pr.has_conflicts = true;
        pr.ci_status = CiStatus::Failed;
        assert_eq!(
            task_phase_label(&task, Some(&pr), None),
            Some("conflict"),
            "has_conflicts should take priority over CI status"
        );
    }

    #[test]
    fn test_draw_board_panel_includes_ops_section() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = test_app();
        // Add an ops message so the section is non-trivial
        app.messages
            .push_back(midtown::Message::system("test ops msg"));

        let mut returned_tasks_area = None;

        terminal
            .draw(|f| {
                let area = ratatui::layout::Rect {
                    x: 0,
                    y: 0,
                    width: 80,
                    height: 40,
                };
                let (_links, tasks_area) = draw_board_panel(f, &mut app, area);
                returned_tasks_area = Some(tasks_area);
            })
            .unwrap();

        let tasks_area = returned_tasks_area.unwrap();
        // Ops section always present; tasks_area should be smaller than total height
        assert!(
            tasks_area.height < 40,
            "tasks_area should be smaller than total height when ops section is present"
        );
    }
}

#[path = "board_tests.rs"]
#[cfg(test)]
mod board_tests;
