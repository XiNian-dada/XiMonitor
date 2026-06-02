//! NodeLite Agent Library.

pub mod collector;
pub mod config_io;
pub mod runtime;
pub mod session;
// `support` 只服务于本 crate 内部(runtime / session),不属于对外测试 API,
// 因此收敛为 pub(crate),避免把内部辅助函数暴露给库的下游使用者。
pub(crate) mod support;
