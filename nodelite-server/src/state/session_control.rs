//! 在线会话控制命令与错误边界。

use chrono::{DateTime, Utc};
use tokio::sync::oneshot;

/// 运行中的 WebSocket 会话可接收的控制命令。
pub(crate) enum SessionCommand {
    RefreshToken {
        response: oneshot::Sender<Result<SessionRefreshReply, String>>,
    },
}

/// 一次在线 token 续期完成后返回给调用方的摘要。
#[derive(Debug, Clone)]
pub(crate) struct SessionRefreshReply {
    pub token_expires_at: DateTime<Utc>,
}

/// 向在线节点下发控制命令时可能遇到的失败类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionCommandError {
    NodeOffline,
    SessionClosed,
}

impl std::fmt::Display for SessionCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NodeOffline => f.write_str("node is offline"),
            Self::SessionClosed => f.write_str("node session is no longer available"),
        }
    }
}

impl std::error::Error for SessionCommandError {}
