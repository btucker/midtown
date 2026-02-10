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
                        out.push_str(&format!("  {} - {}\n", cw.name, task_desc));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_response_with_full_info() {
        let json = r#"{
            "daemon_running": true,
            "active_coworkers": 2,
            "pending_tasks": 1,
            "socket_path": "/tmp/midtown.sock",
            "coworkers": [
                {"name": "lex", "status": "running", "current_task": "implement auth", "started_at": "2024-01-01T00:00:00Z"},
                {"name": "park", "status": "running", "current_task": null, "started_at": "2024-01-01T00:00:00Z"}
            ],
            "tasks": [
                {"id": "t1", "subject": "implement auth endpoint", "status": "in_progress", "assignee": "lex"}
            ],
            "pull_requests": [
                {"number": 42, "title": "Add auth", "author": "lex", "status": "awaiting review"}
            ],
            "recent_activity": []
        }"#;

        let response: Response = serde_json::from_str(json).unwrap();

        match response {
            Response::Status(status) => {
                assert!(status.daemon_running);
                assert_eq!(status.active_coworkers, 2);
                assert!(status.full_status.is_some());

                let full = status.full_status.unwrap();
                assert_eq!(full.coworkers.len(), 2);
                assert_eq!(full.coworkers[0].name, "lex");
                assert_eq!(
                    full.coworkers[0].current_task,
                    Some("implement auth".to_string())
                );
                assert_eq!(full.coworkers[1].current_task, None);
                assert_eq!(full.tasks.len(), 1);
                assert_eq!(full.pull_requests.len(), 1);
            }
            _ => panic!("Expected Status response"),
        }
    }

    #[test]
    fn test_coworkers_response_parsing() {
        let json = r#"{"coworkers": [{"name": "lexington", "status": "running", "current_task": null, "started_at": "2026-01-26T20:52:06.779326+00:00"}]}"#;
        let response: Response = serde_json::from_str(json).expect("Should parse");

        match response {
            Response::Coworkers { coworkers } => {
                assert_eq!(coworkers.len(), 1);
                assert_eq!(coworkers[0].name, "lexington");
                assert_eq!(coworkers[0].status, "running");
            }
            other => panic!("Expected Coworkers, got {:?}", other),
        }
    }

    #[test]
    fn test_coworkers_response_with_success_field() {
        // Daemon returns "success": true along with coworkers
        let json = r#"{"success": true, "coworkers": [{"name": "lexington", "status": "running", "current_task": null, "started_at": "2026-01-26T20:52:06.779326+00:00"}]}"#;
        let response: Response =
            serde_json::from_str(json).expect("Should parse with extra fields");

        match response {
            Response::Coworkers { coworkers } => {
                assert_eq!(coworkers.len(), 1);
                assert_eq!(coworkers[0].name, "lexington");
            }
            other => panic!("Expected Coworkers, got {:?}", other),
        }
    }

    #[test]
    fn test_coworker_view_output_format_does_not_match_response_enum() {
        // The coworker.view RPC returns {"success": true, "output": "..."} which
        // doesn't match any Response variant. This is why coworker_view() uses
        // send_raw() instead of send().
        let json = r#"{"success": true, "output": "some terminal output"}"#;
        let result: Result<Response, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "coworker.view output format should NOT deserialize as Response"
        );
    }

    #[test]
    fn test_coworker_view_output_extraction() {
        // Verify the extraction logic used in DaemonClient::coworker_view()
        let raw: serde_json::Value =
            serde_json::from_str(r#"{"success": true, "output": "terminal content here"}"#)
                .unwrap();
        let output = raw
            .get("output")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "RPC response missing 'output' field".to_string());
        assert_eq!(output.unwrap(), "terminal content here");
    }

    #[test]
    fn test_coworker_view_missing_output_field_returns_error() {
        // If the output field is missing, extraction should fail with a clear error
        let raw: serde_json::Value = serde_json::from_str(r#"{"success": true}"#).unwrap();
        let output = raw
            .get("output")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "RPC response missing 'output' field".to_string());
        assert_eq!(output.unwrap_err(), "RPC response missing 'output' field");
    }

    #[test]
    fn test_task_update_response_with_type_field() {
        // The task.update RPC returns {"type": "message", "message": "..."} but the
        // Response::Message variant expects just {"message": "..."}. The "type" field
        // should be ignored during deserialization.
        let json = r#"{"type": "message", "message": "Task !1116 updated"}"#;
        let response: Response =
            serde_json::from_str(json).expect("Should parse task.update response with type field");

        match response {
            Response::Message { message } => {
                assert_eq!(message, "Task !1116 updated");
            }
            other => panic!("Expected Message, got {:?}", other),
        }
    }

    #[test]
    fn test_status_pretty_format() {
        let status = StatusResponse {
            daemon_running: true,
            active_coworkers: 2,
            max_coworkers: Some(16),
            pending_tasks: 1,
            socket_path: "/tmp/test.sock".to_string(),
            lead_session: Some("midtown-lead".to_string()),
            lead_session_active: Some(true),
            full_status: Some(FullStatusInfo {
                coworkers: vec![
                    CoworkerInfo {
                        name: "lex".to_string(),
                        status: "running".to_string(),
                        current_task: Some("implement auth endpoint".to_string()),
                        started_at: None,
                    },
                    CoworkerInfo {
                        name: "park".to_string(),
                        status: "running".to_string(),
                        current_task: None,
                        started_at: None,
                    },
                ],
                tasks: vec![TaskInfo {
                    id: "t1".to_string(),
                    subject: "implement auth endpoint".to_string(),
                    status: "in_progress".to_string(),
                    assignee: Some("lex".to_string()),
                }],
                pull_requests: vec![PrInfo {
                    number: 42,
                    title: "Add auth".to_string(),
                    author: "lex".to_string(),
                    status: "awaiting review".to_string(),
                }],
                recent_activity: vec![],
            }),
        };

        let response = Response::Status(status);
        let pretty = response.to_pretty();

        assert!(pretty.contains("Coworkers: 2/16 active"));
        assert!(pretty.contains("lex - working on: implement auth endpoint"));
        assert!(pretty.contains("park - idle"));
        assert!(pretty.contains("Tasks: 1 open"));
        assert!(pretty.contains("[in_progress] implement auth endpoint (lex)"));
        assert!(pretty.contains("PRs: 1 open"));
        assert!(pretty.contains("PR#42 Add auth (lex) - awaiting review"));
    }
}
