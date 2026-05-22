/// Encode bytes as lowercase hexadecimal text.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::{hex_encode, shell_quote};

    #[test]
    fn hex_encode_uses_lowercase_output() {
        assert_eq!(hex_encode(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }

    #[test]
    fn shell_quote_wraps_and_escapes_single_quotes() {
        assert_eq!(shell_quote("plain/path"), "'plain/path'");
        assert_eq!(shell_quote("it's here"), "'it'\\''s here'");
    }
}
