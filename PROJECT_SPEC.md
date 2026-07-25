# Xenon 开发规格

> 文档状态：需求基线 1.0
> 适用范围：Panel、Agent、Xray 管理、订阅分发和流量统计
> 核心原则：管理界面纯 TUI；用户流量始终来自 Xray；可选网卡绑定只改变订阅响应头；中转只作为订阅地址覆盖项。

## 1. 项目概述

Xenon 是一个使用 Rust 开发的 Xray 节点和订阅管理系统，由主控端 Panel 与被控端 Agent 组成。

Panel 使用终端 TUI 管理节点、用户、订阅、额度和流量，不提供 Web 管理后台。Agent 部署在 Linux VPS 上，负责运行和守护内嵌的 Xray、动态同步用户、采集 Xray 用户流量及系统网卡流量，并向 Panel 上报节点状态。

客户端通过一个最小化、只读的 HTTPS 订阅接口获取配置。该接口不包含登录、管理页面或写操作。

### 1.1 项目目标

- Panel 与 Agent 全栈使用 Rust。
- Panel 提供纯终端、全键盘操作的 TUI。
- Agent 单文件安装，不要求用户手工安装 Xray。
- Xray 二进制嵌入 Agent，并在 Linux 内存文件中运行。
- Agent 主动连接 Panel，不要求开放 Agent 管理端口。
- Xray 用户通过 API 动态增删，不因日常用户变更重启内核。
- 用户流量按订阅、节点分别统计，并在用户维度汇总。
- 订阅可选择一个或多个节点，并输出 VLESS、Mihomo、Sing-box 配置。
- 订阅可选绑定一个或多个被控服务器网卡，用网卡流量填充 `subscription-userinfo`。
- 节点可配置可选的中转 IP/域名和端口，仅用于生成客户端配置。
- 在合理规模下保持低 CPU、低额外内存和较小的 Rust 侧二进制体积。

### 1.2 非目标

- 不开发 Web 管理后台。
- 不保存节点 SSH 密码，不通过 SSH 自动登录服务器。
- 不自动配置中转服务器、防火墙、iptables、nftables 或端口转发。
- 不根据中转拓扑计算链路流量。
- 不把共享网卡流量归因到具体用户。
- 不承诺网卡统计与所有 VPS 商家账单完全一致。
- 第一版不实现集群化 Panel、外部数据库或多主高可用。
- 第一版不承诺 Xray 原生无法提供的按用户精确实时限速。

## 2. 核心业务规则

以下规则是实现时不可混淆的产品边界。

### 2.1 用户流量规则

- 每个订阅拥有独立的 Xray UUID 和统计 Email。
- 每个订阅可关联多个节点。
- 一个订阅的 Xray 流量等于该订阅在所有关联节点上的流量之和。
- 一个用户的 Xray 流量等于该用户所有订阅流量之和。
- 用户主页始终按 Xray 计费流量降序排列。
- 点击用户后，可查看订阅和节点两个层级的 Xray 流量明细。
- 网卡绑定不得改变用户主页、用户排名或 Xray 节点明细。

计算公式：

```text
节点原始流量 = uplink_bytes + downlink_bytes
节点计费流量 = 节点原始流量 * traffic_multiplier
订阅计费流量 = sum(订阅各节点计费流量)
用户计费流量 = sum(用户各订阅计费流量)
```

第一版 `traffic_multiplier` 支持 `1x` 和 `2x`。数据库应使用整数倍率或基点保存，避免浮点误差，并为后续扩展其他倍率留出空间。

### 2.2 网卡绑定规则

- 网卡绑定属于订阅，而不是用户。
- 一个订阅可以绑定一个或多个 Agent 上报的网卡。
- 多个绑定网卡的流量直接求和。
- 网卡绑定只决定该订阅 HTTP 响应头的数据来源。
- 网卡绑定不参与用户 Xray 流量统计、排序、超额判断或 Xray 用户禁用。
- 绑定整张网卡意味着其中包含该服务器上的其他用户、SSH、更新、扫描和其他服务流量，这是预期行为。
- 同一网卡可以被多个订阅引用；Agent 只采集一次，Panel 复用采样数据。

### 2.3 中转规则

- 中转是节点的可选“订阅发布地址”。
- 未配置中转时，客户端配置使用节点落地地址和 Xray 入站端口。
- 配置中转后，客户端配置使用中转 IP/域名和中转端口。
- Agent、Xray 实际监听地址以及 gRPC 管理地址不因中转字段改变。
- Panel 不检测、不创建也不修复中转转发规则。
- 中转可用性仅可通过可选的外部连通性探测判断，不作为第一版必需功能。

### 2.4 用户、订阅与套餐规则

- `User` 是展示、归属和汇总对象。
- `Subscription` 是凭证、节点集合、额度、周期和策略对象。
- 一个用户可以拥有一个或多个订阅。
- TUI 将“创建用户”和“生成订阅”合并成一次向导，但数据库中保持两个对象分离。
- 默认用户为初始化时创建的 `admin`。
- 创建订阅时可以选择 `admin`、已有用户，或直接输入新用户名创建用户。
- 第一版允许直接在订阅上设置策略，不强制创建套餐。
- 套餐作为可选模板：选择套餐时将配置复制到订阅快照，后续修改套餐默认不影响已有订阅。

## 3. 技术选型

### 3.1 通用技术栈

| 范围 | 选型 |
| --- | --- |
| 语言 | Rust stable |
| 异步运行时 | Tokio |
| TUI | ratatui + crossterm |
| Panel 数据库 | SQLite + sqlx |
| Panel/Agent 协议 | gRPC + tonic + prost |
| TLS | rustls |
| 序列化 | Protocol Buffers |
| 日志 | tracing + tracing-subscriber |
| ID | UUID v7 或等价的时序 UUID |
| 时间 | UTC Unix 时间戳，TUI 按本地时区显示 |

### 3.2 支持平台

- Agent：Linux `x86_64` 和 `aarch64`。
- Panel：第一版优先支持 Linux `x86_64`，代码保持可移植性。
- Agent 依赖 Linux 的 `memfd_create`、`prctl`、`/proc` 和 systemd。
- 每个 Agent 发布包只嵌入对应 CPU 架构的 Xray，避免一个包携带多个内核。

