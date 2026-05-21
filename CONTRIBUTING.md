# 贡献指南

感谢你对 NodeLite 的关注！我们欢迎各种形式的贡献。

## 行为准则

请遵守 [Rust Code of Conduct](https://www.rust-lang.org/policies/code-of-conduct)。

## 如何贡献

### 报告 Bug

在提交 issue 前，请：
1. 搜索现有 issues，避免重复
2. 使用最新版本复现问题
3. 提供详细的复现步骤

**Bug 报告应包含：**
- 环境信息（OS、Rust 版本）
- 复现步骤
- 预期行为 vs 实际行为
- 相关日志和错误信息

### 提出功能建议

功能建议应：
1. 说明使用场景和动机
2. 描述期望的行为
3. 考虑对现有功能的影响
4. 提供可能的实现思路（可选）

### 提交代码

#### 开发环境搭建

**前置要求：**
- Rust stable（请以 CI 当前使用的 stable 工具链为准；由于 workspace 使用 `edition = "2024"`，不要使用过旧的 Rust 版本）
- SQLite 3.x
- Git

**克隆仓库：**
```bash
git clone https://github.com/XiNian-dada/NodeLite.git
cd NodeLite
```

**构建项目：**
```bash
# 开发构建
cargo build

# 运行测试
cargo test

# 运行 clippy
cargo clippy -- -D warnings

# 格式化代码
cargo fmt
```

**运行服务：**
```bash
# 服务端
cargo run -p nodelite-server -- --config server.example.toml

# Agent
cargo run -p nodelite-agent -- --config agent.example.toml
```

#### 代码风格

**自动格式化：**
```bash
cargo fmt --all
```

**Linting：**
```bash
cargo clippy --all-targets --all-features -- -D warnings
```

**关键约定：**
- 使用 `?` 传播错误，避免 `.unwrap()`/`.expect()`
- 所有公开 API 必须有文档注释
- 新功能必须有测试
- 单个文件不超过 800 行

详见 [CLAUDE.md](CLAUDE.md)。

#### 测试

**运行所有测试：**
```bash
cargo test --workspace
```

**运行单个测试：**
```bash
cargo test test_name -- --nocapture
```

**测试覆盖率：**
```bash
cargo install cargo-tarpaulin
cargo tarpaulin --out Html --workspace
```

**测试要求：**
- 新功能必须有单元测试
- Bug 修复必须有回归测试
- 测试覆盖率不能降低
- 关键模块（auth、registry、ws）覆盖率 > 85%

#### Commit Message 规范

格式：
```
<type>(<scope>): <subject>

<body>

<footer>
```

**Type:**
- `feat` - 新功能
- `fix` - Bug 修复
- `docs` - 文档
- `style` - 格式（不影响代码逻辑）
- `refactor` - 重构
- `perf` - 性能优化
- `test` - 测试
- `chore` - 构建/工具

**Scope:**
- `server` - 服务端
- `agent` - Agent 端
- `proto` - 协议定义
- `auth` - 认证模块
- `ws` - WebSocket
- `ui` - Web UI

**示例：**
```
fix(auth): prevent timing attack in password verification

Use constant-time comparison from subtle crate instead of
direct byte comparison to prevent timing-based attacks.

Closes #123
```

#### Pull Request 流程

1. **Fork 仓库并创建分支**
   ```bash
   git checkout -b feature/my-feature
   ```

2. **开发并提交**
   ```bash
   # 开发代码
   git add .
   git commit -m "feat(scope): description"
   ```

3. **确保通过所有检查**
   ```bash
   cargo fmt --all --check
   cargo clippy --all-targets -- -D warnings
   cargo test --workspace
   ```

4. **推送并创建 PR**
   ```bash
   git push origin feature/my-feature
   ```

5. **PR 描述应包含：**
   - 变更摘要
   - 动机和背景
   - 测试计划
   - 相关 issue（如有）

**PR 模板：**
```markdown
## 变更摘要
简要描述这个 PR 做了什么。

## 动机
为什么需要这个变更？解决了什么问题？

## 变更内容
- 添加了 X 功能
- 修复了 Y bug
- 重构了 Z 模块

## 测试
- [ ] 添加了单元测试
- [ ] 添加了集成测试
- [ ] 手动测试通过
- [ ] 测试覆盖率未降低

## 检查清单
- [ ] 代码已格式化 (`cargo fmt`)
- [ ] 通过 clippy 检查 (`cargo clippy`)
- [ ] 所有测试通过 (`cargo test`)
- [ ] 更新了相关文档
- [ ] 遵循了 commit message 规范

## 相关 Issue
Closes #issue_number
```

#### 代码审查

PR 会经过以下检查：
- ✅ CI 自动测试通过
- ✅ 代码风格符合规范
- ✅ 测试覆盖充分
- ✅ 文档完整
- ✅ 至少一位维护者审查通过

**审查关注点：**
- 代码正确性和安全性
- 性能影响
- 向后兼容性
- 测试充分性
- 文档完整性

## 开发工作流

### 添加新功能

1. 创建 issue 讨论设计
2. 等待维护者反馈
3. Fork 并创建功能分支
4. 实现功能并添加测试
5. 提交 PR
6. 根据审查意见修改
7. 合并

### 修复 Bug

1. 创建 issue 描述 bug（如不存在）
2. Fork 并创建修复分支
3. 添加回归测试
4. 修复 bug
5. 提交 PR
6. 合并

### 改进文档

文档改进可以直接提交 PR，无需事先创建 issue。

## 发布流程

（仅维护者）

1. 更新 CHANGELOG.md
2. 更新版本号（`Cargo.toml`）
3. 创建 git tag
4. 推送 tag 触发 CI 发布

### 更新 CHANGELOG.md

每次提交如果涉及用户可见的变更，请在 CHANGELOG.md 的 `[Unreleased]` 部分添加说明。

发布新版本时：
1. 将 `[Unreleased]` 内容移到新版本号下
2. 更新版本号和日期
3. 创建 git tag

**CHANGELOG 格式：**
```markdown
## [Unreleased]

### Added
- 新增功能

### Changed
- 功能变更

### Deprecated
- 即将废弃的功能

### Removed
- 移除的功能

### Fixed
- Bug 修复

### Security
- 安全修复
```

遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.0.0/) 格式。

## 获取帮助

- 📖 阅读 [README.md](README.md)
- 📖 阅读 [CLAUDE.md](CLAUDE.md)
- 💬 在 issue 中提问
- 💬 在 [Discussions](https://github.com/XiNian-dada/NodeLite/discussions) 中讨论

## 许可证

贡献的代码将采用 MIT 许可证。
