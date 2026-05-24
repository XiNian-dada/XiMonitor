//! 配置与注册表共用的轻量校验工具。
//!
//! 这些规则同时服务于:
//! - `config.rs` 对 TOML 配置的解析校验;
//! - `registry.rs` 对节点 / install session / runtime identity 的约束检查。
//!
//! 统一放在这里,可以避免两处实现渐渐漂移。

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    message: String,
}

impl ValidationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ValidationError {}

const IDENTIFIER_MAX_CHARS: usize = 128;

pub fn validate_non_empty(field: &str, value: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Err(ValidationError::new(format!("{field} must not be empty")));
    }
    Ok(())
}

pub fn validate_identifier(field: &str, value: &str) -> Result<(), ValidationError> {
    validate_non_empty(field, value)?;
    if value.len() > IDENTIFIER_MAX_CHARS {
        return Err(ValidationError::new(format!(
            "{field} must be <= {IDENTIFIER_MAX_CHARS} characters"
        )));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(ValidationError::new(format!(
            "{field} must use only ASCII letters, numbers, '-', '_' or '.'"
        )));
    }
    Ok(())
}

pub fn normalize_string_list(values: Vec<String>) -> Vec<String> {
    let mut values: Vec<String> = values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    values.sort();
    values.dedup();
    values
}

pub fn validate_tag_list(
    field: &str,
    values: &[String],
    max_tags: usize,
    max_tag_bytes: usize,
) -> Result<(), ValidationError> {
    if values.len() > max_tags {
        return Err(ValidationError::new(format!(
            "{field} must contain at most {max_tags} tags"
        )));
    }
    for (index, value) in values.iter().enumerate() {
        if value.len() > max_tag_bytes {
            return Err(ValidationError::new(format!(
                "{field}[{index}] must be <= {max_tag_bytes} bytes"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ValidationError, normalize_string_list, validate_identifier, validate_non_empty,
        validate_tag_list,
    };

    #[test]
    fn validation_error_displays_original_message() {
        let error = ValidationError::new("boom");
        assert_eq!(error.to_string(), "boom");
    }

    #[test]
    fn normalize_string_list_trims_sorts_and_deduplicates() {
        let values = normalize_string_list(vec![
            " beta ".to_string(),
            "".to_string(),
            "alpha".to_string(),
            "beta".to_string(),
        ]);
        assert_eq!(values, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn validation_helpers_reject_invalid_values() {
        let error = validate_non_empty("name", "   ").expect_err("blank values should fail");
        assert_eq!(error.to_string(), "name must not be empty");

        let error =
            validate_identifier("node_id", "bad value").expect_err("spaces are not allowed");
        assert!(error.to_string().contains("ASCII letters"));

        let error = validate_tag_list("tags", &[String::from("abcdef")], 4, 5)
            .expect_err("oversized tags should fail");
        assert_eq!(error.to_string(), "tags[0] must be <= 5 bytes");
    }
}
