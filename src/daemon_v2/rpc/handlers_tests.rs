use super::parse_duration_secs;

#[test]
fn seconds() {
    assert_eq!(parse_duration_secs("30s"), Some(30));
}

#[test]
fn minutes() {
    assert_eq!(parse_duration_secs("5m"), Some(300));
}

#[test]
fn hours() {
    assert_eq!(parse_duration_secs("2h"), Some(7200));
}

#[test]
fn days() {
    assert_eq!(parse_duration_secs("1d"), Some(86400));
}

#[test]
fn whitespace_trimmed() {
    assert_eq!(parse_duration_secs("  10s  "), Some(10));
}

#[test]
fn empty_string() {
    assert_eq!(parse_duration_secs(""), None);
}

#[test]
fn whitespace_only() {
    assert_eq!(parse_duration_secs("   "), None);
}

#[test]
fn unknown_suffix() {
    assert_eq!(parse_duration_secs("5x"), None);
}

#[test]
fn no_suffix() {
    assert_eq!(parse_duration_secs("123"), None);
}

#[test]
fn non_numeric_prefix() {
    assert_eq!(parse_duration_secs("abcs"), None);
}

#[test]
fn zero_value() {
    assert_eq!(parse_duration_secs("0s"), Some(0));
    assert_eq!(parse_duration_secs("0m"), Some(0));
}

#[test]
fn weeks() {
    assert_eq!(parse_duration_secs("1w"), Some(604800));
    assert_eq!(parse_duration_secs("2w"), Some(1209600));
}

#[test]
fn overflow_returns_none() {
    // Values just above the u64 overflow threshold for each multiplier
    assert_eq!(parse_duration_secs("30500568904944w"), None); // 30500568904944 * 604800 > u64::MAX
    assert_eq!(parse_duration_secs("213503982334602d"), None); // 213503982334602 * 86400 > u64::MAX
    assert_eq!(parse_duration_secs("5124095576030432h"), None); // 5124095576030432 * 3600 > u64::MAX
    assert_eq!(parse_duration_secs("307445734561825861m"), None); // 307445734561825861 * 60 > u64::MAX
    // Seconds don't multiply, so u64::MAX itself is valid
    assert_eq!(parse_duration_secs("18446744073709551615s"), Some(u64::MAX));
    // Values just below the threshold still work
    assert_eq!(
        parse_duration_secs("30500568904943w"),
        Some(30500568904943 * 604800)
    );
}
