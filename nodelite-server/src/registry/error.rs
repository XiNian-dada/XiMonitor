use std::fmt;
use std::path::{Path, PathBuf};

/// Registry 模块公开暴露的错误边界。
///
/// 调用方可以匹配领域错误(未授权、token 过期、节点不存在),
/// 其余底层 I/O / 序列化 / 密码学故障则统一落到明确的基础设施错误分支。
#[derive(Debug)]
pub enum RegistryError {
    Unauthorized,
    TokenExpired {
        node_id: String,
    },
    NodeNotFound(String),
    Validation {
        message: String,
    },
    InvalidConfig {
        field: &'static str,
        message: String,
    },
    UnsupportedPublicBaseUrlScheme(String),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    Serialize {
        source: serde_json::Error,
    },
    MutationTask {
        source: tokio::task::JoinError,
    },
    BackgroundTask {
        source: tokio::task::JoinError,
    },
    Internal {
        context: &'static str,
        source: anyhow::Error,
    },
}

pub type RegistryResult<T> = Result<T, RegistryError>;

impl RegistryError {
    pub fn validation(error: impl ToString) -> Self {
        Self::Validation {
            message: error.to_string(),
        }
    }

    pub fn invalid_config(field: &'static str, error: impl ToString) -> Self {
        Self::InvalidConfig {
            field,
            message: error.to_string(),
        }
    }

    pub fn io(operation: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }

    pub fn parse(path: &Path, source: serde_json::Error) -> Self {
        Self::Parse {
            path: path.to_path_buf(),
            source,
        }
    }

    pub fn serialize(source: serde_json::Error) -> Self {
        Self::Serialize { source }
    }

    pub fn mutation_task(source: tokio::task::JoinError) -> Self {
        Self::MutationTask { source }
    }

    pub fn background_task(source: tokio::task::JoinError) -> Self {
        Self::BackgroundTask { source }
    }

    pub fn internal(context: &'static str, source: anyhow::Error) -> Self {
        Self::Internal { context, source }
    }
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => write!(f, "unauthorized"),
            Self::TokenExpired { node_id } => write!(f, "token expired for node {node_id}"),
            Self::NodeNotFound(node_id) => write!(f, "node not found: {node_id}"),
            Self::Validation { message } => write!(f, "{message}"),
            Self::InvalidConfig { field, message } => write!(f, "invalid {field}: {message}"),
            Self::UnsupportedPublicBaseUrlScheme(scheme) => {
                write!(
                    f,
                    "unsupported public_base_url scheme for agent install: {scheme}"
                )
            }
            Self::Io {
                operation, path, ..
            } => {
                write!(
                    f,
                    "registry I/O failed while {operation} {}",
                    path.display()
                )
            }
            Self::Parse { path, .. } => {
                write!(f, "failed to parse node registry {}", path.display())
            }
            Self::Serialize { .. } => write!(f, "failed to serialize node registry"),
            Self::MutationTask { .. } => write!(f, "registry mutation task failed"),
            Self::BackgroundTask { .. } => write!(f, "registry background task failed"),
            Self::Internal { context, .. } => write!(f, "{context}"),
        }
    }
}

impl std::error::Error for RegistryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            Self::Serialize { source } => Some(source),
            Self::MutationTask { source } => Some(source),
            Self::BackgroundTask { source } => Some(source),
            Self::Internal { source, .. } => Some(source.root_cause()),
            Self::Unauthorized
            | Self::TokenExpired { .. }
            | Self::NodeNotFound(_)
            | Self::Validation { .. }
            | Self::InvalidConfig { .. }
            | Self::UnsupportedPublicBaseUrlScheme(_) => None,
        }
    }
}
