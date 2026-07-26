# Xenon

Xenon 是使用 Rust 开发的纯 TUI 多节点 Xray 管理系统，由主控和 Linux Agent 组成。完整需求、数据模型、计费规则、通信协议和验收标准见 [PROJECT_SPEC.md](PROJECT_SPEC.md)；当前可用性结论、部署步骤、生产准入项和发布路线见 [DEVELOPMENT_STATUS.md](DEVELOPMENT_STATUS.md)。

## 当前阶段

当前为可运行的 Panel/Agent 纵向功能骨架：

- Cargo workspace 已拆分为 `domain`、`protocol`、`storage`、`xray-protocol`、`xray-runner`、`panel`、`agent`。
- Panel 可初始化 SQLite、启动带首帧注册校验的 gRPC 服务、只读多格式订阅服务和五页 TUI 管理界面。
- TUI 已将“主机”和“节点”拆开：主机代表 VPS 与 Agent，协议节点绑定到已有主机；一个主机可保存多个 VLESS Reality、VLESS Encryption 或 VLESS WS 节点配置。
- 新协议节点在 Agent 多入站下发完成前默认保存为禁用；Reality 私钥不进入 Panel 数据。SS2022 入口仅展示规划状态，当前不会创建不完整配置。
- Agent 可读取配置、建立 gRPC 双向流、周期发送心跳和真实 Linux 网卡绝对计数器，并通过 Xray API 动态同步用户。
- Agent 同时上报 CPU 使用率、1/5/15 分钟负载、内存和磁盘用量，Panel TUI 显示最新节点系统指标。
- Agent 默认根据结构化 Xray 配置生成最小服务端 JSON；可选 TLS/Reality，完整 JSON 仅作为高级覆盖项。
- SQLite 已包含 Agent 注册、一次性 Token、用户、订阅、订阅节点、网卡绑定和流量事件 repository。
- 领域 crate 已包含流量倍率、网卡方向、每日/每月/间隔/手动重置周期和订阅输入校验的纯函数及测试。
- Xray memfd 执行边界、mTLS、真实 `/proc` 采集、动态 Xray 用户同步、持久流量队列、网卡响应头和多格式渲染已接入。
- 当前周期已接入 Xray 配额、用户排行和订阅响应头；重置只推进周期边界，不删除原始流量。网卡绑定可独立设置方向、额度、初始用量和重置规则，并在响应头中统一返回 `upload=0; download=<网卡计费用量>`。
- Xray 事件写入时同步生成小时、天和计费周期聚合；网卡绝对计数器同步生成小时、天和绑定周期聚合。原始事件、网卡快照和系统快照按 `[traffic_retention]` 定期清理，周期计费不会因原始数据过期而下降。
- Xray 兼容上限固定为 `Xray-core v26.6.27`，不得自动跟随更高版本。
- Panel/Agent 已支持可配置 tonic mTLS 和独立 TLS Enrollment 端口；Agent 本机生成私钥，Panel 使用一次性 Token 签发客户端证书并把握手证书 SHA-256 指纹与 `agent_id/node_id` 绑定。
- Panel/Agent 协议版本采用双向握手校验，实际嵌入 Xray 版本继续严格限制为 `26.6.27`。Agent 提供无配置 `version-info` 自检；Linux 安装器支持候选版本校验、原子升级、启动失败自动回滚和人工双版本回滚。

## 构建

需要 Rust stable、Linux 运行时，以及 Cargo 能访问依赖缓存或 crates.io：

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

`protoc` 由 `protoc-bin-vendored` 提供，不要求系统单独安装 protobuf 编译器。

## 本地运行

终端一：

```bash
Copy-Item xenon.toml.example xenon.toml
cargo run -p xenon
```

终端二：

```bash
Copy-Item agent.toml.example agent.toml
cargo run -p xenon-agent
```

Panel TUI 顶部使用数字键 `1` 到 `5` 或 `Tab` 在“仪表盘 / 用户 / 节点 / 主机 / 日志”间切换，大写 `R` 立即刷新。用户、节点和主机页统一用 `a` 新建。主机创建后会显示一次性 Agent 安装命令；已有主机可按 `i` 重新签发一小时有效的注册 Token 和安装命令，`e` 编辑主机名称和地址，`u` 显示升级/回滚命令，`r` 吊销 Agent 证书。节点表单内用左右键切换协议类型，`e` 编辑、`d` 启停、大写 `D` 逻辑删除。总览和用户主页始终展示 Xray 流量；网卡绑定只决定 `subscription-userinfo` 的流量来源。用户详情页按 `n` 打开节点勾选器，也可编辑额度、倍率、周期、到期时间和状态，并管理网卡绑定与凭据轮换。`q` 退出。健康检查：

