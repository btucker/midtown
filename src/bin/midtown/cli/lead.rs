use super::Response;
use crate::RemindCommand;
use crate::client::DaemonClient;

pub fn handle_remind(cmd: &RemindCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        RemindCommand::AllWorkMerged { message } => {
            client.reminder_create("all-work-merged", message)
        }
        RemindCommand::List => client.reminder_list(),
        RemindCommand::Cancel { id } => client.reminder_cancel(id),
    }
}
