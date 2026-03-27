use std::os::unix::process::ExitStatusExt;
use std::process::{ExitStatus, Output};

use super::check_cmd_output;

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