### 3.3 Xray 版本基线

- 第一版固定嵌入 `Xray-core v26.6.27`。
- 自动构建、内核更新和 Agent 升级不得选择高于 `v26.6.27` 的 Xray 版本。
- 禁止使用 `latest`、无上限版本范围或运行时自动跟随 Xray 上游版本。
- 高于该版本的 Xray 增加了 VLESS 客户端版本校验，而当前大量客户端尚未跟进，因此在完成完整客户端兼容性验证前不得升级。
- 构建产物必须同时记录固定版本、下载来源和 SHA-256；版本或哈希不匹配时构建失败。
- Panel 节点页面对 Agent 上报的高于上限版本显示错误状态，不应静默接受。

### 3.4 建议 Cargo Workspace

```text
workspace/
├── crates/
│   ├── panel/             # TUI、订阅 HTTP、gRPC 服务、后台任务
│   ├── agent/             # Agent 主程序
│   ├── domain/            # 领域类型、计费与周期算法
│   ├── protocol/          # Panel/Agent protobuf 生成代码
│   ├── xray-protocol/     # Xray API protobuf 生成代码
│   ├── xray-runner/       # 固定版本内核嵌入与 Linux memfd 生命周期
│   ├── config-renderer/   # VLESS/Mihomo/Sing-box 渲染
│   └── storage/           # sqlx repository 与迁移
├── migrations/
├── proto/
├── packaging/
└── Cargo.toml
```

以上是同一代码库内的 crate 划分，不拆分成多个网络微服务。Panel 仍是一个进程，Agent 仍是一个进程。

## 4. 总体架构

```text
                    HTTPS GET /sub/{token}
客户端 ------------------------------------------------> Panel
                                                          |
管理员终端 <---- ratatui TUI ---- Panel Core ---- SQLite  |
                                  |                       |
                                  +---- gRPC/mTLS <--------+---- Agent 主动连接
                                                               |
                                                               +-- Xray gRPC 127.0.0.1
                                                               +-- /proc 系统采集
                                                               +-- Xray 子进程
```

### 4.1 Panel 内部组件

- TUI：节点、用户、订阅、流量、设置和操作弹窗。
- Domain Service：业务校验、周期、额度、聚合与状态机。
- gRPC Server：Agent 注册、长连接、命令和遥测接收。
- Subscription HTTP Server：只读订阅分发与响应头生成。
- Storage：SQLite 事务、迁移、查询与聚合。
- Background Jobs：在线状态、过期处理、配额处理、流量汇总、数据清理和备份。
- Config Renderer：输出 URI、Mihomo 和 Sing-box 配置。

### 4.2 Agent 内部组件

- Control Client：主动连接 Panel，发送心跳和报告，接收期望状态。
- Xray Supervisor：从内存启动、监控、停止和重启 Xray。
- Xray API Client：动态同步用户并读取用户统计。
- System Collector：读取 CPU、内存、磁盘、负载和网卡计数器。
- Reconciler：将 Panel 期望用户状态与 Xray 实际状态对齐。
- Local Spool：短期保存尚未得到 Panel ACK 的关键流量事件。

## 5. 领域模型

### 5.1 User

```text
id
username             唯一、可展示名称
display_name         可选
status               active / disabled
created_at
updated_at
```

初始化数据库时自动创建不可重复的 `admin` 用户。删除用户前必须先处理其订阅；第一版建议仅禁用，不物理删除有历史流量的用户。

### 5.2 Subscription

```text
id
user_id
name
token_hash
xray_uuid
xray_email
status               active / disabled / expired / quota_exhausted
starts_at
expires_at            可为空，表示不过期
traffic_limit_bytes   Xray 用户额度
traffic_multiplier    10000=1x，20000=2x
reset_policy
reset_anchor
current_cycle_start
current_cycle_end
created_at
updated_at
```

要求：

- 明文订阅 Token 只在创建或轮换时展示，数据库仅保存安全哈希。
- `xray_email` 必须稳定且全局唯一，建议包含订阅 ID，不使用可修改的用户名作为唯一键。
- 修改用户名不得中断 Xray 统计。
- 轮换订阅 Token 不必轮换 Xray UUID；两者应支持独立轮换。

### 5.3 SubscriptionNode

```text
subscription_id
node_id
enabled
sort_order
created_at
```

它决定：

- 订阅中出现哪些节点。
- Agent 需要在哪些节点创建该订阅对应的 Xray 用户。
- Xray 流量按哪些节点求和。

### 5.4 Node

```text
id
name
region
agent_status
landing_host
xray_listen_port
publish_host          可选中转 IP/域名
publish_port          可选中转端口
protocol              第一版为 VLESS
transport_settings
tls_settings
desired_revision
last_seen_at
created_at
updated_at
```

配置生成规则：

```text
host = publish_host ?? landing_host
port = publish_port ?? xray_listen_port
```

`publish_host/publish_port` 不下发给 Agent 执行，仅由 Panel 配置渲染器使用。

### 5.5 NicBinding

```text
id
subscription_id
node_id
interface_name
billing_direction     rx_tx / tx_only / rx_only
traffic_limit_bytes
initial_used_bytes
reset_policy
reset_anchor
enabled
bound_at
unbound_at
created_at
updated_at
```

每个绑定拥有自己的网卡额度和统计周期。订阅绑定多个网卡时，响应头中的额度和用量直接求和。

### 5.6 PlanTemplate

套餐为可选功能，建议第二阶段实现：

```text
id
name
traffic_limit_bytes
duration_days
traffic_multiplier
reset_policy
rate_limit_bps        预留字段
created_at
updated_at
```

套餐只用于创建订阅时填充默认值。订阅必须保存自己的策略快照，避免套餐修改产生隐式批量变更。

## 6. Panel 功能需求

### 6.1 首次启动

- 自动创建数据库和执行迁移。
- 创建默认 `admin` 用户。
- 生成 Panel 服务端身份材料或引导导入证书。
- 要求设置订阅服务公开基础 URL。
- 检查 gRPC、订阅 HTTPS 监听地址是否可用。
- 首次启动完成后进入 TUI 看板。

