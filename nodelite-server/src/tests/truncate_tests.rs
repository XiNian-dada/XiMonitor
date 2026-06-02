//! UTF-8 truncation tests.

#[test]
fn truncate_to_byte_boundary_respects_char_boundary() {
    let mut value = "中".repeat(100);
    nodelite_proto::truncate_string_to_byte_boundary(&mut value, 7);
    assert!(value.len() <= 7);
    assert!(value.is_char_boundary(value.len()));
    assert!(value.chars().all(|ch| ch == '中'));

    let mut short = "abc".to_string();
    nodelite_proto::truncate_string_to_byte_boundary(&mut short, 16);
    assert_eq!(short, "abc");
}

#[test]
fn truncate_to_byte_boundary_handles_utf8_widths_with_bounded_scan() {
    let cases = [
        ("aé", 2, "a"),
        ("ab中", 4, "ab"),
        ("abc🦀", 6, "abc"),
        ("🦀", 0, ""),
    ];

    for (input, max_bytes, expected) in cases {
        let mut value = input.to_string();
        nodelite_proto::truncate_string_to_byte_boundary(&mut value, max_bytes);

        assert_eq!(value, expected);
        assert!(value.len() <= max_bytes);
        assert!(value.is_char_boundary(value.len()));
    }
}
