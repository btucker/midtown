use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
use std::thread;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClaudeMode {
    Echo,
    Error,
    ToolUse,
    InitOnly,
    NoResponse,
    HangTurn,
}

impl ClaudeMode {
    fn from_env() -> Self {
        match std::env::var("FAKE_CLAUDE_MODE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "error" => Self::Error,
            "tool" | "tool_use" => Self::ToolUse,
            "init-only" | "init_only" => Self::InitOnly,
            "no-response" | "no_response" | "silent" => Self::NoResponse,
            "hang-turn" | "hang_turn" => Self::HangTurn,
            _ => Self::Echo,
        }
    }
}

fn parse_flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|idx| args.get(idx + 1))
        .cloned()
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.parse::<u64>().ok()
}

fn sleep_ms(ms: Option<u64>) {
    if let Some(delay_ms) = ms {
        thread::sleep(Duration::from_millis(delay_ms));
    }
}

fn sleep_forever() -> ! {
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

fn emit(value: &Value) -> io::Result<()> {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    writeln!(lock, "{value}")?;
    lock.flush()
}

fn response_text(prompt: &str) -> String {
    if let Ok(template) = std::env::var("FAKE_CLAUDE_RESPONSE_TEMPLATE") {
        return template.replace("{prompt}", prompt);
    }
    if let Ok(text) = std::env::var("FAKE_CLAUDE_RESPONSE_TEXT") {
        return text;
    }
    format!("fake-claude response: {prompt}")
}

fn handle_plugin(args: &[String]) -> i32 {
    match args {
        [cmd, sub, flag] if cmd == "plugin" && sub == "list" && flag == "--json" => {
            let payload =
                std::env::var("FAKE_CLAUDE_PLUGIN_LIST_JSON").unwrap_or_else(|_| "[]".to_string());
            println!("{payload}");
            0
        }
        [cmd, sub, action] if cmd == "plugin" && sub == "marketplace" && action == "list" => {
            let payload = std::env::var("FAKE_CLAUDE_MARKETPLACE_LIST").unwrap_or_else(|_| {
                "anthropics/claude-plugins-official (claude-plugins-official)".to_string()
            });
            println!("{payload}");
            0
        }
        [cmd, sub, action, _repo] if cmd == "plugin" && sub == "marketplace" && action == "add" => {
            println!("ok");
            0
        }
        [cmd, sub, _plugin] if cmd == "plugin" && sub == "install" => {
            println!("ok");
            0
        }
        _ => 0,
    }
}

fn run_stream_mode(args: &[String]) -> io::Result<i32> {
    let mode = ClaudeMode::from_env();
    let delay_ms = env_u64("FAKE_CLAUDE_DELAY_MS");

    let session_id = parse_flag_value(args, "--session-id")
        .or_else(|| std::env::var("FAKE_CLAUDE_SESSION_ID").ok())
        .unwrap_or_else(|| "fake-claude-session".to_string());
    let model = parse_flag_value(args, "--model")
        .or_else(|| std::env::var("FAKE_CLAUDE_MODEL").ok())
        .unwrap_or_else(|| "fake-claude-model".to_string());

    if mode != ClaudeMode::NoResponse {
        emit(&json!({
            "type": "system",
            "subtype": "init",
            "session_id": session_id,
            "model": model,
        }))?;
    }

    if mode == ClaudeMode::NoResponse || mode == ClaudeMode::InitOnly {
        sleep_forever();
    }

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let parsed: Value = serde_json::from_str(trimmed).unwrap_or(Value::Null);
        let prompt = parsed
            .pointer("/message/content")
            .and_then(Value::as_str)
            .unwrap_or_default();

        if mode == ClaudeMode::HangTurn {
            sleep_forever();
        }

        sleep_ms(delay_ms);

        let text = response_text(prompt);

        match mode {
            ClaudeMode::Echo => {
                emit(&json!({
                    "type": "assistant",
                    "message": {
                        "role": "assistant",
                        "content": [{"type": "text", "text": text}]
                    },
                    "session_id": session_id,
                }))?;

                emit(&json!({
                    "type": "result",
                    "subtype": "success",
                    "is_error": false,
                    "result": text,
                    "duration_ms": 10,
                    "total_cost_usd": 0.0,
                    "session_id": session_id,
                    "usage": {
                        "input_tokens": 5,
                        "output_tokens": 7,
                        "cache_read_input_tokens": 0,
                        "cache_creation_input_tokens": 0,
                    }
                }))?;
            }
            ClaudeMode::ToolUse => {
                emit(&json!({
                    "type": "assistant",
                    "message": {
                        "role": "assistant",
                        "content": [{
                            "type": "tool_use",
                            "id": "toolu_fake_1",
                            "name": "Bash",
                            "input": {"command": "echo fake"}
                        }]
                    },
                    "session_id": session_id,
                }))?;

                emit(&json!({
                    "type": "user",
                    "message": {
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": "toolu_fake_1",
                            "content": "fake tool output"
                        }]
                    }
                }))?;

                emit(&json!({
                    "type": "assistant",
                    "message": {
                        "role": "assistant",
                        "content": [{"type": "text", "text": text}]
                    },
                    "session_id": session_id,
                }))?;

                emit(&json!({
                    "type": "result",
                    "subtype": "success",
                    "is_error": false,
                    "result": text,
                    "duration_ms": 10,
                    "total_cost_usd": 0.0,
                    "session_id": session_id,
                }))?;
            }
            ClaudeMode::Error => {
                emit(&json!({
                    "type": "result",
                    "subtype": "error",
                    "is_error": true,
                    "result": text,
                    "duration_ms": 10,
                    "total_cost_usd": 0.0,
                    "session_id": session_id,
                }))?;
            }
            ClaudeMode::InitOnly | ClaudeMode::NoResponse | ClaudeMode::HangTurn => {
                // Handled above.
            }
        }
    }

    Ok(0)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.first().is_some_and(|arg| arg == "--version") {
        println!("fake-claude-cli {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    if args.first().is_some_and(|arg| arg == "plugin") {
        std::process::exit(handle_plugin(&args));
    }

    if args.first().is_some_and(|arg| arg == "login") {
        println!("fake login successful");
        return;
    }

    match run_stream_mode(&args) {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("fake-claude-cli error: {err}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_modes() {
        unsafe {
            std::env::set_var("FAKE_CLAUDE_MODE", "tool");
        }
        assert_eq!(ClaudeMode::from_env(), ClaudeMode::ToolUse);

        unsafe {
            std::env::set_var("FAKE_CLAUDE_MODE", "hang-turn");
        }
        assert_eq!(ClaudeMode::from_env(), ClaudeMode::HangTurn);
    }

    #[test]
    fn parses_flag_value() {
        let args = vec![
            "-p".to_string(),
            "x".to_string(),
            "--model".to_string(),
            "sonnet".to_string(),
        ];
        assert_eq!(
            parse_flag_value(&args, "--model"),
            Some("sonnet".to_string())
        );
        assert_eq!(parse_flag_value(&args, "--missing"), None);
    }
}
