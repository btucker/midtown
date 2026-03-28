use std::os::unix::process::ExitStatusExt;
use std::process::{ExitStatus, Output};

use super::{check_cmd_output, check_cmd_result, cmd_stdout, parse_json_warn};

fn make_output(status_code: i32, stdout: &str, stderr: &str) -> Output {
    Output {
        status: ExitStatus::from_raw(status_code << 8), // Unix: code in upper byte
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

#[test]
fn check_cmd_output_returns_output_on_success() {
    let output = make_output(0, "hello\n", "");
    let result = check_cmd_output(Ok(output), "test command");
    assert!(result.is_some());
    let out = result.unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello\n");
}

#[test]
fn check_cmd_output_returns_none_on_nonzero_exit() {
    let output = make_output(1, "", "something went wrong\n");
    let result = check_cmd_output(Ok(output), "test command");
    assert!(result.is_none());
}

#[test]
fn check_cmd_output_returns_none_on_io_error() {
    let err = std::io::Error::new(std::io::ErrorKind::NotFound, "command not found");
    let result = check_cmd_output(Err(err), "test command");
    assert!(result.is_none());
}

// --- check_cmd_result ---

#[test]
fn check_cmd_result_returns_output_on_success() {
    let output = make_output(0, "hello\n", "");
    let result = check_cmd_result(Ok(output));
    assert!(result.is_ok());
    assert_eq!(String::from_utf8_lossy(&result.unwrap().stdout), "hello\n");
}

#[test]
fn check_cmd_result_returns_stderr_on_nonzero_exit() {
    let output = make_output(1, "", "something went wrong\n");
    let result = check_cmd_result(Ok(output));
    assert_eq!(result.unwrap_err(), "something went wrong");
}

#[test]
fn check_cmd_result_returns_error_string_on_io_error() {
    let err = std::io::Error::new(std::io::ErrorKind::NotFound, "command not found");
    let result = check_cmd_result(Err(err));
    assert_eq!(result.unwrap_err(), "command not found");
}

// --- cmd_stdout ---

#[test]
fn cmd_stdout_returns_trimmed_stdout_on_success() {
    let output = make_output(0, "  hello world  \n", "");
    assert_eq!(cmd_stdout(Ok(output)).unwrap(), "hello world");
}

#[test]
fn cmd_stdout_returns_none_on_nonzero_exit() {
    let output = make_output(1, "output", "error");
    assert!(cmd_stdout(Ok(output)).is_none());
}

#[test]
fn cmd_stdout_returns_none_on_io_error() {
    let err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
    assert!(cmd_stdout(Err(err)).is_none());
}

#[test]
fn cmd_stdout_returns_none_on_empty_output() {
    let output = make_output(0, "  \n", "");
    assert!(cmd_stdout(Ok(output)).is_none());
}

// --- parse_json_warn ---

#[test]
fn parse_json_warn_parses_valid_json() {
    let stdout = br#"{"key": "value"}"#;
    let result: Option<serde_json::Value> = parse_json_warn(stdout, "test parse");
    assert!(result.is_some());
    assert_eq!(result.unwrap()["key"], "value");
}

#[test]
fn parse_json_warn_handles_whitespace_padding() {
    let stdout = b"  [1, 2, 3]  \n";
    let result: Option<Vec<i32>> = parse_json_warn(stdout, "test parse");
    assert_eq!(result.unwrap(), vec![1, 2, 3]);
}

#[test]
fn parse_json_warn_returns_none_on_invalid_json() {
    let stdout = b"not json at all";
    let result: Option<serde_json::Value> = parse_json_warn(stdout, "test parse");
    assert!(result.is_none());
}

#[test]
fn parse_json_warn_truncates_safely_on_multibyte_boundary() {
    // Build invalid JSON >200 bytes with multi-byte chars near the boundary.
    // Each emoji is 4 bytes; 50 emojis = 200 bytes. The "x" pushes byte 200
    // into the middle of the last emoji, which would panic with &raw[..200].
    let mut data = String::from("x");
    for _ in 0..50 {
        data.push('\u{1F600}'); // 4-byte emoji
    }
    let result: Option<serde_json::Value> = parse_json_warn(data.as_bytes(), "test truncation");
    assert!(result.is_none()); // must not panic
}
