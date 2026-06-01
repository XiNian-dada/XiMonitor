use anyhow::{Result, anyhow};
use tracing::warn;

/// 安装 rustls 默认的密码套件提供者(ring 后端)。
pub fn install_rustls_crypto_provider() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow!("failed to install rustls crypto provider"))
}

/// 获取 Agent 版本号:优先使用打包时通过环境变量注入的版本,缺失则回退到 Cargo 包版本。
pub fn agent_build_version() -> &'static str {
    option_env!("NODELITE_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// 初始化 `tracing` 日志:支持通过 `RUST_LOG` 调整级别。
pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nodelite_agent=info".into()),
        )
        .with_target(false)
        .compact()
        .init();
}

/// 等待 SIGTERM / SIGINT,任一信号到达即返回。
///
/// 仅在 unix 上监听 SIGTERM;其它平台只听 Ctrl-C。注册失败时退化为 `pending`,
/// 保证另一条信号路径仍能触发 —— 不会因为某个 handler 安装失败而吞掉所有信号。
pub async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            warn!(error = ?error, "failed to listen for ctrl-c");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(error) => {
                warn!(error = ?error, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
