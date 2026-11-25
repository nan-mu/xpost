# XPOST #27 · 先 rebase PR23，再简化 Agent 并完成实验控制接口

在 nan-mu/xpost 的现有独立 worktree 继续 [PR #27](https://github.com/nan-mu/xpost/pull/27)，保留 agent/http-daemon-cli，先 rebase 到 PR23 的 agent/experiment-matrix-v1，再修复。PR27 的 base 改为该 PR23 分支，见第 0 节。审核代码基线为 179ce44d8fe3227b2b80d1b4ddfb50f1e3f13b1a；Gitea 修订基线为 66c07e73f24a711836b58fdcdb155b1efbfa8d03。开工先核对当前 PR head，接续已有实现，不新开 PR。交付同一英文开放 PR，等待维护者审核，不自动合并。

项目共享记录采用 Gitea main@ccc86c1e7252d4f19eb96a586cdab2c682d9b863 的 [GLOSSARY.md](https://gitea.nas.nan-mu.cc/mcp/xpost/src/commit/ccc86c1e7252d4f19eb96a586cdab2c682d9b863/GLOSSARY.md) 与 [decisions.md](https://gitea.nas.nan-mu.cc/mcp/xpost/src/commit/ccc86c1e7252d4f19eb96a586cdab2c682d9b863/decisions.md)。本轮取代上一版后台管理和运行映像一致性要求。沿用 Firmware ELF、XDP ELF、SRAM slot、Cold storage cell 的术语。

## 目标与边界

维护者已确认：Agent 按需启动，实验/调试期间持续运行，结束后退出。Cargo 负责构建与启动；执行会话负责维持进程；Agent 不管理自身后台生命周期。Health 用启动时 espflash Board Info 确认板可访问，除此不主动探索硬件异常。修复清除 XDP 的危险参数，并使 Agent 接口满足 PR23 的实验控制需求。

保留原 xpost-agent Cargo host workspace、package 和唯一 xpost-agent 二进制，CLI 和 HTTP server 同工程。保留已有 typed HTTP、设备 CLI、USB 独占、串口 transcript 与实际操作保护。MCP 保持删除。xpost-test 仍通过 HTTP 访问，不执行 CLI 子进程。Agent 不负责产生刺激包、实验矩阵、run_id 调度、SQL、图表、PR 结果发布。无 Experiment Lease。PR23 业务作为 rebase 后的基线，PR27 只交付 Agent 及必要的接口兼容改动，不接管实验实施；第 4 节规定本轮必须完成的接口。此次真机验证只做 Board Info，不烧录、不跑实验、不探索物理重连/异常 USB。

## 0. 先 rebase PR27 到 PR23

本节是后续修复的前置步骤。使用现有 PR27 worktree，先确认工作树干净、未提交工作已妥善保存。fetch 两个远端分支并记录 SHA。PR27 审核 head 为 179ce44d8fe3227b2b80d1b4ddfb50f1e3f13b1a，PR23 起草时最新 head 为 22f19e2970eaab37a8caeafae65cc3a646ec3c8e；实际 rebase 使用开工时已核对的 PR23 最新远端 head，不退回旧审核基线。

在 PR27 分支上，将原 PR27 独有提交重放到 PR23 head（核对实际 merge-base 后使用 git rebase --onto；不要把 PR23 rebase 到 PR27）。保留 rebase 前的本地引用以供 range-diff 和恢复。解决冲突后核对 PR23 head 是新 HEAD 的祖先、PR27 相对 PR23 的 diff 仅包含 Agent 重构与本轮必要修复。不得改写 PR23、main 或其既有提交，不 cherry-pick 重复的 PR23 提交，不借此丢弃任何 PR23 实验或 Release 改动。完成 rebase 后再执行第 1–4 节。

特别注意 main.rs、http.rs、ops.rs、state.rs、profiles.rs、Cargo.toml/lock、AGENTS.md、xpost-test/common/src/agent.rs 和 Agent 启动说明的双侧修改。以 PR23 的实际业务、固件与客户端契约为基准，接入 PR27 的 CLI、Operation Guard 与 HTTP 入口；彻底删除 MCP 及退役后台管理。保留 PR23 当前的 Release 解析、Firmware ELF SHA-256 绑定、XDP ELF 校验、Perf 采集、EXB2 传输、runner、协议库及 campaign 恢复逻辑。保留 #24/#25/#26 已进入 PR23 的成果。若 PR23 在执行期间继续推进，先记录基线差异；交付前核对最新 head，必要时再 rebase 并补测受影响的接口，不无限等待正在推进的实验分支。

GitHub PR27 的目标分支改为 agent/experiment-matrix-v1（PR23 head 分支），使 PR 页面只展示 PR27 增量；若 PR23 开工前已合并至 main，先核对其提交确实已进入 main，再以包含该结果的 main 为基线。若无法确定等价合入或目标分支消失，报告具体冲突，不擅自选择另一条基线。只允许在实际需要发布 rebase 后的历史时，对 PR27 自己的 head 使用带已核对旧远端 SHA 的 force-with-lease；禁止无条件 force push。远端 PR27 有他人新提交时先读取并保留，不覆盖。通过指定 GitHub MCP 更新同一 PR 的 base/正文，不新建 PR，不合并。Gitea 任务书历史不 rebase。

PR23 已有中量 campaign 和数据库结果。不得停止其运行中的 Agent、runner、采集或切换，不抢占 USB，不修改现有 campaign、run_id、DB 或实验配置。所有主机测试使用隔离 fixture，真实 Board Info 仅在确认 PR23 已释放设备且获准使用时执行。若设备仍被占用，完成其他验收并明确报告 Board Info 待验收；不启动 PR27 的正式中量，不重跑已有效的结果。

## 1. 删除后台生命周期框架

用 implement + tdd 从正常入口改为前台运行。在 xpost-agent 目录：

```fish
cargo run --release --locked -- serve --bind 0.0.0.0:9240
```

以上用于受信实验网络；不指定 bind 时默认 127.0.0.1:9240。实验期间维持同一执行会话；结束后 Ctrl+C/SIGINT（或 SIGTERM）安全退出，释放设备锁，再以 Cargo 重新构建并启动。不得隐式启动或重启服务。配置和日志路径仍从正确 Worktree 解析；保留 XPOST_ROOT、XPOST_PROFILES、--bind、--log-dir。日志直接输出至该终端，保留现有串口日志轮转。CLI 默认连接 127.0.0.1:9240，显式 --endpoint 可选择其他 HTTP origin。没有服务时明确失败。默认本地调用仍需防止误操作另一 Worktree 启动的服务；采用最小的无持久状态 HTTP 身份核对即可，不恢复运行映像比较或实例发现协议。显式 endpoint 是调用者主动选择的目标。

删除 src/lifecycle.rs 中专用于后台管理的代码（[现有实现](https://github.com/nan-mu/xpost/blob/179ce44d8fe3227b2b80d1b4ddfb50f1e3f13b1a/xpost-agent/src/lifecycle.rs) 492 行），只迁移仍必要的设备锁或无状态工具。删除 start、stop、restart、logs、Unix control socket、/control/*、后台启动握手、setsid、PID/last.json/实例目录/command.lock、后台日志和自动启动路径。删除仅用于运行映像身份检查的 build.rs、CDHash、/proc/self/exe 哈希等逻辑和无用依赖；如依赖仍服务于其他业务则保留。合并 status/get status 的重复用途：保留现有 HTTP /v1/status 消费契约和唯一清晰的 CLI 状态入口，不维护第二套状态管理。更新 AGENTS.md、README、CLI help、测试与 TR3000 启动说明，删除已退役生命周期命令及测试。不要以其他进程管理框架替代被删除的代码。

保留真实跨进程 DUT 独占锁（当前单 DUT 场景用主机级 flock 可满足），先获锁再访问设备，退出后释放。信号到来后停止接收新设备操作，等已经接受的操作真正结束再退出；HTTP 超时/客户端断开不等于 spawn_blocking 取消。保留现有 Operation Guard 的关键语义。一个多阶段 flash/fw 或冷装操作内部不得与另一设备操作交错，复用已有串行机制，必要时将排他范围覆盖整条业务操作；不是跨多次请求的实验租约。

验收：从实际 CLI 和 HTTP 入口启动前台测试进程，确认可访问，执行 SIGINT/SIGTERM 后退出；另一个 Worktree 在不同端口尝试同一 DUT 被拒绝，首个进程退出后可启动。模拟一个已接受的阻塞操作超时后仍运行，退出请求必须等待该操作完成；同一设备的两条操作不得交错。检查正常 help 及源码不再有退役后台管理能力。旧的后台进程脱离、PID 复用、同 Git revision 双映像重启、配置继承等测试不再是验收要求，不运行这些退役测试。

## 2. Health 的唯一硬件检查

现在 [http.rs:486–493](https://github.com/nan-mu/xpost/blob/179ce44d8fe3227b2b80d1b4ddfb50f1e3f13b1a/xpost-agent/src/http.rs#L486-L493) 无条件返回 ok:true；[SerialHandler::open](https://github.com/nan-mu/xpost/blob/179ce44d8fe3227b2b80d1b4ddfb50f1e3f13b1a/xpost-serial/src/client/mod.rs#L72-L83) 只解析端口并创建 worker，不实际确认硬件可访问。用 implement + tdd 将 espflash Board Info 接到启动流程：解析配置 → 获 USB 锁 → 用实际选定 DUT 执行 Board Info → 结束下载会话并恢复 App → 初始化/开放 HTTP。复用现有 espflash/SerialHandler 下载路径与板型配置，不另建独立 workspace 或工具。Board Info 成功才允许 GET /health 返回 {"ok":true,"message":"ok"}；失败则启动失败、返回非零、输出实际原因并释放资源。保留 /v1/dut_status 用于已有 App ping/settle 流程，它不是启动健康检查的额外门槛。禁止每次 /health 查询重新执行 Board Info，避免干扰测量；health 表示本次启动已通过 Board Info 且服务在运行，不声称持续探测了 USB。

验收：测试替身注入 Board Info 成功及失败，确认成功开放健康服务且后续重复 health 不再调用 Board Info，失败未开放健康服务。接 USB 的 Mac 在确认无实验占用后从真实 serve 启动路径执行一次 Board Info，记录实际板信息和 health 返回。维护者明确指定 Board Info 走通即满足硬件可访问验收；不追加串口权限、未知设备、真实重连、烧录、Map/Perf 真机探测或正式实验。不能接板时明确报告未实测，不以替身冒充真机。

## 3. 修复清除和忙闲接口

用 diagnosing-bugs + tdd 修复 [http.rs:525–566](https://github.com/nan-mu/xpost/blob/179ce44d8fe3227b2b80d1b4ddfb50f1e3f13b1a/xpost-agent/src/http.rs#L525-L566)。Many 分支把空白项滤掉，可能生成空 Vec；[serial clear](https://github.com/nan-mu/xpost/blob/179ce44d8fe3227b2b80d1b4ddfb50f1e3f13b1a/xpost-serial/src/client/flash/xdp.rs#L189-L219) 将其解释为全区擦除。只有显式单独 "*"（现有字符串及单项数组形式）可进入清除全部路径。缺名、空字符串、空白字符串、空数组、含空项或混合 "*" 的列表均拒绝，不能靠 filter 转换为全部擦除。多名称指定删除先确认所有目标存在再进行第一次擦除，避免前一项已擦除后才发现后一项不存在。固件随装内部明确发起的全区清理语义不变。通过真实 CLI 与 HTTP 路由、记录擦除意图的后端验证合法单名/多名/星号，以及上述失败样例（特别是 [""," "] 和 ["valid","missing"]）；无效请求不能触发 erase，CLI 非零。此处不实际擦除硬件。

[PR27 /v1/status](https://github.com/nan-mu/xpost/blob/179ce44d8fe3227b2b80d1b4ddfb50f1e3f13b1a/xpost-agent/src/http.rs#L495-L497) 调用固定返回 idle 的 ops::get_status，而 [PR23 AgentClient::wait_idle](https://github.com/nan-mu/xpost/blob/22f19e2970eaab37a8caeafae65cc3a646ec3c8e/xpost-test/common/src/agent.rs#L430-L440) 以该字段决定是否继续。在 HTTP 超时后实际任务仍执行时报告 idle，会使后续实验误判设备已空闲。用 Operation Guard 的真实活动计数实现 /v1/status：idle 只在没有已接受的未完成设备操作且可继续接受请求时返回；有进行中/排队操作返回 busy，停止中不返回 idle。保持 JSON ok/message 格式，busy 不是传输失败。status 不能等待 SerialHandler mutex。用慢操作替身覆盖正常执行、HTTP 408 后仍 busy、实际完成后 idle，核对 PR23 wait_idle 不提前成功。不要为这个功能新增实验状态机。

## 4. 在 rebase 后完成全部 PR23 实验控制接口

依据 [PR23 Gitea 任务书 b7af1ff](https://gitea.nas.nan-mu.cc/mcp/xpost/src/commit/b7af1ff1ce2b11838cbd7a57f4322b87952963bf/taskbook.md)、[29 格控制流](https://gitea.nas.nan-mu.cc/mcp/xpost/src/commit/b7af1ff1ce2b11838cbd7a57f4322b87952963bf/references/medium.md)、[最新 AgentClient](https://github.com/nan-mu/xpost/blob/22f19e2970eaab37a8caeafae65cc3a646ec3c8e/xpost-test/common/src/agent.rs) 与 [最新 HTTP 路由](https://github.com/nan-mu/xpost/blob/22f19e2970eaab37a8caeafae65cc3a646ec3c8e/xpost-agent/src/http.rs#L137-L165)。与上一轮审核时的 PR23@5e435e0 不同，22f19e2 已有实际 EXB2 batch 路由和带固件摘要的部署接口，不能再将它们列为后续工作。当前任务必须在 rebase 后从真实实现完整接通 CLI 与 HTTP。实际 PR23 更新时以开工的已核对源码为准，保留新增有效接口。用 implement + tdd 逐项从正常入口验收；不得复制业务、使用空壳、仅写整合清单或让 xpost-test 改用 CLI 子进程。

| 实验能力 | 必须保留及迁移的契约 |
| --- | --- |
| 启动与预检 | /health、/v1/status、/v1/dut_status、/v1/get/init。按第 2、3 节修复 health 和 idle。保留脱板 preflight、preflight --inventory、preflight --xdp NAME，以及 GET /v1/preflight/medium、GET /v1/preflight/xdp/{name}；为 HTTP 预检提供 CLI 入口，不能让离线 preflight 依赖服务或 USB。medium 返回完整 preflight 对象，含 release、required、deployments、issues、prepared、hal_firmware；prepared 中 program/mode/profile/slot 与 firmware_profile/firmware_sha256 供 runner 分组。保留实际 PR23 的 32 个必需部署、36 个独立归档清点与目标布局/固件摘要检查，按最新计划核对目标，不将历史数值硬编码成新的判据。HTTP 200 + ok:false 必须阻断启动。 |
| Firmware ELF 与 XDP ELF 部署 | POST /v1/flash/fw {name,expected_sha256?}；POST /v1/flash/xdp {name,slot?,firmware?,expected_sha256?}。CLI 可传递这些参数；冷装的 firmware 和 expected_sha256 成对提供，slot 预装不能同时带冷装绑定参数。保持 PR23 的 release v0.1.1、v3 索引、已验字节写入、容量和设备身份复核，不恢复旧本地 XDP build。保留 flash.firmware_profile、firmware_sha256、containers 中 program/mode/profile/container_sha256，HAL 容器为空；独立冷装保留其实际身份。不得只保留 message。clear/get xdp、enable/disable xdp、slot 预装语义不变。摘要不匹配时在写入前失败。 |
| 网络 firewall/just_drop/just_tx | 保留 map clear/get/update（单项、批量）、enable/disable xdp、串口记录；完整 Map JSON、计数、合法 token 与各模式返回。网络刺激、排空、计数归属与 RTT 仍由 xpost-test 负责。 |
| Metrics / Trace | POST /v1/perf/prepare {uuid,variant,profile}、arm {epoch}、freeze {epoch}、read {epoch,offset,max_bytes}、release {epoch} 全部接通 CLI。uuid 32 hex、profile 64 hex、variant metrics 或 trace；保留 message 中原 key=value：prepare 的 epoch/boot_id、arm 的 tick、freeze 的 tick/flags/len/digest、read 的 total/digest/n/b64、release 的确认。保留原有 clear/get perf 的仍有效调用。禁止盲目 release 未成功导出的采集、重复 publish 或重试有副作用请求。分块按实际长度和 digest 验证。 |
| exec_bench EXB2 | POST /v1/exec/batch {"input_hex":...}，按 [AgentClient::exb2](https://github.com/nan-mu/xpost/blob/22f19e2970eaab37a8caeafae65cc3a646ec3c8e/xpost-test/common/src/agent.rs#L163-L188) 与 [ops::exec_batch](https://github.com/nan-mu/xpost/blob/22f19e2970eaab37a8caeafae65cc3a646ec3c8e/xpost-agent/src/ops.rs#L752-L814) 接通 typed CLI（例如 exec batch --input-hex 或 --input-file，由执行者选择明确入口）。输入为完整 8258 B EXB2、64 B 头；原实现 reset → 分块 write → 一次 invoke → 分块 read 作为一个独占设备操作。保留原 JSON result.boot_id、begin_tick、end_tick、execution_return、output（标准 base64，完整 8258 B），不能截断或只返回旧 32 B map/OK。保持错误与 invoke 结果未知语义，不隐式再执行；workload 正确性与 completed/status/result/scratch 由现有 runner 校验。不要增加新 board 协议。 |
| case AOT Trace | flash xdp --slot、xdp switch(slot,generation,policy)、xdp status。保留实际预检摘要、flash identity、A/B generation、boot ID、rollback/severe、from/to、quiesce_drop/pass、mcu_us、agent_rx。板端明确失败则 HTTP ok:false、CLI 非零且保留完整原回执；错误 generation 保持原活动槽，timeout 只能表示结果未知，不能自动重试。成功 message 仍由 PR23 case 消费。保留一次 A→B Trace 和 tcpdump 同 run_id 的逻辑，不增加冷切换或测量。 |

每个实际设备操作，包括 PR23 新增的 /v1/exec/batch 和 Perf 五个阶段，都必须经过同一 Operation Guard。guard 直到真实后台操作结束才释放。只读主机预检不进行板端探测或写入；其文件/配置使用与部署路径一致。保留完整的 JSON 结构（flash、preflight、result、Map fields）及 HTTP 200 + ok:false 失败语义，不把所有业务统一成只有 message 的回复。CLI 输出机器可读 JSON，错误非零，不自动重试。复用 PR23 的实际 typed client 和固定协议样例。

针对热切换，PR23 [ops.rs:663–702](https://github.com/nan-mu/xpost/blob/22f19e2970eaab37a8caeafae65cc3a646ec3c8e/xpost-agent/src/ops.rs#L663-L702) 仍把内层 XdpSwitch.ok 格式化为字符串，但外层 Rust Result Ok 可生成 ok:true。修复明确失败的 envelope，保留原始回执和时间戳；保持成功 message 的兼容性。用已知成功、失败/rollback/severe 回执与超时替身验收，不真实切换。不得凭超时推断切换未发生。

整合后运行全部新增 HTTP 路由和 CLI 的正常/失败 fixture，覆盖预检完整字段、摘要绑定与不匹配拒绝、HAL 空容器、Perf 两种 variant 和多块导出摘要、EXB2 输入/输出逐字节长度、单次 invoke、错误回执和忙闲状态。测试实际 PR23 AgentClient 对合成服务的消费，以及 CLI → Agent 的正常 HTTP 入口；至少用共享后端 fixture 检验新路由进入真实操作保护，不能只验证 URL 字符串或 curl。仍有环境阻塞时列出实际缺项，不声称完整中量实测。

## 5. 验收与交付

复用 [首轮 Host 验收](tests/pr27-host-baseline.md) 中与 rebase 后代码、入口、配置和产物仍等价的结果。PR23 当前已报告主机 Agent、Serial、Linux runner 和 Perf 检查通过，以及 15 份 Firmware ELF 摘要的 TR3000 只读预检；这些证据见 [PR23](https://github.com/nan-mu/xpost/pull/23) 和实际提交中的审计记录。不要将其描述为 PR27 新实测。因本轮触及 rebase、路由、设备操作、启动、状态和依赖，重跑受影响的 host tests、CLI/HTTP fixture、前台退出/锁竞争、参数拒绝、EXB2/Perf/部署与真实 AgentClient 回归，格式检查及实际 host build；检查 cargo metadata 唯一 workspace/bin、MCP 依赖已删除。复用仍等价的 PR23 离线 release/preflight 证据；必要时在实际环境做不占 USB 的预检验证。缺失本地 Wi-Fi build inputs 或其他依赖时按 PR23 已有获取途径与身份校验处理，不修改真实实验配置以绕过。未确认的实际结果记录缺项，不以 fixture 冒充真机。

用 code-review 审查 rebase 后最终 diff：核对 PR23 已保留、全部 CLI/HTTP 路由及结构化字段、Board Info、清除安全、busy/idle、失败回执、单一 workspace/bin、后台路径清理、实验边界。主代理亲自运行新增和受影响测试，用 pr 更新同一英文 PR，列明 rebase 前后 PR27 SHA、PR23 基线 SHA、PR base、冲突处理与保留的 PR23 功能、代码/依赖减少、启动/退出命令、Board Info 结果、接口测试、复用的证据和未验收事项。Board Info 是本轮唯一追加真机验证，且只在设备可用且不影响 PR23 时执行。正式 29 格测试、恢复 campaign、run_id 及逐项发布仍由 PR23 的授权流程负责；PR27 不启动或接管这些操作。不自动合并。