```text
GET http://127.0.0.1:18181/healthz
```

订阅只读路由：

```text
GET /sub/<token>              VLESS Base64
GET /sub/<token>/vless        VLESS Base64
GET /sub/<token>/mihomo       Mihomo YAML
GET /sub/<token>/sing-box     Sing-box JSON
```

`[subscription_http]` 可启用原生 HTTPS，也可在反向代理终止 TLS 时保持明文监听在回环或私网地址。`public_base_url` 控制 TUI 生成的外部订阅地址；服务按来源 IP 和 Token 分别限流，访问日志只记录 Token 哈希前 12 位，不记录完整 Token、UUID 或订阅正文。

`[traffic_retention]` 默认保留 30 天 Xray 原始事件和网卡快照、7 天系统快照；小时和天聚合的保留天数设为 `0` 表示长期保留。计费周期聚合不会被自动清理。

数据库运维命令：

```text
xenon backup <destination.db>
xenon check-db [database.db]
xenon restore <backup.db>
```

在线备份使用 SQLite `VACUUM INTO`，同时写入 `<destination.db>.sha256`；恢复强制校验 SHA-256、SQLite 完整性、外键、迁移版本和迁移校验和。Panel 运行时持有数据库独占锁，恢复前必须停止 Panel。恢复会把旧数据库及 WAL/SHM 保存为 `.pre-restore-<pid>-<timestamp>` 回滚文件。`[backup]` 可启用启动时及定时自动备份，并按 `retain_count` 清理最旧的自动备份；备份文件包含订阅 Token 哈希和证书信息，必须按敏感文件保护。

订阅创建向导的重置规则支持 `never`、`manual`、`daily:HH:MM`、`monthly:DAY@HH:MM` 和 `interval:DAYS`。网卡绑定格式为 `node/interface/limit/initial[/direction[/reset]]`，多个绑定使用分号分隔；`direction` 可选 `rx_tx`、`tx_only`、`rx_only`，省略的网卡重置规则继承订阅规则。

无 TTY 的 systemd 或测试环境使用守护模式：

```bash
cargo run -p xenon -- --headless
```

当前 Agent 默认连接本机 Panel，且使用开发期明文 gRPC。不要将这个配置直接暴露到生产环境。独立 TLS Enrollment、一次性注册 Token 消费、客户端证书签发、身份绑定、到期前自动轮换和人工紧急吊销已经实现。首次 mTLS 注册成功后，Agent 会从配置中清除非开发 Token；轮换采用两阶段激活，新证书成功连接后才吊销旧证书。

生成本地开发证书（需要 OpenSSL）：

```bash
./scripts/generate-dev-certs.sh
```

Windows PowerShell：

```powershell
./scripts/generate-dev-certs.ps1
```

仓库还提供 `xenon.mtls.example.toml` 和 `agent.mtls.example.toml` 作为本地 mTLS 联调模板，其中 Xenon 示例同时为订阅端口启用 HTTPS。生产环境必须保护 Enrollment CA 私钥，只开放独立 Enrollment 端口，并保持控制端口强制 mTLS；Agent 只需要预置 Panel/Enrollment 服务端 CA，客户端私钥不会离开 Agent。

Linux 安装器位于 `scripts/install-agent.sh`。Panel 仅在 `[agent_install]` 完整配置并启用后生成一键命令；Agent 二进制必须通过 HTTPS 发布并配置固定 SHA-256。安装器创建受限系统用户，把临时注册配置放在权限为 `0600` 的 `/var/lib/xenon/agent`，注册成功后由 Agent 清除 Token。

升级仍由管理员在目标服务器执行，不需要 Panel 保存 SSH 凭据：

```bash
curl -fsSL --proto '=https' --tlsv1.2 '<script-url>' | sudo sh -s -- \
  --upgrade --binary-url '<binary-url>' --binary-sha256 '<sha256>' --agent-version '<version>'

curl -fsSL --proto '=https' --tlsv1.2 '<script-url>' | sudo sh -s -- --rollback
```

