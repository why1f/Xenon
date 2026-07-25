# Xenon 开发状态与发布规划

更新日期：2026-07-26

## 1. 当前定位

Xenon 是 Rust 编写的多节点 Xray 管理系统：主控使用终端 TUI 管理节点、用户和订阅，Agent 在 Linux 上以内嵌 Xray-core `26.6.27` 提供节点能力。主控不保存 SSH 密码，中转只保存客户端发布地址和端口，不负责中转服务器转发配置。

当前版本为 `0.1.0-alpha.5`。它已经具备完整纵向功能和 Windows 开发环境自动测试，但尚未完成真实 Linux 生产环境的全部验收，因此当前结论是：

- 可以用于本地体验、功能联调和隔离测试环境。
- 可以在自有 Linux 测试 VPS 上进行试运行。
- 不建议立即承载正式付费用户或唯一生产流量。

## 2. 已实现功能

### Panel

- SQLite 单文件数据库、WAL、迁移、计费周期和流量聚合。
- TUI 创建用户与订阅、选择多个节点、额度、到期、重置周期和单双倍计费。
- TUI 总览和节点页提供统一页签、固定状态栏、真实资源仪表、用户流量排行和结构化节点清单。
- TUI 总览提供全节点上/下行实时速率曲线、用户启用/超额/到期摘要和额度百分比。
- 用户首页按 Xray 当前周期计费流量排序，详情显示订阅和节点流量。
- 网卡绑定独立于用户 Xray 统计，只切换 `subscription-userinfo` 的计费来源。
- 节点创建、编辑、启停、证书吊销、逻辑删除和 Agent 安装/升级命令。
- VLESS Base64、Mihomo YAML 和 Sing-box JSON 订阅输出。
- 订阅 HTTPS、IP/Token 双限流和脱敏日志。
- 数据库检查、在线备份、摘要校验、恢复和自动备份保留策略。

### Agent

- tonic gRPC 双向控制流、mTLS Enrollment、证书绑定、轮换和吊销。
- Xray-core 版本严格固定为 `26.6.27`，构建和握手拒绝其他版本。
- `include_bytes!` 嵌入内核，Linux `memfd_create` 执行，不写出 Xray 文件。
- Xray 子进程崩溃监控和自动恢复，Agent 退出时回收子进程。
- Xray API 动态增删用户、重启后恢复期望状态。
- Xray 用户流量差值、网卡绝对计数器、CPU、内存、磁盘和负载采集。
- 未确认流量批次持久化、重放和 Panel 幂等入库。
- 安装器支持 SHA-256 校验、升级失败自动回滚和人工双版本回滚。

### 当前自动验证

- Workspace 38 项测试通过。
- `cargo fmt --all -- --check` 通过。
- `cargo clippy --workspace --all-targets -- -D warnings` 通过。
- TUI 所有主要页面通过 `24x4` 小终端无终端渲染测试。
- TUI 总览和节点页通过带真实模型数据的 `120x36` 内容与选中状态测试。
- Windows 开发 Panel 健康检查可用。

## 3. 尚未完成的生产准入项

以下项目完成前，不应把 `0.1.0-alpha.5` 标为 production-ready：

1. 在目标 Linux 发行版执行 `scripts/linux-e2e.sh`，验证真实 Xray、memfd、崩溃重启、父进程回收和网卡计数器。
2. 使用 systemd 安装脚本进行安装、重启、升级失败自动回滚和人工回滚演练。
3. 使用正式 CA 和域名完成公网 mTLS、Enrollment、证书轮换及吊销演练。
4. 执行 Panel 进程/主机重启、Agent 断线期间流量补报、数据库备份恢复灾难演练。
5. 在真实 Xray API 下执行 100、500、1000 用户同步和采集压力测试，记录 RSS、CPU、延迟和丢批情况。
6. 对订阅端点、安装脚本、证书权限和日志进行独立安全审查。
7. 在现有 CI 和双架构 Linux 构建基础上建立版本发布、制品签名和升级兼容流程。

## 4. 使用方式

### 4.1 Windows 本地体验

当前工作区已经有 Panel 运行配置，可执行：

```powershell
cargo build -p xenon
.\target\debug\xenon.exe
```

无 TTY 环境使用：

```powershell
.\target\debug\xenon.exe --headless
Invoke-WebRequest -UseBasicParsing http://127.0.0.1:18181/healthz
```

