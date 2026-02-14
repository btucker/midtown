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
    /// Raw JSON value (for plugin RPC passthrough)
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
    /// Whether the Lead tmux session is active
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

impl Response {
    #[allow(dead_code)]
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

                    // Coworkers section
                    let coworker_header = match status.max_coworkers {
                        Some(max) => {
                            format!("Coworkers: {}/{} active\n", full.coworkers.len(), max)
                        }
                        None => format!("Coworkers: {} active\n", full.coworkers.len()),
                    };
                    out.push_str(&coworker_header);
                    for cw in &full.coworkers {
                        let task_desc = match &cw.current_task {
                            Some(task) => format!("working on: {}", task),
                            None => "idle".to_string(),
                        };
                        // Format provider:profile (e.g., "claude: ben@quotably.com")
                        let auth_info = match (&cw.provider, &cw.profile) {
                            (Some(provider), Some(profile)) => {
                                format!(" ({}: {})", provider, profile)
                            }
                            _ => String::new(),
                        };
                        out.push_str(&format!("  {} - {}{}\n", cw.name, task_desc, auth_info));
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
                if coworkers.is_empty() {
                    return "No active coworkers".to_string();
                }
                let mut out = String::from("Coworkers\n─────────────────────────────\n");
                for cw in coworkers {
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
