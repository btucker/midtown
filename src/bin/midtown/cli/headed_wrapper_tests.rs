use super::apply_submit_key;

#[test]
fn submit_key_appends_carriage_return() {
    assert_eq!(apply_submit_key("hello".to_string(), true), "hello\r");
    assert_eq!(apply_submit_key("hello\n".to_string(), true), "hello\r");
    assert_eq!(apply_submit_key("hello\r\n".to_string(), true), "hello\r");
}

#[test]
fn submit_key_noop_when_submit_false() {
    assert_eq!(apply_submit_key("hello".to_string(), false), "hello");
}
