//! Web Push notification support for the Midtown PWA.
//!
//! Implements W3C Push API with VAPID authentication, supporting iOS Safari 16.4+.
//! VAPID keys are auto-generated on first use and stored in ~/.midtown/push/.
//! Push subscriptions from clients are stored as JSON in the same directory.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, error, info, warn};

/// VAPID key pair stored on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredVapidKeys {
    /// PEM-encoded ES256 key pair
    pem: String,
}

/// A push subscription from a browser client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushSubscription {
    pub endpoint: String,
    /// Base64url-encoded P-256 public key
    pub p256dh: String,
    /// Base64url-encoded auth secret
    pub auth: String,
}

/// A push notification payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushPayload {
    pub title: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Manages VAPID keys, subscriptions, and sending push notifications.
pub struct PushManager {
    push_dir: PathBuf,
}

impl PushManager {
    /// Create a new PushManager, ensuring the storage directory exists.
    pub fn new() -> std::io::Result<Self> {
        let push_dir = crate::paths::midtown_base_dir().join("push");
        std::fs::create_dir_all(&push_dir)?;
        Ok(Self { push_dir })
    }

    /// Create a PushManager from an explicit path.
    pub fn from_path(push_dir: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&push_dir)?;
        Ok(Self { push_dir })
    }

    /// Get the push directory path.
    pub fn push_dir(&self) -> &std::path::Path {
        &self.push_dir
    }

    /// Get or generate the VAPID key pair.
    fn vapid_keypair(
        &self,
    ) -> Result<jwt_simple::prelude::ES256KeyPair, Box<dyn std::error::Error>> {
        let key_path = self.push_dir.join("vapid_keys.json");

        if key_path.exists() {
            let data = std::fs::read_to_string(&key_path)?;
            let stored: StoredVapidKeys = serde_json::from_str(&data)?;
            let kp = jwt_simple::prelude::ES256KeyPair::from_pem(&stored.pem)?;
            debug!("Loaded existing VAPID keys");
            Ok(kp)
        } else {
            let kp = jwt_simple::prelude::ES256KeyPair::generate();
            let pem = kp.to_pem()?;
            let stored = StoredVapidKeys { pem };
            std::fs::write(&key_path, serde_json::to_string_pretty(&stored)?)?;
            info!("Generated new VAPID key pair at {:?}", key_path);
            Ok(jwt_simple::prelude::ES256KeyPair::from_pem(&stored.pem)?)
        }
    }

    /// Get the VAPID public key in uncompressed base64url format (for the frontend).
    pub fn vapid_public_key_base64(&self) -> Result<String, Box<dyn std::error::Error>> {
        use jwt_simple::prelude::ECDSAP256PublicKeyLike;
        let kp = self.vapid_keypair()?;
        let es256_pk = kp.public_key();
        // Access the underlying P256PublicKey via trait method, then get uncompressed bytes
        let p256_pk = es256_pk.public_key();
        let raw = p256_pk.to_bytes_uncompressed();
        Ok(base64url_encode(&raw))
    }

    /// Path to the subscriptions file.
    fn subscriptions_path(&self) -> PathBuf {
        self.push_dir.join("subscriptions.json")
    }

    /// Load all stored subscriptions.
    pub fn load_subscriptions(&self) -> Vec<PushSubscription> {
        let path = self.subscriptions_path();
        if !path.exists() {
            return Vec::new();
        }
        match std::fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    /// Save subscriptions to disk.
    fn save_subscriptions(&self, subs: &[PushSubscription]) -> std::io::Result<()> {
        let data = serde_json::to_string_pretty(subs).map_err(std::io::Error::other)?;
        std::fs::write(self.subscriptions_path(), data)
    }

    /// Add a new subscription (deduplicates by endpoint).
    pub fn add_subscription(&self, sub: PushSubscription) -> std::io::Result<()> {
        let mut subs = self.load_subscriptions();
        // Remove any existing subscription with the same endpoint (re-subscribe)
        subs.retain(|s| s.endpoint != sub.endpoint);
        subs.push(sub);
        self.save_subscriptions(&subs)
    }

    /// Remove a subscription by endpoint.
    pub fn remove_subscription(&self, endpoint: &str) -> std::io::Result<()> {
        let mut subs = self.load_subscriptions();
        subs.retain(|s| s.endpoint != endpoint);
        self.save_subscriptions(&subs)
    }

    /// Send a push notification to all subscribers.
    /// Returns the number of successful sends and removes expired/invalid subscriptions.
    pub async fn send_to_all(&self, payload: &PushPayload) -> usize {
        let subs = self.load_subscriptions();
        if subs.is_empty() {
            debug!("No push subscriptions to notify");
            return 0;
        }

        let kp = match self.vapid_keypair() {
            Ok(kp) => kp,
            Err(e) => {
                error!("Failed to load VAPID keys: {}", e);
                return 0;
            }
        };

        let json_payload = match serde_json::to_string(payload) {
            Ok(j) => j,
            Err(e) => {
                error!("Failed to serialize push payload: {}", e);
                return 0;
            }
        };

        let client = reqwest::Client::new();
        let mut success_count = 0;
        let mut expired_endpoints = Vec::new();

        for sub in &subs {
            match self.send_one(&client, &kp, sub, &json_payload).await {
                Ok(()) => {
                    success_count += 1;
                    debug!("Push sent to {}", sub.endpoint);
                }
                Err(PushError::Gone) => {
                    info!("Subscription expired, removing: {}", sub.endpoint);
                    expired_endpoints.push(sub.endpoint.clone());
                }
                Err(PushError::Other(e)) => {
                    warn!("Push failed for {}: {}", sub.endpoint, e);
                }
            }
        }

        // Clean up expired subscriptions
        if !expired_endpoints.is_empty() {
            let mut subs = self.load_subscriptions();
            subs.retain(|s| !expired_endpoints.contains(&s.endpoint));
            if let Err(e) = self.save_subscriptions(&subs) {
                warn!("Failed to save subscriptions after cleanup: {}", e);
            }
        }

        info!(
            "Push notifications sent: {}/{} succeeded",
            success_count,
            subs.len()
        );
        success_count
    }

    /// Send a push notification to a single subscriber.
    async fn send_one(
        &self,
        client: &reqwest::Client,
        kp: &jwt_simple::prelude::ES256KeyPair,
        sub: &PushSubscription,
        json_payload: &str,
    ) -> Result<(), PushError> {
        use web_push_native::WebPushBuilder;

        let endpoint: http::Uri = sub
            .endpoint
            .parse()
            .map_err(|e| PushError::Other(format!("Invalid endpoint URI: {}", e)))?;

        let p256dh_bytes = base64url_decode(&sub.p256dh)
            .map_err(|e| PushError::Other(format!("Invalid p256dh: {}", e)))?;
        let ua_public = p256::PublicKey::from_sec1_bytes(&p256dh_bytes)
            .map_err(|e| PushError::Other(format!("Invalid p256dh key: {}", e)))?;

        let auth_bytes = base64url_decode(&sub.auth)
            .map_err(|e| PushError::Other(format!("Invalid auth: {}", e)))?;
        if auth_bytes.len() != 16 {
            return Err(PushError::Other(format!(
                "Auth secret must be 16 bytes, got {}",
                auth_bytes.len()
            )));
        }
        let ua_auth = *web_push_native::Auth::from_slice(&auth_bytes);

        let builder = WebPushBuilder::new(endpoint, ua_public, ua_auth)
            .with_valid_duration(std::time::Duration::from_secs(12 * 3600));

        let signed_builder = builder.with_vapid(kp, "mailto:info@midtown.sh");

        let request = signed_builder
            .build(json_payload.as_bytes())
            .map_err(|e| PushError::Other(format!("Failed to build push request: {}", e)))?;

        // Convert http::Request to reqwest
        let (parts, body) = request.into_parts();
        let url = parts.uri.to_string();
        let mut req = client.post(&url).body(body);
        for (name, value) in &parts.headers {
            req = req.header(name.as_str(), value.to_str().unwrap_or(""));
        }

        let response = req
            .send()
            .await
            .map_err(|e| PushError::Other(format!("HTTP request failed: {}", e)))?;

        let status = response.status().as_u16();
        match status {
            200..=202 => Ok(()),
            404 | 410 => Err(PushError::Gone),
            _ => {
                let body = response.text().await.unwrap_or_default();
                Err(PushError::Other(format!(
                    "Push service returned {}: {}",
                    status, body
                )))
            }
        }
    }
}

