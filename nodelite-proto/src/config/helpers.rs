use ipnet::IpNet;
use url::{Url, form_urlencoded};

use super::{ConfigError, MAX_NODE_TAG_BYTES, MAX_NODE_TAGS};
use crate::netutil::uses_insecure_remote_url;
use crate::validation::{normalize_string_list, validate_non_empty, validate_tag_list};

/// 校验 URL 字段:能被解析,并且采用了允许的协议方案。
pub(super) fn validate_url(field: &str, value: &str, schemes: &[&str]) -> Result<(), ConfigError> {
    let parsed =
        Url::parse(value).map_err(|error| ConfigError::new(format!("invalid {field}: {error}")))?;
    if !schemes.iter().any(|scheme| *scheme == parsed.scheme()) {
        return Err(ConfigError::new(format!(
            "{field} must use one of these schemes: {}",
            schemes.join(", ")
        )));
    }
    Ok(())
}

pub(super) fn normalize_tags(field: &str, values: Vec<String>) -> Result<Vec<String>, ConfigError> {
    let values = normalize_string_list(values);
    validate_tag_list(field, &values, MAX_NODE_TAGS, MAX_NODE_TAG_BYTES)?;
    Ok(values)
}

/// 校验 SHA-256 摘要:长度必须是 64 个十六进制字符。
pub(super) fn validate_sha256(field: &str, value: &str) -> Result<(), ConfigError> {
    validate_non_empty(field, value)?;
    if value.len() != 64 || !value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(ConfigError::new(format!(
            "{field} must be a 64-character hexadecimal SHA-256 digest"
        )));
    }
    Ok(())
}

/// 兼容常见的 TOTP secret 输入形式:
/// - 纯 RFC4648 Base32
/// - 带空格/连字符/小写的手工录入
/// - 直接粘贴 `otpauth://...?...secret=...`
/// - 只粘贴 `secret=...` 这样的查询片段
pub fn normalize_totp_secret(value: &str) -> String {
    let candidate =
        extract_totp_secret_candidate(value).unwrap_or_else(|| value.trim().to_string());
    candidate
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace() && *ch != '-')
        .collect::<String>()
        .to_ascii_uppercase()
}

fn extract_totp_secret_candidate(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if let Ok(url) = Url::parse(trimmed)
        && url.scheme().eq_ignore_ascii_case("otpauth")
    {
        return extract_secret_query_value(url.query().unwrap_or_default());
    }

    if trimmed.contains("secret=") {
        return extract_secret_query_value(trimmed.trim_start_matches('?'));
    }

    None
}

fn extract_secret_query_value(query: &str) -> Option<String> {
    form_urlencoded::parse(query.as_bytes()).find_map(|(key, value)| {
        key.eq_ignore_ascii_case("secret")
            .then(|| value.into_owned())
            .filter(|value| !value.trim().is_empty())
    })
}

fn decode_totp_secret_bytes(normalized: &str) -> Option<Vec<u8>> {
    base32::decode(base32::Alphabet::Rfc4648 { padding: false }, normalized)
        .or_else(|| base32::decode(base32::Alphabet::Rfc4648 { padding: true }, normalized))
}

pub(super) fn validate_totp_secret(field: &str, value: &str) -> Result<(), ConfigError> {
    validate_non_empty(field, value)?;
    let decoded = decode_totp_secret_bytes(value);
    let Some(decoded) = decoded else {
        return Err(ConfigError::new(format!(
            "{field} must be a valid RFC4648 base32 TOTP secret"
        )));
    };
    if decoded.len() < 10 {
        return Err(ConfigError::new(format!(
            "{field} must decode to at least 10 bytes"
        )));
    }
    Ok(())
}

pub(super) fn uses_insecure_remote_public_base_url(public_base_url: &str) -> bool {
    uses_insecure_remote_url(public_base_url, "http")
}

