//! Rendering logic for the Midtown sidebar plugin.
//!
//! Uses `println!()` for output — Zellij captures plugin stdout as the pane
//! content. Text is styled using ANSI escape codes since Zellij renders plugin
//! panes as terminal output.

use crate::state::{PluginState, Section, View};
use midtown_types::{CoworkerSummary, TaskSummary};

// ANSI color codes
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const REVERSE: &str = "\x1b[7m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BLUE: &str = "\x1b[34m";
const MAGENTA: &str = "\x1b[35m";
const CYAN: &str = "\x1b[36m";
const RED: &str = "\x1b[31m";
const WHITE: &str = "\x1b[37m";

/// Main render entry point.
pub fn render(state: &PluginState, rows: usize, cols: usize) {
    match &state.view {
        View::Main => render_main(state, rows, cols),
        View::CoworkerStream { name } => render_coworker_stream(state, name, rows, cols),
    }
}

/// Render the main dashboard view.
fn render_main(state: &PluginState, rows: usize, cols: usize) {
    // Header
    print_header("Midtown", cols, state.daemon_version.as_deref());

    if let Some(ref error) = state.error {
        println!("{RED}  ⚠ {}{RESET}", error);
        println!();
    }

    if !state.connected {
        println!("{DIM}  Connecting to daemon...{RESET}");
        return;
    }

    // Lead nudges (if any)
    if !state.lead_nudges.is_empty() {
        println!("{YELLOW}{BOLD}  ── Nudges ──{RESET}");
        for nudge in state.lead_nudges.iter().take(3) {
            let truncated = truncate(nudge, cols.saturating_sub(4));
            println!("{YELLOW}  {}{RESET}", truncated);
        }
        if state.lead_nudges.len() > 3 {
            println!("{DIM}  +{} more{RESET}", state.lead_nudges.len() - 3);
        }
        println!();
    }

    // Tasks section
    let task_header = format!("Tasks ({})", state.tasks.len());
    println!("{BOLD}{CYAN}  ── {} ──{RESET}", task_header);

    if state.tasks.is_empty() {
        println!("{DIM}  No tasks{RESET}");
    } else {
        let mut global_idx = 0;
        for task in &state.tasks {
            let selected = state.section == Section::Tasks && state.task_index == global_idx;
            render_task(task, selected, cols);
            global_idx += 1;
        }
    }
    println!();

    // Coworkers section
    let coworker_header = format!("Coworkers ({})", state.coworkers.len());
    println!("{BOLD}{CYAN}  ── {} ──{RESET}", coworker_header);

    if state.coworkers.is_empty() {
        println!("{DIM}  No coworkers{RESET}");
    } else {
        for (i, cw) in state.coworkers.iter().enumerate() {
            let selected = state.section == Section::Coworkers && state.coworker_index == i;
            render_coworker(cw, selected, cols);
        }
    }
    println!();

    // Footer with help
    render_footer(state, rows, cols);
}

/// Render a single task line.
fn render_task(task: &TaskSummary, selected: bool, cols: usize) {
    let status_icon = match task.status.as_str() {
        "pending" => format!("{YELLOW}○{RESET}"),
        "in_progress" => format!("{GREEN}●{RESET}"),
        "completed" => format!("{DIM}✓{RESET}"),
        _ => format!("{DIM}?{RESET}"),
    };

    let id_display = if task.id.starts_with('!') {
        task.id.clone()
    } else {
        format!("!{}", task.id)
    };

    let owner_str = task
        .owner
        .as_ref()
        .map(|o| format!(" {DIM}[{}]{RESET}", o))
        .unwrap_or_default();

    let pr_str = task
        .pr_number
        .map(|n| format!(" {MAGENTA}PR#{}{RESET}", n))
        .unwrap_or_default();

    let max_subject_len = cols
        .saturating_sub(6) // indent + icon
        .saturating_sub(id_display.len() + 1)
        .saturating_sub(if task.owner.is_some() { 12 } else { 0 })
        .saturating_sub(if task.pr_number.is_some() { 8 } else { 0 });

    let subject = truncate(&task.subject, max_subject_len);

    if selected {
        println!(
            "{REVERSE}  {} {BLUE}{}{RESET}{REVERSE} {}{}{}{RESET}",
            status_icon, id_display, subject, owner_str, pr_str
        );
    } else {
        println!(
            "  {} {BLUE}{}{RESET} {}{}{}",
            status_icon, id_display, subject, owner_str, pr_str
        );
    }
}

/// Render a single coworker line.
fn render_coworker(cw: &CoworkerSummary, selected: bool, cols: usize) {
    let status_icon = coworker_status_icon(cw);
    let status_color = coworker_status_color(cw);

    let task_str = cw
        .current_task
        .as_ref()
        .map(|t| {
            let max_len = cols.saturating_sub(cw.name.len() + 16);
            format!(" {}", truncate(t, max_len))
        })
        .unwrap_or_default();

    if selected {
        println!(
            "{REVERSE}  {} {}{}{RESET}{REVERSE}{}{RESET}",
            status_icon, status_color, cw.name, task_str
        );
    } else {
        println!(
            "  {} {}{}{RESET}{}",
            status_icon, status_color, cw.name, task_str
        );
    }
}

/// Get a status icon for a coworker.
fn coworker_status_icon(cw: &CoworkerSummary) -> String {
    if cw.has_usage_limit {
        format!("{RED}⚠{RESET}")
    } else if cw.has_api_error {
        format!("{RED}✗{RESET}")
    } else if !cw.is_alive {
        format!("{RED}✗{RESET}")
    } else {
        match cw.status.as_str() {
            "idle" => format!("{DIM}◌{RESET}"),
            "developing" => format!("{GREEN}⚡{RESET}"),
            "testing" => format!("{YELLOW}⧗{RESET}"),
            "pull_request" => format!("{MAGENTA}↗{RESET}"),
            "reviewing" => format!("{CYAN}👁{RESET}"),
            "claiming" => format!("{BLUE}…{RESET}"),
            "debugging" => format!("{RED}🔍{RESET}"),
            _ => format!("{GREEN}●{RESET}"),
        }
    }
}

/// Get a status color for a coworker name.
fn coworker_status_color(cw: &CoworkerSummary) -> &'static str {
    if !cw.is_alive || cw.has_api_error || cw.has_usage_limit {
        RED
    } else {
        match cw.status.as_str() {
            "idle" => DIM,
            "developing" => GREEN,
            "testing" => YELLOW,
            "pull_request" => MAGENTA,
            "reviewing" => CYAN,
            _ => WHITE,
        }
    }
}

