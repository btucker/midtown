use crate::matrix_bridge::client::MatrixClient;

pub fn run_outbound_sync(_client: &MatrixClient, _project: &str) -> Result<(), String> {
    if _project.trim().is_empty() {
        return Err("project cannot be empty".to_string());
    }
    Ok(())
}
