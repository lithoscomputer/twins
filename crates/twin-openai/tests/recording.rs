use twin_openai::record::request_hash;

#[test]
fn request_hash_ignores_whitespace_and_recursive_object_key_order() {
    let first = br#"{"model":"gpt-test","messages":[{"role":"user","content":"hello"}],"metadata":{"b":2,"a":1}}"#;
    let reordered = br#"{
        "metadata": {"a": 1, "b": 2},
        "messages": [{"content": "hello", "role": "user"}],
        "model": "gpt-test"
    }"#;

    let hash = request_hash(first).expect("valid request JSON");
    assert_eq!(Some(hash), request_hash(reordered));
}

#[test]
fn request_hash_preserves_array_order_and_values() {
    let hash = request_hash(br#"{"input":["first","second"]}"#).expect("valid JSON");
    assert_ne!(
        Some(hash.clone()),
        request_hash(br#"{"input":["second","first"]}"#)
    );
    assert_ne!(Some(hash), request_hash(br#"{"input":["first","third"]}"#));
    assert_eq!(request_hash(b"not JSON"), None);
}
