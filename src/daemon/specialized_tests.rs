use super::*;

// Test role for unit testing
struct TestRole {
    model: String,
    persist: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct TestRequest {
    query: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct TestResponse {
    answer: String,
}

impl SpecializedRole for TestRole {
    type Request = TestRequest;
    type Response = TestResponse;

    fn role_name(&self) -> &'static str {
        "test-role"
    }

    fn system_prompt(&self) -> String {
        "You are a test assistant.".to_string()
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn persist_session(&self) -> bool {
        self.persist
    }

    fn format_request(&self, request: &Self::Request) -> String {
        format!("Query: {}", request.query)
    }

    fn parse_response(&self, raw: &str) -> Result<Self::Response, String> {
        if raw.trim().is_empty() {
            return Err("Empty response".to_string());
        }
        Ok(TestResponse {
            answer: raw.trim().to_string(),
        })
    }
}

#[test]
fn test_role_trait_basics() {
    let role = TestRole {
        model: "haiku".to_string(),
        persist: false,
    };

    assert_eq!(role.role_name(), "test-role");
    assert_eq!(role.model(), "haiku");
    assert!(!role.persist_session());
    assert_eq!(role.max_budget_usd(), 0.50); // default
    assert!(role.allow_tools()); // default

    let request = TestRequest {
        query: "What is 2+2?".to_string(),
    };
    let formatted = role.format_request(&request);
    assert_eq!(formatted, "Query: What is 2+2?");

    let response = role.parse_response("4").unwrap();
    assert_eq!(response.answer, "4");

    let err = role.parse_response("").unwrap_err();
    assert_eq!(err, "Empty response");
}

#[test]
fn test_role_with_custom_budget() {
    struct CustomBudgetRole;

    impl SpecializedRole for CustomBudgetRole {
        type Request = TestRequest;
        type Response = TestResponse;

        fn role_name(&self) -> &'static str {
            "custom-budget"
        }

        fn system_prompt(&self) -> String {
            "Test".to_string()
        }

        fn model(&self) -> &str {
            "opus"
        }

        fn persist_session(&self) -> bool {
            false
        }

        fn max_budget_usd(&self) -> f64 {
            1.0 // Override default
        }

        fn format_request(&self, _request: &Self::Request) -> String {
            "test".to_string()
        }

        fn parse_response(&self, raw: &str) -> Result<Self::Response, String> {
            Ok(TestResponse {
                answer: raw.to_string(),
            })
        }
    }

    let role = CustomBudgetRole;
    assert_eq!(role.max_budget_usd(), 1.0);
}

#[test]
fn test_is_corruption_error() {
    let normal_error = std::io::Error::other("Some error");
    assert!(!SpecializedCoworker::is_corruption_error(&normal_error));

    let corruption_error = std::io::Error::other("Tool names must be unique: duplicate 'foo'");
    assert!(SpecializedCoworker::is_corruption_error(&corruption_error));
}

#[test]
fn test_specialized_result_serialization() {
    let result = SpecializedResult {
        response: TestResponse {
            answer: "42".to_string(),
        },
        session_id: Some("test-session".to_string()),
        cost_usd: 0.05,
        duration_ms: 1500,
    };

    let json = serde_json::to_string(&result).unwrap();
    assert!(json.contains("\"answer\":\"42\""));
    assert!(json.contains("\"cost_usd\":0.05"));
    assert!(json.contains("\"duration_ms\":1500"));
}

#[test]
fn test_specialized_error_display() {
    let err = SpecializedError::ParseError("Invalid JSON".to_string());
    assert_eq!(err.to_string(), "Response parsing failed: Invalid JSON");

    let err = SpecializedError::SessionError("test error".to_string());
    assert_eq!(err.to_string(), "Session returned error: test error");

    let err = SpecializedError::Timeout(Duration::from_secs(120));
    assert_eq!(err.to_string(), "Session timed out after 120s");

    let err = SpecializedError::CorruptionRetryFailed;
    assert_eq!(err.to_string(), "Session corruption detected, retry failed");
}