enum PushError {
    Gone,
    Other(String),
}

/// Base64url encode without padding.
fn base64url_encode(data: &[u8]) -> String {
    use base64ct::{Base64UrlUnpadded, Encoding};
    Base64UrlUnpadded::encode_string(data)
}

/// Base64url decode (handles both padded and unpadded).
fn base64url_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64ct::{Base64UrlUnpadded, Encoding};
    // Strip any padding characters
    let s = s.trim_end_matches('=');
    Base64UrlUnpadded::decode_vec(s).map_err(|e| format!("base64url decode error: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64url_roundtrip() {
        let data = b"hello world";
        let encoded = base64url_encode(data);
        let decoded = base64url_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_push_payload_serialization() {
        let payload = PushPayload {
            title: "Test".to_string(),
            body: "Hello".to_string(),
            tag: Some("mention".to_string()),
            url: None,
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("\"title\":\"Test\""));
        assert!(json.contains("\"tag\":\"mention\""));
        assert!(!json.contains("\"url\""));
    }

    #[test]
    fn test_push_subscription_deserialization() {
        let json = r#"{
            "endpoint": "https://fcm.googleapis.com/fcm/send/abc123",
            "p256dh": "BNcRdreALRFXTkOOUHK1EtK2wtaz5Ry4YfYCA_0QTpQtUbVlUls0VJXg7A8u-Ts1XbjhazAkj7I99e8p8V-X_IA",
            "auth": "tBHItJI5svbpC7_eFgOn9A"
        }"#;
        let sub: PushSubscription = serde_json::from_str(json).unwrap();
        assert!(sub.endpoint.starts_with("https://"));
        assert!(!sub.p256dh.is_empty());
        assert!(!sub.auth.is_empty());
    }

    #[test]
    fn test_push_manager_creation() {
        // PushManager::new() should succeed (creates ~/.midtown/push/)
        let mgr = PushManager::new().unwrap();
        assert!(mgr.push_dir.ends_with("push"));
    }

    #[test]
    fn test_vapid_key_generation_and_reload() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let mgr = PushManager {
            push_dir: temp_dir.path().to_path_buf(),
        };

        // First call generates keys
        let pub_key1 = mgr.vapid_public_key_base64().unwrap();
        assert!(!pub_key1.is_empty());

        // Second call loads the same keys
        let pub_key2 = mgr.vapid_public_key_base64().unwrap();
        assert_eq!(pub_key1, pub_key2);
    }

    #[test]
    fn test_subscription_storage() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let mgr = PushManager {
            push_dir: temp_dir.path().to_path_buf(),
        };

        // Initially empty
        assert!(mgr.load_subscriptions().is_empty());

        // Add a subscription
        let sub = PushSubscription {
            endpoint: "https://example.com/push/1".to_string(),
            p256dh: "test_key".to_string(),
            auth: "test_auth".to_string(),
        };
        mgr.add_subscription(sub.clone()).unwrap();

        let subs = mgr.load_subscriptions();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].endpoint, "https://example.com/push/1");

        // Re-subscribe with same endpoint replaces
        let sub2 = PushSubscription {
            endpoint: "https://example.com/push/1".to_string(),
            p256dh: "new_key".to_string(),
            auth: "new_auth".to_string(),
        };
        mgr.add_subscription(sub2).unwrap();
        let subs = mgr.load_subscriptions();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].p256dh, "new_key");

        // Add second subscription
        let sub3 = PushSubscription {
            endpoint: "https://example.com/push/2".to_string(),
            p256dh: "key2".to_string(),
            auth: "auth2".to_string(),
        };
        mgr.add_subscription(sub3).unwrap();
        assert_eq!(mgr.load_subscriptions().len(), 2);

        // Remove by endpoint
        mgr.remove_subscription("https://example.com/push/1")
            .unwrap();
        let subs = mgr.load_subscriptions();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].endpoint, "https://example.com/push/2");
    }
}