升级前安装器运行候选二进制的 `version-info`，要求协议版本为 `0.1`、Xray 上限和实际嵌入版本均为 `26.6.27`。旧二进制保存在 `/usr/local/lib/xenon-agent/xenon-agent.previous`；新服务不能连续稳定运行 5 秒时自动恢复旧版。人工回滚会交换当前/上一版本，因此可再次执行回到原版本。协议发生变化的发布应先在旧 Panel 上升级 Agent，再升级 Panel，保证滚动升级期间仍能连接。

## Linux 端到端验证

真实 memfd、父子进程生命周期和 `/proc` 计数器验证只能在 Linux 上执行。测试脚本拒绝不匹配的版本或摘要，并使用临时 Panel 数据库、非特权端口和开发注册口令完成一次隔离联调：

```bash
XRAY_BINARY_PATH=/path/to/xray \
XRAY_BINARY_VERSION=26.6.27 \
XRAY_BINARY_SHA256=<64位小写sha256> \
./scripts/linux-e2e.sh
```

脚本构建 release Panel/Agent，核对 Agent 的嵌入版本与摘要，确认 Xray 的 `/proc/<pid>/exe` 指向 `memfd:xray-core`，强制杀死 Xray 后要求 Agent 在 5 秒内拉起新进程，结束 Agent 后要求 Xray同步退出，并逐接口对照 `/proc/net/dev` 与 sysfs 绝对计数器。输出同时包含 Panel/Agent 二进制体积与空闲 Agent RSS。临时文件和进程在退出时自动清理。

对已运行 Agent 做可重复的空闲资源采样：

```bash
AGENT_PID=$(pidof xenon-agent) SAMPLE_SECONDS=30 ./scripts/resource-benchmark.sh
```

该脚本输出 Agent 二进制体积、RSS/峰值 RSS、采样区间 CPU basis points 和 Xray 子进程 RSS。它不声称代表 1000 用户负载；批量用户性能必须在真实 Xray API、真实用户同步和流量查询下另做压力测试。

## GitHub Linux 构建

`.github/workflows/linux-build.yml` 为 `x86_64-unknown-linux-gnu` 和 `aarch64-unknown-linux-gnu` 构建 release 制品。工作流使用 Zig 将最低运行时固定为 glibc 2.17，并扫描成品的 GLIBC 符号版本防止兼容性回退。它在 `main` 的构建相关文件变化、`v*` 标签和手动触发时运行，从 XTLS/Xray-core 固定标签 `v26.6.27` 下载架构对应的官方压缩包，先校验仓库中固定的压缩包 SHA-256，再把提取后内核的 SHA-256 交给 Agent 构建脚本验证。`v*` 标签构建只有在两个架构均成功后才自动创建 GitHub Pre-release，并附加两个压缩包及其 SHA-256 文件。

每个架构上传一个保留 14 天的 GitHub Actions artifact：

```text
xenon-linux-x86_64.tar.gz
xenon-linux-x86_64.tar.gz.sha256
xenon-linux-aarch64.tar.gz
xenon-linux-aarch64.tar.gz.sha256
```

压缩包包含 `xenon`、已嵌入对应架构 Xray 的 `xenon-agent`、systemd 单元、Agent 安装器、项目许可证和 Xray 许可证。Actions artifact 是测试制品；正式发布仍应创建版本标签、保留摘要并完成 Linux E2E 后再对外提供。

### 正式一键安装主控

在面向公网的 Linux 主控机上（x86_64/ARM64 + systemd）：

```bash
curl -fsSL https://raw.githubusercontent.com/why1f/Xenon/main/scripts/install-panel.sh | sudo bash
```

安装器会下载最新 Release、生成自签服务端 CA/证书与 Agent 客户端 CA、写入启用 mTLS 和 Enrollment 的 `/etc/xenon/xenon.toml`，并以 systemd 服务启动主控。有域名时用 `sudo XENON_HOST=panel.example.com bash` 指定；否则自动使用公网 IPv4。

装好后执行 `sudo xenon-tui` 进入 TUI，切换到“主机 [4]”并按 `a` 创建主机，把弹窗中的命令放到目标 Linux VPS 以 root 执行即可完成被控 Agent 安装：命令内置架构自适应的 Agent 二进制地址、双架构 SHA-256 和 base64 内嵌的主控 CA。关闭过弹窗时，在主机页选中该主机按 `i` 即可重新生成。Agent 注册上线后，再到“节点 [3]”为该主机配置协议节点。

