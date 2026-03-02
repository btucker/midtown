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

    // Warn if the daemon is already running a headless lead session
    if let Ok(client) = crate::client::DaemonClient::connect()
        && let Ok(super::Response::Message { message }) = client.status()
        && message.contains("Lead:")
        && !message.contains("Lead: not running")
    {
        eprintln!(
            "Warning: A daemon-managed lead session is already running.\n\
             This headed session will be independent. Use `midtown session attach lead`\n\
             to attach to the daemon session instead, or proceed to start a separate one."
        );
    }

    let mut config = midtown::launch::LaunchConfig::lead(&repo_name, channel);
    config.session_mode = midtown::launch::SessionMode::Resume;

    // Load channel notes into domain_context for channel leads
    if let Some(channel_name) = channel {
        let base_dir = midtown::paths::projects_dir_for_repo(&repo_name);
        let notes = midtown::load_channel_notes(&base_dir, channel_name);
        if !notes.is_empty()
            && let midtown::launch::CoworkerRole::ChannelLead {
                ref mut domain_context,
                ..
            } = config.role
        {
            *domain_context = notes;
        }
    }

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

    // Write initial prompt to temp file (gives lead its first instructions on fresh sessions)
    let initial_prompt_file = if let Some(ref prompt_text) = config.initial_prompt {
        let path =
            std::env::temp_dir().join(format!("midtown-lead-initial-{}.md", std::process::id()));
        std::fs::write(&path, prompt_text)
            .map_err(|e| format!("Failed to write initial prompt: {}", e))?;
        Some(path)
    } else {
        None
    };

    // Ensure plugins/skills are installed before launching
    if let Err(e) = midtown::platform_launch::run_platform_prelaunch_hook(config.auth_provider) {
        eprintln!(
            "Warning: Platform pre-launch hook failed (continuing): {}",
            e
        );
    }

    // Build the full shell command (env vars + sandbox + CLI args)
    let launch = config.to_shell_command(
        &settings_file,
        &prompt_file,
        initial_prompt_file.as_deref(),
        &cwd,
        &repo_name,
    );

    // exec() replaces this process — this line never returns on success
    let err = std::process::Command::new("sh")
        .arg("-lc")
        .arg(&launch.shell_command)
        .exec();
    Err(format!("Failed to exec: {}", err))
}
