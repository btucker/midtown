use serde::{Deserialize, Serialize};

/// Response wrapper for CLI output formatting
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Response {
    /// Simple message response
    Message { message: String },
    /// Status response with system info
    Status(StatusResponse),
    /// List of coworkers
    Coworkers { coworkers: Vec<CoworkerInfo> },
    /// Channel messages
    Messages { messages: Vec<ChannelMessage> },
    /// List of tasks
    Tasks { tasks: Vec<TaskInfo> },
    /// List of PRs
    PullRequests { pull_requests: Vec<PrInfo> },
    /// List of headless sessions that can be attached
    Sessions { sessions: Vec<SessionInfo> },
    /// Raw JSON value passthrough
    Json { value: serde_json::Value },
}

/// Basic status response (legacy)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub daemon_running: bool,
    pub active_coworkers: usize,
    #[serde(default)]
    pub max_coworkers: Option<usize>,
    pub pending_tasks: usize,
    pub socket_path: String,
    /// Lead session name (usually "midtown-lead")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lead_session: Option<String>,
    /// Whether the Lead session is active
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lead_session_active: Option<bool>,
    /// Full status info (optional, for expanded status command)
    #[serde(flatten)]
    pub full_status: Option<FullStatusInfo>,
}

/// Comprehensive status information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FullStatusInfo {
    /// List of active coworkers with their tasks
    #[serde(default)]
    pub coworkers: Vec<CoworkerInfo>,
    /// Open tasks (pending or in progress)
    #[serde(default)]
    pub tasks: Vec<TaskInfo>,
    /// Recent open pull requests
    #[serde(default)]
    pub pull_requests: Vec<PrInfo>,
    /// Recent channel activity summary
    #[serde(default)]
    pub recent_activity: Vec<ActivitySummary>,
}

