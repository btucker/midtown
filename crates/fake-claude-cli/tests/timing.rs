use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

fn fake_claude_bin() -> &'static str {
    env!("CARGO_BIN_EXE_fake-claude-cli")
}

#[test]
fn delay_mode_delays_assistant_output() {
    let mut child = Command::new(fake_claude_bin())
        .args([
            "--model",
            "sonnet",
            "--session-id",
            "session_delay",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
        ])
        .env("FAKE_CLAUDE_MODE", "echo")
        .env("FAKE_CLAUDE_DELAY_MS", "120")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn fake-claude-cli");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    let mut init_line = String::new();
    reader.read_line(&mut init_line).expect("read init line");
    assert!(init_line.contains("\"type\":\"system\""));

    let start = Instant::now();
    writeln!(
        stdin,
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}"
    )
    .expect("write prompt");
    stdin.flush().expect("flush prompt");
    drop(stdin);

    let mut assistant_line = String::new();
    reader
        .read_line(&mut assistant_line)
        .expect("read assistant line");
    let elapsed = start.elapsed();

    assert!(assistant_line.contains("\"type\":\"assistant\""));
    assert!(
        elapsed >= Duration::from_millis(90),
        "expected >=90ms delay, saw {elapsed:?}"
    );

    let mut result_line = String::new();
    reader
        .read_line(&mut result_line)
        .expect("read result line");
    assert!(result_line.contains("\"type\":\"result\""));

    let status = child.wait().expect("wait fake-claude-cli");
    assert!(status.success());
}

#[test]
fn hang_turn_mode_stays_silent_after_init() {
    let mut child = Command::new(fake_claude_bin())
        .args([
            "--model",
            "sonnet",
            "--session-id",
            "session_hang",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
        ])
        .env("FAKE_CLAUDE_MODE", "hang-turn")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn fake-claude-cli");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    let mut init_line = String::new();
    reader.read_line(&mut init_line).expect("read init line");
    assert!(init_line.contains("\"type\":\"system\""));

    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut line = String::new();
        let n = reader.read_line(&mut line).unwrap_or(0);
        let _ = tx.send((n, line));
    });

    writeln!(
        stdin,
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}"
    )
    .expect("write prompt");
    stdin.flush().expect("flush prompt");

    assert!(
        rx.recv_timeout(Duration::from_millis(150)).is_err(),
        "hang-turn unexpectedly produced output quickly"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = handle.join();
}