/// Render the coworker stream view.
fn render_coworker_stream(state: &PluginState, name: &str, rows: usize, cols: usize) {
    // Header
    let header = format!("{} stream", name);
    print_header(&header, cols, None);
    println!("{DIM}  Press ESC/q to go back{RESET}");
    println!();

    if state.stream_events.is_empty() {
        println!("{DIM}  No recent events{RESET}");
        return;
    }

    // Available rows for events (subtract header lines)
    let available_rows = rows.saturating_sub(5);
    let total_events = state.stream_events.len();

    // Calculate visible window
    let scroll = state
        .stream_scroll_offset
        .min(total_events.saturating_sub(available_rows));
    let end = (scroll + available_rows).min(total_events);

    for event in &state.stream_events[scroll..end] {
        let time = event.timestamp.format("%H:%M:%S");
        let type_color = match event.event_type.as_str() {
            "assistant" => GREEN,
            "user" => BLUE,
            "result" => YELLOW,
            _ if event.event_type.starts_with("system") => CYAN,
            _ => DIM,
        };

        let max_content = cols.saturating_sub(14);
        let content = truncate(&event.content, max_content);

        println!("  {DIM}{}{RESET} {type_color}{}{RESET}", time, content);
    }

    // Scroll indicator
    if total_events > available_rows {
        println!();
        println!(
            "{DIM}  [{}/{}]{RESET}",
            scroll + 1,
            total_events.saturating_sub(available_rows) + 1
        );
    }
}

/// Print the header bar.
fn print_header(title: &str, cols: usize, version: Option<&str>) {
    let version_str = version.map(|v| format!(" v{}", v)).unwrap_or_default();
    let header = format!(" 🌃 {}{} ", title, version_str);
    let padding = cols.saturating_sub(header.len());
    println!("{BOLD}{BLUE}{}{}{RESET}", header, "─".repeat(padding));
}

/// Render the footer with navigation help.
fn render_footer(state: &PluginState, _rows: usize, cols: usize) {
    let help = if state.section == Section::Coworkers {
        "↑↓ nav  ⏎ stream  a attach"
    } else {
        "↑↓ navigate"
    };
    let line = format!("  {}", help);
    println!("{DIM}{}{RESET}", truncate(&line, cols));
}

/// Truncate a string to fit within a given width.
fn truncate(s: &str, max_len: usize) -> String {
    if max_len == 0 {
        return String::new();
    }
    if s.len() <= max_len {
        s.to_string()
    } else if max_len <= 3 {
        s.chars().take(max_len).collect()
    } else {
        let mut result: String = s.chars().take(max_len - 1).collect();
        result.push('…');
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_long() {
        assert_eq!(truncate("hello world", 8), "hello w…");
    }

    #[test]
    fn test_truncate_zero() {
        assert_eq!(truncate("hello", 0), "");
    }
}
