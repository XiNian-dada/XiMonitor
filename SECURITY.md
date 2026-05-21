# NodeLite Security Notes

本文档描述 **当前实现**、**已知限制**、**运维建议** 与 **后续改进方向**。  
如果这里的描述与代码不一致，应以代码为准，并优先修正文档。

## 当前实现

### Web 面板认证

代码位置：
- `nodelite-server/src/auth.rs`
- `nodelite-server/src/handlers/auth_routes.rs`
- `nodelite-proto/src/config/raw.rs`

当前 Web UI 与受保护 API 走 `[auth]` 配置下的只读 Basic Auth，不再使用旧的环境变量模式。

当前特性：
- Basic Auth 头比较使用 `subtle::ConstantTimeEq`，不是普通 `==`
- 未开启认证时，仅允许 loopback listener 直接放行；公网监听要求显式配置认证
- 密码强度由服务端统一校验：至少 12 字符，并且包含大小写字母、数字、特殊字符
- 常见弱密码会被拒绝
- 启用 `enable_2fa = true` 时，TOTP secret 由服务端保存，验证通过后才会发放已认证会话 cookie

### 2FA 与只读认证限流

代码位置：
- `nodelite-server/src/auth.rs`
- `nodelite-server/src/admission.rs`
- `nodelite-server/src/handlers/auth_routes.rs`

当前 2FA / 只读认证保护包括：
- TOTP 校验仅接受当前 step，不接受前后窗口漂移
- 同一 TOTP step 有重放保护
- pending 2FA session 连续失败达到上限后立即失效
- `/api/verify-2fa` 有独立的 IP 维度失败窗口与封禁
- 只读 Basic Auth 主路径也有独立失败限流
- `/api/settings/*` 等敏感写操作会叠加更严格的失败预算
- 被封禁时返回 `429 Too Many Requests` 与 `Retry-After`

### Agent Token 生命周期

代码位置：
- `nodelite-server/src/registry.rs`
- `nodelite-server/src/registry/token.rs`
- `nodelite-server/src/ws.rs`

当前 Agent 凭证模型：
- `issue-node` 生成 256-bit 随机 token
- 新签发 token 在磁盘上只保存 Argon2id 哈希，不再保存明文
- 旧版本遗留的明文 token 会在加载 registry 时自动迁移到 `token_hash`
- 每个节点维护 `token_generation`，用于在 WebSocket 热路径上判断当前会话是否已被轮换
- token 默认有效期 30 天
- 距离过期不足 7 天时，在线节点可自动续期
- install token 是一次性、短时有效的引导凭证

### 传输层与真实客户端 IP

代码位置：
- `nodelite-proto/src/config.rs`
- `nodelite-proto/src/config/raw.rs`
- `nodelite-server/src/admission.rs`

当前传输与来源识别规则：
- 远程明文 HTTP 只有在显式 opt-in 时才允许
- 开启 2FA 时要求 `public_base_url` 为 HTTPS
- 服务端新增 `server.trusted_proxies` CIDR 配置
- 只有当 TCP peer 是 loopback 或命中 `trusted_proxies` 时，NodeLite 才会信任 `X-Forwarded-For` / `X-Real-IP`
- `X-Forwarded-For` 按从右向左解析，跳过可信代理链，避免伪造左侧来源绕过限流

### 审计日志

代码位置：
- `nodelite-server/src/audit.rs`
- `nodelite-server/src/handlers/api.rs`
- `nodelite-server/src/handlers/auth_routes.rs`

当前审计能力：
- 独立 SQLite 审计库
- 支持记录认证失败、TOTP 成功/失败、Token 误用、限流事件、节点连接事件
- `/api/audit-log` 可按时间、事件类型、成功状态等条件查询
- 审计写入走 best-effort，不反向阻塞主业务路径

### 文件与本地密钥材料保护

代码位置：
- `nodelite-server/src/fs_security.rs`
- `nodelite-server/src/history.rs`
- `nodelite-server/src/audit.rs`
- `nodelite-server/src/registry/storage.rs`

当前本地安全约束：
- registry、history、snapshot、audit 等敏感文件会尽量收紧到私有权限
- 安装脚本默认使用私有 `umask`
- 配置、registry 与数据目录按 root-owned / private mode 方向初始化
- `ServerConfig` / `ReadonlyAuthConfig` 不允许直接整结构序列化回 API，避免把密码或 TOTP secret 泄露到 JSON 输出

## 已知限制

### Basic Auth 仍然是共享凭证模型

当前 UI 认证不是多用户系统，也没有细粒度角色模型。  
如果多个管理员共用一组 Basic Auth 凭证，审计粒度仍然有限。

### 2FA 会话状态保存在服务端内存

服务重启后，pending / authenticated 2FA session 会丢失。  
这是有意的安全取舍，但意味着重启后浏览器需要重新完成认证。

### Agent token 仍然是 bearer secret

虽然 token 已经 hash-at-rest、带有效期且支持续期/轮换，但如果 agent 主机本身被攻破，攻击者在 token 失效前仍可能复用该凭证。

### IP 维度限流仍然有 NAT / 共享出口误差

`trusted_proxies` 解决了远程反代归因问题，但限流粒度仍是 IP。  
在 NAT、大型出口网关或共享代理后，多个真实客户端仍可能共用配额。

### 审计日志覆盖的是关键安全事件，不是全量操作审计

当前审计更偏向认证、限流与 token 生命周期事件，还不是完整的“谁在什么时候修改了每一条配置”的全链路审计系统。

## 运维建议

### 部署建议

- 对公网部署启用 HTTPS / WSS，并把 TLS 终止代理写进 `server.trusted_proxies`
- 为 `[auth]` 使用高强度密码，并开启 2FA
- 只在明确隔离的测试环境下使用远程明文 HTTP
- 将 NodeLite 放在反向代理、防火墙或 WAF 后面，而不是直接裸露到公网

### 凭证与主机保护

- 定期轮换只读密码和节点 token
- 及时删除下线节点，避免长期保留无效凭证
- 限制 `server.toml`、`server.json`、SQLite 数据文件的读取权限
- 如果通过网页执行更新，仍应把宿主机权限边界和 systemd sandbox 一并考虑进去

### 审计与告警

- 启用审计日志并定期检查 `login_failure`、`rate_limit_exceeded`、`token_invalid`
- 对高频失败认证、重复 token 误用、异常节点连接设置告警
- 在反向代理层保留原始访问日志，便于和 NodeLite 审计记录交叉分析

## 后续改进方向

- 更细粒度的管理员身份与会话管理
- 更完整的配置变更审计与差异留痕
- 更强的 token 撤销/批量轮换工具
- 更丰富的上游代理/WAF 部署建议与自动化检查
