use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

fn fake_codex_bin() -> &'static str {
    env!("CARGO_BIN_EXE_fake-codex-cli")
}

fn write_json_line(stdin: &mut std::process::ChildStdin, payload: &str) {
    writeln!(stdin, "{payload}").expect("write jsonrpc payload");
    stdin.flush().expect("flush jsonrpc payload");
}

#[test]
fn delay_mode_delays_turn_ack() {
    let mut child = Command::new(fake_codex_bin())
        .arg("app-server")
        .env("FAKE_CODEX_MODE", "echo")
        .env("FAKE_CODEX_DELAY_MS", "120")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn fake-codex-cli");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    write_json_line(
        &mut stdin,
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}",
    );
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("read initialize response");
    assert!(line.contains("\"id\":1"));

    write_json_line(
        &mut stdin,
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"thread/start\",\"params\":{\"model\":\"gpt-5.3-codex\"}}",
    );
    line.clear();
    reader
        .read_line(&mut line)
        .expect("read thread/start response");
    assert!(line.contains("\"id\":2"));
    line.clear();
    reader
        .read_line(&mut line)
        .expect("read thread/started notify");
    assert!(line.contains("\"method\":\"thread/started\""));

    let start = Instant::now();
    write_json_line(
        &mut stdin,
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"turn/start\",\"params\":{\"threadId\":\"t1\",\"input\":[{\"type\":\"text\",\"text\":\"hello\"}]}}",
    );

    line.clear();
    reader.read_line(&mut line).expect("read turn/start ack");
    let elapsed = start.elapsed();

    assert!(line.contains("\"id\":3"));
    assert!(
        elapsed >= Duration::from_millis(90),
        "expected >=90ms delay, saw {elapsed:?}"
    );

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn hang_turn_mode_stays_silent_after_turn_start() {
    let mut child = Command::new(fake_codex_bin())
        .arg("app-server")
        .env("FAKE_CODEX_MODE", "hang-turn")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn fake-codex-cli");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    write_json_line(
        &mut stdin,
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}",
    );
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("read initialize response");

    write_json_line(
        &mut stdin,
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"thread/start\",\"params\":{\"model\":\"gpt-5.3-codex\"}}",
    );
    line.clear();
    reader
        .read_line(&mut line)
        .expect("read thread/start response");
    line.clear();
    reader
        .read_line(&mut line)
        .expect("read thread/started notify");

    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut next = String::new();
        let n = reader.read_line(&mut next).unwrap_or(0);
        let _ = tx.send((n, next));
    });

    write_json_line(
        &mut stdin,
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"turn/start\",\"params\":{\"threadId\":\"t1\",\"input\":[{\"type\":\"text\",\"text\":\"hello\"}]}}",
    );

    assert!(
        rx.recv_timeout(Duration::from_millis(150)).is_err(),
        "hang-turn unexpectedly produced output quickly"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = handle.join();
}