Windows 开发构建不会嵌入或运行 Xray，只用于 Panel、数据库、订阅和 TUI 开发。

### 4.2 Linux 测试构建

准备官方或自行验证的 Xray-core `26.6.27` Linux 二进制：

```bash
export XRAY_BINARY_PATH=/absolute/path/to/xray
export XRAY_BINARY_VERSION=26.6.27
export XRAY_BINARY_SHA256=$(sha256sum "$XRAY_BINARY_PATH" | awk '{print $1}')

cargo build --release -p xenon -p xenon-agent
./target/release/xenon-agent version-info
./scripts/linux-e2e.sh
```

Linux release 构建在缺少内核、版本不等于 `26.6.27` 或摘要不一致时会直接失败。

### 4.3 测试 VPS 部署顺序

1. 在 Panel 主机准备正式服务端证书、独立 Agent 客户端 CA 和 HTTPS 订阅域名。
2. 从 `xenon.toml.example` 生成私有 `xenon.toml`，启用 `[tls]`、`[enrollment]`、`[subscription_http]`、`[backup]` 和 `[agent_install]`。
3. 启动 Panel，在 TUI 创建节点并取得一次性安装命令。
4. 在目标 Linux VPS 以 root 执行安装命令，安装器创建受限用户和 systemd 服务。
5. 在节点页确认 Agent、Xray、修订同步和系统指标正常。
6. 创建测试用户与订阅，选择节点和额度；需要商家网卡参考值时再绑定对应网卡。
7. 分别验证 VLESS、Mihomo、Sing-box 客户端订阅和 `subscription-userinfo`。
8. 演练 Xray kill、Agent 重启、Panel 重启、证书吊销和备份恢复后再扩大使用范围。

详细配置、按键和运维命令见 `README.md`，完整业务规则和验收标准见 `PROJECT_SPEC.md`。

## 5. GitHub 发布状态

源码仓库已公开在 <https://github.com/why1f/Xenon>。当前 Release 为测试预发布版；公开仓库不代表已经完成生产准入，正式部署前仍须完成下述 Linux 实机验收和安全检查。

只提交源码、迁移、示例配置、脚本和文档。不得提交：

- `dev-certs/`、任何 `*.key`/`*.pem` 私钥。
- `data/`、数据库、WAL、SHM、运行锁和备份。
- 私有 `xenon.toml`、`agent.toml`、Token、UUID、真实节点 IP 和下载凭据。
- `target/` 构建产物。
- 旧的 `singbox-manager/` 参考项目，除非单独确认其来源和许可证。

推荐初始化流程：

```powershell
git init
git branch -M main
git add .
git status
git diff --cached --check
git commit -m "Initial Xenon implementation"
git remote add origin https://github.com/why1f/Xenon.git
git push -u origin main
```

执行 `git add` 后必须人工检查暂存文件，确认没有数据库、证书私钥、生产配置和内嵌 Xray 二进制。不要把 GitHub 当作 Agent 二进制的可信发布机制本身；正式制品应有固定版本、SHA-256、发布说明和可回滚的下载地址。

本项目源码使用 MIT 许可证。Xray-core 二进制和其他第三方依赖仍受各自许可证约束；发布嵌入 Xray 的 Agent 制品前应保留相应许可证与通知，并完成依赖许可证清单。

## 6. 后续路线

### P0：生产准入

- 完成 Linux E2E、systemd、mTLS 和灾难恢复实机验收。
- 为现有双架构 GitHub Pre-release 增加制品签名和稳定版发布流程。
- 形成最小部署手册、证书轮换手册和故障回滚手册。

### P1：容量与稳定性

- 100/500/1000 用户真实压力测试。
- Agent 断线、Panel 重启和网络抖动故障注入。
- 根据基准结果优化内存、CPU、批次大小和二进制体积。

### P2：管理体验

- 可选套餐模板，创建订阅时复制策略快照，模板更新不隐式修改旧订阅。
- 将 TUI 按键状态迁移到独立 reducer，增加后台刷新期间的选择和表单保持测试。
- 增加节点、用户、证书和备份的诊断/审计视图。

### P3：正式发布

- 发布候选版本和升级兼容矩阵。
- 完成安全审查、许可证清单和第三方依赖审计。
- Linux amd64/arm64 制品、SHA-256 和可验证发布说明。
