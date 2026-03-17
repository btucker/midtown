use super::RemindCommand;
use clap::Parser;

/// Helper to parse RemindCommand from args
#[derive(Parser)]
struct TestCli {
    #[command(subcommand)]
    cmd: RemindCommand,
}

#[test]
fn test_parse_cron_subcommand() {
    let cli = TestCli::parse_from(["test", "cron", "0 9 * * MON", "standup time"]);
    match cli.cmd {
        RemindCommand::Cron {
            cron_expr,
            message,
            repeat,
        } => {
            assert_eq!(cron_expr, "0 9 * * MON");
            assert_eq!(message, "standup time");
            assert_eq!(repeat, -1, "Cron default repeat should be -1 (indefinite)");
        }
        _ => panic!("Expected Cron subcommand"),
    }
}

#[test]
fn test_parse_cron_with_repeat() {
    let cli = TestCli::parse_from([
        "test",
        "cron",
        "*/5 * * * *",
        "check status",
        "--repeat",
        "3",
    ]);
    match cli.cmd {
        RemindCommand::Cron { repeat, .. } => {
            assert_eq!(repeat, 3);
        }
        _ => panic!("Expected Cron subcommand"),
    }
}

#[test]
fn test_parse_all_work_merged_default_repeat() {
    let cli = TestCli::parse_from(["test", "all-work-merged", "deploy"]);
    match cli.cmd {
        RemindCommand::AllWorkMerged { message, repeat } => {
            assert_eq!(message, "deploy");
            assert_eq!(repeat, 0, "AllWorkMerged default repeat should be 0 (once)");
        }
        _ => panic!("Expected AllWorkMerged subcommand"),
    }
}

#[test]
fn test_parse_repeat_negative_one() {
    let cli = TestCli::parse_from(["test", "all-work-merged", "forever", "--repeat", "-1"]);
    match cli.cmd {
        RemindCommand::AllWorkMerged { repeat, .. } => {
            assert_eq!(repeat, -1);
        }
        _ => panic!("Expected AllWorkMerged subcommand"),
    }
}
