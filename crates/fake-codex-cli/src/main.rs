use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
use std::thread;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CodexMode {
    Echo,
    Error,
    Tool,
    NoResponse,
    HangStart,
    HangTurn,
}

impl CodexMode {
    fn from_env() -> Self {
        match std::env::var("FAKE_CODEX_MODE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "error" => Self::Error,
            "tool" => Self::Tool,
            "no-response" | "no_response" | "silent" => Self::NoResponse,
            "hang-start" | "hang_start" => Self::HangStart,
            "hang-turn" | "hang_turn" => Self::HangTurn,
            _ => Self::Echo,
        }
    }
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
    if let Ok(template) = std::env::var("FAKE_CODEX_RESPONSE_TEMPLATE") {
        return template.replace("{prompt}", prompt);
    }
    if let Ok(text) = std::env::var("FAKE_CODEX_RESPONSE_TEXT") {
        return text;
    }
    format!("fake-codex response: {prompt}")
}

fn thread_id_from_request(request: &Value, default_id: &str) -> String {
    request
        .pointer("/params/threadId")
        .and_then(Value::as_str)
        .filter(|thread_id| !thread_id.is_empty())
        .unwrap_or(default_id)
        .to_string()
}

fn prompt_from_turn_start(request: &Value) -> String {
    request
        .pointer("/params/input/0/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn respond_ok(id: &Value, result: Value) -> io::Result<()> {
    emit(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
}

fn run_app_server() -> io::Result<i32> {
    let mode = CodexMode::from_env();
    let delay_ms = env_u64("FAKE_CODEX_DELAY_MS");
    let default_thread_id =
        std::env::var("FAKE_CODEX_THREAD_ID").unwrap_or_else(|_| "fake-codex-thread".to_string());

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(_) => continue,
        };

        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let maybe_id = request.get("id").cloned();

        if mode == CodexMode::NoResponse {
            sleep_forever();
        }

        sleep_ms(delay_ms);

        match method {
            "initialize" => {
                if let Some(id) = maybe_id.as_ref() {
                    respond_ok(id, json!({ "capabilities": {} }))?;
                }
            }
            "thread/start" | "thread/resume" | "thread/fork" => {
                if mode == CodexMode::HangStart {
                    sleep_forever();
                }

                let thread_id = thread_id_from_request(&request, &default_thread_id);

                if let Some(id) = maybe_id.as_ref() {
                    respond_ok(
                        id,
                        json!({
                            "thread": {
                                "id": thread_id,
                            }
                        }),
                    )?;
                }

                emit(&json!({
                    "jsonrpc": "2.0",
                    "method": "thread/started",
                    "params": {
                        "thread": {
                            "id": thread_id,
                        }
                    }
                }))?;
            }
            "turn/start" => {
                if mode == CodexMode::HangTurn {
                    sleep_forever();
                }

                if let Some(id) = maybe_id.as_ref() {
                    respond_ok(id, json!({ "accepted": true }))?;
                }

                let prompt = prompt_from_turn_start(&request);
                let reply = response_text(&prompt);

                if mode == CodexMode::Tool {
                    emit(&json!({
                        "jsonrpc": "2.0",
                        "method": "item/started",
                        "params": {
                            "item": {
                                "type": "commandExecution",
                                "id": "cmd_fake_1",
                                "command": "echo fake"
                            }
                        }
                    }))?;

                    emit(&json!({
                        "jsonrpc": "2.0",
                        "method": "item/completed",
                        "params": {
                            "item": {
                                "type": "commandExecution",
                                "id": "cmd_fake_1",
                                "aggregatedOutput": "fake tool output",
                                "status": "completed",
                                "exitCode": 0
                            }
                        }
                    }))?;
                }

                emit(&json!({
                    "jsonrpc": "2.0",
                    "method": "item/agentMessage/delta",
                    "params": {
                        "delta": reply,
                    }
                }))?;

                emit(&json!({
                    "jsonrpc": "2.0",
                    "method": "item/completed",
                    "params": {
                        "item": {
                            "type": "agentMessage",
                            "text": reply,
                        }
                    }
                }))?;

                if mode == CodexMode::Error {
                    emit(&json!({
                        "jsonrpc": "2.0",
                        "method": "turn/completed",
                        "params": {
                            "turn": {
                                "status": "failed",
                                "error": {
                                    "message": "fake codex failure"
                                }
                            }
                        }
                    }))?;
                } else {
                    emit(&json!({
                        "jsonrpc": "2.0",
                        "method": "turn/completed",
                        "params": {
                            "turn": {
                                "status": "completed"
                            }
                        }
                    }))?;
                }
            }
            _ => {
                if let Some(id) = maybe_id.as_ref() {
                    respond_ok(id, json!({}))?;
                }
            }
        }
    }

    Ok(0)
}

fn run_non_app_server(args: &[String]) -> i32 {
    if args.first().is_some_and(|arg| arg == "--version") {
        println!("fake-codex-cli {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }

    if args.first().is_some_and(|arg| arg == "login") {
        println!("fake codex login successful");
        return 0;
    }

    if args.first().is_some_and(|arg| arg == "--resume") {
        println!("fake codex interactive mode");
        return 0;
    }

    eprintln!(
        "fake-codex-cli: passthrough success for invocation: {:?}",
        args
    );
    0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.first().is_some_and(|arg| arg == "app-server") {
        match run_app_server() {
            Ok(code) => std::process::exit(code),
            Err(err) => {
                eprintln!("fake-codex-cli error: {err}");
                std::process::exit(1);
            }
        }
    }

    std::process::exit(run_non_app_server(&args));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_modes() {
        unsafe {
            std::env::set_var("FAKE_CODEX_MODE", "tool");
        }
        assert_eq!(CodexMode::from_env(), CodexMode::Tool);

        unsafe {
            std::env::set_var("FAKE_CODEX_MODE", "hang-turn");
        }
        assert_eq!(CodexMode::from_env(), CodexMode::HangTurn);
    }

    #[test]
    fn extracts_turn_prompt() {
        let req = json!({
            "params": {
                "input": [
                    { "type": "text", "text": "hello" }
                ]
            }
        });
        assert_eq!(prompt_from_turn_start(&req), "hello");
    }
}
