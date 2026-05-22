//! 共享文本裁剪工具。

/// Return a prefix no longer than `max_bytes`, ending at a UTF-8 character boundary.
pub fn truncate_to_byte_boundary(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }

    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// Truncate a `String` in place to at most `max_bytes`, preserving UTF-8 validity.
pub fn truncate_string_to_byte_boundary(value: &mut String, max_bytes: usize) {
    let cutoff = truncate_to_byte_boundary(value, max_bytes).len();
    value.truncate(cutoff);
}

#[cfg(test)]
mod tests {
    use super::{truncate_string_to_byte_boundary, truncate_to_byte_boundary};

    #[test]
    fn truncate_to_byte_boundary_returns_valid_prefix() {
        assert_eq!(truncate_to_byte_boundary("日志abcdef", 4), "日");
        assert_eq!(truncate_to_byte_boundary("abc", 16), "abc");
        assert_eq!(truncate_to_byte_boundary("🦀", 0), "");
    }

    #[test]
    fn truncate_string_to_byte_boundary_mutates_in_place() {
        let mut value = "ab中".to_string();
        truncate_string_to_byte_boundary(&mut value, 4);

        assert_eq!(value, "ab");
    }
}
