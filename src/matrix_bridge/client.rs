use serde_json::Value;

#[derive(Debug, Clone)]
pub struct MatrixClient {
    pub homeserver_url: String,
    homeserver_domain: String,
    pub access_token: String,
}

impl MatrixClient {
    pub fn new(homeserver_url: impl Into<String>, access_token: impl Into<String>) -> Self {
        let homeserver_url = homeserver_url.into();
        let homeserver_domain = infer_homeserver_domain(&homeserver_url);
        Self {
            homeserver_url,
            homeserver_domain,
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
        Ok(format!("#{}:{}", channel_name, self.homeserver_domain))
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

fn infer_homeserver_domain(homeserver_url: &str) -> String {
    let without_path = homeserver_url.trim_end_matches('/')
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or("matrix.local");
    if without_path.is_empty() {
        "matrix.local".to_string()
    } else {
        without_path.to_string()
    }
}
