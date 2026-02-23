use crate::matrix_bridge::client::MatrixClient;

pub fn run_inbound_sync(_client: &MatrixClient, _room_id: &str) -> Result<(), String> {
    if _room_id.trim().is_empty() {
        return Err("room_id cannot be empty".to_string());
    }
    Ok(())
}