pub(super) fn parse_trusted_proxies(values: Vec<String>) -> Result<Vec<IpNet>, ConfigError> {
    normalize_string_list(values)
        .into_iter()
        .map(|value| {
            value.parse::<IpNet>().map_err(|error| {
                ConfigError::new(format!(
                    "invalid server.trusted_proxies entry {value:?}: {error}"
                ))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use ipnet::IpNet;

    use super::{
        normalize_tags, normalize_totp_secret, parse_trusted_proxies,
        uses_insecure_remote_public_base_url, validate_sha256, validate_totp_secret, validate_url,
    };

    #[test]
    fn validate_url_accepts_allowed_schemes_and_rejects_invalid_inputs() {
        validate_url(
            "server.public_base_url",
            "https://monitor.example.com",
            &["http", "https"],
        )
        .expect("https urls should pass");

        let error = validate_url("agent.server", "ftp://monitor.example.com", &["ws", "wss"])
            .expect_err("unexpected schemes should fail");
        assert_eq!(
            error.to_string(),
            "agent.server must use one of these schemes: ws, wss"
        );

        let error = validate_url("agent.server", "not a url", &["ws"])
            .expect_err("malformed urls should fail");
        assert!(error.to_string().contains("invalid agent.server"));
    }

    #[test]
    fn normalize_tags_trims_sorts_and_validates_lengths() {
        let tags = normalize_tags(
            "agent.tags",
            vec![" edge ".to_string(), "prod".to_string(), "edge".to_string()],
        )
        .expect("small tag lists should pass");
        assert_eq!(tags, vec!["edge".to_string(), "prod".to_string()]);

        let error = normalize_tags("agent.tags", vec!["a".repeat(257)])
            .expect_err("oversized tags should fail");
        assert_eq!(error.to_string(), "agent.tags[0] must be <= 256 bytes");
    }

    #[test]
    fn validate_sha256_rejects_empty_short_and_non_hex_values() {
        validate_sha256(
            "install.agent_release_sha256_x86_64",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("64-character hexadecimal digests should pass");

        let error = validate_sha256("checksum", "")
            .expect_err("blank digests should fail before hex validation");
        assert_eq!(error.to_string(), "checksum must not be empty");

        let error = validate_sha256("checksum", "abcd").expect_err("short digests should fail");
        assert_eq!(
            error.to_string(),
            "checksum must be a 64-character hexadecimal SHA-256 digest"
        );

        let error = validate_sha256("checksum", &format!("{}{}", "g", "0".repeat(63)))
            .expect_err("non-hex digests should fail");
        assert_eq!(
            error.to_string(),
            "checksum must be a 64-character hexadecimal SHA-256 digest"
        );
    }

    #[test]
    fn normalize_totp_secret_extracts_secret_from_supported_forms() {
        assert_eq!(
            normalize_totp_secret(
                " otpauth://totp/NodeLite?issuer=NodeLite&secret=jbsw y3dp-ehpk3pxp "
            ),
            "JBSWY3DPEHPK3PXP"
        );
        assert_eq!(
            normalize_totp_secret("?issuer=NodeLite&secret=abcd-efgh"),
            "ABCDEFGH"
        );
        assert_eq!(normalize_totp_secret(" ab cd-ef "), "ABCDEF");
    }

    #[test]
    fn validate_totp_secret_checks_decoding_and_minimum_length() {
        validate_totp_secret("auth.totp_secret", "JBSWY3DPEHPK3PXP")
            .expect("10-byte decoded secrets should pass");

        let error = validate_totp_secret("auth.totp_secret", "NOT-BASE32!!!")
            .expect_err("invalid base32 should fail");
        assert_eq!(
            error.to_string(),
            "auth.totp_secret must be a valid RFC4648 base32 TOTP secret"
        );

        let error = validate_totp_secret("auth.totp_secret", "JBSWY3DP")
            .expect_err("decoded secrets shorter than 10 bytes should fail");
        assert_eq!(
            error.to_string(),
            "auth.totp_secret must decode to at least 10 bytes"
        );
    }

    #[test]
    fn insecure_public_base_url_only_flags_remote_http() {
        assert!(uses_insecure_remote_public_base_url(
            "http://monitor.example.com"
        ));
        assert!(!uses_insecure_remote_public_base_url(
            "https://monitor.example.com"
        ));
        assert!(!uses_insecure_remote_public_base_url(
            "http://127.0.0.1:8080"
        ));
    }

    #[test]
    fn parse_trusted_proxies_normalizes_and_parses_networks() {
        let proxies = parse_trusted_proxies(vec![
            " 10.0.0.0/8 ".to_string(),
            "192.168.0.0/16".to_string(),
            "10.0.0.0/8".to_string(),
        ])
        .expect("valid proxy cidrs should pass");
        assert_eq!(
            proxies,
            vec![
                "10.0.0.0/8".parse::<IpNet>().expect("cidr should parse"),
                "192.168.0.0/16"
                    .parse::<IpNet>()
                    .expect("cidr should parse"),
            ]
        );

        let error = parse_trusted_proxies(vec!["bad-cidr".to_string()])
            .expect_err("invalid proxy cidrs should fail");
        assert!(
            error
                .to_string()
                .contains("invalid server.trusted_proxies entry \"bad-cidr\"")
        );
    }
}
