# XPOST · 将 Agent 重构为 HTTP Daemon + CLI

开发仓库：[nan-mu/xpost](https://github.com/nan-mu/xpost)。新建独立 worktree 和开发分支，提交一个以最新 `main` 为 base 的英文 PR，留给维护者审核。当前审查基线为 `main@9cb03b795c8d45c5d107cf8e63b90dca6f1dcb56`。执行前获取最新远端 main，记录实际起点。预计 PR #27；编号由 GitHub 分配，不要求修改既有 PR。

## 目标与范围

维护者已选择方案 D：完全移除 xpost-agent 的 MCP 入口，保留 HTTP 服务；在同一个 xpost-agent 可执行文件中实现后台 Daemon 和短生命周期 CLI。开发者重新编译后显式重启，实验期间服务独立于 Shell、T3 Code 和 Codex 会话运行。TR3000 上的 xpost-test 继续直接使用现有 HTTP 客户端。

保留 `xpost-agent/Cargo.toml` 现有独立 host workspace、package、唯一 `[[bin]] name = "xpost-agent"`。CLI 和 Daemon 仅是同一二进制的不同子命令。不新增 Cargo workspace、crate、第二个可执行目标或 MCP proxy。Daemon 是唯一 USB 所有者。CLI 的设备操作通过 HTTP，不直接打开串口。保持 xpost-serial、设备协议、固件与 XDP ELF 的部署语义不变。实现设备级进程独占，不实现跨多条请求的 Experiment Lease。本轮不自动重启服务、不进行正式性能实验、不修改实验矩阵、数据采集或数据库。

## 现状与兼容边界

- [main 的 main.rs](https://github.com/nan-mu/xpost/blob/9cb03b795c8d45c5d107cf8e63b90dca6f1dcb56/xpost-agent/src/main.rs) 记录现有 HTTP 契约、默认端口示例与旧 MCP 入口；`http.rs` 和 `mcp.rs` 是两种入口，`ops.rs`、`state.rs` 共享操作与 SerialHandler。现有设备错误可能是 HTTP 200 加 `ok:false`，不是仅靠 HTTP 状态判断。
- [main 的 AGENTS.md](https://github.com/nan-mu/xpost/blob/9cb03b795c8d45c5d107cf8e63b90dca6f1dcb56/AGENTS.md#agent-access) 要求 Agent 必须使用 MCP，禁止 HTTP 替代，并给出旧启动命令。维护者本轮明确撤销这些 MCP 专用限制；替换其 Agent access 章节。禁止绕过 Daemon 直接操作串口的限制仍有效。启动、停止或重启必须是明确的操作，不因普通设备命令隐式发生。
- [PR #23](https://github.com/nan-mu/xpost/pull/23) 在起草时仍开放，观察版本为 [9e73d84](https://github.com/nan-mu/xpost/tree/9e73d84a6ac248c90c6b1f55cab33335c27cecf5/xpost-agent)。它已有 Release 接入、只读 preflight 及新的 perf 操作；其 Mac HTTP 临时启动路径服务于 TR3000 实验。当前任务独立于 #23，不将该 PR 的实验或部署改动 cherry-pick 到本 PR。若执行时 #23 已合入 main，保留并迁移合入后的全部 HTTP 能力；若仍未合入，按实际 main 实现，读 #23 的 HTTP 路由与 Agent 文档确认整合风险，列出尚未进入 main 的接口，交维护者在整合时核对。不得宣称未合入的接口已经实测。#25/#26 已合入 main，保留其修改。
- 术语沿用仓库：Firmware ELF、XDP ELF、SRAM slot。若基线存在 `docs/user-decisions.md`，按其规则保留旧记录并追加本次用户决定；不覆盖无关决定。本任务的 MCP 迁移是用户明确授权的变更，不是绕过旧规则执行实验的许可。

## 1. 独立后台服务与显式进程管理

用 `implement` 和 `tdd` 从公开 CLI 入口建立可运行的服务管理切片，再接入现有设备操作。实现以下子命令。具体参数布局、内部模块划分、锁与进程管理库由执行者选择；命令的含义不得改变。

| 命令 | 可观察行为 |
| --- | --- |
| `xpost-agent serve` | 前台运行 HTTP Daemon；接收现有 bind/log-dir 配置，启动成功后可访问 /health。 |
| `xpost-agent start` | 启动同一二进制的后台 Daemon；等待本次实例完成初始化和 HTTP 就绪后才成功退出。 |
| `xpost-agent stop` | 安全停止当前 Worktree 的受管理实例，等待实际退出及 USB 释放。 |
| `xpost-agent restart` | 停止当前 Worktree 的受管理旧实例，再启动当前调用的二进制；验证新实例就绪。 |
| `xpost-agent status` | 显示运行、未运行或异常状态及实例身份；可机器读取，不打开 USB、不启动服务。 |
| `xpost-agent logs` | 查看受管理实例的后台服务日志；保留原有串口 transcript 及轮转机制。 |

默认监听 `127.0.0.1:9240`；提供显式 `--bind` 以继续支持实验局域网，例如 Mac 上 `0.0.0.0:9240`。说明这是无认证的现有设备控制接口，只在受信实验网络暴露；默认不监听外部网卡。CLI 默认连接本地服务，支持显式 endpoint；保持 `XPOST_ROOT`、`XPOST_PROFILES` 和相对路径语义，解析并记录实际 Worktree、profiles 与日志目录，不能依赖后台进程偶然继承的 cwd。服务管理仅操作本地、经身份核验的实例，不对任意远端 endpoint 发送进程控制请求。

后台进程的生命周期独立于启动它的 CLI 和 Shell：启动者退出、终端关闭或客户端断连后继续提供服务。不依赖用户手动配置 MCP、launchd 或 systemd；不要求安装常驻系统级服务。记录足够的日志与启动错误。启动失败、端口占用、USB 不可用、就绪超时必须返回非零并处理本次未完成启动的进程与状态，不误操作其他实例。相同配置的重复 start 不产生新进程；若配置或版本不同，明确拒绝并报告差异，不隐式 restart。停止不存在的实例不得杀死其他进程；陈旧 PID 记录及 PID 复用不能导致误杀。

stop/restart 不得在已经接受的设备写入、烧录、装载或其他正在运行的操作尚未完成时强制终止 Daemon。允许等待安全完成或明确拒绝并返回忙碌；开始安全停止后不再接收新写入。HTTP 超时或客户端断开不等于 spawn_blocking 中的操作已取消。不要将此保护扩展为跨请求的实验独占机制。维护者负责在实验全部结束后再显式重启。

验收（真实子进程和 HTTP 请求；使用测试替身避免设备写入）：

- 在一个 Shell 启动 Daemon，等待 start 成功；结束启动 Shell，再从另一 Shell 请求 /health 与 status，实例身份不变。客户端断开后重复查询。stop 后 HTTP 不再可达，原实例退出。
- 重复及并发 start 只产生一个受管理实例。模拟端口占用、启动失败、超时、陈旧 PID、无服务和另一个无关进程占用 PID/端口，确认返回状态正确且不终止无关进程。
- 模拟尚未结束的设备操作（包括 HTTP 请求超时后的后台工作），尝试 stop/restart，检查未中断设备操作且不会并行启动新实例；完成操作后可正常停止。不要只依据 HTTP 响应判断设备操作已经结束。

## 2. 设备级独占与运行版本

`state::open` 打开共享 SerialHandler；锁必须覆盖整个 Daemon 对物理 DUT 的占用期，包括 flash 导致的 USB 重连。使用操作系统可释放的独占机制或等效可靠方式。锁不以 Worktree、相对日志目录或 HTTP 端口作为唯一身份。同一物理设备在不同 Worktree、不同监听端口下也只能有一个 Daemon；不能确认设备独占时失败关闭。锁在首次设备操作之前获得，在设备资源释放后释放。退出或崩溃后可恢复，无关 Worktree 不得通过 start、stop 或 restart 抢占设备。

status 需要提供 PID、实例唯一标识、监听 endpoint、规范化 Worktree、实际配置来源、日志路径、正在运行的二进制身份，以及 DUT 的可用状态或明确的未知/忙碌状态。服务进程身份不能只靠 PID 或 Git SHA。记录 Git revision（可用时标识 dirty）和能够区分同一 Git revision 下不同编译产物的身份，例如启动时实际可执行映像的 SHA-256。运行身份在该进程生命周期内稳定；不能在查询 status 时直接读取已被新构建覆盖的路径，误报旧进程已更新。普通 status 不应因阻塞的 USB 操作无限挂起。保持 /v1/status 既有消费契约，管理状态使用独立接口或兼容的增量字段。

从当前 Worktree 使用 CLI 时，能检查服务身份是否匹配；默认本地设备操作不能静默命中另一个 Worktree 的服务。版本不匹配时报告旧服务身份及显式 restart 的办法。status/logs 仍能用于诊断，不因版本差异失去查看能力。

用 `tdd` 验收：
- 用同一 Git revision 的两个可区分的真实构建验证：启动 A，构建 B（或将同一运行路径替换为 B），A 的服务身份仍为 A；执行 B 的 restart 后，新实例身份为 B。验证不是仅通过比较 Git SHA 或当前磁盘文件。编译失败不会停止旧服务。
- 从两个不同 Worktree 以不同端口申请同一测试设备身份，后启动者明确失败。原进程、锁与 HTTP 服务保持有效；原实例退出后，新实例可启动。验证设备身份相同的路径别名及重连处理，不能只测试端口碰撞。使用实际跨进程锁并验证崩溃后的释放。记录测试替身与真机证据的边界。
- 在第二 Worktree 使用生命周期命令，确认不能停止或重启第一 Worktree 的实例；本地 CLI 不会向错误实例发出写请求。

## 3. 将现有设备能力全部迁移至 CLI，移除 MCP

用 `implement` 和 `tdd` 逐类接通 HTTP 客户端。以执行基线 `http.rs` 的路由和 `mcp.rs` 的工具定义为迁移清单。CLI 暴露全部当前设备控制能力，不能只实现生命周期命令或只实现部分 MCP tools。建议沿用以下动词/对象命令结构；完整参数和响应见实际 HTTP 契约，`--help` 必须说明调用形式。

| CLI | 对应 HTTP |
| --- | --- |
| `dut status`、`get init`、`get finish` | /v1/dut_status、/v1/get/init、/v1/get/finish |
| `flash fw <name>`、`flash xdp <name> [--slot N]` | /v1/flash/fw、/v1/flash/xdp |
| `clear xdp <names>`、`get xdp` | /v1/clear/xdp、/v1/get/xdp |
| `enable xdp`、`disable xdp` | /v1/enable/xdp、/v1/disable/xdp |
| `xdp switch`、`xdp status` | /v1/xdp/switch、/v1/xdp/status；保留 slot、generation、policy |
| `enable exec`、`disable exec` | /v1/enable/exec、/v1/disable/exec |
| `clear map <name>`、`get map <name>`、`update map <name>` | /v1/clear/map/{name}、/v1/get/map/{name}、/v1/update/map/{name}；支持单项及批量 |
| `clear perf`、`get perf` | /v1/clear/perf、/v1/get/perf |
| `get serial`、`run` | /v1/get/serial、/v1/run；保留时间窗口和现有调试命令边界 |

若 #23 已进入开发基线，保留原有离线 `preflight`（不打开 USB），为 /v1/preflight/medium、/v1/preflight/xdp/{name} 及 /v1/perf/{prepare,arm,freeze,read,release} 提供 CLI 入口，并保留 flash identity、preflight 结果、epoch、boot_id、offset 和 digest 等结构化返回值。未合入时不额外实现 #23 的业务逻辑。已存在的 HTTP 能力以源码为准；完成的迁移清单需能从 CLI 的 help 与路由表逐项核对。

CLI 与 xpost-test 共同使用现有 HTTP 路由；不复制 ops、串口访问或另造 RPC。保留请求参数、方法、成功与失败响应、Map JSON、Perf 数据和长操作等待语义。CLI 提供机器可读 JSON，stdout 不混入日志；保留结构化字段。连接失败、HTTP 失败、响应解析失败、业务 `ok:false` 均以非零退出。设备命令不自动启动 Daemon、不盲目重试有副作用的请求。普通命令无服务时明确提示如何启动。

删除 `mcp.rs`、`mod mcp`、`/mcp` 路由及 MCP service/session 初始化、`rmcp` 和仅用于 MCP 的 schema 依赖，更新 Cargo.lock。移除受版本控制的 xpost-agent MCP 配置、旧工具说明、启动方法和禁止 HTTP 的 Agent 规范；不修改其他项目或用户全局 Harness 的配置。不要保留 stdio MCP、兼容 MCP 路径、旧的隐式 daemon 启动入口或 CLI 到串口的备用路径。

验收：
- 用记录请求的 HTTP 测试服务逐项检查 CLI 设备能力的方法、路径、关键参数、响应及退出码。Map 单项/批量、slot、generation、policy、serial 时间窗口与结构化数据必须覆盖；不能以 curl 成功代替 CLI 验收。检查 HTTP 200 + ok:false、不合法 JSON 和连接失败均非零退出。
- 对照执行基线 HTTP route table 和 xpost-test 的真实客户端验证 /health 和 /v1/* 的兼容性。使用独立固定的期望请求/响应样例，不从被测 CLI 的结果生成期望。保留现有 typed endpoint 及 map/perf framing。执行基线适用的 host 回归测试。
- 检查同一 Cargo manifest 的 metadata 仍只有原有 xpost-agent 可执行目标；原独立 host workspace 不变。构建后检查依赖和源码，/mcp 请求不再进入 MCP 服务，help、README、AGENTS 和运行说明均不再要求 MCP。文档中说明删除旧入口的迁移信息不算残留调用路径。

## 4. 真实使用入口与交付

在仓库中用简短操作说明替代旧 Agent MCP 说明；让 Codex 先读 `xpost-agent --help` 并检查命令退出码。记录从实际 xpost-agent workspace 构建、前台调试、后台启动、status、logs、显式 restart、stop 及 CLI 控制设备的完整流程。记录 TR3000 保持原 `[agent].endpoint` HTTP 配置、Mac 显式监听实验网络的方法。不要要求改 T3/Codex MCP 注册，也不要让 xpost-test 通过 subprocess 执行 CLI。

主代理亲自从正常入口执行上述未验证的测试。按仓库 Cargo 配置选择 host target（Mac 示例 `aarch64-apple-darwin`），执行 xpost-agent 的 host 测试、格式检查及实际 build；在不运行实验、不写数据库的前提下验证 xpost-test HTTP 客户端的相关回归。用测试替身验证进程管理、HTTP 调用和独占。只有确认无实验、设备可安全使用且获得设备操作授权时才进行真实硬件验证；本轮不安排烧录或正式性能实验。不要用替身通过冒充真机通过。没有真机证据则在 PR 中明确指出实际 USB 独占、重连和释放尚待真机确认。

用 `code-review` 审查最终 diff：检查全部迁移、同一 workspace/binary、旧路径删除、HTTP 契约、进程身份、并发与错误处理、文档、#23 整合边界。使用 `pr` 汇总英文 PR，列明基线、实际命令与结果、被测进程/二进制身份、锁竞争证据、HTTP 兼容证据和未验收项。遵循 T3 的 `task-brief-to-pr` 流程及指定 GitHub MCP 创建新 PR，不使用 gh 绕过；不自动合并，不修改 #23 分支或触发正式实验。任务书作者仅完成静态源码调查，未运行任何编译、设备或实验测试；没有已有测试结果可复用。