需要放行端口：`50051`（gRPC mTLS）、`50052`（Enrollment）、`18181`（订阅 HTTP）以及各节点的 Xray 端口。

### 一键卸载

主控（加 `--purge` 连配置、证书和数据库一起删除）：

```bash
curl -fsSL https://raw.githubusercontent.com/why1f/Xenon/main/scripts/install-panel.sh | sudo bash -s -- --uninstall --purge
```

被控 Agent：

```bash
curl -fsSL https://raw.githubusercontent.com/why1f/Xenon/main/scripts/install-agent.sh | sudo bash -s -- --uninstall
```

### 从测试环境切换到正式版

不能直接在测试机上跑正式安装命令：测试配置（回环、明文 Token）会被保留而不是覆盖，正式安装器检测到它会拒绝继续。正确顺序是先卸载再安装：

```bash
curl -fsSL https://raw.githubusercontent.com/why1f/Xenon/main/scripts/install-panel.sh | sudo bash -s -- --uninstall --purge
curl -fsSL https://raw.githubusercontent.com/why1f/Xenon/main/scripts/install-panel.sh | sudo bash
```

卸载时会自动识别并移除本机的回环测试 Agent；`--purge` 会删除测试数据库，正式环境从全新状态开始。

### 单机一键测试安装

公开仓库可在 x86_64 或 ARM64、使用 systemd 的 Linux 测试机上匿名下载并安装：

```bash
curl -fsSL https://raw.githubusercontent.com/why1f/Xenon/main/scripts/bootstrap-test.sh | sudo bash
```

引导脚本默认下载最新 Release，自动识别架构并验证 Release 中的 SHA-256。也可以显式指定版本：

```bash
curl -fsSL https://raw.githubusercontent.com/why1f/Xenon/main/scripts/bootstrap-test.sh \
  | sudo XENON_VERSION=v0.1.0-alpha.6 bash
```

也可以从 [GitHub Releases](https://github.com/why1f/Xenon/releases) 手动下载对应架构的压缩包；包内 `scripts/install-test.sh` 可同时安装 Xenon 和 Agent：

```bash
sha256sum -c xenon-linux-<arch>.tar.gz.sha256
tar -xzf xenon-linux-<arch>.tar.gz
cd xenon-linux-<arch>
sudo ./scripts/install-test.sh
```

该脚本仅创建回环测试环境：控制端口、订阅端口和 Xray 入站均只监听 `127.0.0.1`，使用 `development-only` 注册口令且不启用 TLS，不能用于生产或公网监听。旧测试配置中的订阅端口 `18081` 会自动迁移到 `18181`。安装后检查：

```bash
curl http://127.0.0.1:18181/healthz
sudo systemctl status xenon xenon-agent
sudo journalctl -u xenon -u xenon-agent -f
```

进入 TUI 用一条命令即可；它会临时停止 headless 主控、打开 TUI，退出时自动恢复服务：

```bash
sudo xenon-tui
```

正式多服务器部署不要使用 `install-test.sh`。应在启用 mTLS/Enrollment 的 Xenon 中创建节点，再执行 TUI 生成的 `scripts/install-agent.sh` 命令；生产安装器不会接受明文下载地址。

将 `xenon.toml` 的 `[tls]` 文件路径指向 `dev-certs/server.crt`、`server.key`、`ca.crt`，将 `agent.toml` 的 `[tls]` 文件路径指向 `ca.crt`、`agent.crt`、`agent.key`，并将 Agent 的地址改为 `https://panel.internal:50051`。开发机需要将 `panel.internal` 解析到 Xenon 地址。证书脚本只用于本地测试，不能代替生产 CA、证书轮换和一次性注册 Token。

## 目录

```text
crates/domain       纯领域类型和计费算法
crates/protocol     Panel/Agent protobuf 与 tonic 代码生成
crates/storage      SQLite 连接、WAL 和迁移
crates/xray-runner  固定版本内核嵌入与 Linux memfd 生命周期
crates/panel        Panel TUI、gRPC server、订阅 HTTP server
crates/agent        Agent 控制流、采集器和 Xray 生命周期边界
proto               gRPC 协议源文件
migrations          SQLite schema 迁移
```

`singbox-manager` 是旧的单节点参考项目，本 workspace 不依赖它，也不修改它。
