//! UI rendering for the chat TUI

use std::collections::HashMap;

use chrono::{DateTime, Local, Utc};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap},
};

use midtown::{Message, MessageType};

use super::app::{App, CiStatus, MessageRenderCache, RepoStatus};
use super::mermaid::{self, ContentSegment, MermaidCache};

/// A hyperlink to be rendered after ratatui draws (using OSC 8 sequences)
#[derive(Debug, Clone, PartialEq)]
pub struct Hyperlink {
    /// Screen x coordinate
    pub x: u16,
    /// Screen y coordinate
    pub y: u16,
    /// Text to display (will be rewritten with OSC 8 wrapping)
    pub text: String,
    /// URL to link to
    pub url: String,
    /// Optional color for the first character (CI status dot)
    pub first_char_color: Option<Color>,
}

/// Gutter width for timestamp: " HH:MM " = 7 chars
const TIMESTAMP_GUTTER_WIDTH: usize = 7;

/// Avenue names mapped to colors (position-based assignment)
const AVENUE_COLORS: &[(&str, Color)] = &[
    ("lexington", Color::Cyan),
    ("park", Color::Green),
    ("madison", Color::LightRed),
    ("broadway", Color::Magenta),
    ("amsterdam", Color::Blue),
    ("columbus", Color::Red),
    ("riverside", Color::LightCyan),
    ("york", Color::LightGreen),
    ("pleasant", Color::LightMagenta),
    ("vernon", Color::LightBlue),
    // Overflow names
    ("bleecker", Color::Indexed(208)), // orange
    ("houston", Color::Indexed(213)),  // pink
    ("canal", Color::Indexed(117)),    // light blue
    ("spring", Color::Indexed(156)),   // light green
    ("prince", Color::Indexed(183)),   // lavender
    ("mercer", Color::Indexed(216)),   // salmon
];

/// Check if a sender is a "system-like" sender that should be grouped together
/// (daemon, system) without blank lines between consecutive messages.
///
/// Note: "github" was previously included here but is now treated like a regular
/// sender for spacing purposes, so github messages get blank line separation
/// matching coworker messages. GitHub content is still styled DarkGray via
/// `is_dim_sender`.
fn is_system_like_sender(sender: &str) -> bool {
    matches!(sender.to_lowercase().as_str(), "daemon" | "system")
}

/// Check if a sender's message content should be rendered in DarkGray.
/// This includes system-like senders and github.
fn is_dim_sender(sender: &str) -> bool {
    matches!(
        sender.to_lowercase().as_str(),
        "daemon" | "github" | "system"
    )
}

/// Get color for a sender name
fn get_sender_color(name: &str) -> Color {
    match name.to_lowercase().as_str() {
        "lead" | "user" => Color::LightYellow,
        "daemon" => Color::DarkGray,
        "github" => Color::DarkGray,
        "system" => Color::DarkGray,
        _ => {
            // Check avenue colors
            for (avenue, color) in AVENUE_COLORS {
                if name.to_lowercase() == *avenue {
                    return *color;
                }
            }
            // Custom user display names get the same color as lead/user
            if midtown::config::get_user_display_name()
                .is_some_and(|dn| dn.eq_ignore_ascii_case(name))
            {
                return Color::LightYellow;
            }
            // Default for unknown names
            Color::White
        }
    }
}

/// Calculate height for repo status lines (1 per repo, minimum 1)
fn repo_status_height(app: &App) -> u16 {
    let count = app.repo_statuses.len();
    if count > 1 { count as u16 } else { 1 }
}

/// Draw the main UI
///
/// Returns hyperlinks that should be rendered after ratatui draws.
/// These need to be written directly to the terminal using OSC 8 escape sequences,
/// bypassing ratatui's buffer system (which doesn't support hyperlinks).
pub fn draw(f: &mut Frame, app: &mut App) -> Vec<Hyperlink> {
    // Split-panel layout: repo status (top), then horizontal split (board left | chat right), usage (bottom)
    let status_height = repo_status_height(app);
    let usage_height = if app.usage_data.is_some() { 4 } else { 0 };

    let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(status_height),
            Constraint::Min(10),
            Constraint::Length(usage_height),
        ])
        .split(f.area());

    // Draw repo status at top
    draw_repo_status_lines(f, app, vertical_chunks[0]);

    // Split the main area horizontally: board (left 40%) | chat (right 60%)
    let horizontal_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(40), // Board panel
            Constraint::Percentage(60), // Chat panel
        ])
        .split(vertical_chunks[1]);

    let hyperlinks = draw_board_panel(f, app, horizontal_chunks[0]);
    draw_chat_panel(f, app, horizontal_chunks[1]);

    if app.usage_data.is_some() {
        draw_usage_bars(f, app, vertical_chunks[2]);
    }

    hyperlinks
}

/// Format relative time (e.g., "3 minutes ago", "2 hours ago", "1 day ago")
fn format_relative_time(time: DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(time);
    let minutes = duration.num_minutes();
    let hours = duration.num_hours();
    let days = duration.num_days();

    if days > 0 {
        if days == 1 {
            "1 day ago".to_string()
        } else {
            format!("{} days ago", days)
        }
    } else if hours > 0 {
        if hours == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{} hours ago", hours)
        }
    } else if minutes > 0 {
        if minutes == 1 {
            "1 minute ago".to_string()
        } else {
            format!("{} minutes ago", minutes)
        }
    } else {
        "just now".to_string()
    }
}

/// Draw the board panel (left side) with channel swimlanes
/// Compute indentation level for each task based on dependency structure.
/// Returns a HashMap mapping task ID to indentation level (0 = no indent, 1 = indent one level, etc.)
fn compute_task_indentation(tasks: &[&super::app::KanbanTask]) -> HashMap<String, usize> {
    use std::collections::{HashMap, HashSet};

    let mut indentation: HashMap<String, usize> = HashMap::new();
    let mut processed: HashSet<String> = HashSet::new();

    // Build a map of task IDs for quick lookup
    let task_map: HashMap<String, &super::app::KanbanTask> =
        tasks.iter().map(|t| (t.id.clone(), *t)).collect();

    // Process tasks in order (sorted by ID), computing indentation recursively
    for task in tasks {
        compute_indentation_recursive(&task.id, &task_map, &mut indentation, &mut processed);
    }

    indentation
}

/// Recursive helper to compute indentation level for a task
fn compute_indentation_recursive(
    task_id: &str,
    task_map: &HashMap<String, &super::app::KanbanTask>,
    indentation: &mut HashMap<String, usize>,
    processed: &mut std::collections::HashSet<String>,
) -> usize {
    // If already computed, return cached value
    if let Some(&level) = indentation.get(task_id) {
        return level;
    }

    // Guard against cycles
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

    // If no dependencies, no indentation
    if task.blocked_by.is_empty() {
        indentation.insert(task_id.to_string(), 0);
        return 0;
    }

    // Find the first unresolved dependency in the current task list
    // (Dependencies are "unresolved" if they exist in this task list)
    let first_blocker = task
        .blocked_by
        .iter()
        .find(|blocker_id| task_map.contains_key(blocker_id.as_str()));

    let level = if let Some(blocker_id) = first_blocker {
        // Indent one level more than the blocker
        let blocker_level =
            compute_indentation_recursive(blocker_id, task_map, indentation, processed);
        blocker_level + 1
    } else {
        // All blockers are resolved or not in this list - no indentation
        0
    };

    indentation.insert(task_id.to_string(), level);
    level
}

fn draw_board_panel(f: &mut Frame, app: &mut App, area: Rect) -> Vec<Hyperlink> {
    use ratatui::layout::{Constraint, Direction, Layout};
    use std::collections::{BTreeMap, HashMap};

    // Split board area vertically: tasks at top, coworkers at bottom
    let active_coworker_count = app.coworkers.len();
    let coworker_section_height = if active_coworker_count > 0 {
        // 1 header line + N coworker rows + 2 for table borders
        active_coworker_count as u16 + 3
    } else {
        0 // No coworkers, don't show the section
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if coworker_section_height > 0 {
            vec![
                Constraint::Min(10),                         // Task swimlanes
                Constraint::Length(coworker_section_height), // Coworker status
            ]
        } else {
            vec![Constraint::Min(10)] // No coworkers, use full area for tasks
        })
        .split(area);

    let tasks_area = chunks[0];

    let mut lines = Vec::new();
    let hyperlinks = Vec::new();

    // Default channel matches the daemon's ChannelRouter default ("midtown")
    let main_channel = "midtown";

    let mut tasks_by_channel: BTreeMap<String, Vec<&super::app::KanbanTask>> = BTreeMap::new();
    let (pending, in_progress, _completed) = app.tasks_by_status();

    // Combine all active tasks and group by channel
    for task in in_progress.iter().chain(pending.iter()) {
        let channel_key = task.channel.as_deref().unwrap_or(main_channel).to_string();
        tasks_by_channel.entry(channel_key).or_default().push(task);
    }

    // Calculate available width for wrapping (subtract 2 for borders)
    let wrap_width = area.width.saturating_sub(2).max(20) as usize;

    // Count active PRs per channel (for CI status indicators)
    let mut prs_by_channel: HashMap<String, Vec<&super::app::KanbanPr>> = HashMap::new();
    for pr in &app.prs {
        if let Some(task_id) = midtown::tasks::extract_task_id_from_pr_title(&pr.title) {
            // Find which channel this task belongs to
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
            lines.push(Line::from("")); // Blank line between channels
        }
        first_channel = false;

        // Channel header with task count, unread count, and CI status
        let task_count = tasks.len();
        let mut header_parts =
            if let Some(&unread_count) = app.channel_unread_counts.get(channel_name.as_str()) {
                // Show unread count if there are unread messages
                vec![format!(
                    "  #{} ({}) — {} tasks",
                    channel_name, unread_count, task_count
                )]
            } else {
                // No unread messages, just show task count
                vec![format!("  #{} — {} tasks", channel_name, task_count)]
            };

        // Add CI status indicator if there are active PRs for this channel
        if let Some(channel_prs) = prs_by_channel.get(channel_name)
            && !channel_prs.is_empty()
        {
            // Determine overall CI status: red if any failed, yellow if any running,
            // green only if all passed, no indicator if all unknown
            let has_failed = channel_prs
                .iter()
                .any(|pr| pr.ci_status == super::app::CiStatus::Failed);
            let has_running = channel_prs
                .iter()
                .any(|pr| pr.ci_status == super::app::CiStatus::Running);
            let has_passed = channel_prs
                .iter()
                .any(|pr| pr.ci_status == super::app::CiStatus::Passed);

            let ci_indicator = if has_failed {
                Some(" 🔴")
            } else if has_running {
                Some(" 🟡")
            } else if has_passed {
                Some(" 🟢")
            } else {
                None // All Unknown — no indicator
            };
            if let Some(indicator) = ci_indicator {
                header_parts.push(indicator.to_string());
            }
        }

        let channel_header = header_parts.join("");

        // Check if this channel is selected
        let is_selected = app.board_selection.as_ref().is_some_and(|sel| match sel {
            super::app::BoardSelection::Channel(ch) => ch == channel_name,
            _ => false,
        });

        let mut style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        if is_selected {
            style = style.bg(Color::DarkGray);
        }

        lines.push(Line::from(vec![Span::styled(channel_header, style)]));
        lines.push(Line::from("")); // Blank line after header

        // Compute indentation levels for tasks based on dependencies
        let task_indentation = compute_task_indentation(tasks);

        // Render tasks for this channel
        for task in tasks {
            // Get indentation level for this task (each level = 2 spaces)
            let indent_level = task_indentation.get(&task.id).copied().unwrap_or(0);
            let task_indent = "  ".repeat(indent_level);
            let status_marker = if task.status == super::app::TaskStatus::InProgress {
                "● "
            } else {
                "○ "
            };

            // Build prefix so continuation lines can align with subject text
            let prefix = format!("{}{} !{} ", task_indent, status_marker, task.id);
            let prefix_width = prefix.len();

            let task_line = format!("{}{}", prefix, task.subject);

            // Check if this task is selected
            let is_task_selected = app.board_selection.as_ref().is_some_and(|sel| match sel {
                super::app::BoardSelection::Task(ch, tid) => ch == channel_name && tid == &task.id,
                _ => false,
            });

            // Pre-wrap the task line if it exceeds available width
            let wrapped_lines = wrap_content(&task_line, wrap_width);
            for (i, wrapped) in wrapped_lines.iter().enumerate() {
                let text = if i == 0 {
                    wrapped.to_string()
                } else {
                    // Continuation lines: indent to align with subject text (reduced by 2 spaces)
                    let indent_width = prefix_width.saturating_sub(2);
                    format!(
                        "{:width$}{}",
                        "",
                        wrapped.trim_start(),
                        width = indent_width
                    )
                };

                let color = if task.status == super::app::TaskStatus::InProgress {
                    Color::Green
                } else {
                    Color::DarkGray
                };

                let mut style = Style::default().fg(color);
                if is_task_selected {
                    style = style.bg(Color::DarkGray);
                }

                lines.push(Line::from(vec![Span::styled(text, style)]));
            }
        }
    }

    // Render the tasks panel
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Board")
        .style(Style::default().fg(Color::White));

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, tasks_area);

    // Render the coworker status section if there are active coworkers
    if coworker_section_height > 0 {
        draw_coworker_status(f, app, chunks[1]);
    }

    hyperlinks
}

