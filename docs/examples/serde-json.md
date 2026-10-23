# Pinned serde_json API examples

These examples check serde_json 1.0.151 from a separate committed lockfile.
They do not claim compatibility with every serde_json release.

```rust
use serde_json::{Value, json};
fn parse_count(raw: &str) -> serde_json::Result<u64> {
    let value: Value = serde_json::from_str(raw)?;
    Ok(value["count"].as_u64().unwrap_or(0))
}
fn main() {
    let encoded = serde_json::to_string(&json!({"count": 3})).unwrap();
    assert_eq!(parse_count(&encoded).unwrap(), 3);
}
#[test]
fn malformed_input_is_an_error() {
    assert!(parse_count("{").is_err());
    assert_eq!(parse_count(r#"{"count":7}"#).unwrap(), 7);
}
```

`from_str` returns a result; it is not itself a JSON value.

```rust,compile_fail,E0308
fn main() {
    let value: serde_json::Value = serde_json::from_str("null");
    println!("{value}");
}
```

Do not execute a program merely because its compilation succeeded.

```rust,no_run
fn main() {
    let _: serde_json::Value = serde_json::from_str("null").unwrap();
    panic!("this example is compile-only");
}
```
