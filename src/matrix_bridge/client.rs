use reqwest::StatusCode;
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct MatrixClient {
    pub homeserver_url: String,
    homeserver_domain: String,
    pub access_token: String,
}

impl MatrixClient {
    pub fn new(homeserver_url: impl Into<String>, access_token: impl Into<String>) -> Self {
        let homeserver_url = homeserver_url.into().trim_end_matches('/').to_string();
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

        let response = self
            .http_client()
            .post(self.api_url("/_matrix/client/v3/register"))
            .bearer_auth(&self.access_token)
            .json(&json!({
                "auth": { "type": "m.login.application_service" },
                "username": username,
            }))
            .send()
            .map_err(|e| format!("failed to register virtual user '{username}': {e}"))?;

        if response.status().is_success() {
            return Ok(());
        }

        let status = response.status();
        let error = parse_error(response);
        if status == StatusCode::BAD_REQUEST
            && error
                .get("errcode")
                .and_then(Value::as_str)
                .is_some_and(|code| code == "M_USER_IN_USE")
        {
            return Ok(());
        }

        Err(format!(
            "failed to ensure virtual user '{username}': {status} {}",
            error
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("no details")
        ))
    }

    pub fn ensure_room_exists(&self, channel_name: &str) -> Result<String, String> {
        if channel_name.trim().is_empty() {
            return Err("channel name cannot be empty".to_string());
        }

        let room_alias = format!("#{}:{}", channel_name, self.homeserver_domain);
        let directory_url = format!(
            "{}/_matrix/client/v3/directory/room/{}",
            self.homeserver_url,
            percent_encode(&room_alias),
        );
        let directory = self
            .http_client()
            .get(&directory_url)
            .bearer_auth(&self.access_token)
            .send()
            .map_err(|e| format!("failed to check room alias '{room_alias}': {e}"))?;

        if directory.status().is_success() {
            let directory = parse_json::<Value>(directory)?;
            if let Some(room_id) = directory.get("room_id").and_then(Value::as_str) {
                return Ok(room_id.to_string());
            }
        } else if directory.status() != StatusCode::NOT_FOUND {
            let status = directory.status();
            let error = parse_error(directory);
            return Err(format!(
                "failed to lookup room alias '{room_alias}': {status} {}",
                error
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("no details"),
            ));
        }

        let create_url = self.api_url("/_matrix/client/v3/createRoom");
        let created = self
            .http_client()
            .post(&create_url)
            .bearer_auth(&self.access_token)
            .json(&json!({
                "name": room_alias,
                "room_alias_name": channel_name,
                "preset": "private_chat",
                "visibility": "private",
            }))
            .send()
            .map_err(|e| format!("failed to create room '{room_alias}': {e}"))?;

        if !created.status().is_success() {
            let status = created.status();
            let error = parse_error(created);
            return Err(format!(
                "failed to create room '{channel_name}': {status} {}",
                error
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("no details"),
            ));
        }

        let created = parse_json::<Value>(created)?;
        let room_id = created
            .get("room_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "room creation response missing room_id".to_string())?;

        Ok(room_id.to_string())
    }

    pub fn send_message(
        &self,
        room_id: &str,
        sender_user_id: &str,
        body: &str,
        thread_parent_event_id: Option<&str>,
    ) -> Result<String, String> {
        if room_id.is_empty() || sender_user_id.is_empty() {
            return Err("room_id and sender_user_id are required".to_string());
        }
        if body.is_empty() {
            return Ok(String::new());
        }

        let thread_parent_event_id = thread_parent_event_id.filter(|id| !id.is_empty());
        let mut payload = json!({
            "msgtype": "m.text",
            "body": body,
        });
        if let Some(parent_event_id) = thread_parent_event_id {
            payload["m.relates_to"] = json!({
                "m.in_reply_to": {
                    "event_id": parent_event_id,
                },
                "rel_type": "m.thread",
            });
        }

        let txn_id = random_txn_id();
        let response = self
            .http_client()
            .put(self.room_send_url(room_id, &txn_id))
            .bearer_auth(&self.access_token)
            .json(&payload)
            .send()
            .map_err(|e| {
                format!("failed to send Matrix message to {room_id} as {sender_user_id}: {e}")
            })?;

        if response.status().is_success() {
            let response = parse_json::<Value>(response)?;
            if let Some(event_id) = response.get("event_id").and_then(Value::as_str) {
                return Ok(event_id.to_string());
            }
            return Ok(String::new());
        }

        let status = response.status();
        let error = parse_error(response);
        Err(format!(
            "failed to send Matrix message to {room_id} as {sender_user_id}: {status} {}",
            error
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("no details"),
        ))
    }

    pub fn post_tool_payload(&self, room_id: &str, payload: &Value) -> Result<String, String> {
        if room_id.is_empty() {
            return Err("room_id is required".to_string());
        }
        if payload.is_null() {
            return Err("payload cannot be null".to_string());
        }

        let payload = serde_json::to_string_pretty(payload)
            .map_err(|e| format!("failed to serialize payload: {e}"))?;
        self.send_message(
            room_id,
            &format!("@midtown:{}", self.homeserver_domain),
            &format!("```json\n{payload}\n```"),
            None,
        )
    }

    pub fn homeserver_domain(&self) -> &str {
        &self.homeserver_domain
    }

    pub fn user_id(&self, username: &str) -> String {
        format!("@{}:{}", username, self.homeserver_domain)
    }

    fn http_client(&self) -> reqwest::blocking::Client {
        reqwest::blocking::Client::new()
    }

    fn api_url(&self, path: &str) -> String {
        format!("{base}{path}", base = self.homeserver_url)
    }

    fn room_send_url(&self, room_id: &str, txn_id: &str) -> String {
        format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{txn_id}",
            self.homeserver_url,
            percent_encode(room_id),
        )
    }
}

fn infer_homeserver_domain(homeserver_url: &str) -> String {
    let without_path = homeserver_url.trim_end_matches('/');
    let without_scheme = without_path
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let without_path = without_scheme.split('/').next().unwrap_or("matrix.local");

    if without_path.is_empty() {
        "matrix.local".to_string()
    } else {
        without_path.to_string()
    }
}

fn parse_error(response: reqwest::blocking::Response) -> Value {
    response
        .json::<Value>()
        .unwrap_or_else(|_| Value::Object(Default::default()))
}

fn parse_json<T: serde::de::DeserializeOwned>(
    response: reqwest::blocking::Response,
) -> Result<T, String> {
    response
        .json::<T>()
        .map_err(|e| format!("failed to parse Matrix API response: {e}"))
}

fn percent_encode(input: &str) -> String {
    let mut encoded = String::with_capacity(input.len() * 3);
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric()
            || byte == b'-'
            || byte == b'_'
            || byte == b'.'
            || byte == b'~'
        {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn random_txn_id() -> String {
    format!(
        "{:08x}{:08x}{:08x}",
        fastrand::u32(..),
        fastrand::u32(..),
        fastrand::u32(..)
    )
}