/// Draw the coworker status section (bottom of board sidebar)
fn draw_coworker_status(f: &mut Frame, app: &App, area: Rect) {
    // Split area: header at top, table below
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Header line
            Constraint::Min(0),    // Table rows
        ])
        .split(area);

    // Header: "Coworkers (N/10)" in cyan, bold
    let active_count = app.coworkers.len();
    let max_coworkers = 10; // Hardcoded constant matching daemon's default
    let header = format!("  Coworkers ({}/{})", active_count, max_coworkers);
    let header_paragraph = Paragraph::new(Line::from(vec![Span::styled(
        header,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    f.render_widget(header_paragraph, chunks[0]);

    // Build table rows for coworkers
    let rows: Vec<Row> = app
        .coworkers
        .iter()
        .map(|cw| {
            // Health dot
            let health_dot = "●"; // U+25CF BLACK CIRCLE
            let health_color = match cw.health.as_str() {
                "green" => Color::Green,
                "yellow" => Color::Yellow,
                "red" => Color::Red,
                _ => Color::Green,
            };

            // Build cells: [health_dot, name, task_id, phase, pr_number]
            let mut cells = vec![
                Cell::from(health_dot).style(Style::default().fg(health_color)),
                Cell::from(cw.name.clone()),
            ];

            // Task ID (!1108)
            cells.push(Cell::from(
                cw.task_id.map(|id| format!("!{}", id)).unwrap_or_default(),
            ));

            // Phase abbreviation (dev/test/PR/etc)
            cells.push(Cell::from(cw.phase.clone().unwrap_or_default()));

            // PR number (#123)
            cells.push(Cell::from(
                cw.pr_number
                    .map(|pr| format!("#{}", pr))
                    .unwrap_or_default(),
            ));

            Row::new(cells)
        })
        .collect();

    // Define column widths
    // [dot, name, task, phase, PR]
    let widths = [
        Constraint::Length(2), // Health dot + space
        Constraint::Min(10),   // Name (flexible)
        Constraint::Length(6), // Task ID (!1234)
        Constraint::Length(6), // Phase (dev/test/PR/etc)
        Constraint::Length(5), // PR number (#123)
    ];

    // Create table (no borders, no header)
    let table = Table::new(rows, widths)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .style(Style::default().fg(Color::White)),
        )
        .column_spacing(1); // Space between columns

    f.render_widget(table, chunks[1]);
}

/// Draw stacked repo status lines (one per repo, or single line for single-repo)
fn draw_repo_status_lines(f: &mut Frame, app: &App, area: Rect) {
    if app.repo_statuses.len() > 1 {
        // Multi-repo: render one line per repo
        let lines: Vec<Line> = app
            .repo_statuses
            .iter()
            .map(|(info, status)| build_repo_status_line(&info.label, status, area.width))
            .collect();
        let paragraph = Paragraph::new(lines);
        f.render_widget(paragraph, area);
    } else {
        // Single-repo: use the primary status and repo_name
        let line = build_repo_status_line(&app.repo_name, &app.repo_status, area.width);
        let paragraph = Paragraph::new(line);
        f.render_widget(paragraph, area);
    }
}

/// Build a single repo status line with commit, CI, and release info
fn build_repo_status_line(repo_label: &str, status: &RepoStatus, width: u16) -> Line<'static> {
    // Background color matching tmux status bar (colour236 = dark gray)
    let bg = Color::Indexed(236);

    let mut spans = Vec::new();

    // Repo name (dim)
    spans.push(Span::styled(
        format!(" {}  ", repo_label),
        Style::default().fg(Color::DarkGray).bg(bg),
    ));

    // Commit hash and time
    if !status.commit_hash.is_empty() {
        spans.push(Span::styled(
            status.commit_hash.clone(),
            Style::default().fg(Color::Yellow).bg(bg),
        ));
        if let Some(commit_time) = status.commit_time {
            spans.push(Span::styled(
                format!("  {}  ", format_relative_time(commit_time)),
                Style::default().fg(Color::DarkGray).bg(bg),
            ));
        } else {
            spans.push(Span::styled("  ", Style::default().bg(bg)));
        }
    }

    // CI status dot
    let (ci_char, ci_color) = match status.ci_status {
        CiStatus::Passed => ("●", Color::Rgb(0, 208, 80)),
        CiStatus::Failed => ("●", Color::Red),
        CiStatus::Running => ("●", Color::Yellow),
        CiStatus::Unknown => ("○", Color::DarkGray),
    };
    spans.push(Span::styled(
        ci_char.to_string(),
        Style::default().fg(ci_color).bg(bg),
    ));
    spans.push(Span::styled("  ", Style::default().bg(bg)));

    // Release info
    if let Some(tag) = &status.release_tag {
        spans.push(Span::styled(
            "Releases: ".to_string(),
            Style::default().fg(Color::DarkGray).bg(bg),
        ));
        spans.push(Span::styled(
            tag.to_string(),
            Style::default().fg(Color::Cyan).bg(bg),
        ));
        if let Some(release_time) = status.release_time {
            spans.push(Span::styled(
                format!("  {}", format_relative_time(release_time)),
                Style::default().fg(Color::DarkGray).bg(bg),
            ));
        }
    }

    // Fill rest of line with background
    let content_len: usize = spans.iter().map(|s| s.content.len()).sum();
    if content_len < width as usize {
        spans.push(Span::styled(
            " ".repeat(width as usize - content_len),
            Style::default().bg(bg),
        ));
    }

    Line::from(spans)
}

/// Draw the chat panel showing messages
fn draw_chat_panel(f: &mut Frame, app: &mut App, area: Rect) {
    // Calculate dynamic input bar height based on text wrapping
    let input_bar_height = calculate_input_bar_height(&app.input_text, area.width);

    // Split chat panel vertically: messages (top) + input bar (bottom)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),                   // Messages area
            Constraint::Length(input_bar_height), // Input bar (dynamic height)
        ])
        .split(area);

    draw_chat_messages(f, app, chunks[0]);
    draw_input_bar(f, app, chunks[1]);

    // Draw autocomplete dropdown overlay if showing
    if app.autocomplete.show {
        draw_autocomplete_dropdown(f, app, chunks[1]);
    }
}

/// Draw the chat messages area (top of chat panel)
fn draw_chat_messages(f: &mut Frame, app: &mut App, area: Rect) {
    let title = if app.selection_mode {
        format!(" #{} [SELECT] ", app.selected_channel)
    } else {
        format!(" #{} ", app.selected_channel)
    };
    let border_color = if app.selection_mode {
        Color::Yellow
    } else {
        Color::DarkGray
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);

    // Update visible height for scroll calculations
    app.visible_height = inner.height as usize;

    // Clamp scroll_offset to prevent unexpected jumps when visible_height changes.
    // This fixes a bug where kanban board resizing could cause the chat to
    // unexpectedly scroll to the beginning of history.
    app.clamp_scroll_offset();

    // Check if we can reuse cached rendered lines (avoids expensive mermaid/markdown
    // parsing when only the input bar changed between frames).
    let cache_key = app.message_cache_key(inner.width);
    if let Some(ref cache) = app.message_render_cache
        && cache.cache_key == cache_key
    {
        let paragraph = Paragraph::new(cache.lines.clone());
        f.render_widget(block, area);
        f.render_widget(paragraph, inner);
        app.diagram_sources.clone_from(&cache.diagram_sources);
        return;
    }

    // Cache miss — full render

    // Get cached current_tasks lookup first, then visible messages.
    // We clone current_tasks and visible messages to avoid holding a mutable
    // borrow across the loop (needed because we also read mermaid_cache).
    let current_tasks = app.current_tasks().clone();
    let user_display_name = app.user_display_name.clone();
    let visible: Vec<Message> = app.visible_messages().to_vec();

    // Build lines for messages, tracking previous sender for grouping.
    let mut lines: Vec<Line> = Vec::new();
    let prev_sender: Option<&str> = None;

    // Collect mermaid sources that need rendering (to avoid borrow conflicts)
    let mut mermaid_to_render: Vec<String> = Vec::new();

    // Reset diagram index for this render pass
    app.diagram_sources.clear();

    // Track previous sender by index for lifetime management
    for (idx, msg) in visible.iter().enumerate() {
        let segments = mermaid::parse_content_segments(&msg.content);
        let has_mermaid = segments
            .iter()
            .any(|s| matches!(s, ContentSegment::Mermaid(_)));
        let prev = if idx > 0 {
            Some(visible[idx - 1].from.as_str())
        } else {
            prev_sender
        };

        if !has_mermaid {
            // Fast path: no mermaid content, use existing render pipeline
            let msg_lines = render_message(
                msg,
                inner.width as usize,
                prev,
                &current_tasks,
                user_display_name.as_deref(),
            );
            lines.extend(msg_lines);
        } else {
            // Message contains mermaid: render segments individually
            render_message_with_mermaid(
                msg,
                &segments,
                inner.width as usize,
                prev,
                &current_tasks,
                user_display_name.as_deref(),
                &app.mermaid_cache,
                &mut lines,
                &mut app.diagram_sources,
                &mut mermaid_to_render,
            );
        }
    }

    // Queue any un-cached mermaid diagrams for background rendering
    for source in mermaid_to_render {
        app.mermaid_cache.get_or_render(&source);
    }

    // Handle line truncation based on scroll position.
    let total_lines = lines.len();
    let visible_lines = if total_lines > inner.height as usize {
        if app.is_at_max_scroll() {
            lines.truncate(inner.height as usize);
            lines
        } else {
            let truncation_offset = total_lines - inner.height as usize;
            lines.split_off(truncation_offset)
        }
    } else {
        lines
    };

    // Store in cache for future frames
    app.message_render_cache = Some(MessageRenderCache::new(
        visible_lines.clone(),
        app.diagram_sources.clone(),
        cache_key,
    ));

    let paragraph = Paragraph::new(visible_lines);

    f.render_widget(block, area);
    f.render_widget(paragraph, inner);
}

/// Calculate the required height for the input bar based on wrapped text
///
/// Returns total height including borders (2) and content lines (1 minimum, 6 maximum).
fn calculate_input_bar_height(input_text: &str, area_width: u16) -> u16 {
    const PROMPT_WIDTH: usize = 3; // "› "
    const CURSOR_WIDTH: usize = 1; // "█"
    const MIN_CONTENT_LINES: u16 = 1;
    const MAX_CONTENT_LINES: u16 = 6;
    const BORDER_HEIGHT: u16 = 2; // Top and bottom borders

    // Account for borders when calculating available width
    let available_width = area_width.saturating_sub(2) as usize;
    if available_width == 0 {
        return BORDER_HEIGHT + MIN_CONTENT_LINES;
    }

    // Calculate text width after prompt and cursor
    let content_width = available_width.saturating_sub(PROMPT_WIDTH + CURSOR_WIDTH);
    if content_width == 0 {
        return BORDER_HEIGHT + MIN_CONTENT_LINES;
    }

    // Count wrapped lines (wrap_content splits on '\n' then wraps each line)
    let line_count = if input_text.is_empty() {
        1
    } else {
        wrap_content(input_text, content_width).len()
    };

    // Clamp to min/max and add borders
    let content_lines = (line_count as u16).clamp(MIN_CONTENT_LINES, MAX_CONTENT_LINES);
    BORDER_HEIGHT + content_lines
}

/// Draw the input bar at the bottom of the chat panel
fn draw_input_bar(f: &mut Frame, app: &App, area: Rect) {
    use super::app::FocusedPane;

    let is_focused = app.focused_pane == FocusedPane::InputBar;
    let border_color = if is_focused {
        Color::Yellow
    } else {
        Color::DarkGray
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);

    // Show input text with cursor
    let prompt = "› ";
    let char_count = app.input_text.chars().count();
    let text_with_cursor = if is_focused && app.input_cursor == char_count {
        format!("{}{}█", prompt, app.input_text)
    } else if is_focused {
        // Convert character index to byte index for split_at
        let byte_idx = app
            .input_text
            .char_indices()
            .nth(app.input_cursor)
            .map(|(idx, _)| idx)
            .unwrap_or(app.input_text.len());
        let (before, after) = app.input_text.split_at(byte_idx);
        format!("{}{}█{}", prompt, before, after)
    } else {
        format!("{}{}", prompt, app.input_text)
    };

    let paragraph = Paragraph::new(text_with_cursor).wrap(Wrap { trim: false });

    f.render_widget(block, area);
    f.render_widget(paragraph, inner);
}