### 6.2 主看板

显示：

- 节点总数、在线、离线和异常数量。
- 用户和有效订阅数量。
- 当前统计周期 Xray 总计费流量。
- 最近发生的节点掉线、Xray 重启、配额耗尽和证书异常。
- 节点 CPU、内存、磁盘和网卡速率摘要。

数据刷新不应阻塞键盘输入。TUI 使用后台状态快照，数据库和网络操作不得直接运行在渲染线程。

### 6.3 节点管理

支持：

- 创建、编辑、禁用和逻辑删除节点。
- 生成一次性 Agent 注册 Token。
- 显示一行 `curl` 或 `wget` 安装命令。
- 查看节点在线状态、Agent 版本、Xray 版本和最后心跳。
- 查看 CPU、内存、磁盘、系统负载和每张网卡速率。
- 查看 Xray 运行状态、PID、启动时间和重启次数。
- 查看已同步用户数、期望修订号和 Agent 已应用修订号。
- 手动触发状态重新同步或 Xray 重启。
- 配置落地地址、Xray 端口、协议参数及 TLS 参数。
- 可选填写中转 IP/域名和中转端口。
- 预览该节点生成的 VLESS 地址。

中转字段校验：

- `publish_host` 与 `publish_port` 必须同时为空或同时有效。
- 域名不得包含 URL scheme 或路径。
- 端口范围为 `1..=65535`。
- 保存中转字段时不向 Agent 发送中转执行命令。

### 6.4 用户主页

- 默认按当前周期 Xray 计费流量降序排列。
- 支持按用户名、状态和流量范围筛选。
- 每行显示用户名、订阅数、节点数、当前用量、额度状态和最近活动时间。
- 网卡绑定值不得出现在主页主流量列中。
- 对相同流量使用稳定的次级排序，避免刷新时列表跳动。

用户详情显示：

- 用户基本信息。
- 该用户的所有订阅。
- 每个订阅当前周期的 Xray 上行、下行、原始总量、倍率和计费总量。
- 每个订阅在各节点上的相同明细。
- 可选的网卡响应头数据预览必须放在独立区域，并标明它不参与用户统计。

### 6.5 创建订阅向导

一次向导完成用户创建和订阅生成：

1. 选择默认 `admin`、已有用户或输入新用户名。
2. 填写订阅名称。
3. 选择一个或多个节点。
4. 设置生效时间和到期时间。
5. 设置 Xray 流量额度。
6. 设置流量重置规则。
7. 选择 `1x` 或 `2x` 计费倍率。
8. 可选选择套餐模板并允许覆盖模板值。
9. 可选绑定一个或多个节点网卡及其网卡额度。
10. 确认后创建 UUID、统计 Email 和订阅 Token。
11. 展示订阅 URL、标准 VLESS URI 和配置导出操作。

创建过程必须使用数据库事务。任何一步失败不得留下只有用户、没有订阅或只有订阅、没有节点关系的半成品；如果新建用户后后续失败，应在同一事务中回滚。

### 6.6 订阅管理

支持：

- 查看、编辑、启用、禁用订阅。
- 增加或移除订阅节点。
- 修改额度、倍率、周期和到期时间。
- 立即重置当前周期用量，但保留历史账目。
- 轮换订阅访问 Token。
- 轮换 Xray UUID，并同步所有相关节点。
- 查看 Agent 同步进度和失败节点。
- 查看订阅 URL 和不同客户端格式。
- 预览本次 HTTP 响应头。
- 管理可选网卡绑定。

修改节点集合或 UUID 时，Panel 更新 `desired_revision`。订阅只有在所有在线目标 Agent ACK 后显示为“已同步”；离线节点显示“等待同步”。

### 6.7 网卡绑定管理

- 只能选择 Agent 最近上报过的真实接口。
- 创建绑定时记录当前绝对计数器为周期基线。
- 支持 `RX+TX`、`仅 TX`、`仅 RX` 三种计费方向。
- 默认使用 `RX+TX`，管理员应根据 VPS 商家规则调整。
- 支持为每个绑定设置网卡总额度、重置规则和初始已用流量。
- 解除绑定时保留历史记录并设置 `unbound_at`。
- 网卡消失、计数器回退或节点重装时显示异常状态，不生成负流量。
- 多网卡绑定直接汇总，不进行中转识别或链路去重。

### 6.8 套餐模板

套餐不是第一版核心依赖。实现后支持：

- 新建、编辑、复制、禁用模板。
- 订阅创建时应用模板。
- 应用后允许覆盖单项参数。
- 编辑模板默认不更新已有订阅。
- 如需批量同步，必须显示影响订阅数量并进行二次确认。

### 6.9 配置输出

第一版输出：

- 标准 VLESS URI 列表。
- Base64 通用订阅内容。
- Mihomo YAML。
- Sing-box JSON。

输出要求：

- 所有格式由统一的中间配置模型渲染，避免各格式分别拼接业务逻辑。
- URI 使用结构化编码，不直接拼接未转义的名称或参数。
- 节点名称在同一订阅内唯一且稳定。
- 配置使用节点 `publish_host/publish_port` 覆盖落地地址。
- 被禁用、未生效、已过期或额度耗尽的订阅不返回可用节点。
- 单个节点同步失败时，默认仍返回其他已就绪节点，并在 TUI 标记部分同步状态。

### 6.10 最小订阅 HTTP 服务

必须提供只读接口，否则客户端无法获得 `subscription-userinfo` 响应头。

建议路由：

```text
GET /sub/{token}                 自动或通用格式
GET /sub/{token}/vless          VLESS/Base64
GET /sub/{token}/mihomo         Mihomo YAML
GET /sub/{token}/sing-box       Sing-box JSON
```

要求：

- 只允许 `GET` 和可选的 `HEAD`。
- 不提供后台、登录、用户列表或任何写接口。
- Token 使用至少 256 位加密安全随机数。
- 数据库只保存 Token 哈希。
- 使用 HTTPS；若由反向代理终止 TLS，Panel 只监听回环地址或私网地址。
- 响应包含 `Cache-Control: no-store`。
- 错误响应不得泄露用户、节点或 Token 是否曾存在。
- 对单 Token 和来源 IP 做温和限流。
- 日志中不得记录完整 Token、UUID 或完整订阅内容。

