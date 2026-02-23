use serde_json::Value;

#[derive(Debug, Clone)]
pub struct MatrixClient {
    pub homeserver_url: String,
    pub access_token: String,
}

impl MatrixClient {
    pub fn new(homeserver_url: impl Into<String>, access_token: impl Into<String>) -> Self {
        Self {
            homeserver_url: homeserver_url.into(),
            access_token: access_token.into(),
        }
    }

    pub fn ensure_virtual_user_exists(&self, username: &str) -> Result<(), String> {
        if username.trim().is_empty() {
            return Err("username cannot be empty".to_string());
        }
        Ok(())
    }

    pub fn ensure_room_exists(&self, channel_name: &str) -> Result<String, String> {
        if channel_name.trim().is_empty() {
            return Err("channel name cannot be empty".to_string());
        }
        Ok(format!("#{}:midtown.local", channel_name))
    }

    pub fn send_message(
        &self,
        room_id: &str,
        sender_user_id: &str,
        body: &str,
    ) -> Result<(), String> {
        if room_id.is_empty() || sender_user_id.is_empty() {
            return Err("room_id and sender_user_id are required".to_string());
        }
        if body.is_empty() {
            return Ok(());
        }
        Ok(())
    }

    pub fn post_tool_payload(&self, room_id: &str, payload: &Value) -> Result<(), String> {
        if room_id.is_empty() {
            return Err("room_id is required".to_string());
        }
        if payload.is_null() {
            return Err("payload cannot be null".to_string());
        }
        Ok(())
    }
}