/// Draw autocomplete dropdown above the input bar
fn draw_autocomplete_dropdown(f: &mut Frame, app: &App, input_area: Rect) {
    let items = &app.autocomplete.items;
    if items.is_empty() {
        return;
    }

    // Calculate dropdown dimensions
    let item_count = items.len().min(8); // Show max 8 items
    let dropdown_height = (item_count * 2) as u16; // 2 lines per item (value + description or blank)
    let dropdown_width = 40u16.min(input_area.width.saturating_sub(4));

    // Position dropdown above input bar (with 1-line gap)
    let dropdown_y = input_area
        .y
        .saturating_sub(dropdown_height)
        .saturating_sub(1);
    let dropdown_x = input_area.x + 2; // Indent slightly from input bar

    let dropdown_area = Rect {
        x: dropdown_x,
        y: dropdown_y,
        width: dropdown_width,
        height: dropdown_height,
    };

    // Build dropdown lines
    let mut lines = Vec::new();
    for (i, item) in items.iter().enumerate().take(item_count) {
        let is_selected = i == app.autocomplete.selected_index;

        // Item value line (highlighted if selected)
        let value_style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        lines.push(Line::from(vec![Span::styled(
            format!(" {} ", item.value),
            value_style,
        )]));

        // Description line (if present)
        if let Some(ref desc) = item.description {
            let desc_text = if desc.len() > dropdown_width as usize - 4 {
                format!(" {}...", &desc[..dropdown_width as usize - 7])
            } else {
                format!(" {}", desc)
            };
            let desc_style = if is_selected {
                Style::default().fg(Color::Black).bg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            lines.push(Line::from(vec![Span::styled(desc_text, desc_style)]));
        } else {
            // Blank line for spacing
            lines.push(Line::from(""));
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black));

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, dropdown_area);
}

/// Precomputed values shared by message rendering functions.
///
/// Avoids duplicating display name resolution, color lookup, sender visibility,
/// content style, and extra indent calculations across `render_message` and
/// `render_message_with_mermaid`.
struct MessageRenderContext {
    time: String,
    display_from: String,
    color: Color,
    show_sender: bool,
    content_style: Style,
    /// Extra indent beyond the timestamp gutter (2 for action "* ", crosspost prefix len, or 0).
    extra_indent: usize,
}

impl MessageRenderContext {
    fn new(msg: &Message, prev_sender: Option<&str>, user_display_name: Option<&str>) -> Self {
        let local_time = msg.timestamp.with_timezone(&Local);
        let time = local_time.format("%H:%M").to_string();

        let display_from: String = if msg.from == "user" {
            user_display_name.unwrap_or("user").to_string()
        } else {
            msg.from.clone()
        };

        let color = get_sender_color(&display_from);
        let show_sender = prev_sender.is_none_or(|prev| prev != msg.from);

        let content_style = match msg.message_type {
            MessageType::Action => Style::default().fg(color),
            MessageType::System => Style::default().fg(Color::DarkGray),
            _ if is_dim_sender(&msg.from) => Style::default().fg(Color::DarkGray),
            _ => Style::default().fg(Color::White),
        };

        let extra_indent = if msg.message_type == MessageType::Action {
            2 // "* "
        } else if let Some(ref source_channel) = msg.source_channel {
            2 + 6 + source_channel.chars().count() + 3 // "★ from #channel | "
        } else {
            0
        };

        Self {
            time,
            display_from,
            color,
            show_sender,
            content_style,
            extra_indent,
        }
    }

    /// Content width available after timestamp gutter and extra indent.
    fn content_width(&self, width: usize) -> usize {
        width.saturating_sub(TIMESTAMP_GUTTER_WIDTH + self.extra_indent)
    }

    /// Total indent width (timestamp gutter + extra indent).
    fn indent_width(&self) -> usize {
        TIMESTAMP_GUTTER_WIDTH + self.extra_indent
    }
}

/// Push the sender header (optional blank line + sender name line) into `lines`.
///
/// The blank-line logic differs slightly for action messages vs. regular messages:
/// - Action messages: blank line unless prev sender was system-like
/// - Regular messages: blank line unless both prev and current are system-like
fn push_sender_header(
    msg: &Message,
    ctx: &MessageRenderContext,
    prev_sender: Option<&str>,
    current_tasks: &HashMap<String, String>,
    width: usize,
    lines: &mut Vec<Line<'static>>,
) {
    let add_blank = if msg.message_type == MessageType::Action {
        prev_sender.is_some_and(|prev| !is_system_like_sender(prev))
    } else if let Some(prev) = prev_sender {
        !(is_system_like_sender(prev) && is_system_like_sender(&msg.from))
    } else {
        false
    };
    if add_blank {
        lines.push(Line::from(""));
    }
    let current_task = current_tasks.get(&msg.from.to_lowercase());
    lines.push(build_sender_line(
        &ctx.display_from,
        &msg.message_type,
        ctx.color,
        current_task,
        width,
    ));
}

/// Build the first content line with appropriate timestamp prefix.
///
/// Dispatches to action ("* "), crosspost ("★ from #channel | "), or plain timestamp format.
fn build_first_content_line(
    msg: &Message,
    ctx: &MessageRenderContext,
    content: &str,
) -> Line<'static> {
    if msg.message_type == MessageType::Action {
        build_action_timestamp_line(&ctx.time, content, ctx.color, ctx.content_style)
    } else if let Some(ref source_channel) = msg.source_channel {
        build_crosspost_timestamp_line(&ctx.time, content, source_channel, ctx.content_style)
    } else {
        build_timestamp_line(&ctx.time, content, ctx.content_style)
    }
}

/// Build a continuation line (non-first content line) with proper indentation.
fn build_continuation_line(ctx: &MessageRenderContext, content: &str) -> Line<'static> {
    let indent = " ".repeat(ctx.indent_width());
    let mut spans = vec![Span::raw(indent)];
    spans.extend(parse_markdown(content, ctx.content_style));
    Line::from(spans)
}

/// Render a single message into one or more Lines.
///
/// Handles three message variants (action, crosspost, regular) through a unified
/// flow: compute context → optional sender header → timestamp first line →
/// indented continuation lines.
fn render_message(
    msg: &Message,
    width: usize,
    prev_sender: Option<&str>,
    current_tasks: &HashMap<String, String>,
    user_display_name: Option<&str>,
) -> Vec<Line<'static>> {
    let ctx = MessageRenderContext::new(msg, prev_sender, user_display_name);

    let content_width = ctx.content_width(width);
    if content_width == 0 {
        return vec![];
    }

    let content_lines = wrap_content(&msg.content, content_width);
    let mut result = Vec::new();

    if ctx.show_sender {
        push_sender_header(msg, &ctx, prev_sender, current_tasks, width, &mut result);
    }

    for (i, content) in content_lines.iter().enumerate() {
        if i == 0 {
            result.push(build_first_content_line(msg, &ctx, content));
        } else {
            result.push(build_continuation_line(&ctx, content));
        }
    }

    result
}

/// Render a message that contains mermaid code fences.
///
/// Splits the message content into text and mermaid segments, rendering
/// text normally and inserting selectable placeholders for mermaid diagrams.
/// Each diagram gets a numbered label that the user can select to open
/// in a fullscreen viewer.
#[allow(clippy::too_many_arguments)]
fn render_message_with_mermaid(
    msg: &Message,
    segments: &[ContentSegment],
    width: usize,
    prev_sender: Option<&str>,
    current_tasks: &HashMap<String, String>,
    user_display_name: Option<&str>,
    mermaid_cache: &MermaidCache,
    lines: &mut Vec<Line<'static>>,
    diagram_sources: &mut Vec<String>,
    mermaid_to_render: &mut Vec<String>,
) {
    let ctx = MessageRenderContext::new(msg, prev_sender, user_display_name);

    let content_width = ctx.content_width(width);
    if content_width == 0 {
        return;
    }

    if ctx.show_sender {
        push_sender_header(msg, &ctx, prev_sender, current_tasks, width, lines);
    }

    let mut is_first_content_line = true;

    for segment in segments {
        match segment {
            ContentSegment::Text(text) => {
                let content_lines = wrap_content(text, content_width);
                for content in &content_lines {
                    if is_first_content_line {
                        lines.push(build_first_content_line(msg, &ctx, content));
                        is_first_content_line = false;
                    } else {
                        lines.push(build_continuation_line(&ctx, content));
                    }
                }
            }
            ContentSegment::Mermaid(source) => {
                // Extract diagram type from the first line for the label
                let diagram_type = source
                    .lines()
                    .next()
                    .unwrap_or("diagram")
                    .split_whitespace()
                    .next()
                    .unwrap_or("diagram");

                let indent = " ".repeat(ctx.indent_width());

                if let Some(diagram) = mermaid_cache.get_cached(source) {
                    // Diagram is ready: show inline ASCII art with separators
                    let diagram_num = diagram_sources.len() + 1;
                    diagram_sources.push(source.clone());

                    // Top separator: --- graph type ---
                    let top_sep = format!("{}--- {} ---", indent, diagram_type);
                    lines.push(Line::from(Span::styled(
                        top_sep,
                        Style::default().fg(Color::DarkGray),
                    )));

                    // ASCII art lines (cyan, indented, truncated to content_width)
                    // Use chars().take() for safe truncation of multi-byte UTF-8
                    // (box-drawing characters like ┌│└─ are 3 bytes each)
                    for art_line in diagram.ascii_art.lines() {
                        let truncated: String = art_line.chars().take(content_width).collect();
                        lines.push(Line::from(Span::styled(
                            format!("{}{}", indent, truncated),
                            Style::default().fg(Color::Cyan),
                        )));
                    }

                    // Bottom separator with browser hint
                    let bottom_sep = if diagram_num <= 9 {
                        format!("{}--- press {} to open in browser ---", indent, diagram_num,)
                    } else {
                        format!("{}--- {} ---", indent, diagram_type)
                    };
                    lines.push(Line::from(Span::styled(
                        bottom_sep,
                        Style::default().fg(Color::DarkGray),
                    )));
                    is_first_content_line = false;
                } else if mermaid_cache.is_pending(source) {
                    // Rendering in progress: show placeholder
                    let placeholder = format!("{}[rendering {}...]", indent, diagram_type);
                    lines.push(Line::from(Span::styled(
                        placeholder,
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    )));
                    is_first_content_line = false;
                } else {
                    // Not yet queued: show placeholder and queue for rendering
                    let placeholder = format!("{}[{} diagram]", indent, diagram_type);
                    lines.push(Line::from(Span::styled(
                        placeholder,
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    )));
                    mermaid_to_render.push(source.clone());
                    is_first_content_line = false;
                }
            }
        }
    }
}

/// Build a line with the sender name and optionally their current task
///
/// Format: "**name**" or "**name** - Task subject" (task is not bold)
fn build_sender_line(
    display_name: &str,
    message_type: &MessageType,
    color: Color,
    current_task: Option<&String>,
    width: usize,
) -> Line<'static> {
    match message_type {
        MessageType::System => Line::from(vec![Span::styled(
            String::from("<system>"),
            Style::default().fg(Color::DarkGray),
        )]),
        _ => {
            let mut spans = vec![Span::styled(
                display_name.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )];

            // Add current task if available
            if let Some(task) = current_task {
                // Calculate available space for task (width - name - " - ")
                // Use chars().count() for UTF-8 safe length calculation
                let prefix_len = display_name.chars().count() + 3; // " - " = 3 chars
                let available = width.saturating_sub(prefix_len);

                if available > 5 {
                    // Only show if we have reasonable space
                    // Use chars() for UTF-8 safe truncation to avoid panics on multi-byte chars
                    let truncated_task = if task.chars().count() > available {
                        let truncated: String =
                            task.chars().take(available.saturating_sub(1)).collect();
                        format!("{}…", truncated)
                    } else {
                        task.clone()
                    };

                    spans.push(Span::styled(
                        format!(" - {}", truncated_task),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }

            Line::from(spans)
        }
    }
}

/// Build a timestamp line with message content: " HH:MM message"
fn build_timestamp_line(time: &str, content: &str, content_style: Style) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!(" {} ", time),
        Style::default().fg(Color::DarkGray),
    )];
    spans.extend(parse_markdown(content, content_style));
    Line::from(spans)
}

/// Build a timestamp line for action messages: " HH:MM * message"
/// The "*" is in the actor's color to indicate this is an action/status message
fn build_action_timestamp_line(
    time: &str,
    content: &str,
    actor_color: Color,
    content_style: Style,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(format!(" {} ", time), Style::default().fg(Color::DarkGray)),
        Span::styled("* ", Style::default().fg(actor_color)),
    ];
    spans.extend(parse_markdown(content, content_style));
    Line::from(spans)
}