## 7. `subscription-userinfo` 规范

响应格式：

```http
subscription-userinfo: upload=<bytes>; download=<bytes>; total=<bytes>; expire=<unix-seconds>
```

所有数值使用十进制字节整数。客户端通常通过 `upload + download` 计算已用流量。

### 7.1 未绑定网卡

使用当前订阅、当前重置周期、所有订阅节点上的 Xray 流量：

```text
upload   = sum(各节点 xray_uplink) * traffic_multiplier
download = sum(各节点 xray_downlink) * traffic_multiplier
total    = subscription.traffic_limit_bytes
expire   = subscription.expires_at
```

无到期时间时，按照目标客户端兼容策略省略 `expire`，不要伪造无限大的时间戳。

### 7.2 绑定网卡

绑定一个或多个网卡时，响应头完全切换为网卡数据：

```text
upload   = 0
download = sum(各绑定按 billing_direction 计算的当前周期计费用量)
total    = sum(绑定项 traffic_limit_bytes)
expire   = subscription.expires_at
```

为避免不同客户端对 RX/TX 或 upload/download 方向采用不同解释，绑定网卡时统一把全部已用量放入 `download`。计费方向只控制服务器网卡计数器如何进入计费用量：

| 模式 | 绑定计费用量 | 响应头 upload | 响应头 download |
| --- | ---: | ---: | ---: |
| RX+TX | RX + TX | 0 | RX + TX |
| 仅 TX | TX | 0 | TX |
| 仅 RX | RX | 0 | RX |

这里不尝试把服务器视角的 RX/TX 还原为应用层上传下载，只保证客户端用 `upload + download` 得到正确的商家计费参考值。

网卡绑定场景下：

- 不应用订阅的 Xray `traffic_multiplier`。
- 使用网卡绑定自己的额度和周期。
- `initial_used_bytes` 加入对应绑定的已用量。
- 某个绑定数据暂时不可用时，返回最近已确认值并在 TUI 告警，不将其突然归零。
- 网卡用量超过额度时，响应头最多显示实际已用值；剩余量由客户端截断为零。
- 网卡额度耗尽默认不移除 Xray 用户，因为绑定只影响响应头。

### 7.3 多周期处理

- Xray 额度周期属于订阅。
- 网卡额度周期属于每个网卡绑定。
- 主页用户统计使用订阅的 Xray 周期。
- 响应头使用当前数据源对应的周期。
- 若多个网卡绑定的周期不同，直接汇总各绑定当前周期值；TUI 必须分别展示其周期边界。

为避免客户端看到难以解释的汇总，TUI 应提示管理员尽量为同一订阅绑定相同重置周期的网卡，但不强制限制。

## 8. Xray 管理需求

### 8.1 内核嵌入

- 构建时使用 `include_bytes!` 嵌入与目标架构匹配的 Xray 二进制。
- 内嵌版本固定为 `Xray-core v26.6.27`，构建脚本必须拒绝更高版本。
- 构建流程校验 Xray 文件 SHA-256，避免错误或被替换的内核进入发布包。
- 发布信息记录 Xray 版本、来源、许可证和哈希。
- GeoIP、GeoSite 等资源必须明确版本和供应方式；若运行配置不需要，应避免无条件嵌入以控制体积。

### 8.2 内存启动

- Agent 使用 `memfd_create` 创建匿名内存文件。
- 写入 Xray 二进制后设置可执行权限及可用的文件 sealing。
- 通过 `/proc/self/fd/<fd>` 或 Linux 支持的等价方式执行。
- Xray 最小启动配置通过继承文件描述符或标准输入提供，不生成长期明文配置文件。
- Xray API 只监听 `127.0.0.1` 或 Unix socket，不对公网暴露。

内存启动减少落盘文件，但不代表进程无法被系统管理员、安全软件或 `/proc` 检测。产品文档不得使用“完全防扫描”之类不可验证的表述。

### 8.3 子进程守护

- systemd 使用 `KillMode=control-group` 管理 Agent 和 Xray。
- Agent 为 Xray 设置父进程退出信号，降低孤儿进程风险。
- Agent 正常退出时先停止 Xray，再退出自身。
- Xray 意外退出后，首次重启目标小于 1 秒。
- 连续失败采用有上限的指数退避，防止配置错误造成每秒无限重启。
- 达到失败阈值后仍保持 Agent 在线并向 Panel 报告 `xray_failed`。
- 每次 Xray 启动生成新的 `xray_instance_id`，用于识别统计计数器重置。

### 8.4 动态用户同步

- Panel 数据库保存期望状态，Xray 不是状态真源。
- Agent 启动、重新连接或 Xray 重启后执行完整协调。
- 日常变更通过 Xray gRPC Handler API 动态添加或移除用户。
- 当前用户不因其他用户增删而断开连接。
- 每次同步携带单调递增的 `desired_revision`。
- Agent ACK 包含已应用修订、成功数量和逐项错误。
- Panel 定期发送状态摘要，Agent 对比哈希，修复遗漏的命令。

订阅在以下条件任一满足时从目标节点移除：

- 订阅被禁用。
- 用户被禁用。
- 当前时间晚于 `expires_at`。
- 当前 Xray 计费周期用量达到 `traffic_limit_bytes`。
- 节点与订阅关系被移除。

周期重置或额度提高后，符合条件的用户应自动重新加入 Xray。

### 8.5 Xray 日志

- Xray stdout/stderr 继承给 Agent/systemd journal。
- Agent 日志包含节点 ID、Xray 实例 ID 和事件类型。
- 默认不输出订阅 Token、完整 UUID、用户 Email 或客户端配置。
- 支持通过配置调整日志级别，但生产默认不得开启高频调试日志。

## 9. Agent 功能需求

### 9.1 注册与连接

- Agent 安装时使用一次性注册 Token 连接 Panel。
- Token 短时有效、只能使用一次，数据库保存哈希。
- 注册成功后获得节点唯一身份和长期客户端证书。
- 后续使用 mTLS，不继续使用安装 Token。
- 支持证书轮换和节点吊销。
- Agent 主动建立并维护一条 gRPC 双向长连接。
- 使用 keepalive、重连退避和随机抖动。