/// Summary of recent channel activity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivitySummary {
    pub timestamp: String,
    pub from: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoworkerInfo {
    pub name: String,
    pub status: String,
    pub current_task: Option<String>,
    pub started_at: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    /// Whether this coworker is a channel lead session.
    /// Channel leads are persistent domain experts and should not count
    /// toward the coworker slot limit.
    #[serde(default)]
    pub is_channel_lead: bool,
    /// Cumulative input tokens used this session (0 if not yet reported).
    #[serde(default)]
    pub input_tokens: u64,
    /// Cumulative output tokens generated this session (0 if not yet reported).
    #[serde(default)]
    pub output_tokens: u64,
    /// Live workflow phase from coworker_records (e.g., "review", "dev", "PR").
    /// Takes priority over `current_task` for activity display, except "idle"
    /// and "done" which always show as idle regardless of `current_task`.
    #[serde(default)]
    pub phase: Option<String>,
    /// PR number currently associated with this coworker's activity.
    #[serde(default)]
    pub pr_number: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub from: String,
    pub message: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInfo {
    pub id: String,
    pub subject: String,
    pub status: String,
    pub assignee: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrInfo {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub session_id: String,
    pub status: String,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub last_active: Option<String>,
    #[serde(default)]
    pub task: Option<String>,
}

/// Format a token count with k/M suffix for compact display.
///
/// Examples: 0 → "", 500 → "500", 1500 → "1.5k", 1_200_000 → "1.2M"
fn format_tokens(n: u64) -> String {
    if n == 0 {
        String::new()
    } else if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Format token usage as a compact "in/out" suffix for status lines.
///
/// Returns an empty string if no tokens have been reported yet.
fn token_suffix(input: u64, output: u64) -> String {
    if input == 0 && output == 0 {
        String::new()
    } else {
        format!(" [{}/{}]", format_tokens(input), format_tokens(output))
    }
}

/// Build a human-readable activity string for a coworker.
///
/// Uses the live `phase` field (from coworker_records) when available,
/// falling back to `current_task` (task file ownership) for backward compat.
fn coworker_activity(cw: &CoworkerInfo) -> String {
    match cw.phase.as_deref() {
        Some("review") => match cw.pr_number {
            Some(pr) => format!("reviewing PR #{}", pr),
            None => "reviewing".to_string(),
        },
        Some("PR") => match cw.pr_number {
            Some(pr) => format!("PR open #{}", pr),
            None => match cw.current_task.as_deref() {
                Some(task) => format!("opening PR: {}", task),
                None => "opening PR".to_string(),
            },
        },
        Some("dev") => match cw.current_task.as_deref() {
            Some(task) => format!("developing: {}", task),
            None => "developing".to_string(),
        },
        Some("test") => match cw.current_task.as_deref() {
            Some(task) => format!("testing: {}", task),
            None => "testing".to_string(),
        },
        Some("debug") => match cw.current_task.as_deref() {
            Some(task) => format!("debugging: {}", task),
            None => "debugging".to_string(),
        },
        Some("claim") => "claiming task".to_string(),
        Some("done") | Some("idle") => "idle".to_string(),
        None => match cw.current_task.as_deref() {
            Some(task) => format!("working on: {}", task),
            None => "idle".to_string(),
        },
        Some(other) => match cw.current_task.as_deref() {
            Some(task) => format!("{}: {}", other, task),
            None => other.to_string(),
        },
    }
}

impl Response {
    pub fn message(msg: impl Into<String>) -> Self {
        Response::Message {
            message: msg.into(),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|e| format!("{{\"error\": \"{}\"}}", e))
    }

    pub fn to_pretty(&self) -> String {
        match self {
            Response::Message { message } => message.clone(),
            Response::Status(status) => {
                // If we have full status info, use the rich format
                if let Some(ref full) = status.full_status {
                    let mut out = String::new();

                    // Coworkers section — channel leads are persistent domain experts,
                    // not workers competing for slots, so exclude them from the count.
                    let worker_count = full
                        .coworkers
                        .iter()
                        .filter(|cw| !cw.is_channel_lead)
                        .count();
                    let coworker_header = match status.max_coworkers {
                        Some(max) => {
                            format!("Coworkers: {}/{} active\n", worker_count, max)
                        }
                        None => format!("Coworkers: {} active\n", worker_count),
                    };
                    out.push_str(&coworker_header);
                    for cw in full.coworkers.iter().filter(|cw| !cw.is_channel_lead) {
                        let task_desc = coworker_activity(cw);
                        // Format provider:profile (e.g., "claude: ben@quotably.com")
                        let auth_info = match (&cw.provider, &cw.profile) {
                            (Some(provider), Some(profile)) => {
                                format!(" ({}: {})", provider, profile)
                            }
                            _ => String::new(),
                        };
                        let tokens = token_suffix(cw.input_tokens, cw.output_tokens);
                        out.push_str(&format!(
                            "  {} - {}{}{}\n",
                            cw.name, task_desc, tokens, auth_info
                        ));
                    }

                    // Lead Sessions section — channel leads are domain experts per channel
                    let lead_sessions: Vec<_> = full
                        .coworkers
                        .iter()
                        .filter(|cw| cw.is_channel_lead)
                        .collect();
                    if !lead_sessions.is_empty() {
                        out.push_str(&format!(
                            "\nLead Sessions: {} active\n",
                            lead_sessions.len()
                        ));
                        for cw in &lead_sessions {
                            let task_desc = coworker_activity(cw);
                            let auth_info = match (&cw.provider, &cw.profile) {
                                (Some(provider), Some(profile)) => {
                                    format!(" ({}: {})", provider, profile)
                                }
                                _ => String::new(),
                            };
                            let tokens = token_suffix(cw.input_tokens, cw.output_tokens);
                            out.push_str(&format!(
                                "  {} - {}{}{}\n",
                                cw.name, task_desc, tokens, auth_info
                            ));
                        }
                    }

                    // Tasks section
                    let open_tasks: Vec<_> = full
                        .tasks
                        .iter()
                        .filter(|t| t.status != "completed")
                        .collect();
                    out.push_str(&format!("\nTasks: {} open\n", open_tasks.len()));
                    for task in open_tasks {
                        let assignee = task.assignee.as_deref().unwrap_or("");
                        let assignee_str = if assignee.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", assignee)
                        };
                        out.push_str(&format!(
                            "  [{}] {}{}\n",
                            task.status, task.subject, assignee_str
                        ));
                    }

                    // PRs section
                    out.push_str(&format!("\nPRs: {} open\n", full.pull_requests.len()));
                    for pr in &full.pull_requests {
                        out.push_str(&format!(
                            "  PR#{} {} ({}) - {}\n",
                            pr.number, pr.title, pr.author, pr.status
                        ));
                    }

                    // Recent activity section (if any)
                    if !full.recent_activity.is_empty() {
                        out.push_str("\nRecent activity:\n");
                        for activity in full.recent_activity.iter().take(5) {
                            out.push_str(&format!(
                                "  [{}] {}: {}\n",
                                activity.timestamp, activity.from, activity.summary
                            ));
                        }
                    }

                    out.trim_end().to_string()
                } else {
                    // Legacy minimal format
                    let lead_status = match status.lead_session_active {
                        Some(true) => "running",
                        Some(false) => "stopped",
                        None => "unknown",
                    };
                    {
                        let coworker_display = match status.max_coworkers {
                            Some(max) => format!("{}/{}", status.active_coworkers, max),
                            None => format!("{}", status.active_coworkers),
                        };
                        format!(
                            "Midtown Status\n\
                             ─────────────────────────────\n\
                             Daemon:           {}\n\
                             Lead session:     {}\n\
                             Active coworkers: {}\n\
                             Pending tasks:    {}\n\
                             Socket:           {}",
                            if status.daemon_running {
                                "running"
                            } else {
                                "stopped"
                            },
                            lead_status,
                            coworker_display,
                            status.pending_tasks,
                            status.socket_path
                        )
                    }
                }
            }
            Response::Coworkers { coworkers } => {
                let workers: Vec<_> = coworkers.iter().filter(|cw| !cw.is_channel_lead).collect();
                if workers.is_empty() {
                    return "No active coworkers".to_string();
                }
                let mut out = String::from("Coworkers\n─────────────────────────────\n");
                for cw in workers {
                    // Format provider:profile (e.g., "claude: ben@quotably.com")
                    let auth_info = match (&cw.provider, &cw.profile) {
                        (Some(provider), Some(profile)) => {
                            format!(" ({}: {})", provider, profile)
                        }
                        _ => String::new(),
                    };
                    out.push_str(&format!(
                        "{:<12} {:8} {}{}\n",
                        cw.name,
                        cw.status,
                        cw.current_task.as_deref().unwrap_or("-"),
                        auth_info
                    ));
                }
                out.trim_end().to_string()
            }
            Response::Messages { messages } => {
                if messages.is_empty() {
                    return "No messages".to_string();
                }
                let mut out = String::new();
                for msg in messages {
                    out.push_str(&format!(
                        "[{}] {}: {}\n",
                        msg.timestamp, msg.from, msg.message
                    ));
                }
                out.trim_end().to_string()
            }
            Response::Tasks { tasks } => {
                if tasks.is_empty() {
                    return "No tasks".to_string();
                }
                let mut out = String::from("Tasks\n─────────────────────────────\n");
                for task in tasks {
                    out.push_str(&format!(
                        "{:<10} {:10} {}\n",
                        task.id, task.status, task.subject
                    ));
                }
                out.trim_end().to_string()
            }
            Response::Sessions { sessions } => {
                if sessions.is_empty() {
                    return "No attachable sessions".to_string();
                }
                let mut out = String::from("Headless Sessions\n─────────────────────────────\n");
                for session in sessions {
                    let task_suffix = session
                        .task
                        .as_ref()
                        .map(|task| format!(" task:!{}", task))
                        .unwrap_or_default();
                    out.push_str(&format!(
                        "{:<12} {:8} {:<20}{}\n",
                        session.name, session.status, session.session_id, task_suffix
                    ));
                }
                out.trim_end().to_string()
            }
            Response::Json { value } => serde_json::to_string_pretty(value)
                .unwrap_or_else(|e| format!("{{\"error\": \"{}\"}}", e)),
            Response::PullRequests { pull_requests } => {
                if pull_requests.is_empty() {
                    return "No pull requests".to_string();
                }
                let mut out = String::from("Pull Requests\n─────────────────────────────\n");
                for pr in pull_requests {
                    out.push_str(&format!(
                        "PR#{:<5} {:10} {:12} {}\n",
                        pr.number, pr.status, pr.author, pr.title
                    ));
                }
                out.trim_end().to_string()
            }
        }
    }
}

#[path = "response_tests.rs"]
#[cfg(test)]
mod tests;
