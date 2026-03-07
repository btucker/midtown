#[test]
fn test_task_id_prefix_stripping() {
    // Verify our prefix-stripping logic
    let id = "!42";
    let stripped = id
        .strip_prefix('#')
        .or_else(|| id.strip_prefix('!'))
        .unwrap_or(id);
    assert_eq!(stripped, "42");

    let id2 = "#42";
    let stripped2 = id2
        .strip_prefix('#')
        .or_else(|| id2.strip_prefix('!'))
        .unwrap_or(id2);
    assert_eq!(stripped2, "42");

    let id3 = "42";
    let stripped3 = id3
        .strip_prefix('#')
        .or_else(|| id3.strip_prefix('!'))
        .unwrap_or(id3);
    assert_eq!(stripped3, "42");
}
