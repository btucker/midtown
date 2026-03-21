use ratatui::style::Color;

use super::{get_sender_color_with_leads, parse_css_color};

#[test]
fn test_parse_css_color_valid_hex() {
    assert_eq!(parse_css_color("#ff5f5f"), Some(Color::Rgb(255, 95, 95)));
    assert_eq!(parse_css_color("#000000"), Some(Color::Rgb(0, 0, 0)));
    assert_eq!(parse_css_color("#ffffff"), Some(Color::Rgb(255, 255, 255)));
    assert_eq!(parse_css_color("#5faf5f"), Some(Color::Rgb(95, 175, 95)));
}

#[test]
fn test_parse_css_color_invalid() {
    assert_eq!(parse_css_color("ff5f5f"), None); // missing #
    assert_eq!(parse_css_color("#fff"), None); // too short
    assert_eq!(parse_css_color("#gggggg"), None); // invalid hex
    assert_eq!(parse_css_color(""), None);
}

#[test]
fn test_color_override_takes_precedence() {
    // "park" would normally be Green, but override wins
    let color = get_sender_color_with_leads("park", &[], Some("#ff0000"));
    assert_eq!(color, Color::Rgb(255, 0, 0));
}

#[test]
fn test_color_override_none_falls_through() {
    let color = get_sender_color_with_leads("park", &[], None);
    assert_eq!(color, Color::Green);
}

#[test]
fn test_invalid_color_override_falls_through() {
    let color = get_sender_color_with_leads("park", &[], Some("not-a-color"));
    assert_eq!(color, Color::Green);
}