### 9.2 心跳和系统状态

默认每 10 秒上报，周期可配置且加入随机抖动：

```text
agent_version
xray_version
uptime
cpu_usage
load_average
memory_total / memory_used
disk_total / disk_used
xray_status / pid / restart_count
desired_revision / applied_revision
interface absolute counters
```

系统数据来源：

- CPU：`/proc/stat` 差值。
- 负载：`/proc/loadavg`。
- 内存：`/proc/meminfo`。
- 网卡：`/proc/net/dev`。
- 磁盘：`statvfs` 或等价系统调用。

避免为了这些字段引入重型系统信息依赖。

### 9.3 网卡采集

- 上报每张非 loopback 网卡的 `rx_bytes`、`tx_bytes` 绝对值。
- 上报 Agent 当前 `boot_id`、采样序号和采样时间。
- Panel 依据相邻绝对值计算差值和速率。
- 计数器下降、`boot_id` 改变或接口重新出现时建立新基线，不产生负流量。
- Agent 不需要知道哪些订阅绑定了网卡。
- Panel 可下发接口忽略列表，默认忽略 `lo`。

### 9.4 Xray 用户统计

- 默认每 10 秒读取一次 Xray Stats API。
- 按稳定 `xray_email` 获取 uplink/downlink。
- 流量必须带 `node_id`、`subscription_id`、`xray_instance_id` 和报告序号。
- Agent 或 Panel 必须确保重复发送不会重复入账。
- Xray 实例切换或统计值回退时建立新基线。
- 用户被动态删除前，Agent 应尽可能完成最后一次统计采集。
- 批量查询和批量上报，避免每个用户建立独立请求或 gRPC 消息。

建议优先读取非重置的绝对计数器，再按 Xray 实例计算差值。若目标 Xray API 只能稳定提供“读取并重置”，Agent 必须先生成唯一事件并立即写入本地 spool，再等待 Panel ACK。

### 9.5 本地可靠队列

- 未 ACK 的流量事件必须可在短期 Agent 重启后恢复。
- 队列应设置磁盘上限、条目上限和告警阈值。
- 心跳和瞬时系统指标可以丢弃旧值，只保留最新快照。
- 流量事件不能因普通断线直接丢弃。
- Panel ACK 使用连续序号或明确事件 ID。
- 队列超过上限时，Agent报告严重告警，不静默覆盖未确认流量。

### 9.6 安装与 systemd

Panel 展示类似命令：

```sh
curl -fsSL --proto '=https' --tlsv1.2 <configured-script-url> | sudo sh -s -- \
  --panel <control-endpoint> --enrollment <enrollment-endpoint> \
  --server-name <tls-name> --node <node-id> --token <one-time-token> \
  --binary-url <agent-binary-url> --binary-sha256 <sha256> --ca-url <ca-url>
```

安装脚本要求：

- 检测 CPU 架构和 Linux/systemd 环境。
- 通过 HTTPS 下载 Agent，并校验签名或固定哈希。
- 创建权限受限的系统用户和状态目录。
- 写入 systemd unit 并启动服务。
- 不请求或回传 SSH 密码。
- Token 不写入长期可读日志；注册成功后从配置中移除。
- 支持幂等安装、升级和卸载。

当前升级实现不向常驻 Agent 引入下载依赖：Panel TUI 生成需要管理员在节点执行的 HTTPS 安装器命令。安装器校验发布 SHA-256，并调用候选 Agent 的 `version-info` 核对 Agent 版本、Panel/Agent 协议版本、Xray 上限、实际嵌入版本和内核存在性；切换前保留上一二进制，systemd 启动稳定性检查失败时自动回滚。Panel 注册和 Enrollment 双向强制协议版本 `0.1`，且继续拒绝任何不是 Xray-core `26.6.27` 的实际内核版本或版本策略。兼容发布先在旧 Panel 上滚动升级 Agent，再升级 Panel。

当前实现的注册安全边界：Agent 在本机生成私钥和 CSR，通过独立的仅服务端 TLS Enrollment 端口提交一次性 Token；Panel 校验 CSR 签名并强制签发 `clientAuth` 证书，在同一事务中消费 Token、保存公钥/证书指纹并绑定 `agent_id` 与 `node_id`。后续控制连接从 mTLS 握手取得客户端叶证书 DER 并要求指纹精确匹配；仅拥有同一客户端 CA 签发的其他证书不能冒用节点。Agent 会在到期窗口前通过已认证控制端口申请轮换证书；新证书首次成功握手后 Panel 才激活它并吊销旧证书，已吊销证书的存量连接在下一消息周期断开。Panel TUI 可按节点执行人工紧急吊销。

## 10. Panel/Agent 通信协议

### 10.1 连接方向

Agent 始终主动连接 Panel。Panel 不直接访问 Agent 的公网端口。

### 10.2 逻辑消息

Agent -> Panel：

```text
RegisterRequest
Heartbeat
SystemSnapshot
InterfaceSnapshotBatch
XrayTrafficBatch
DesiredStateAck
CommandResult
AgentLogEvent
```

Panel -> Agent：

```text
RegisterResponse
DesiredStateDelta
DesiredStateSnapshot
RestartXray
SetLogLevel
RotateCertificate
TrafficAck
```

### 10.3 协议要求

- 每条连接声明协议版本、Agent 版本和能力集合。
- 新增 protobuf 字段使用向后兼容编号，不复用已删除字段编号。
- 命令携带唯一 ID、创建时间、过期时间和期望修订号。
- Agent 对重复命令返回之前的结果，不重复执行非幂等操作。
- 流量批次携带唯一批次 ID 和单调序号。
- Panel 在同一事务内完成流量入库和去重标记。
- 时间以 Panel 为周期裁定基准，Agent 时间仅用于诊断和采样排序。
- 检测 Agent 与 Panel 时钟偏差并在 TUI 告警。

## 11. 流量存储与计算

### 11.1 Xray 流量事件

推荐字段：

```text
event_id
agent_id
node_id
subscription_id
xray_instance_id
sequence
interval_start
interval_end
uplink_delta
downlink_delta
received_at
```

