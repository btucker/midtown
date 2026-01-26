//! UI rendering for the chat TUI

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use midtown::MessageType;

use super::app::App;

/// Avenue names mapped to colors (position-based assignment)
const AVENUE_COLORS: &[(&str, Color)] = &[
    ("lexington", Color::Cyan),
    ("park", Color::Green),
    ("madison", Color::Yellow),
    ("broadway", Color::Magenta),
    ("amsterdam", Color::Blue),
    ("columbus", Color::Red),
];

/// Get color for a sender name
fn get_sender_color(name: &str) -> Color {
    match name.to_lowercase().as_str() {
        "lead" => Color::White,
        "github" => Color::DarkGray,
        "system" => Color::DarkGray,
        _ => {
            // Check avenue colors
            for (avenue, color) in AVENUE_COLORS {
                if name.to_lowercase() == *avenue {
                    return *color;
                }
            }
            // Default for unknown names
            Color::White
        }
    }
}

/// Draw the main UI
pub fn draw(f: &mut Frame, app: &mut App) {
    // Split into team panel (30%) and chat panel (70%)
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(f.area());

    draw_team_panel(f, app, chunks[0]);
    draw_chat_panel(f, app, chunks[1]);
}

/// Draw the team panel showing coworkers and their status
fn draw_team_panel(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Team ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);

    // Build list items for coworkers
    let items: Vec<ListItem> = app
        .coworkers
        .iter()
        .flat_map(|cw| {
            let color = get_sender_color(&cw.name);
            let name_line = Line::from(Span::styled(
                &cw.name,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));

            let status_line = match &cw.last_action {
                Some(action) => {
                    // Truncate long actions
                    let display = if action.len() > 25 {
                        format!("  {}...", &action[..22])
                    } else {
                        format!("  {}", action)
                    };
                    Line::from(Span::styled(display, Style::default().fg(Color::DarkGray)))
                }
                None => Line::from(Span::styled(
                    "  (idle)",
                    Style::default().fg(Color::DarkGray),
                )),
            };

            vec![ListItem::new(name_line), ListItem::new(status_line)]
        })
        .collect();

    let list = List::new(items);

    f.render_widget(block, area);
    f.render_widget(list, inner);
}

/// Draw the chat panel showing messages
fn draw_chat_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .title(" #midtown ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);

    // Update visible height for scroll calculations
    app.visible_height = inner.height as usize;

    // Get visible messages
    let visible = app.visible_messages();

    // Build lines for messages
    let lines: Vec<Line> = visible
        .iter()
        .map(|msg| {
            let time = msg.timestamp.format("%H:%M").to_string();
            let color = get_sender_color(&msg.from);

            match msg.message_type {
                MessageType::Action => {
                    // IRC-style action: HH:MM * name action
                    Line::from(vec![
                        Span::styled(format!("{} ", time), Style::default().fg(Color::DarkGray)),
                        Span::styled("* ", Style::default().fg(color)),
                        Span::styled(
                            format!("{} ", msg.from),
                            Style::default().fg(color).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(&msg.content, Style::default().fg(color)),
                    ])
                }
                MessageType::System => {
                    // System message: HH:MM <system> message
                    Line::from(vec![
                        Span::styled(format!("{} ", time), Style::default().fg(Color::DarkGray)),
                        Span::styled("<system> ", Style::default().fg(Color::DarkGray)),
                        Span::styled(&msg.content, Style::default().fg(Color::DarkGray)),
                    ])
                }
                _ => {
                    // Regular message: HH:MM <name> message
                    Line::from(vec![
                        Span::styled(format!("{} ", time), Style::default().fg(Color::DarkGray)),
                        Span::styled("<", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            &msg.from,
                            Style::default().fg(color).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled("> ", Style::default().fg(Color::DarkGray)),
                        Span::styled(&msg.content, Style::default().fg(Color::White)),
                    ])
                }
            }
        })
        .collect();

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });

    f.render_widget(block, area);
    f.render_widget(paragraph, inner);
}
