use std::collections::BTreeMap;
use twin_core::scenario::validate_response;

#[test]
fn response_status_must_be_a_final_three_digit_code() {
    for status in [0, 99, 100, 199, 1000, u16::MAX] {
        assert_eq!(
            validate_response(status, &BTreeMap::new(), None, None),
            Err("invalid final response status".to_owned())
        );
    }
    for status in [200, 204, 400, 429, 529, 599, 999] {
        validate_response(status, &BTreeMap::new(), None, None).expect("valid status");
    }
}

#[test]
fn response_headers_must_have_valid_names_and_values() {
    for (name, value) in [("bad name", "ok"), ("x-test", "bad\r\nvalue")] {
        let headers = BTreeMap::from([(name.to_owned(), value.to_owned())]);
        assert_eq!(
            validate_response(200, &headers, None, None),
            Err("invalid response header".to_owned())
        );
    }
    assert_eq!(
        validate_response(200, &BTreeMap::new(), Some("bad\nvalue"), None),
        Err("invalid content type".to_owned())
    );
    assert_eq!(
        validate_response(429, &BTreeMap::new(), None, Some("bad\nvalue")),
        Err("invalid Retry-After header".to_owned())
    );
    validate_response(
        429,
        &BTreeMap::from([("x-test".to_owned(), "value".to_owned())]),
        Some("application/json; charset=utf-8"),
        Some("Wed, 21 Oct 2015 07:28:00 GMT"),
    )
    .expect("valid headers");
}