唯一约束至少覆盖 Agent、Xray 实例和序号，保证重试幂等。

### 11.2 网卡采样

推荐字段：

```text
node_id
boot_id
interface_name
sample_sequence
sampled_at
rx_absolute
tx_absolute
rx_delta
tx_delta
```

绑定创建时保存基线或绑定时间。计算绑定用量时只计入 `bound_at` 之后、当前网卡周期内的数据。

### 11.3 聚合

- 原始事件用于纠错和短期明细。
- 定期生成小时、天和计费周期聚合。
- TUI 列表查询聚合表，不扫描全部原始事件。
- 原始事件保留期可配置，默认 30 天。
- 小时聚合默认长期保留或按管理员配置清理。
- 删除历史前必须确保相关聚合已完成。

当前实现采用 SQLite `AFTER INSERT` 触发器原子维护 Xray 小时、天、计费周期聚合以及网卡小时、天、绑定周期聚合；幂等事件被 `INSERT OR IGNORE` 拒绝时不会重复累计。Panel 后台按配置清理原始事件、网卡快照、系统快照和可选的小时/天聚合，计费查询使用周期聚合，不会因原始数据过期而回退。

### 11.4 重置规则

支持：

- 不重置。
- 每日指定 UTC 时间。
- 每月指定日期和时间。
- 从订阅生效时间起每 N 天。
- 管理员手动重置。

重置不删除原始流量，只创建新的计费周期。对于每月 29、30、31 日，应明确采用“当月最后一天”策略。

### 11.5 配额状态

Xray 配额判断：

```text
current_xray_billed = (uplink + downlink) * multiplier
quota_exhausted = current_xray_billed >= traffic_limit_bytes
```

- `traffic_limit_bytes` 为空表示不限量。
- 到达额度后 Panel 更新期望状态，Agent 从相关节点移除用户。
- 最多允许一个采集周期加同步延迟的自然透支。
- 周期重置后自动恢复。
- 网卡响应头额度不得触发此状态。

## 12. SQLite 设计要求

建议核心表：

```text
users
subscriptions
subscription_nodes
nodes
agents
agent_certificates
registration_tokens
nic_bindings
xray_traffic_events
xray_traffic_hourly
nic_samples
nic_traffic_hourly
desired_state_revisions
command_results
plan_templates
schema_migrations
```

要求：

- 开启 WAL。
- 开启 foreign keys。
- 设置合理的 busy timeout。
- 写操作通过有限数量的后台写入任务批量提交。
- 流量、额度一律使用 64 位整数或 SQLite 可安全表示的非负整数范围。
- 所有关键去重键建立唯一索引。
- 用户列表、节点列表和周期汇总查询建立覆盖索引。
- schema 变更只能通过版本化迁移执行。
- `sqlx` 使用离线元数据支持可复现构建。
- 定期执行在线备份，并提供恢复校验命令。

## 13. TUI 信息架构

### 13.1 一级页面

```text
Dashboard | Nodes | Users | Subscriptions | Traffic | Settings
```

### 13.2 操作规范

- 全键盘操作。
- 方向键或 `j/k` 移动。
- `Enter` 打开详情。
- `Esc` 返回或关闭弹窗。
- `/` 搜索。
- `Tab/Shift+Tab` 切换区域。
- `r` 刷新或重试当前对象。
- `n` 创建当前类型对象。
- 危险操作必须显示明确确认弹窗。
- 快捷键可配置，但同一键在各页面含义保持一致。

### 13.3 状态展示

- 在线、离线、同步中、异常必须同时通过文字和颜色表达，不能只依赖颜色。
- 时间统一显示时区并提供精确时间。
- 流量使用 IEC 单位显示，详情可查看精确字节。
- 列表宽度不足时优先隐藏次要列，不截断用户名、节点名和关键状态。
- 后台刷新不得改变当前选中对象；对象消失时选择最邻近行。
- 错误提示必须包含可操作原因，不显示原始堆栈给普通界面。

## 14. 安全需求

### 14.1 管理面

- TUI 是唯一管理界面。
- Panel 数据库、配置和密钥文件权限默认仅服务用户可读。
- gRPC 使用 mTLS。
- Xray API 只监听本机。
- Agent 身份可以单独吊销，不使用全局共享永久 Token。

### 14.2 订阅面

- 订阅端点只读。
- Token 不可预测、可轮换、可吊销。
- HTTP 日志屏蔽 Token。
- 响应不包含 Panel 内部节点地址之外的管理信息。
- 禁用、过期订阅统一返回无敏感差异的响应。

### 14.3 密钥和敏感信息

- 注册 Token 和订阅 Token 只存哈希。
- TLS 私钥加密或最少使用严格文件权限保存。
- 数据库备份视为敏感文件。
- 日志不得包含完整 Token、私钥或完整订阅正文。
- 安装脚本和发布二进制必须支持完整性校验。

## 15. 性能与资源目标

以下为第一版工程目标，验收时以指定测试环境实测，不把 Xray 自身开销算入 Rust Agent 额外开销。

- Agent 空闲额外 RSS 目标不超过 20 MiB。
- 1000 个动态用户、10 秒采集周期下，Agent 平均 CPU 目标低于单核 2%。
- Agent Rust 代码和依赖的剥离后体积目标不超过 15 MiB；最终文件还包含 Xray，因此总大小以嵌入内核为主。
- Panel 支持至少 100 个在线节点和 10000 个订阅的日常 TUI 操作。
- 用户列表聚合查询目标在 200 ms 内完成。
- TUI 输入到下一帧显示目标低于 100 ms。
- 心跳和流量使用批量 protobuf 消息，不为每个用户创建独立网络请求。
- Panel 不在内存中长期保存完整时序数据，只缓存界面所需快照。

建议 Release 配置：

```toml
[profile.release]
lto = true
codegen-units = 1
panic = "abort"
strip = true
```

是否使用 `opt-level = "z"` 应分别对 Agent 体积和 CPU 做基准测试，不盲目牺牲采集与 protobuf 性能。

## 16. 故障与恢复

### 16.1 Panel 重启