/// Build a timestamp line for cross-posted insights: " HH:MM ★ from #channel | message"
/// The "★" and channel attribution are styled distinctly to indicate cross-posting
fn build_crosspost_timestamp_line(
    time: &str,
    content: &str,
    source_channel: &str,
    content_style: Style,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(format!(" {} ", time), Style::default().fg(Color::DarkGray)),
        Span::styled("★ ", Style::default().fg(Color::Yellow)),
        Span::styled(
            format!("from #{} | ", source_channel),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    spans.extend(parse_markdown(content, content_style));
    Line::from(spans)
}

/// Wrap content text into lines that fit the given width
fn wrap_content(content: &str, width: usize) -> Vec<String> {
    let mut result = Vec::new();
    for line in content.split('\n') {
        let wrapped = wrap_line(line, width);
        for w in wrapped {
            result.push(w.to_string());
        }
    }
    result
}

/// Parse markdown in text and return styled spans
///
/// Handles:
/// - **bold** -> BOLD modifier
/// - *italic* -> ITALIC modifier
/// - `code` -> Cyan color
fn parse_markdown(text: &str, base_style: Style) -> Vec<Span<'static>> {
    if text.is_empty() {
        return vec![Span::styled(String::new(), base_style)];
    }

    let mut spans = Vec::new();
    let mut chars = text.char_indices().peekable();
    let mut current_pos = 0;

    while let Some((i, c)) = chars.next() {
        match c {
            '`' => {
                // Code span - look for closing backtick
                if let Some(end) = text[i + 1..].find('`') {
                    // Add any text before this marker
                    if i > current_pos {
                        spans.push(Span::styled(text[current_pos..i].to_string(), base_style));
                    }
                    // Add the code span (without backticks)
                    let code_text = &text[i + 1..i + 1 + end];
                    spans.push(Span::styled(
                        code_text.to_string(),
                        Style::default().fg(Color::Cyan),
                    ));
                    // Skip past the closing backtick
                    current_pos = i + 1 + end + 1;
                    // Advance the iterator
                    for _ in 0..end + 1 {
                        chars.next();
                    }
                }
            }
            '*' => {
                // Check for ** (bold) or single * (italic)
                if chars.peek().is_some_and(|(_, next_c)| *next_c == '*') {
                    // Bold: **text**
                    chars.next(); // consume second *
                    if let Some(end) = text[i + 2..].find("**") {
                        if i > current_pos {
                            spans.push(Span::styled(text[current_pos..i].to_string(), base_style));
                        }
                        let bold_text = &text[i + 2..i + 2 + end];
                        spans.push(Span::styled(
                            bold_text.to_string(),
                            base_style.add_modifier(Modifier::BOLD),
                        ));
                        current_pos = i + 2 + end + 2;
                        // Skip past closing **
                        for _ in 0..end + 2 {
                            chars.next();
                        }
                    }
                } else {
                    // Italic: *text*
                    if let Some(end) = text[i + 1..].find('*') {
                        // Make sure it's not ** (start of bold)
                        if end > 0 && !text[i + 1..].starts_with('*') {
                            if i > current_pos {
                                spans.push(Span::styled(
                                    text[current_pos..i].to_string(),
                                    base_style,
                                ));
                            }
                            let italic_text = &text[i + 1..i + 1 + end];
                            spans.push(Span::styled(
                                italic_text.to_string(),
                                base_style.add_modifier(Modifier::ITALIC),
                            ));
                            current_pos = i + 1 + end + 1;
                            // Skip past closing *
                            for _ in 0..end + 1 {
                                chars.next();
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Add any remaining text
    if current_pos < text.len() {
        spans.push(Span::styled(text[current_pos..].to_string(), base_style));
    }

    // If no spans were added (e.g., no markdown found), return the whole text
    if spans.is_empty() {
        return vec![Span::styled(text.to_string(), base_style)];
    }

    spans
}

/// Wrap a single line of text to fit within the given width
///
/// Uses word boundaries when possible, falls back to character wrapping.
/// Handles UTF-8 multi-byte characters correctly by using character indices.
fn wrap_line(text: &str, width: usize) -> Vec<&str> {
    // Clamp width to minimum 1 to prevent infinite loop
    let width = width.max(1);

    if text.is_empty() {
        return vec![""];
    }
    // Use character count, not byte length (UTF-8 chars can be multi-byte)
    if text.chars().count() <= width {
        return vec![text];
    }

    let mut result = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        let char_count = remaining.chars().count();
        if char_count <= width {
            result.push(remaining);
            break;
        }

        // Find the byte position of the width-th character (safe boundary)
        let byte_pos = remaining
            .char_indices()
            .nth(width)
            .map(|(i, _)| i)
            .unwrap_or(remaining.len());

        // Try to find a word boundary within the width limit
        let break_at = remaining[..byte_pos]
            .rfind(' ')
            .map(|pos| pos + 1) // Include the space in current line
            .unwrap_or(byte_pos); // Fall back to hard break at char boundary

        let (line, rest) = remaining.split_at(break_at);
        result.push(line.trim_end()); // Remove trailing space from wrapped line
        remaining = rest.trim_start(); // Remove leading space from next line
    }

    result
}

/// Draw the usage progress bars (session + weekly utilization).
///
/// Renders two compact lines showing utilization percentage as progress bars
/// with color thresholds: green <60%, yellow 60-80%, red >80%.
fn draw_usage_bars(f: &mut Frame, app: &App, area: Rect) {
    let usage = match &app.usage_data {
        Some(data) => data,
        None => return,
    };

    let title = match &usage.account_email {
        Some(email) => format!(" Usage ({}) ", email),
        None => " Usage ".to_string(),
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height < 2 {
        return;
    }

    let session_line = render_usage_line(
        "Session",
        usage.session_util,
        usage.session_resets.as_ref(),
        true,
    );
    let week_line = render_usage_line(
        "Week   ",
        usage.week_util,
        usage.week_resets.as_ref(),
        false,
    );

    let lines = vec![session_line, week_line];
    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, inner);
}

/// Render a single usage progress bar line.
///
/// Format: `Label ████████░░░░░░░░░░ XX%  ~Xh remaining  ↻ reset_time`
fn render_usage_line(
    label: &str,
    utilization: f64,
    resets_at: Option<&DateTime<Utc>>,
    is_session: bool,
) -> Line<'static> {
    let color = usage_color(utilization);
    let pct = utilization.round() as u32;

    // Bar width: fill available space after label (8) + pct (5) + reset info (~12)
    // Use a fixed bar width of 20 characters
    let bar_width: usize = 20;
    let filled = ((utilization / 100.0) * bar_width as f64).round() as usize;
    let empty = bar_width.saturating_sub(filled);

    let bar_filled: String = "█".repeat(filled);
    let bar_empty: String = "░".repeat(empty);

    let (estimate_text, reset_text) = match resets_at {
        Some(r) => (
            estimate_time_to_full(utilization, r, is_session),
            format_reset_time(r, is_session),
        ),
        None => ("—".to_string(), "—".to_string()),
    };

    Line::from(vec![
        Span::styled(format!(" {label} "), Style::default().fg(Color::DarkGray)),
        Span::styled(bar_filled, Style::default().fg(color)),
        Span::styled(bar_empty, Style::default().fg(Color::DarkGray)),
        Span::styled(format!(" {:>3}%", pct), Style::default().fg(color)),
        Span::styled(
            format!("  {estimate_text}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("  ↻ {reset_text}"),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

/// Choose bar color based on utilization threshold.
fn usage_color(utilization: f64) -> Color {
    if utilization >= 80.0 {
        Color::Red
    } else if utilization >= 60.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

/// Estimate time until utilization reaches 100% based on current consumption rate.
///
/// Uses the known window duration (5h session, 7d weekly) and current utilization
/// to extrapolate when usage will hit the limit. Returns "—" if rate is zero
/// (no consumption) or utilization is already at/above 100%.
fn estimate_time_to_full(utilization: f64, resets_at: &DateTime<Utc>, is_session: bool) -> String {
    if utilization <= 0.0 || utilization >= 100.0 {
        return "—".to_string();
    }

    let now = Utc::now();
    let time_until_reset = resets_at.signed_duration_since(now);
    let secs_until_reset = time_until_reset.num_seconds();

    if secs_until_reset <= 0 {
        return "—".to_string();
    }

    // Total window duration in seconds
    let window_secs: f64 = if is_session {
        5.0 * 3600.0 // 5 hours
    } else {
        7.0 * 24.0 * 3600.0 // 7 days
    };

    // Elapsed time in this window = total_window - time_remaining
    let elapsed_secs = window_secs - secs_until_reset as f64;
    if elapsed_secs <= 0.0 {
        return "—".to_string();
    }

    // Rate = utilization percentage per second
    let rate = utilization / elapsed_secs;
    // Time to reach 100% from current utilization
    let remaining_pct = 100.0 - utilization;
    let secs_to_full = remaining_pct / rate;

    format_duration_estimate(secs_to_full)
}

/// Format a duration in seconds as a human-readable estimate string.
fn format_duration_estimate(secs: f64) -> String {
    let minutes = (secs / 60.0).round() as i64;
    if minutes < 1 {
        "~<1m left".to_string()
    } else if minutes < 60 {
        format!("~{minutes}m left")
    } else {
        let hours = minutes / 60;
        let remaining_mins = minutes % 60;
        if remaining_mins == 0 {
            format!("~{hours}h left")
        } else {
            format!("~{hours}h{remaining_mins}m left")
        }
    }
}

/// Format reset time for display.
///
/// Session: "H:MMam/pm" (e.g., "4:59pm")
/// Weekly: "Mon DD" (e.g., "Feb 11")
/// Returns "now" if the reset time is in the past.
fn format_reset_time(resets_at: &DateTime<Utc>, is_session: bool) -> String {
    if *resets_at <= Utc::now() {
        return "now".to_string();
    }
    let local = resets_at.with_timezone(&Local);
    if is_session {
        local.format("%-I:%M%P").to_string()
    } else {
        local.format("%b %-d").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap_line_empty() {
        assert_eq!(wrap_line("", 40), vec![""]);
    }

    #[test]
    fn test_wrap_line_fits() {
        assert_eq!(wrap_line("hello world", 40), vec!["hello world"]);
    }

    #[test]
    fn test_wrap_line_word_boundary() {
        // "hello world" is 11 chars, with width 7 it should wrap at the space
        assert_eq!(wrap_line("hello world", 7), vec!["hello", "world"]);
    }

    #[test]
    fn test_wrap_line_hard_break() {
        // Word too long to fit, must hard break
        assert_eq!(wrap_line("abcdefghij", 5), vec!["abcde", "fghij"]);
    }

    #[test]
    fn test_wrap_line_multiple_wraps() {
        let text = "this is a longer message that needs multiple wraps";
        let wrapped = wrap_line(text, 15);
        // Each line should be at most 15 characters (not bytes)
        for line in &wrapped {
            assert!(
                line.chars().count() <= 15,
                "Line too long: {} ({} chars)",
                line,
                line.chars().count()
            );
        }
        // Reassembling should give us the original (minus spaces at wrap points)
        let rejoined: String = wrapped.join(" ");
        assert_eq!(rejoined.replace("  ", " "), text);
    }

    #[test]
    fn test_wrap_line_utf8_box_drawing() {
        // Box-drawing characters are 3 bytes each in UTF-8
        let text = "┌─ Team ─────────────────────────────────────────┐";
        // This should NOT panic - the bug was slicing at byte boundaries
        let wrapped = wrap_line(text, 40);
        // Verify we get valid UTF-8 strings back
        for line in &wrapped {
            assert!(line.chars().count() <= 40);
        }
    }

    #[test]
    fn test_wrap_line_zero_width() {
        // Zero width should not cause infinite loop - clamp to minimum 1
        let wrapped = wrap_line("hello", 0);
        // Should produce single-character chunks
        assert_eq!(wrapped, vec!["h", "e", "l", "l", "o"]);
    }

    #[test]
    fn test_parse_markdown_plain_text() {
        let base = Style::default().fg(Color::White);
        let spans = parse_markdown("hello world", base);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello world");
    }

    #[test]
    fn test_parse_markdown_bold() {
        let base = Style::default().fg(Color::White);
        let spans = parse_markdown("hello **bold** world", base);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "hello ");
        assert_eq!(spans[1].content, "bold");
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[2].content, " world");
    }

    #[test]
    fn test_parse_markdown_italic() {
        let base = Style::default().fg(Color::White);
        let spans = parse_markdown("hello *italic* world", base);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "hello ");
        assert_eq!(spans[1].content, "italic");
        assert!(spans[1].style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(spans[2].content, " world");
    }

    #[test]
    fn test_parse_markdown_code() {
        let base = Style::default().fg(Color::White);
        let spans = parse_markdown("run `cargo test` now", base);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "run ");
        assert_eq!(spans[1].content, "cargo test");
        assert_eq!(spans[1].style.fg, Some(Color::Cyan));
        assert_eq!(spans[2].content, " now");
    }

    #[test]
    fn test_parse_markdown_mixed() {
        let base = Style::default().fg(Color::White);
        let spans = parse_markdown("**bold** and `code`", base);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "bold");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[1].content, " and ");
        assert_eq!(spans[2].content, "code");
        assert_eq!(spans[2].style.fg, Some(Color::Cyan));
    }

    #[test]
    fn test_continuation_lines_have_consistent_indent() {
        use chrono::Utc;

        // Create messages from users with different name lengths (3 content lines)
        let short_name_msg = Message {
            id: "1".to_string(),
            from: "a".to_string(),
            content: "line1\nline2\nline3".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let long_name_msg = Message {
            id: "2".to_string(),
            from: "lexington".to_string(),
            content: "line1\nline2\nline3".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };

        let current_tasks = HashMap::new();

        // New layout: name line, then 3 content lines (timestamp + 2 continuations)
        // Total = 4 lines: sender, timestamp+line1, indent+line2, indent+line3
        let short_lines = render_message(&short_name_msg, 80, None, &current_tasks, None);
        let long_lines = render_message(&long_name_msg, 80, None, &current_tasks, None);

        assert_eq!(short_lines.len(), 4, "Expected 4 lines: sender + 3 content");
        assert_eq!(long_lines.len(), 4, "Expected 4 lines: sender + 3 content");

        // Extract the indent from continuation lines (3rd and 4th line, first span)
        let short_indent = &short_lines[2].spans[0].content;
        let long_indent = &long_lines[2].spans[0].content;

        // Continuation lines should have the SAME indent (7 spaces) regardless of username length
        assert_eq!(
            short_indent.len(),
            TIMESTAMP_GUTTER_WIDTH,
            "Indent should be {} chars, got {}",
            TIMESTAMP_GUTTER_WIDTH,
            short_indent.len()
        );
        assert_eq!(
            short_indent.len(),
            long_indent.len(),
            "Continuation indent should be consistent"
        );
    }

    #[test]
    fn test_consecutive_messages_skip_sender_name() {
        use chrono::Utc;

        let msg1 = Message {
            id: "1".to_string(),
            from: "columbus".to_string(),
            content: "first message".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let msg2 = Message {
            id: "2".to_string(),
            from: "columbus".to_string(),
            content: "second message".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };

        let current_tasks = HashMap::new();

        // First message (no previous sender) - shows sender line + timestamp line
        let lines1 = render_message(&msg1, 80, None, &current_tasks, None);
        assert_eq!(lines1.len(), 2); // sender line + timestamp+content line

        // Second message from same sender - shows only timestamp + content (no sender)
        let lines2 = render_message(&msg2, 80, Some("columbus"), &current_tasks, None);
        assert_eq!(lines2.len(), 1); // just timestamp + content

        // Different sender - shows blank line + sender line + timestamp line
        let lines3 = render_message(&msg2, 80, Some("lexington"), &current_tasks, None);
        assert_eq!(lines3.len(), 3); // blank + sender line + timestamp+content line

        // Verify first message has sender name on first line
        let first_line_content: String =
            lines1[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(first_line_content.contains("columbus"));

        // Verify same-sender message has timestamp, not actor
        let same_sender_content: String =
            lines2[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!same_sender_content.contains("columbus"));
        assert!(same_sender_content.contains(":")); // Has timestamp like "10:12"
    }

    #[test]
    fn test_action_message_format() {
        use chrono::Utc;

        let msg = Message {
            id: "1".to_string(),
            from: "park".to_string(),
            content: "completed task 3".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Action,
            channel: None,
            source_channel: None,
            session_id: None,
        };

        let current_tasks = HashMap::new();

        // Action messages now follow standard format:
        // Line 0: actor name (when sender changes)
        // Line 1: " HH:MM * message" with * in actor color
        let lines = render_message(&msg, 80, None, &current_tasks, None);
        assert_eq!(lines.len(), 2, "Expected 2 lines: actor name + message");

        // First line should be actor name
        let first_line: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            first_line.contains("park"),
            "First line should contain actor name, got: {}",
            first_line
        );

        // Second line should have format " HH:MM * message"
        let second_line: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            second_line.contains("* "),
            "Message line should contain '* ', got: {}",
            second_line
        );
        assert!(
            second_line.contains("completed task 3"),
            "Message line should contain content, got: {}",
            second_line
        );
        assert!(
            second_line.contains(":"),
            "Message line should contain timestamp, got: {}",
            second_line
        );

        // Verify the spans on the message line: timestamp, "* ", content
        assert!(
            lines[1].spans.len() >= 3,
            "Expected at least 3 spans: timestamp, '* ', content"
        );
        // First span should be timestamp " HH:MM "
        assert!(
            lines[1].spans[0].content.contains(":"),
            "First span should be timestamp"
        );
        // Second span should be "* "
        assert_eq!(
            lines[1].spans[1].content, "* ",
            "Second span should be '* '"
        );
    }

    #[test]
    fn test_system_message_format() {
        use chrono::Utc;

        let msg = Message {
            id: "1".to_string(),
            from: "system".to_string(),
            content: "Session started".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::System,
            channel: None,
            source_channel: None,
            session_id: None,
        };

        let current_tasks = HashMap::new();

        // System messages now render through standard path: sender line + timestamp line
        let lines = render_message(&msg, 80, None, &current_tasks, None);
        assert_eq!(lines.len(), 2); // sender line + content line

        // First line is the sender name (<system>)
        let sender: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(sender, "<system>");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::DarkGray));

        // Second line has timestamp + content in DarkGray
        let content: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(content.contains("Session started"));
    }

    #[test]
    fn test_format_relative_time() {
        use chrono::{Duration, Utc};

        let now = Utc::now();

        // Just now (less than a minute ago)
        assert_eq!(format_relative_time(now), "just now");

        // Minutes ago
        assert_eq!(
            format_relative_time(now - Duration::minutes(1)),
            "1 minute ago"
        );
        assert_eq!(
            format_relative_time(now - Duration::minutes(30)),
            "30 minutes ago"
        );
        assert_eq!(
            format_relative_time(now - Duration::minutes(59)),
            "59 minutes ago"
        );

        // Hours ago
        assert_eq!(format_relative_time(now - Duration::hours(1)), "1 hour ago");
        assert_eq!(
            format_relative_time(now - Duration::hours(5)),
            "5 hours ago"
        );
        assert_eq!(
            format_relative_time(now - Duration::hours(23)),
            "23 hours ago"
        );

        // Days ago
        assert_eq!(format_relative_time(now - Duration::days(1)), "1 day ago");
        assert_eq!(format_relative_time(now - Duration::days(7)), "7 days ago");
    }

    #[test]
    fn test_chat_shows_newest_messages_when_content_exceeds_height() {
        // Bug reproduction: When messages render to more lines than visible_height,
        // the chat should show the NEWEST messages at the bottom, not the oldest.
        //
        // Each message from a different sender renders to ~3 lines:
        // - blank line (separator)
        // - sender name line
        // - timestamp + content line
        //
        // So 10 messages from different senders = ~30 lines of content.
        // If visible_height is 10, we should see the LAST 10 lines (newest messages),
        // not the FIRST 10 lines (oldest messages).
        use chrono::Utc;

        // Create 10 messages from different senders (each will take ~3 lines)
        let messages: Vec<Message> = (0..10)
            .map(|i| Message {
                id: i.to_string(),
                from: format!("user{}", i), // Different sender each time = new sender line
                content: format!("message content {}", i),
                timestamp: Utc::now(),
                message_type: MessageType::Text,
                channel: None,
                source_channel: None,
                session_id: None,
            })
            .collect();

        let current_tasks = HashMap::new();

        // Render all messages
        let mut all_lines: Vec<Line> = Vec::new();
        let mut prev_sender: Option<&str> = None;
        for msg in &messages {
            let msg_lines = render_message(msg, 80, prev_sender, &current_tasks, None);
            all_lines.extend(msg_lines);
            prev_sender = Some(&msg.from);
        }

        // Verify we have more lines than a typical visible_height
        assert!(
            all_lines.len() > 10,
            "Expected more than 10 lines, got {}",
            all_lines.len()
        );

        // The LAST line should contain content from the LAST message (message 9)
        let last_line: String = all_lines
            .last()
            .unwrap()
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            last_line.contains("message content 9"),
            "Last line should contain newest message, got: {}",
            last_line
        );

        // Now simulate what draw_chat_panel does: take only visible_height lines
        // BUG: Currently it would show the FIRST 10 lines (oldest messages)
        // FIX: It should show the LAST 10 lines (newest messages)
        let visible_height = 10;

        // Current buggy behavior: takes from beginning
        let buggy_visible: Vec<_> = all_lines.iter().take(visible_height).collect();

        // Fixed behavior: takes from end
        let fixed_visible: Vec<_> = all_lines
            .iter()
            .skip(all_lines.len().saturating_sub(visible_height))
            .collect();

        // The buggy version would show oldest messages (message 0)
        let buggy_content: String = buggy_visible
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();

        // The fixed version shows newest messages (message 9)
        let fixed_content: String = fixed_visible
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();

        // Verify the bug: buggy shows old, fixed shows new
        assert!(
            buggy_content.contains("message content 0"),
            "Buggy version should contain oldest message"
        );
        assert!(
            !buggy_content.contains("message content 9"),
            "Buggy version should NOT contain newest message"
        );
        assert!(
            fixed_content.contains("message content 9"),
            "Fixed version should contain newest message"
        );
    }

    #[test]
    fn test_smooth_scrolling_always_shows_last_lines() {
        // Test for smooth scrolling behavior.
        //
        // The scroll system has two components:
        // 1. visible_messages() returns a window of messages based on scroll_offset
        // 2. draw_chat_panel() truncates rendered lines to fit the visible area
        //
        // For smooth scrolling, we should ALWAYS take the LAST N lines of rendered
        // content (except at max scroll). This ensures that scrolling by 1 message
        // produces a proportional visual shift, not a jarring jump.
        //
        // The OLD buggy behavior switched from LAST to FIRST lines when scroll_offset
        // changed from 0 to 1, causing a massive visual discontinuity.
        use chrono::Utc;

        // Create 20 messages, each from a different sender (so each takes ~3 lines)
        let messages: Vec<Message> = (0..20)
            .map(|i| Message {
                id: i.to_string(),
                from: format!("user{}", i),
                content: format!("message content {}", i),
                timestamp: Utc::now(),
                message_type: MessageType::Text,
                channel: None,
                source_channel: None,
                session_id: None,
            })
            .collect();

        let current_tasks = HashMap::new();

        // Simulate scroll_offset=0 (bottom): visible_messages returns messages 10..20
        let at_bottom_messages = &messages[10..20];
        let mut at_bottom_lines: Vec<Line> = Vec::new();
        let mut prev_sender: Option<&str> = None;
        for msg in at_bottom_messages {
            at_bottom_lines.extend(render_message(msg, 80, prev_sender, &current_tasks, None));
            prev_sender = Some(&msg.from);
        }

        // Simulate scroll_offset=1 (scrolled up by 1): visible_messages returns messages 9..19
        let scrolled_one_messages = &messages[9..19];
        let mut scrolled_one_lines: Vec<Line> = Vec::new();
        let mut prev_sender: Option<&str> = None;
        for msg in scrolled_one_messages {
            scrolled_one_lines.extend(render_message(msg, 80, prev_sender, &current_tasks, None));
            prev_sender = Some(&msg.from);
        }

        let visible_height = 10;

        // Both cases should take LAST N lines for smooth scrolling
        let bottom_visible: Vec<_> = if at_bottom_lines.len() > visible_height {
            at_bottom_lines
                .iter()
                .skip(at_bottom_lines.len() - visible_height)
                .collect()
        } else {
            at_bottom_lines.iter().collect()
        };

        let scrolled_visible: Vec<_> = if scrolled_one_lines.len() > visible_height {
            scrolled_one_lines
                .iter()
                .skip(scrolled_one_lines.len() - visible_height)
                .collect()
        } else {
            scrolled_one_lines.iter().collect()
        };

        // Extract content
        let bottom_content: String = bottom_visible
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        let scrolled_content: String = scrolled_visible
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();

        // At bottom should show newest messages (19, 18, etc.)
        assert!(
            bottom_content.contains("message content 19")
                || bottom_content.contains("message content 18"),
            "At bottom should show newest messages, got: {}",
            bottom_content
        );

        // Scrolled up by 1 should show slightly older content (18, 17, etc.)
        // but NOT a massive jump to messages 9, 10, etc.
        assert!(
            scrolled_content.contains("message content 18")
                || scrolled_content.contains("message content 17"),
            "Scrolled by 1 should show slightly older messages, got: {}",
            scrolled_content
        );

        // Verify the expected content shift direction.
        // Key insight: visible_messages() returns different message ranges:
        // - At bottom (messages 10..20): includes message 19, excludes message 9
        // - Scrolled up 1 (messages 9..19): excludes message 19, includes message 9
        //
        // With LAST N lines truncation, we see the end of each range's rendered content.
        // The critical test: message 19 should appear at bottom but NOT when scrolled,
        // proving the message selection shifted correctly.
        let bottom_has_19 = bottom_content.contains("message content 19");
        let scrolled_has_19 = scrolled_content.contains("message content 19");

        // At bottom (messages 10..20) must show message 19
        assert!(
            bottom_has_19,
            "At bottom should show message 19 (newest in 10..20 range), got: {}",
            bottom_content
        );

        // Scrolled view (messages 9..19) must NOT show message 19
        assert!(
            !scrolled_has_19,
            "Scrolled view should NOT show message 19 (not in 9..19 range), got: {}",
            scrolled_content
        );

        // Verify smooth scrolling: both views use LAST N lines, so content shifts
        // proportionally. The scrolled view shows slightly older messages (18, 17, etc.)
        // rather than jumping to much older content (would show 9, 10, 11 with FIRST N).
        assert!(
            scrolled_content.contains("message content 18"),
            "Scrolled view should show message 18 (near end of 9..19 range), got: {}",
            scrolled_content
        );
    }

    #[test]
    fn test_system_like_messages_grouped_together() {
        use chrono::Utc;

        // Test that system-like messages (daemon, github, system) are grouped together
        // with blank lines separating them from regular messages.
        //
        // Expected behavior:
        // - Blank line BEFORE system-like messages (unless prev was also system-like)
        // - No blank line between consecutive system-like messages
        // - No blank line after system-like messages when followed by regular messages

        // Helper to count blank lines in rendered output
        fn count_blank_lines(lines: &[Line]) -> usize {
            lines
                .iter()
                .filter(|l| l.spans.iter().all(|s| s.content.is_empty()))
                .count()
        }

        let current_tasks = HashMap::new();

        // Test 1: Regular -> daemon (system-like) should add blank before daemon
        let _regular_msg = Message {
            id: "1".to_string(),
            from: "madison".to_string(),
            content: "working on task".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let daemon_msg = Message {
            id: "2".to_string(),
            from: "daemon".to_string(),
            content: "Called in coworker".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };

        let daemon_lines = render_message(&daemon_msg, 80, Some("madison"), &current_tasks, None);
        assert!(
            count_blank_lines(&daemon_lines) == 1,
            "Should have blank line before daemon message after regular sender"
        );

        // Test 2: daemon -> daemon (both system-like) should NOT add blank
        let daemon_msg2 = Message {
            id: "3".to_string(),
            from: "daemon".to_string(),
            content: "Another daemon event".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let daemon_lines2 = render_message(&daemon_msg2, 80, Some("daemon"), &current_tasks, None);
        assert_eq!(
            count_blank_lines(&daemon_lines2),
            0,
            "Should NOT have blank line between consecutive daemon messages"
        );

        // Test 3: daemon -> github should add blank (github is not system-like)
        let github_msg = Message {
            id: "4".to_string(),
            from: "github".to_string(),
            content: "Check passed".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let github_lines = render_message(&github_msg, 80, Some("daemon"), &current_tasks, None);
        assert_eq!(
            count_blank_lines(&github_lines),
            1,
            "Should have blank line between daemon and github messages"
        );

        // Test 4: daemon -> regular (park) SHOULD add blank line (different sender types)
        let park_msg = Message {
            id: "5".to_string(),
            from: "park".to_string(),
            content: "back to work".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let park_lines = render_message(&park_msg, 80, Some("daemon"), &current_tasks, None);
        assert_eq!(
            count_blank_lines(&park_lines),
            1,
            "Should have blank line when transitioning from system-like to regular"
        );

        // Test 5: Verify is_system_like_sender helper
        assert!(is_system_like_sender("daemon"));
        assert!(!is_system_like_sender("github")); // github is NOT system-like (gets blank lines)
        assert!(is_system_like_sender("system"));
        assert!(is_system_like_sender("DAEMON")); // case insensitive
        assert!(!is_system_like_sender("madison"));
        assert!(!is_system_like_sender("park"));
    }

    #[test]
    fn test_github_messages_have_blank_line_spacing() {
        use chrono::Utc;

        // GitHub messages should have blank lines separating them from other senders,
        // just like coworker messages. They should NOT be grouped with daemon/system.

        fn count_blank_lines(lines: &[Line]) -> usize {
            lines
                .iter()
                .filter(|l| l.spans.iter().all(|s| s.content.is_empty()))
                .count()
        }

        let current_tasks = HashMap::new();

        // Test 1: daemon -> github should have a blank line (github is not system-like)
        let github_msg = Message {
            id: "1".to_string(),
            from: "github".to_string(),
            content: "Check passed".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let github_lines = render_message(&github_msg, 80, Some("daemon"), &current_tasks, None);
        assert_eq!(
            count_blank_lines(&github_lines),
            1,
            "Should have blank line between daemon and github messages"
        );

        // Test 2: github -> daemon should have a blank line
        let daemon_msg = Message {
            id: "2".to_string(),
            from: "daemon".to_string(),
            content: "Called in coworker".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let daemon_lines = render_message(&daemon_msg, 80, Some("github"), &current_tasks, None);
        assert_eq!(
            count_blank_lines(&daemon_lines),
            1,
            "Should have blank line between github and daemon messages"
        );

        // Test 3: coworker -> github should have a blank line
        let github_lines2 = render_message(&github_msg, 80, Some("park"), &current_tasks, None);
        assert_eq!(
            count_blank_lines(&github_lines2),
            1,
            "Should have blank line between coworker and github messages"
        );

        // Test 4: github -> coworker should have a blank line
        let park_msg = Message {
            id: "3".to_string(),
            from: "park".to_string(),
            content: "working".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let park_lines = render_message(&park_msg, 80, Some("github"), &current_tasks, None);
        assert_eq!(
            count_blank_lines(&park_lines),
            1,
            "Should have blank line between github and coworker messages"
        );

        // Test 5: github content should still be DarkGray (visual distinction preserved)
        let github_lines3 = render_message(&github_msg, 80, None, &current_tasks, None);
        // The content line is line index 1 (after sender line)
        // Content spans should be DarkGray
        assert!(
            github_lines3.len() >= 2,
            "Github message should have sender + content lines"
        );
        let content_line = &github_lines3[1];
        // Find the content span (not the timestamp span)
        let has_dark_gray_content = content_line
            .spans
            .iter()
            .any(|s| s.style.fg == Some(Color::DarkGray) && !s.content.contains(':'));
        assert!(
            has_dark_gray_content,
            "Github message content should be DarkGray"
        );
    }

    #[test]
    fn test_sender_line_shows_current_task() {
        use chrono::Utc;

        let msg = Message {
            id: "1".to_string(),
            from: "park".to_string(),
            content: "working on feature".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };

        // Test with a current task
        let mut current_tasks = HashMap::new();
        current_tasks.insert(
            "park".to_string(),
            "Fix chat TUI timestamp formatting".to_string(),
        );

        let lines = render_message(&msg, 80, None, &current_tasks, None);

        // First line should be sender name with task
        let first_line_content: String =
            lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            first_line_content.contains("park"),
            "Should contain sender name"
        );
        assert!(
            first_line_content.contains("Fix chat TUI timestamp formatting"),
            "Should contain current task"
        );
        assert!(
            first_line_content.contains(" - "),
            "Should have separator between name and task"
        );

        // Test without a current task - should just show name
        let empty_tasks = HashMap::new();
        let lines_no_task = render_message(&msg, 80, None, &empty_tasks, None);
        let first_line_no_task: String = lines_no_task[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            first_line_no_task.contains("park"),
            "Should contain sender name"
        );
        assert!(
            !first_line_no_task.contains(" - "),
            "Should NOT have task separator when no task"
        );
    }

    #[test]
    fn test_sender_line_truncates_long_task() {
        use chrono::Utc;

        let msg = Message {
            id: "1".to_string(),
            from: "park".to_string(),
            content: "test".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };

        let mut current_tasks = HashMap::new();
        current_tasks.insert(
            "park".to_string(),
            "This is a very long task description that should be truncated".to_string(),
        );

        // Narrow width should truncate the task
        let lines = render_message(&msg, 30, None, &current_tasks, None);
        let first_line_content: String =
            lines[0].spans.iter().map(|s| s.content.as_ref()).collect();

        // Should be truncated (contains ellipsis)
        assert!(
            first_line_content.contains("…") || first_line_content.len() <= 30,
            "Long task should be truncated"
        );
    }

    #[test]
    fn test_sender_line_case_insensitive_lookup() {
        use chrono::Utc;

        // Message from "Park" (capitalized)
        let msg = Message {
            id: "1".to_string(),
            from: "Park".to_string(),
            content: "test".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };

        // Task stored with lowercase "park"
        let mut current_tasks = HashMap::new();
        current_tasks.insert("park".to_string(), "Fix something".to_string());

        let lines = render_message(&msg, 80, None, &current_tasks, None);
        let first_line_content: String =
            lines[0].spans.iter().map(|s| s.content.as_ref()).collect();

        // Should find the task despite case difference
        assert!(
            first_line_content.contains("Fix something"),
            "Should find task with case-insensitive lookup"
        );
    }

    // --- Tests for input box expansion / wrapping ---

    #[test]
    fn test_usage_color_green() {
        assert_eq!(usage_color(0.0), Color::Green);
        assert_eq!(usage_color(59.9), Color::Green);
    }

    #[test]
    fn test_usage_color_yellow() {
        assert_eq!(usage_color(60.0), Color::Yellow);
        assert_eq!(usage_color(79.9), Color::Yellow);
    }

    #[test]
    fn test_usage_color_red() {
        assert_eq!(usage_color(80.0), Color::Red);
        assert_eq!(usage_color(100.0), Color::Red);
    }

    #[test]
    fn test_render_usage_line_produces_spans() {
        let resets_at = Utc::now() + chrono::Duration::hours(3);
        let line = render_usage_line("Session", 50.0, Some(&resets_at), true);
        // Should have 6 spans: label, filled bar, empty bar, pct, estimate, reset
        assert_eq!(line.spans.len(), 6);
    }

    #[test]
    fn test_render_usage_line_bar_proportions() {
        let resets_at = Utc::now();
        // At 50%, should have 10 filled (out of 20) and 10 empty
        let line = render_usage_line("Test   ", 50.0, Some(&resets_at), true);
        let filled_content = &line.spans[1].content;
        let empty_content = &line.spans[2].content;
        assert_eq!(filled_content.chars().count(), 10);
        assert_eq!(empty_content.chars().count(), 10);
    }

    #[test]
    fn test_render_usage_line_zero_percent() {
        let resets_at = Utc::now();
        let line = render_usage_line("Test   ", 0.0, Some(&resets_at), true);
        let filled_content = &line.spans[1].content;
        let empty_content = &line.spans[2].content;
        assert_eq!(filled_content.chars().count(), 0);
        assert_eq!(empty_content.chars().count(), 20);
    }

    #[test]
    fn test_render_usage_line_full_percent() {
        let resets_at = Utc::now();
        let line = render_usage_line("Test   ", 100.0, Some(&resets_at), true);
        let filled_content = &line.spans[1].content;
        let empty_content = &line.spans[2].content;
        assert_eq!(filled_content.chars().count(), 20);
        assert_eq!(empty_content.chars().count(), 0);
    }

    #[test]
    fn test_render_usage_line_none_resets_at() {
        let line = render_usage_line("Session", 0.0, None, true);
        // Should still produce 6 spans with em-dash placeholders for estimate and reset
        assert_eq!(line.spans.len(), 6);
        let estimate = &line.spans[4].content;
        let reset = &line.spans[5].content;
        assert!(
            estimate.contains('—'),
            "Estimate should contain em-dash when resets_at is None: {:?}",
            estimate
        );
        assert!(
            reset.contains('—'),
            "Reset should contain em-dash when resets_at is None: {:?}",
            reset
        );
    }

    #[test]
    fn test_format_reset_time_past_returns_now() {
        let past = Utc::now() - chrono::Duration::hours(1);
        assert_eq!(format_reset_time(&past, true), "now");
        assert_eq!(format_reset_time(&past, false), "now");
    }

    #[test]
    fn test_format_reset_time_future_returns_formatted() {
        let future = Utc::now() + chrono::Duration::hours(2);
        let result = format_reset_time(&future, true);
        // Should be a time format like "4:59pm", not "now"
        assert_ne!(result, "now");
        assert!(
            result.contains(':'),
            "Session format should contain colon: {}",
            result
        );

        let result = format_reset_time(&future, false);
        // Should be a date format like "Feb 11", not "now"
        assert_ne!(result, "now");
    }

    #[test]
    fn test_estimate_time_to_full_zero_utilization() {
        let resets_at = Utc::now() + chrono::Duration::hours(3);
        assert_eq!(estimate_time_to_full(0.0, &resets_at, true), "—");
    }

    #[test]
    fn test_estimate_time_to_full_already_full() {
        let resets_at = Utc::now() + chrono::Duration::hours(3);
        assert_eq!(estimate_time_to_full(100.0, &resets_at, true), "—");
    }

    #[test]
    fn test_estimate_time_to_full_past_reset() {
        let resets_at = Utc::now() - chrono::Duration::hours(1);
        assert_eq!(estimate_time_to_full(50.0, &resets_at, true), "—");
    }

    #[test]
    fn test_estimate_time_to_full_session_midpoint() {
        // 50% used, 2.5h remaining in 5h window → consumed in 2.5h elapsed
        // Rate = 50/9000 per sec → time to 100% = 50 / (50/9000) = 9000s = 2.5h
        let resets_at = Utc::now() + chrono::Duration::minutes(150); // 2.5h left
        let result = estimate_time_to_full(50.0, &resets_at, true);
        assert!(result.contains("left"), "Expected 'left' in: {result}");
        assert!(result.starts_with('~'), "Expected '~' prefix in: {result}");
    }

    #[test]
    fn test_estimate_time_to_full_high_utilization() {
        // 90% used in a 5h session, 30min remaining
        // Elapsed = 4.5h = 16200s, rate = 90/16200
        // Time to 100% = 10 / (90/16200) = 1800s = 30min
        let resets_at = Utc::now() + chrono::Duration::minutes(30);
        let result = estimate_time_to_full(90.0, &resets_at, true);
        assert!(result.contains("left"), "Expected 'left' in: {result}");
    }

    #[test]
    fn test_format_duration_estimate_minutes() {
        assert_eq!(format_duration_estimate(1800.0), "~30m left");
    }

    #[test]
    fn test_format_duration_estimate_hours() {
        assert_eq!(format_duration_estimate(7200.0), "~2h left");
    }

    #[test]
    fn test_format_duration_estimate_hours_and_minutes() {
        assert_eq!(format_duration_estimate(5400.0), "~1h30m left");
    }

    #[test]
    fn test_format_duration_estimate_less_than_one_minute() {
        assert_eq!(format_duration_estimate(10.0), "~<1m left");
    }

    // --- Mermaid placeholder rendering tests ---

    /// Helper to create a dummy RenderedDiagram for cache injection
    fn dummy_rendered_diagram() -> mermaid::RenderedDiagram {
        mermaid::RenderedDiagram {
            ascii_art: "┌───────┐\n│ Hello │\n└───────┘".to_string(),
            svg: "<svg>test</svg>".to_string(),
        }
    }

    /// Helper to create a test Message
    fn test_message(content: &str) -> Message {
        Message {
            id: "1".to_string(),
            from: "park".to_string(),
            content: content.to_string(),
            timestamp: chrono::Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        }
    }

    #[test]
    fn test_cached_diagram_shows_inline_ascii_art() {
        let source = "graph TD\n  A-->B";
        let mut cache = MermaidCache::new();
        cache.insert_cached(source, dummy_rendered_diagram());

        let msg = test_message("ignored"); // content unused; segments drive rendering
        let segments = vec![ContentSegment::Mermaid(source.to_string())];
        let current_tasks = HashMap::new();
        let mut lines = Vec::new();
        let mut diagram_sources = Vec::new();
        let mut mermaid_to_render = Vec::new();

        render_message_with_mermaid(
            &msg,
            &segments,
            80,
            None,
            &current_tasks,
            None,
            &cache,
            &mut lines,
            &mut diagram_sources,
            &mut mermaid_to_render,
        );

        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n");

        // Should contain top separator with diagram type
        assert!(
            all_text.contains("--- graph ---"),
            "Expected top separator, got: {}",
            all_text
        );
        // Should contain ASCII art content
        assert!(
            all_text.contains("Hello"),
            "Expected ASCII art content, got: {}",
            all_text
        );
        // Should contain bottom separator with browser hint
        assert!(
            all_text.contains("--- press 1 to open in browser ---"),
            "Expected bottom separator with hint, got: {}",
            all_text
        );

        // Bottom separator should be styled DarkGray
        let bottom_line = lines.last().unwrap();
        assert_eq!(bottom_line.spans[0].style.fg, Some(Color::DarkGray));

        // ASCII art lines should be Cyan
        // Find a line with ASCII art (contains box drawing chars)
        let ascii_line = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.contains("Hello")));
        assert!(ascii_line.is_some(), "Should have an ASCII art line");
        assert_eq!(ascii_line.unwrap().spans[0].style.fg, Some(Color::Cyan));
    }

    #[test]
    fn test_pending_diagram_shows_rendering_placeholder() {
        let source = "sequenceDiagram\n  A->>B: hello";
        let mut cache = MermaidCache::new();
        cache.insert_pending(source);

        let msg = test_message("ignored");
        let segments = vec![ContentSegment::Mermaid(source.to_string())];
        let current_tasks = HashMap::new();
        let mut lines = Vec::new();
        let mut diagram_sources = Vec::new();
        let mut mermaid_to_render = Vec::new();

        render_message_with_mermaid(
            &msg,
            &segments,
            80,
            None,
            &current_tasks,
            None,
            &cache,
            &mut lines,
            &mut diagram_sources,
            &mut mermaid_to_render,
        );

        let placeholder_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            placeholder_text.contains("[rendering sequenceDiagram...]"),
            "Expected rendering placeholder, got: {}",
            placeholder_text
        );

        // Placeholder should be styled DarkGray + Italic
        let placeholder_line = lines.last().unwrap();
        assert_eq!(placeholder_line.spans[0].style.fg, Some(Color::DarkGray));
        assert!(
            placeholder_line.spans[0]
                .style
                .add_modifier
                .contains(Modifier::ITALIC)
        );

        // Pending diagrams should NOT be tracked in diagram_sources
        assert!(diagram_sources.is_empty());
    }

    #[test]
    fn test_unqueued_diagram_shows_plain_placeholder_and_queues() {
        let source = "flowchart LR\n  A-->B";
        let cache = MermaidCache::new(); // empty cache, nothing pending

        let msg = test_message("ignored");
        let segments = vec![ContentSegment::Mermaid(source.to_string())];
        let current_tasks = HashMap::new();
        let mut lines = Vec::new();
        let mut diagram_sources = Vec::new();
        let mut mermaid_to_render = Vec::new();

        render_message_with_mermaid(
            &msg,
            &segments,
            80,
            None,
            &current_tasks,
            None,
            &cache,
            &mut lines,
            &mut diagram_sources,
            &mut mermaid_to_render,
        );

        let placeholder_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            placeholder_text.contains("[flowchart diagram]"),
            "Expected plain placeholder, got: {}",
            placeholder_text
        );

        // Should queue the source for rendering
        assert_eq!(mermaid_to_render.len(), 1);
        assert_eq!(mermaid_to_render[0], source);

        // Unqueued diagrams should NOT be tracked in diagram_sources
        assert!(diagram_sources.is_empty());
    }

    #[test]
    fn test_diagram_numbering_sequential() {
        let sources: Vec<String> = (0..3)
            .map(|i| format!("graph TD\n  A{}-->B{}", i, i))
            .collect();
        let mut cache = MermaidCache::new();
        for s in &sources {
            cache.insert_cached(s, dummy_rendered_diagram());
        }

        let msg = test_message("ignored");
        let segments: Vec<ContentSegment> = sources
            .iter()
            .map(|s| ContentSegment::Mermaid(s.clone()))
            .collect();
        let current_tasks = HashMap::new();
        let mut lines = Vec::new();
        let mut diagram_sources = Vec::new();
        let mut mermaid_to_render = Vec::new();

        render_message_with_mermaid(
            &msg,
            &segments,
            80,
            None,
            &current_tasks,
            None,
            &cache,
            &mut lines,
            &mut diagram_sources,
            &mut mermaid_to_render,
        );

        // Should have 3 diagram sources tracked
        assert_eq!(diagram_sources.len(), 3);

        // Check sequential numbering in bottom separators
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all_text.contains("--- press 1 to open in browser ---"));
        assert!(all_text.contains("--- press 2 to open in browser ---"));
        assert!(all_text.contains("--- press 3 to open in browser ---"));
    }

    #[test]
    fn test_diagram_cap_at_9_shortcuts() {
        let sources: Vec<String> = (0..11)
            .map(|i| format!("graph TD\n  X{}-->Y{}", i, i))
            .collect();
        let mut cache = MermaidCache::new();
        for s in &sources {
            cache.insert_cached(s, dummy_rendered_diagram());
        }

        let msg = test_message("ignored");
        let segments: Vec<ContentSegment> = sources
            .iter()
            .map(|s| ContentSegment::Mermaid(s.clone()))
            .collect();
        let current_tasks = HashMap::new();
        let mut lines = Vec::new();
        let mut diagram_sources = Vec::new();
        let mut mermaid_to_render = Vec::new();

        render_message_with_mermaid(
            &msg,
            &segments,
            80,
            None,
            &current_tasks,
            None,
            &cache,
            &mut lines,
            &mut diagram_sources,
            &mut mermaid_to_render,
        );

        // All 11 diagrams should be tracked in diagram_sources
        assert_eq!(diagram_sources.len(), 11);

        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n");

        // Diagrams 1-9 should have numbered browser hints
        for i in 1..=9 {
            assert!(
                all_text.contains(&format!("--- press {} to open in browser ---", i)),
                "Diagram {} should have a numbered browser hint",
                i
            );
        }

        // Diagrams 10-11 should have unnumbered separators
        let numbered_count = all_text.matches("to open in browser ---").count();
        assert_eq!(
            numbered_count, 9,
            "Only 9 diagrams should have browser hints"
        );
    }

    #[test]
    fn test_mixed_text_and_mermaid_segments() {
        let source = "graph TD\n  A-->B";
        let mut cache = MermaidCache::new();
        cache.insert_cached(source, dummy_rendered_diagram());

        let msg = test_message("ignored");
        let segments = vec![
            ContentSegment::Text("Before the diagram".to_string()),
            ContentSegment::Mermaid(source.to_string()),
            ContentSegment::Text("After the diagram".to_string()),
        ];
        let current_tasks = HashMap::new();
        let mut lines = Vec::new();
        let mut diagram_sources = Vec::new();
        let mut mermaid_to_render = Vec::new();

        render_message_with_mermaid(
            &msg,
            &segments,
            80,
            None,
            &current_tasks,
            None,
            &cache,
            &mut lines,
            &mut diagram_sources,
            &mut mermaid_to_render,
        );

        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(all_text.contains("Before the diagram"));
        assert!(all_text.contains("--- graph ---"));
        assert!(all_text.contains("--- press 1 to open in browser ---"));
        assert!(all_text.contains("After the diagram"));
    }

    #[test]
    fn test_diagram_type_extracted_from_first_line() {
        // Different diagram types should show their type in the placeholder
        let test_cases = vec![
            ("sequenceDiagram\n  A->>B: hello", "sequenceDiagram"),
            ("classDiagram\n  class Animal", "classDiagram"),
            ("flowchart LR\n  A-->B", "flowchart"),
            ("pie title Pets\n  \"Dogs\": 60", "pie"),
        ];

        for (source, expected_type) in test_cases {
            let mut cache = MermaidCache::new();
            cache.insert_cached(source, dummy_rendered_diagram());

            let msg = test_message("ignored");
            let segments = vec![ContentSegment::Mermaid(source.to_string())];
            let current_tasks = HashMap::new();
            let mut lines = Vec::new();
            let mut diagram_sources = Vec::new();
            let mut mermaid_to_render = Vec::new();

            render_message_with_mermaid(
                &msg,
                &segments,
                80,
                None,
                &current_tasks,
                None,
                &cache,
                &mut lines,
                &mut diagram_sources,
                &mut mermaid_to_render,
            );

            let all_text: String = lines
                .iter()
                .flat_map(|l| l.spans.iter())
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>()
                .join("");
            assert!(
                all_text.contains(&format!("--- {} ---", expected_type)),
                "Expected diagram type '{}' in separator, got: {}",
                expected_type,
                all_text
            );
        }
    }

    #[test]
    fn test_action_message_mermaid_placeholder_extra_indent() {
        // Action messages have a "* " prefix (2 extra chars) which increases
        // the indent_width from TIMESTAMP_GUTTER_WIDTH (7) to 9.
        // Mermaid diagram lines (separators + ASCII art) should use this wider indent.
        let source = "graph TD\n  A-->B";
        let mut cache = MermaidCache::new();
        cache.insert_cached(source, dummy_rendered_diagram());

        let msg = Message {
            id: "1".to_string(),
            from: "park".to_string(),
            content: "ignored".to_string(),
            timestamp: chrono::Utc::now(),
            message_type: MessageType::Action,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let segments = vec![ContentSegment::Mermaid(source.to_string())];
        let current_tasks = HashMap::new();
        let mut lines = Vec::new();
        let mut diagram_sources = Vec::new();
        let mut mermaid_to_render = Vec::new();

        render_message_with_mermaid(
            &msg,
            &segments,
            80,
            None,
            &current_tasks,
            None,
            &cache,
            &mut lines,
            &mut diagram_sources,
            &mut mermaid_to_render,
        );

        // Action messages: indent_width = TIMESTAMP_GUTTER_WIDTH + 2 = 9
        let action_indent = " ".repeat(TIMESTAMP_GUTTER_WIDTH + 2);

        // All diagram lines (top separator, ASCII art, bottom separator) should
        // use the wider action message indent.
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            all_text.contains(&format!("{}--- graph ---", action_indent)),
            "Action message top separator should have {} chars indent, got:\n{}",
            TIMESTAMP_GUTTER_WIDTH + 2,
            all_text
        );
        assert!(
            all_text.contains(&format!(
                "{}--- press 1 to open in browser ---",
                action_indent
            )),
            "Action message bottom separator should have {} chars indent",
            TIMESTAMP_GUTTER_WIDTH + 2,
        );

        // Verify every ASCII art line (Cyan spans) has the correct indent
        for line in &lines {
            for span in &line.spans {
                if span.style.fg == Some(Color::Cyan) {
                    let text = span.content.as_ref();
                    assert!(
                        text.starts_with(&action_indent),
                        "ASCII art line should have {} chars indent, got: {:?}",
                        TIMESTAMP_GUTTER_WIDTH + 2,
                        text
                    );
                }
            }
        }

        // Compare with normal text message indent
        let normal_msg = test_message("ignored");
        let mut normal_lines = Vec::new();
        let mut normal_diagram_sources = Vec::new();
        let mut normal_mermaid_to_render = Vec::new();

        render_message_with_mermaid(
            &normal_msg,
            &segments,
            80,
            None,
            &current_tasks,
            None,
            &cache,
            &mut normal_lines,
            &mut normal_diagram_sources,
            &mut normal_mermaid_to_render,
        );

        let normal_indent = " ".repeat(TIMESTAMP_GUTTER_WIDTH);
        let normal_text: String = normal_lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n");

        // Normal message should use the narrower indent
        assert!(
            normal_text.contains(&format!("{}--- graph ---", normal_indent)),
            "Normal message top separator should have {} chars indent",
            TIMESTAMP_GUTTER_WIDTH,
        );
        // Verify every normal ASCII art line has the narrower indent
        for line in &normal_lines {
            for span in &line.spans {
                if span.style.fg == Some(Color::Cyan) {
                    let text = span.content.as_ref();
                    assert!(
                        text.starts_with(&normal_indent),
                        "Normal ASCII art line should have {} chars indent, got: {:?}",
                        TIMESTAMP_GUTTER_WIDTH,
                        text
                    );
                    // Should NOT have the wider action indent
                    assert!(
                        !text.starts_with(&action_indent),
                        "Normal ASCII art line should NOT have action indent, got: {:?}",
                        text
                    );
                }
            }
        }
    }

    #[test]
    fn test_narrow_terminal_does_not_panic_on_unicode_ascii_art() {
        // Box-drawing characters (┌, │, └, ─) are 3 bytes in UTF-8.
        // Byte-indexing truncation can land mid-character and panic.
        let source = "graph TD\n  A-->B";
        let mut cache = MermaidCache::new();
        cache.insert_cached(
            source,
            mermaid::RenderedDiagram {
                ascii_art: "┌──────────────────┐\n│ A long box label │\n└──────────────────┘"
                    .to_string(),
                svg: "<svg>test</svg>".to_string(),
            },
        );

        let msg = test_message("ignored");
        let segments = vec![ContentSegment::Mermaid(source.to_string())];
        let current_tasks = HashMap::new();

        // Use a very narrow width that will force truncation mid-character
        for width in [15, 18, 20, 25] {
            let mut lines = Vec::new();
            let mut diagram_sources = Vec::new();
            let mut mermaid_to_render = Vec::new();

            render_message_with_mermaid(
                &msg,
                &segments,
                width,
                None,
                &current_tasks,
                None,
                &cache,
                &mut lines,
                &mut diagram_sources,
                &mut mermaid_to_render,
            );

            // Should not panic — just verify we got some output
            assert!(!lines.is_empty(), "Should produce lines at width {}", width);
        }
    }

    #[test]
    fn test_render_crosspost_message() {
        // Create a cross-posted message with source_channel set
        let mut msg = test_message("The tower::Layer stack composes auth providers independently.");
        msg.source_channel = Some("auth-refactor".to_string());

        let current_tasks = HashMap::new();
        let width = 80;

        let lines = render_message(&msg, width, None, &current_tasks, None);

        // Should have at least 2 lines: sender line + content line
        assert!(
            lines.len() >= 2,
            "Expected at least 2 lines, got {}",
            lines.len()
        );

        // Content line should contain the ★ prefix (first span after timestamp)
        let content_line = &lines[1];
        let spans = &content_line.spans;

        // Find the ★ span (should be the second span after timestamp)
        let star_span = spans.iter().find(|s| s.content.contains('★'));
        assert!(
            star_span.is_some(),
            "Expected to find ★ prefix in content line"
        );

        // Find the channel attribution span
        let channel_span = spans.iter().find(|s| s.content.contains("#auth-refactor"));
        assert!(
            channel_span.is_some(),
            "Expected to find #auth-refactor channel attribution"
        );
    }

    #[test]
    fn test_render_crosspost_utf8_channel_name() {
        // Verify that multi-byte UTF-8 channel names produce correct continuation alignment.
        // "design-café" has a multi-byte char (é = 2 bytes). Using .len() would overcount,
        // causing misaligned continuation lines.
        let long_content = "First line of content that is long enough to wrap onto a second continuation line for testing alignment";
        let mut msg = test_message(long_content);
        msg.source_channel = Some("design-café".to_string());

        let current_tasks = HashMap::new();
        let width = 60; // Narrow enough to force wrapping

        let lines = render_message(&msg, width, None, &current_tasks, None);

        // Should have sender line + at least 2 content lines (first + continuation)
        assert!(
            lines.len() >= 3,
            "Expected at least 3 lines (sender + 2 content), got {}",
            lines.len()
        );

        // The continuation line (index 2) should start with whitespace indent.
        // With correct .chars().count(), the indent is:
        //   TIMESTAMP_GUTTER_WIDTH (7) + prefix_len (2+6+11+3 = 22) = 29 spaces
        // With buggy .len(), it would be 7 + (2+6+12+3) = 30 spaces (é is 2 bytes)
        let continuation_line = &lines[2];
        let first_span_content = &continuation_line.spans[0].content;

        // "design-café" has 11 chars, so prefix_len = 2+6+11+3 = 22
        let expected_indent = 7 + 22; // TIMESTAMP_GUTTER_WIDTH + prefix_len
        assert_eq!(
            first_span_content.len(),
            expected_indent,
            "Continuation indent should be {} spaces, got {} (possible byte/char mismatch)",
            expected_indent,
            first_span_content.len()
        );
    }

    #[test]
    fn test_render_crosspost_with_mermaid() {
        // Create a cross-posted message with mermaid content
        let source = "graph TD\n  A-->B";
        let mut msg = test_message("ignored");
        msg.source_channel = Some("design".to_string());

        let segments = vec![
            ContentSegment::Text("Architecture insight: ".to_string()),
            ContentSegment::Mermaid(source.to_string()),
        ];

        let mut cache = MermaidCache::new();
        cache.insert_cached(source, dummy_rendered_diagram());

        let current_tasks = HashMap::new();
        let width = 80;

        let mut lines = Vec::new();
        let mut diagram_sources = Vec::new();
        let mut mermaid_to_render = Vec::new();

        render_message_with_mermaid(
            &msg,
            &segments,
            width,
            None,
            &current_tasks,
            None,
            &cache,
            &mut lines,
            &mut diagram_sources,
            &mut mermaid_to_render,
        );

        // Should produce output
        assert!(
            !lines.is_empty(),
            "Expected at least some lines for cross-posted mermaid"
        );

        // Content line should contain the ★ prefix
        let has_star = lines
            .iter()
            .any(|line| line.spans.iter().any(|s| s.content.contains('★')));
        assert!(
            has_star,
            "Expected to find ★ prefix in cross-posted mermaid message"
        );
    }

    // --- Tests for input box height calculation ---

    #[test]
    fn test_calculate_input_bar_height_empty_text() {
        // Empty text should use minimum height: 1 content line + 2 borders = 3
        let height = calculate_input_bar_height("", 80);
        assert_eq!(height, 3);
    }

    #[test]
    fn test_calculate_input_bar_height_short_text() {
        // Short text that fits in one line: 1 content line + 2 borders = 3
        let height = calculate_input_bar_height("Hello", 80);
        assert_eq!(height, 3);
    }

    #[test]
    fn test_calculate_input_bar_height_wraps_long_text() {
        // Text longer than available width should wrap
        // Available width = 80 - 2 (borders) - 3 (prompt "› ") - 1 (cursor "█") = 74
        let long_text = "a".repeat(150); // 150/74 = 2.02, wraps to 3 lines
        let height = calculate_input_bar_height(&long_text, 80);
        assert_eq!(
            height, 5,
            "150 chars should wrap to 3 lines: 3 + 2 borders = 5"
        );
    }

    #[test]
    fn test_calculate_input_bar_height_max_lines() {
        // Very long text should be clamped at max 6 content lines
        let very_long_text = "a".repeat(1000);
        let height = calculate_input_bar_height(&very_long_text, 80);
        assert_eq!(height, 8, "Max 6 content lines + 2 borders = 8");
    }

    #[test]
    fn test_calculate_input_bar_height_narrow_terminal() {
        // Narrow terminal should still produce valid height
        let height = calculate_input_bar_height("Hello world", 10);
        assert!(height >= 3, "Minimum height should be 3");
        assert!(height <= 8, "Maximum height should be 8");
    }

    #[test]
    fn test_calculate_input_bar_height_zero_width() {
        // Edge case: zero width should return minimum height
        let height = calculate_input_bar_height("test", 0);
        assert_eq!(height, 3, "Zero width should return minimum height");
    }

    #[test]
    fn test_calculate_input_bar_height_with_newlines() {
        // Text with explicit newlines
        let text = "Line 1\nLine 2\nLine 3";
        let height = calculate_input_bar_height(text, 80);
        // wrap_content splits on '\n' first, giving 3 lines + 2 borders = 5
        assert_eq!(height, 5, "3 content lines + 2 border lines = 5");
    }

    // Helper to build a KanbanTask for indentation tests
    use super::super::app::{KanbanTask, TaskStatus};

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
        // A blocks B blocks C → B indented 1, C indented 2
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
        // A blocks B, B blocks A — cycle should not panic, both get 0 or 1
        let tasks = [make_task("A", vec!["B"]), make_task("B", vec!["A"])];
        let task_refs: Vec<&KanbanTask> = tasks.iter().collect();
        let indent = compute_task_indentation(&task_refs);

        // With cycle detection, processed guard returns 0 for the cycle-back reference.
        // A is processed first: looks up B → B looks up A → A is in processed → returns 0 → B = 1.
        // Then A continues: B is now cached as 1, so A = B_level + 1 = 2? No —
        // Let's trace: A processed first, blocked_by=["B"].
        // A enters processed set. Recurse into B.
        // B enters processed set. B blocked_by=["A"]. A is in processed → return 0.
        // B = 0 + 1 = 1. B cached.
        // Back to A: blocker B returned 1. A = 1 + 1 = 2. A cached.
        // Actually: wait, re-read the code. A calls into B before A itself is cached.
        // The cycle guard returns 0 for A (when B tries to recurse into A).
        // So B = 0 + 1 = 1, A = 1 + 1 = 2.
        // This is fine — the key property is no infinite loop.
        assert!(indent.contains_key("A"));
        assert!(indent.contains_key("B"));
        // No panic/infinite loop is the main assertion. Values are deterministic:
        assert_eq!(indent.get("B"), Some(&1));
        assert_eq!(indent.get("A"), Some(&2));
    }

    #[test]
    fn test_indentation_missing_blocker() {
        // Task blocked by a task not in the list → treated as resolved, no indentation
        let tasks = [make_task("A", vec!["Z"])];
        let task_refs: Vec<&KanbanTask> = tasks.iter().collect();
        let indent = compute_task_indentation(&task_refs);

        assert_eq!(indent.get("A"), Some(&0));
    }

    #[test]
    fn test_indentation_diamond_dependency() {
        // Diamond: A blocks B, A blocks C, both B and C block D
        //   A
        //  / \
        // B   C
        //  \ /
        //   D
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
        // D is blocked by B (first in list, present in task_map) → indent = B_level + 1 = 2
        assert_eq!(indent.get("D"), Some(&2));
    }

    #[test]
    fn test_indentation_partial_blockers() {
        // B is blocked by both A (in list) and Z (not in list)
        // First unresolved dependency in task_map is A → indent relative to A
        let tasks = [make_task("A", vec![]), make_task("B", vec!["Z", "A"])];
        let task_refs: Vec<&KanbanTask> = tasks.iter().collect();
        let indent = compute_task_indentation(&task_refs);

        assert_eq!(indent.get("A"), Some(&0));
        // Z is not in the list, so first matching blocker is A
        assert_eq!(indent.get("B"), Some(&1));
    }

    #[test]
    fn test_indentation_three_node_cycle() {
        // A → B → C → A (triangular cycle)
        let tasks = [
            make_task("A", vec!["C"]),
            make_task("B", vec!["A"]),
            make_task("C", vec!["B"]),
        ];
        let task_refs: Vec<&KanbanTask> = tasks.iter().collect();
        let indent = compute_task_indentation(&task_refs);

        // Should not panic or loop. All tasks should have entries.
        assert!(indent.contains_key("A"));
        assert!(indent.contains_key("B"));
        assert!(indent.contains_key("C"));
    }
}
