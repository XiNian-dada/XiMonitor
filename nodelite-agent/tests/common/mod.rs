//! 集成测试共享辅助。
//!
//! 每个集成测试文件以 `mod common;` 引入;并非每个文件都会用到全部条目,
//! 因此放开 dead_code 警告。

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// RAII 临时目录:构造时创建唯一目录,析构时递归删除。
///
/// 相比"测试末尾手动 `remove_dir_all`"的写法,Drop 在 unwind(panic)时
/// 仍会执行,因此断言失败也不会泄漏临时目录。唯一性由进程 PID + 进程内
/// 原子计数器保证,无需读取系统时钟(从而避免 `SystemTime` 上的 unwrap)。
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(prefix: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create unique temp dir for integration test");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