- SQLite 是状态真源。
- 重启后恢复周期任务和 Agent 会话。
- Agent 自动重连并发送当前修订和未 ACK 流量。
- Panel 对比期望状态并按需下发差异或完整快照。

### 16.2 Agent 断线

- Panel 在超过心跳阈值后标记离线。
- 用户和节点配置仍保留。
- Agent 保留未 ACK 流量事件。
- 恢复连接后先完成身份验证，再补报事件和协调状态。

### 16.3 Xray 重启

- Agent 生成新的 Xray 实例 ID。
- 重新应用全部期望用户。
- 统计计算识别实例变化，不把新计数器与旧绝对值相减。
- Panel 展示重启次数和最后退出原因。

### 16.4 网卡计数器异常

- `boot_id` 变化、计数下降或接口消失时停止旧序列。
- 新序列从零增量基线开始。
- 绑定页面显示数据中断区间。
- 不通过猜测填补缺失流量。

## 17. 限速功能边界

Xray 动态用户 API 可以增删用户和获取统计，但不提供通用、可靠的按用户 Mbps 限速能力。

第一版建议：

- 数据库和套餐保留 `rate_limit_bps` 字段。
- TUI 明确标注未启用时不生效。
- 不虚假宣称已完成限速。
- 配额和到期控制通过动态移除用户实现。

后续如必须实现精确限速，需要单独评审以下方案之一：

- 每订阅独立入站/端口并结合 Linux `tc`。
- 可传递 socket mark 的独立网络路径。
- 在 Xray 前增加支持身份限速的代理层。

这些方案会增加端口、规则和内存开销，不纳入轻量第一版。

## 18. 测试要求

### 18.1 单元测试

- Xray 上下行汇总和 1x/2x 倍率。
- 用户、订阅、节点多层聚合。
- 网卡 `RX+TX`、仅 RX、仅 TX 计费用量及统一 `upload=0` 响应头映射。
- 多网卡直接求和。
- 周期边界、闰年、月末和手动重置。
- 网卡计数器回退和 `boot_id` 切换。
- 流量事件幂等去重。
- 中转地址覆盖和 URI 编码。
- VLESS、Mihomo、Sing-box 渲染快照。
- 额度、到期和状态机转换。

### 18.2 集成测试

- Panel/Agent 注册、mTLS 和证书轮换。
- Agent 断线重连和未 ACK 流量重放。
- 期望状态同步、重复命令和修订冲突。
- SQLite 并发读写、迁移、备份和恢复。
- 订阅 Token 哈希查找、轮换和吊销。
- HTTP 响应正文和 `subscription-userinfo`。

### 18.3 Linux 端到端测试

- Agent 从 memfd 启动真实 Xray。
- 动态增删用户不重启 Xray。
- 杀死 Xray 后 Agent 自动恢复并重新同步用户。
- 停止 systemd 服务后不存在遗留 Xray 进程。
- `/proc/net/dev` 采样与系统工具读取值在合理误差范围内。
- Panel 重启期间 Agent 补报流量且无重复入账。

### 18.4 TUI 测试

- 关键 reducer/state 使用无终端单元测试。
- 小终端窗口下无崩溃、无重叠。
- 后台刷新不丢失选择和输入内容。
- 创建订阅向导可回退、取消并保证事务完整。

## 19. 验收标准

第一版完成必须同时满足：

1. Panel 能在 TUI 中创建节点并生成一次性安装命令。
2. Agent 能注册、使用 mTLS 长连接并持续上报状态。
3. Agent 能从嵌入资源通过 memfd 启动和守护 Xray。
4. Panel 能创建 `admin` 或新用户的订阅，并一次选择多个节点。
5. Agent 能通过 Xray API 动态增删用户，Xray 重启后能恢复期望状态。
6. Xray 流量按订阅和节点正确入账，重复上报不会重复累加。
7. 用户主页按当前周期 Xray 计费总量排序，详情可查看各订阅、各节点流量。
8. 网卡绑定不改变用户主页的任何 Xray 流量数据。
9. 未绑定网卡时，订阅响应头使用订阅 Xray 流量和订阅额度。
10. 绑定网卡时，响应头使用绑定网卡流量及其额度，并正确支持三种计费方向。
11. 节点中转字段只改变生成配置的地址和端口，不触发 Agent 转发操作。
12. 订阅端点无管理写接口，Token 可轮换并且日志不泄露完整 Token。
13. 配额耗尽、到期、周期重置和节点离线状态符合本文状态规则。
14. 停止 Agent systemd 服务后 Xray 被一并清理。
15. 关键单元测试、集成测试和 Linux 端到端测试通过。

## 20. 分阶段开发计划

### Phase 1：工程基础

- Cargo workspace、配置、日志和错误模型。
- SQLite 迁移和核心领域模型。
- protobuf 协议与版本协商。
- Panel TUI 主框架。

### Phase 2：节点与 Agent

- 注册 Token、mTLS 和长连接。
- systemd 安装流程。
- 系统与网卡采集。
- 节点 TUI、在线状态和安装命令。

### Phase 3：Xray 生命周期

- 架构化 Xray 嵌入与哈希校验。
- memfd 启动、日志、停止与崩溃恢复。
- Xray 本地 gRPC API。
- 用户期望状态协调。

### Phase 4：用户与订阅

- 合并式创建订阅向导。
- 用户、订阅和节点关系。
- UUID、Email、Token 生命周期。
- 配额、到期和重置状态机。

### Phase 5：流量

- Xray 用户流量采集、可靠上报和幂等入库。
- 用户主页排序和节点明细。
- 网卡绝对计数器、绑定、周期和响应头切换。
- 小时/天聚合和数据保留。

### Phase 6：订阅分发

- 最小只读 HTTPS 接口。
- VLESS/Base64、Mihomo、Sing-box 渲染。
- 中转地址覆盖。
- Token 轮换、限流和安全日志。

### Phase 7：发布质量

