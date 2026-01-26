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
    /// Nudge configuration
    NudgeConfig(NudgeConfigResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub daemon_running: bool,
    pub active_coworkers: usize,
    pub pending_tasks: usize,
    pub socket_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoworkerInfo {
    pub name: String,
    pub status: String,
    pub current_task: Option<String>,
    pub started_at: Option<String>,
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
pub struct NudgeConfigResponse {
    pub enabled: bool,
    pub interval_secs: u64,
    pub message_template: String,
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
            Response::Status(status) => format!(
                "Midtown Status\n\
                 ─────────────────────────────\n\
                 Daemon:          {}\n\
                 Active coworkers: {}\n\
                 Pending tasks:   {}\n\
                 Socket:          {}",
                if status.daemon_running {
                    "running"
                } else {
                    "stopped"
                },
                status.active_coworkers,
                status.pending_tasks,
                status.socket_path
            ),
            Response::Coworkers { coworkers } => {
                if coworkers.is_empty() {
                    return "No active coworkers".to_string();
                }
                let mut out = String::from("Coworkers\n─────────────────────────────\n");
                for cw in coworkers {
                    out.push_str(&format!(
                        "{:<12} {:8} {}\n",
                        cw.name,
                        cw.status,
                        cw.current_task.as_deref().unwrap_or("-")
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
                    out.push_str(&format!("[{}] {}: {}\n", msg.timestamp, msg.from, msg.message));
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
                        task.id,
                        task.status,
                        task.subject
                    ));
                }
                out.trim_end().to_string()
            }
            Response::PullRequests { pull_requests } => {
                if pull_requests.is_empty() {
                    return "No pull requests".to_string();
                }
                let mut out = String::from("Pull Requests\n─────────────────────────────\n");
                for pr in pull_requests {
                    out.push_str(&format!(
                        "#{:<5} {:10} {:12} {}\n",
                        pr.number, pr.status, pr.author, pr.title
                    ));
                }
                out.trim_end().to_string()
            }
            Response::NudgeConfig(config) => {
                format!(
                    "Nudge Configuration\n\
                     ─────────────────────────────\n\
                     Enabled:  {}\n\
                     Interval: {} seconds\n\
                     Template: {}",
                    if config.enabled { "yes" } else { "no" },
                    config.interval_secs,
                    config.message_template
                )
            }
        }
    }
}
