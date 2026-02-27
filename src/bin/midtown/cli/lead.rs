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

/// Boot a headed (interactive terminal) lead session.
///
/// Builds a `LaunchConfig::lead()`, writes settings and system prompt files,
/// then `exec()`s into the `claude` CLI — replacing this process entirely.
/// Uses `SessionMode::Resume` so it continues an existing session or starts fresh.
pub fn handle_lead_boot(channel: Option<&str>) -> Result<(), String> {
    use std::os::unix::process::CommandExt;

    let repo_name = midtown::paths::detect_repo_name()
        .ok_or("Not in a git repository. Run from a git repo.")?;

    let mut config = midtown::launch::LaunchConfig::lead(&repo_name, channel);
    config.session_mode = midtown::launch::SessionMode::Resume;

    // Resolve auth profile for this project/provider
    let profile_dir = midtown::auth::active_profile_dir_for_project_with_provider(
        &repo_name,
        config.auth_provider,
    );
    config.auth_profile_dir = Some(profile_dir);

    // Generate the system prompt based on role
    let system_prompt = match &config.role {
        midtown::launch::CoworkerRole::Lead => midtown::agents::main_lead_system_prompt(&repo_name),
        midtown::launch::CoworkerRole::ChannelLead {
            channel_name,
            domain_context,
        } => midtown::agents::channel_lead_system_prompt(channel_name, domain_context, &repo_name),
        _ => unreachable!("LaunchConfig::lead() always produces Lead or ChannelLead role"),
    };

    // Write system prompt to temp file (headed sessions use $(cat ...) for shell expansion)
    let prompt_file =
        std::env::temp_dir().join(format!("midtown-lead-prompt-{}.md", std::process::id()));
    std::fs::write(&prompt_file, &system_prompt)
        .map_err(|e| format!("Failed to write system prompt: {}", e))?;

    // Write role-appropriate settings file
    let settings_file = if matches!(config.role, midtown::launch::CoworkerRole::Lead) {
        midtown::settings::write_lead_settings_file()
    } else {
        midtown::settings::write_coworker_settings_file()
    }
    .map_err(|e| format!("Failed to write settings: {}", e))?;

    let cwd =
        std::env::current_dir().map_err(|e| format!("Failed to get current directory: {}", e))?;

    // Build the full shell command (env vars + sandbox + CLI args)
    let launch = config.to_shell_command(&settings_file, &prompt_file, None, &cwd, &repo_name);

    // exec() replaces this process — this line never returns on success
    let err = std::process::Command::new("sh")
        .arg("-lc")
        .arg(&launch.shell_command)
        .exec();
    Err(format!("Failed to exec: {}", err))
}
