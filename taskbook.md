# XPOST #23 · 接入 libbpf-mcu v0.1.1，交付中量实验入口

在 `nan-mu/xpost` 的现有独立 worktree 继续 [PR #23](https://github.com/nan-mu/xpost/pull/23)，保留 `agent/experiment-matrix-v1` → `agent/rbpf-vm`。代码审阅基线是 GitHub XPOST 提交 `9e73d84a6ac248c90c6b1f55cab33335c27cecf5`。开工记录 PR 的实际 head，沿用包含该代码基线的后续工作。本次修订接续 Gitea 任务书提交 `bd91249e7f3e149ab31b7e1286bb68b64cee0f26`。交付同一开放 PR。维护者审核就绪后在 T3 发令启动完整中量实验，再以正式 case 的一次 AOT Trace 交付 tcpdump 和事件时间戳两张实测图。

## 1. 延续 #23 已合入的实现

已发布的 #23 head 包含变基后的 `b473cf1`，以及 [#25 和 #26 的改动](tests/main-integration.md)。用 `code-review` 核对后续 diff 的设备身份、容量与写入边界。保留 #25 将 exec SRAM 槽和 copy buffer 提到 12,288 B、实际固件 cold cell 16,384 B 的行为。Agent 冷装上限按本次实际固件与分区处理，网络等载体维持各自限制。保留 #26 主机编码按计算所得长度分配的行为。Compact ELF 格式本身取消人为整文件上限，实际镜像受 SRAM 槽约束，实际写入受分区和传输约束。在已变基分支保留 #24 的设备身份复核及从同一份已核验字节写入的路径，避免引回 main 旧的本机 XDP build、配置回退或按路径重开文件。按 [配置说明](references/agent-profiles.md) 更新实际示例。将 [代码设计约定](references/code-design.md) 中适用内容纳入 XPOST 的长期设计文档，从现有架构说明引用。

## 2. 临时使用后台 Agent 和 HTTP

本轮实验由接 USB 的 Mac 后台运行现有 `xpost-agent`，TR3000 上的 `xpost-test` 通过其现有 HTTP 客户端访问。按 [后台 HTTP 启动步骤](references/agent-profiles.md#后台-http-启动步骤) 设置监听和 `[agent].endpoint`。实验使用 `/health` 和 `/v1/*`，不依赖 MCP 工具或会话。常驻方式和 MCP 传输的长期设计交由独立 PR。

`d803192` 已接通 v0.1.1 下载、v3 索引、缓存和固件绑定，删除了 `xpost-artifact`、`build_root` 和旧解析路径。复用 [主机预检记录](tests/agent-preflight.md) 与 [正式发布包检查](tests/release-v0.1.1.md)。固件随装和独立冷装沿用已验字节、固件与分区身份检查，以及排他设备操作。仍按上游程序 stem 和模式从归档选择容器，包括用 `case_mark_idle` 选择 `case_mark` 的 AOT 镜像。

用 `diagnosing-bugs` 从现有 `GET /v1/preflight/medium` 和 runner 追到目标准备和写入。中量仅核对 32 个程序×部署模式组合。网络三程序和 EXB2 七程序各检查 AOT/JIT/rBPF，case 两程序各检查 AOT。全量归档的 36 个容器继续作为独立清点证据。按实际计划另外核对每份链接固件及其目标槽。Metrics 和 Trace 各用实际构建产物及摘要，避免多个 profile 同名输出路径被后一次编译覆盖而误判。case 核对 A、B 两槽。隔离 JIT/rBPF 的 case 定址诊断，使它们的结果不进入中量就绪判定。用现有 HTTP 路径验证 TR3000 可读健康状态和预检报告，再用本轮规定的短冒烟确认 HTTP 触发的实际装载和采集。预检业务结果 `ok:false` 必须保留具体原因，阻塞相关写入，即便 HTTP 状态为 200。已通过的同版 CLI 归档验证直接引用。

## 3. XPOST 负责实际定址、装载和切换

用 `implement` 补齐 AOT 槽 B 的定址及板端 warmup、发布验收。最新主机预检已确认 `case_mark_idle` 因镜像地址不符且缺少定位信息被拒绝。从实际链接 ELF、XPM1 及分区配置取布局。发布的 v0.1.1 原生 Compact ELF 已定位至 `0x40380200`，helper 为 `0x40380000`，所有容器未见 ELF relocation 节。地址、helper、map 与布局已经匹配时，验证并复用发布字节。需要换槽时由 Agent 以发布的 ELF 载荷为基础定址，复用能证明与该载荷对应的定位信息，更新地址相关字段与摘要。部署不重链接 .o，不重跑 JIT。libbpf-mcu `retarget` 的重建可作离线对照。[v0.1.1 case 双槽对照](tests/case-v0.1.1.md) 已用正式发布载荷证明两个 case 程序在受测 A/B 布局中可保持 payload 不变。先将该证据接入 Agent 的槽位校验和容器封装，核对受测发布身份、目标地址和实际固件布局。保持已验证的 payload，更新目标槽地址和需更新的容器字段。此证据的适用范围限于所测 AOT 载荷与布局。超出该范围时先验证，无法证明安全定址就在写入前失败，并列出程序、模式、地址和所缺信息。板端预装非活动 SRAM 槽成功后才发布入口。发布前的定址、预装或校验失败时，保持当前槽与 generation 可运行。rBPF 沿用解释器校验和 map 绑定路径。

已在 QEMU 验证过三个 LLVM AOT 样例的零址到单槽改写，复用 [主机定址验证](tests/aot-single-slot.md)，跳过同范围复跑。用 `tdd` 核对新增 Agent 路径：热切换限定 AOT。对 case 的 A、B 槽定址，复用已记录的同布局直接链接载荷作独立预期。验收 Agent 准备后的实际目标地址、容器字段和 payload 摘要，并在板上核对 A/B 预装与发布。按 [AOT 热切换伪代码](references/medium.md#aot-热切换) 完成预装和发布，并核对发布前失败时活动槽与 generation 不变。任一必需 AOT 槽无法安全准备或未通过实测时，将 case 和整体中量就绪标为阻塞。冷安装和 warmup 属于准备。正式 case 只采集一次 AOT Trace 切换。采集同一次切换的 tcpdump 包记录和事件时间戳，并按 [case 两张交付图](references/medium.md#case-两张交付图) 生成两张图。验收活动槽、generation、boot ID 和数据面实际输出。网络和 EXB2 按各自模式验证正常装载。#25 与 v0.1.1 已有历史超限样本结果，复用 [容量与合并证据](tests/main-integration.md) 和 [发布镜像大小](tests/release-v0.1.1.md#镜像与容量)，只验证接入后的固件、运输及板端加载。缓存中的派生镜像与固件摘要、模式、helper 和目标槽绑定。

## 4. 删除旧 case 实验路径

用 `implement` 把 `case` 设为热切换的唯一实验选择，中量调度复用该入口。把两份策略的功能探测并入切换准备。复用探针发送和抓包设施，改用 v0.1.1 的 JTX1/CAS2 判据。删除旧目的 IP、5 字节探针和 case 的 `rxcnt` 检查。移除旧探测逐镜像清空冷安装表的流程。记录准备阶段的设备动作，正式采集前恢复 A 活动、B 已预装的状态。复用现有 A/B 预装、发布、tcpdump 和时间戳采集代码。根据 [AOT 热切换伪代码](references/medium.md#aot-热切换) 改写参与执行的实验模块注释。

- 删除 `case.rs` 的 `run_cold_latency` 单槽停用、覆写、重新启用循环，以及仅供这段测量使用的辅助代码。保留冷安装和两槽 warmup 所用的公共操作和写入检查。
- 将 `hot_swap.rs` 的 100 次往返测量改为一次 A→B Trace。删除独立的 `hot_swap` 实验选择、配置分派和与之并行的旧写入路径。中量与显式 `case` 走同一套切换代码。准备探测和短冒烟单列。
- 清理仅供退役实验使用的写入、建表与字段初始化、配置示例、命令说明及作图代码。先查 `case_latency`、`hot_swap_config` 等旧结构的实际调用方。仍需使用的抓包、Trace 与查询迁移到按 run_id 读取的正式入口。已有数据库表和历史记录保留可查询。替换 README 的旧冷/热对照说明，删除经引用检查只供该图使用的 `docs/figures/update-latency.png` 和 `.pdf`。旧结果通过 Git 历史追溯。将本轮正式 case 范围追加到 `docs/user-decisions.md`，保留原决策的历史。
- 用 `code-review` 从 `main.rs` 分派、配置校验、默认中量和数据库写入追到调用方。确认旧 `hot_swap` 选择在设备和数据库访问前被拒绝，冷切换循环及退役写入已从源码移除。用现有计划预检核对 case 只有 AOT Trace 一格。待 T3 正式发令后，从同一次切换的 run_id 查到一份原始抓包和一份 Trace，并验收两张图。

## 5. 接通中量实验和数据交付

保留 #23 已迁移的 `metrics` 和 `trace` 固件 profile 与构建入口。新 runner 和数据库使用相同命名，沿用 [项目已有命名决定](https://github.com/nan-mu/xpost/blob/b5a44072ed57657c7482758412cccf1fd93df6fa/docs/user-decisions.md#2026-11-15-collection-name)。按 [命名迁移表](references/agent-profiles.md#采集命名迁移) 核对 runner/config 分派、DB 新记录及文档是否使用新名。用 `code-review` 从实际入口检查旧 `cag`/`t` 形式是否退出正式路径，从 runner 的新名称各执行一条 Metrics、Trace 短探针，核对实际固件采集输出和新数据库记录的变体。将这两条检查并入本来要跑的采集冒烟，避免重复运行。保留历史数据、按需保留历史配置读取。采集变体、执行模式、上游产物名各用各自的字段。

用 `implement` 和 `tdd` 接通中量调度、实际固件的 perf prepare/arm/freeze/read/release、embeprom 指标、四模式写库及逐项查询。七个 EXB2 workload 使用 8258 B 缓冲、64 B 头，替换仍可达的旧 32 B map 与配置。复用 [v0.1.1 发布源码的七项 EXB2 主机向量](tests/exb2-host-vectors.md) 和 [四模式 SQL 夹具结果](tests/postgres-fixtures.md)。剩余验收从实际 runner 追到设备结果及 run_id。清理旧三模式写入、隐式重刷调用及失效入口，保留历史数据。按固件身份调度，同一固件的速率和重复放在烧录循环内层。依 [逐模块伪代码](references/medium.md) 把各项控制流和指标口径放到对应模块的 //! 注释。正式运行按 [中量矩阵](references/medium.md#中量矩阵) 逐格完成固定的 29 格。exec_bench 的四种执行模式全部只测 Metrics。case 只测 AOT Trace。同步更新代码仓库的 `xpost-test/procedures/medium.md`、预检、runner 和 PR 描述中的旧矩阵。按第 4 节清理旧 case 路径。以实验族、模式和变体为键，对账正式计划与实际数据库 run 的格集合及每格的 workload、流量条件和有效观测次数。case 正式只启动 AOT Trace，验收恰好一次成功 A→B 切换。历史记录、准备和短冒烟单列，失败和重试保留原始记录。逐项报告缺项和超出计划的执行。目标全程 ≤60 分钟，列出固件烧录、冷装、预装、切换次数。增速率点或重复次数时，固件烧录数不增加。交付可执行的短冒烟选择及启动命令。

**就绪交付。** 将 `run=["medium"]` 接到真实调度。以待交付的代码版本完成 TR3000 的 HTTP 预检，从正式 runner 的调度入口预览计划，逐格核对 29 格的实验族、模式、变体及格内条件。发令前执行 Metrics 与 Trace 短冒烟并按 run_id 查库。将 A/B 真机准备与切换验收纳入 Trace 短冒烟。从同一次冒烟的 run_id 读回原始 tcpdump 和所需 Trace 边界，验证两份证据可导出。冒烟单独记账。将实测过的版本、启动命令和任何阻塞写入英文 PR。维护者在 T3 发令后，用 `./xpost-test/scripts/xpost-test.fish` 启动正式中量。该脚本会构建、上传并运行 runner。

用 `code-review` 核对 v0.1.1 到实际运行、存储、查询的调用链和故障返回。主代理亲自实测剩余验收。对于已做的验证，先核对产物版本及摘要、代码和运行入口等适用条件，等价时直接引用并跳过。旧版本和原型记录只覆盖明确标注的范围。版本、摘要或路径变化时更新对应 tests 记录，写明旧结果仍适用的部分、实际补测结果和未验证的入口。用 `pr` 在同一英文 PR 列实际版本、复用证据、剩余验证、启动命令及就绪情况。缺项逐项标明。维护者在 T3 发令后，每完成一项按 run_id 查库分析。发现异常则修复、更新英文 PR，再将受影响项入队，保留原始尝试。case 分开记录发布结果和证据导出状态。发布前确认失败时可重试。已发布或发布结果未知时，先核对设备状态及原始记录，从同次切换补导证据。原始证据不足则报告缺项，由维护者决定是否重跑。将实测图嵌入 PR，标明单位、条件、样本数和 run_id。case 的两张图引用同一 run_id 和切换序号，明确主机与 DUT 的时钟边界。报告总墙钟和设备动作次数。