- 故障注入、端到端测试和恢复演练。
- Linux 真实环境通过 `scripts/linux-e2e.sh` 验证 release 构建、memfd 执行、Xray 崩溃重启、Agent 退出后的子进程回收，以及 `/proc/net/dev` 与 sysfs 网卡计数器一致性。
- `scripts/resource-benchmark.sh` 对常驻 Agent 采样二进制体积、RSS、峰值 RSS、CPU basis points 和 Xray 子进程 RSS。
- 1000 用户规模下的真实 Xray API 用户同步、流量查询压力测试和基于实测结果的二进制裁剪仍待 Linux 压测环境完成，不能用未嵌入内核的开发构建代替。
- Agent 升级与回滚。
- 数据库备份与恢复验证。
- 可选套餐模板。

## 21. 第一版明确取舍

- 创建用户和生成订阅在 TUI 中合并，后台模型分离。
- 订阅策略直接填写；套餐只做可选模板，可后置。
- 用户主页只展示 Xray 计费流量。
- 网卡绑定只控制 `subscription-userinfo`，不影响用户统计和用户启停。
- 多网卡绑定直接求和。
- 中转只保存发布 IP/域名和端口，不负责转发实现。
- 第一版先完成额度与到期控制，不实现复杂的按用户实时限速。
- Panel 和 Agent 各自保持单进程，避免不必要的服务拆分。

这组取舍是后续实现、测试和验收的统一依据。任何改变核心业务规则的需求，应先更新本文档和对应测试，再修改代码。

## 22. 参考 `singbox-manager` 的工程约定

`singbox-manager` 是本地已有的单节点 sing-box TUI 工具。本项目可以复用它已经验证过的交互和运维习惯，但不能直接复制其单节点数据模型。

### 22.1 建议继承的约定

- TUI 是主入口，CLI 只作为安装、诊断、脚本集成和灾难恢复辅助入口。
- 页面状态、键盘处理、业务服务和持久化分层，按键处理不直接执行长时间外部命令。
- TUI 执行 `journalctl`、安装、升级等外部命令时，先暂停终端渲染，命令结束后恢复 TUI。
- 表单根据协议或对象类型动态增删字段，只显示当前配置真正需要的参数。
- 状态提示设置生命周期，成功、警告和错误信息在一段时间后自动清除，同时保留日志页可追溯记录。
- 长列表保留选择位置，刷新或后台更新不应让选中行无故跳动。
- 所有配置变更先做结构校验和目标程序校验，校验成功后再原子替换并 reload。
- 提供 `status`、`check`、`doctor` 三类诊断能力：状态查看、单项校验、完整环境检查。
- `doctor` 使用 `OK/WARN/ERR` 分级，并且每条错误给出下一步排查建议。
- 数据库使用按编号的迁移文件，发布时检查迁移是否完整、可重复执行和可回滚备份。
- 提供明确的 backup/restore 流程，恢复前校验文件来源、SQLite 完整性和 schema 版本。

当前实现提供 `backup`、`check-db`、`restore` 灾难恢复命令。备份通过 SQLite 在线一致性快照生成并附带 SHA-256 文件；恢复要求 Panel 数据库独占锁，校验文件哈希、页完整性、外键、迁移版本及迁移 SQL 校验和，并在切换前保留现库及 WAL/SHM 回滚副本。可选后台任务在启动时和固定小时周期创建备份，只清理具有 `panel-*.db` 命名的自动备份。
- 订阅格式使用显式 `?type=` 优先、客户端 User-Agent 其次、默认格式兜底的选择顺序。
- 发布采用静态 Linux 二进制、systemd unit、安装脚本和 release checklist 闭环。
- `cargo fmt`、`cargo build --locked`、`cargo clippy`、`cargo test` 应成为每次发布的固定门槛。

### 22.2 本项目的差异化实现

- 旧工具以“用户名”为 Xray 统计和订阅主体；本项目必须以 `subscription_id` 生成稳定 Email，用户只是多个订阅的聚合视图。
- 旧工具可以把用户、UUID、额度和已用流量放在一张用户表；本项目必须拆分 `users`、`subscriptions`、`subscription_nodes`、`nic_bindings` 和原始流量事件。
- 旧工具的额度和倍率可能使用浮点字段；本项目统一使用字节整数和整数倍率，避免小数转换造成额度误差。
- 旧工具的单机节点允许直接编辑本地内核配置；本项目以 Panel 期望状态为真源，Agent 只负责协调 Xray 实际状态。
- 旧工具可以把本地订阅服务和内核放在同一个 systemd 服务中；本项目的 Panel 订阅 HTTP 服务与 Agent/Xray 服务分离，但 Panel 仍保持单进程。
- 旧工具的 nginx、端口复用和中转脚本不属于本项目核心；本项目只保存可选的订阅发布地址覆盖项。
- 旧工具的浏览器订阅统计页面可以作为未来的只读扩展，但不作为管理后台，也不进入第一版验收范围。

### 22.3 推荐的辅助命令

虽然管理工作流以 TUI 为主，发布后的可维护性建议提供以下无状态或低风险命令：

```text
panel status                 Panel 服务和数据库状态
panel doctor                 Panel、数据库、证书、订阅端点和 Agent 总体检查
panel backup <path>          创建一致性备份
panel restore <path>         停止写入后校验并恢复备份
panel sub show <id>          打印订阅 URL 和响应头预览
panel sub rotate <id>        轮换订阅 Token
agent status                 Agent、Xray、同步修订和采集状态
agent doctor                 Agent、memfd、Xray API、systemd 和接口检查
```

这些命令不得绕过领域服务直接修改 SQLite；所有写操作仍应复用与 TUI 相同的 service 层和事务。

### 22.4 新项目的发布检查清单增补

在旧项目发布清单基础上增加：

- Panel/Agent protobuf 版本兼容测试。
- mTLS 注册、证书轮换和节点吊销测试。
- Agent 离线期间的流量 spool 与 ACK 重放测试。
- Xray 实例重启后用户重建和统计基线测试。
- 同一订阅多个节点的 Xray 汇总测试。
- 同一订阅多个网卡绑定的 RX/TX 响应头测试。
- 网卡计数器回退、接口重命名和节点重装测试。
- 中转发布地址只改变导出配置、不改变 Agent 行为的回归测试。
- 订阅端点 Token 不出现在访问日志和错误响应中。
- 数据库备份恢复后，订阅 Token 哈希、用户额度和流量周期保持一致。
