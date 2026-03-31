/// Upload endpoint must return `path` (full filesystem path) in addition to `filename`.
/// Regression test: previously only returned `filename`, causing the web UI to show
/// "📎 undefined" because `result.path` was missing.
#[tokio::test]
async fn upload_response_includes_path_and_filename() {
    use axum::routing::post;

    let app: axum::Router = axum::Router::new().route("/api/upload", post(super::routes::upload));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    let client = reqwest::Client::new();
    let form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(b"fake-png-data".to_vec())
            .file_name("test-upload-image.png")
            .mime_str("image/png")
            .unwrap(),
    );
    let resp = client
        .post(format!("http://{addr}/api/upload"))
        .multipart(form)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(json["ok"], true);
    assert_eq!(json["filename"], "test-upload-image.png");
    assert!(
        json["path"]
            .as_str()
            .is_some_and(|p| p.contains("test-upload-image.png")),
        "response must include 'path' with the file location, got: {json}"
    );

    // Clean up
    if let Some(path) = json["path"].as_str() {
        let _ = std::fs::remove_file(path);
    }
}
