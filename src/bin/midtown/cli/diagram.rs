use clap::Subcommand;

use super::Response;

#[derive(Subcommand, Clone)]
pub enum DiagramCommand {
    /// Validate mermaid diagram syntax via selkie (reads from stdin)
    Validate,
}

pub fn handle(cmd: &DiagramCommand) -> Result<Response, String> {
    match cmd {
        DiagramCommand::Validate => handle_validate(),
    }
}

fn handle_validate() -> Result<Response, String> {
    use std::io::Read;

    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| format!("Failed to read from stdin: {}", e))?;

    let input = input.trim();
    if input.is_empty() {
        return Err("No input provided. Pipe mermaid source via stdin.".to_string());
    }

    selkie::render::render_text(input).map_err(|e| format!("Invalid mermaid diagram: {}", e))?;

    Ok(Response::Message {
        message: "Valid mermaid diagram.".to_string(),
    })
}
