# XPOST #23 · 接入 libbpf-mcu v0.1.1，交付中量实验入口

在 `nan-mu/xpost` 的独立 worktree 继续 [PR #23](https://github.com/nan-mu/xpost/pull/23)，沿用 `agent/experiment-matrix-v1` → `agent/rbpf-vm`。GitHub 审阅基线为 `5e435e083292cb7cb5d592c9406fdf76c791ee92`。开工记录实际 PR head，接续已有工作。Gitea 修订基线为 `20ab29f20b9d1fae43055af58b6bd5fe7eb6a890`。

**执行授权。** 维护者本轮已授权执行者在实现完成、只读预检通过后直接启动 29 格中量。中量承担全部真机验收。删除独立冒烟及二次发令的步骤。每产生一项结果，立即在同一英文 PR 展示并接收反馈。最终将 PR 留给维护者审核。

## 1. 保留 #23 已实现的行为

#23 已包含变基后的 `b473cf1`，以及 [#25 和 #26 的改动](tests/main-integration.md)。用 `code-review` 核对后续 diff 的设备身份、容量与写入边界。保留 #25 将 exec SRAM 槽和 copy buffer 提到 12,288 B、实际固件 cold cell 16,384 B 的行为。Agent 冷装上限按本次实际固件与分区处理，网络等载体维持各自限制。保留 #26 主机编码按计算所得长度分配的行为。Compact ELF 格式本身取消人为整文件上限，实际镜像受 SRAM 槽约束，实际写入受分区和传输约束。在已变基分支保留 #24 的设备身份复核及从同一份已核验字节写入的路径，避免引回 main 旧的本机 XDP build、配置回退或按路径重开文件。按 [配置说明](references/agent-profiles.md) 更新实际示例。将 [代码设计约定](references/code-design.md) 中适用内容纳入 XPOST 的长期设计文档，从现有架构说明引用。

## 2. 临时使用后台 Agent 和 HTTP

本轮实验由接 USB 的 Mac 后台运行现有 `xpost-agent`，TR3000 上的 `xpost-test` 通过其现有 HTTP 客户端访问。按 [后台 HTTP 启动步骤](references/agent-profiles.md#后台-http-启动步骤) 设置监听和 `[agent].endpoint`。实验使用 `/health` 和 `/v1/*`，不依赖 MCP 工具或会话。常驻方式和 MCP 传输的长期设计交由独立 PR。

`d803192` 已接通 v0.1.1 下载、v3 索引、缓存和固件绑定，删除了 `xpost-artifact`、`build_root` 和旧解析路径。复用 [主机预检记录](tests/agent-preflight.md) 与 [正式发布包检查](tests/release-v0.1.1.md)。固件随装和独立冷装沿用已验字节、固件与分区身份检查，以及排他设备操作。仍按上游程序 stem 和模式从归档选择容器，包括用 `case_mark_idle` 选择 `case_mark` 的 AOT 镜像。

基线的 [PR 实测记录](tests/agent-preflight.md#5e435e0-复验) 已覆盖主机 32/32 所需程序×模式、41/41 目标和四份 HAL 固件，以及只读 29 格预览。合成数据库夹具只覆盖 case 写入与查询。基线的 [中量执行入口](https://github.com/nan-mu/xpost/blob/5e435e083292cb7cb5d592c9406fdf76c791ee92/xpost-test/common/src/exam/medium.rs#L109-L121) 在预检之后退出。

用 `diagnosing-bugs` 从 `GET /v1/preflight/medium` 和 runner 追到目标准备和写入。中量仅核对 32 个程序×部署模式组合。网络三程序和 EXB2 七程序各检查 AOT/JIT/rBPF，case 两程序各检查 AOT。全量归档的 36 个容器继续作为独立清点证据。按实际计划另外核对每份链接固件及其目标槽。Metrics 和 Trace 各用实际构建产物及摘要，避免多个 profile 同名输出路径被后一次编译覆盖而误判。case 核对 A、B 两槽。隔离 JIT/rBPF 的 case 定址诊断，使它们的结果不进入中量就绪判定。从 TR3000 只读核对健康状态和预检报告。实际装载与采集从中量中的相应测试验证。任一必需项预检失败则停止本轮启动和设备写入。即使 HTTP 状态为 200，`ok:false` 也算失败。记录具体原因，修复并重做受影响的预检。已通过的同版 CLI 归档验证直接引用。

## 3. XPOST 负责实际定址、装载和切换

沿用 `5e435e0` 已实现的 AOT 槽 B 定址。主机预检已通过 `case_mark_idle`，板端 warmup 与发布仍待中量实测。从实际链接 ELF、XPM1 及分区配置取布局。发布的 v0.1.1 原生 Compact ELF 已定位至 `0x40380200`，helper 为 `0x40380000`，所有容器未见 ELF relocation 节。地址、helper、map 与布局已经匹配时，验证并复用发布字节。需要换槽时由 Agent 以发布的 ELF 载荷为基础定址，复用能证明与该载荷对应的定位信息，更新地址相关字段与摘要。部署不重链接 .o，不重跑 JIT。libbpf-mcu `retarget` 的重建可作离线对照。[v0.1.1 case 双槽对照](tests/case-v0.1.1.md) 已用正式发布载荷证明两个 case 程序在受测 A/B 布局中可保持 payload 不变。复用 Agent 已接通的槽位校验和容器封装，核对受测发布身份、目标地址和实际固件布局。保持已验证的 payload，更新目标槽地址和需更新的容器字段。此证据的适用范围限于所测 AOT 载荷与布局。超出该范围时先验证，无法证明安全定址就在写入前失败，并列出程序、模式、地址和所缺信息。板端预装非活动 SRAM 槽成功后才发布入口。发布前的定址、预装或校验失败时，保持当前槽与 generation 可运行。rBPF 沿用解释器校验和 map 绑定路径。

已在 QEMU 验证过三个 LLVM AOT 样例的零址到单槽改写，复用 [主机定址验证](tests/aot-single-slot.md)，跳过同范围复跑。用 `tdd` 核对新增 Agent 路径：热切换限定 AOT。对 case 的 A、B 槽定址，复用已记录的同布局直接链接载荷作独立预期。验收 Agent 准备后的实际目标地址、容器字段和 payload 摘要，并在板上核对 A/B 预装与发布。按 [AOT 热切换伪代码](references/medium.md#aot-热切换) 完成预装和发布，并核对发布前失败时活动槽与 generation 不变。只读预检发现必需槽无法安全准备时，中量停在写入前。板端预装或发布验收在中量 case 中完成，失败按第 6 节处理。冷安装和 warmup 属于准备。正式 case 只采集一次 AOT Trace 切换。采集同一次切换的 tcpdump 包记录和事件时间戳，并按 [case 两张交付图](references/medium.md#case-两张交付图) 生成两张图。验收活动槽、generation、boot ID 和数据面实际输出。网络和 EXB2 按各自模式验证正常装载。#25 与 v0.1.1 已有历史超限样本结果，复用 [容量与合并证据](tests/main-integration.md) 和 [发布镜像大小](tests/release-v0.1.1.md#镜像与容量)，只验证接入后的固件、运输及板端加载。缓存中的派生镜像与固件摘要、模式、helper 和目标槽绑定。

## 4. 删除旧 case 实验路径

用 `implement` 把现有 `case` 接入中量，作为热切换的唯一实验。复用 v0.1.1 的 JTX1/CAS2 探针、A/B 预装、发布、tcpdump 和时间戳采集。记录准备阶段的设备动作。正式采集前恢复 A 活动、B 已预装的状态。按 [AOT 热切换伪代码](references/medium.md#aot-热切换) 更新参与执行的模块注释。

- 基线已删除单槽 `run_cold_latency`、旧 `hot_swap` 选择与批量往返实现。保留公共冷安装和 A/B warmup，沿用基线的单次 A→B Trace。
- 删除 `case-smoke` 选择、分派和配置说明。准备探测放在 case 自身的计时窗外。旧目的 IP、5 字节探针、case 的 `rxcnt` 和逐镜像清空冷安装表的旧路径不再进入实验。
- 清理仅供退役实验使用的写入、建表与字段初始化、配置示例、命令说明及作图代码。先查 `case_latency`、`hot_swap_config` 等旧结构的实际调用方。仍需使用的抓包、Trace 与查询迁移到按 run_id 读取的正式入口。已有数据库表和历史记录保留可查询。替换 README 的旧冷/热对照说明，删除经引用检查只供该图使用的 `docs/figures/update-latency.png` 和 `.pdf`。旧结果通过 Git 历史追溯。将本轮正式 case 范围追加到 `docs/user-decisions.md`，保留原决策的历史。
- 用 `code-review` 从 `main.rs` 分派、配置校验、默认中量和数据库写入追到调用方。确认旧 `hot_swap` 和 `case-smoke` 选择在设备和数据库访问前被拒绝，冷切换循环及退役写入已从源码移除。用现有计划预检核对 case 只有 AOT Trace 一格。中量 case 完成时，从同一次切换的 run_id 查到原始抓包和 Trace，并在 PR 展示两张图。

## 5. 接通中量实验和数据交付

保留 #23 已迁移的 `metrics` 和 `trace` 固件 profile 与构建入口。新 runner 和数据库使用相同命名，沿用 [项目已有命名决定](https://github.com/nan-mu/xpost/blob/b5a44072ed57657c7482758412cccf1fd93df6fa/docs/user-decisions.md#2026-11-15-collection-name)。按 [命名迁移表](references/agent-profiles.md#采集命名迁移) 核对 runner/config 分派、DB 新记录及文档是否使用新名。用 `code-review` 从实际入口检查旧 `cag`/`t` 形式是否退出正式路径，从中量已有的 Metrics、Trace 采集核对实际固件输出和新数据库记录的变体。保留历史数据、按需保留历史配置读取。采集变体、执行模式、上游产物名各用各自的字段。

用 `implement` 和 `tdd` 接通中量调度、实际固件的 perf prepare/arm/freeze/read/release、embeprom 指标、四模式写库及逐项查询。七个 EXB2 workload 使用 8258 B 缓冲、64 B 头，替换仍可达的旧 32 B map 与配置。复用 [v0.1.1 发布源码的七项 EXB2 主机向量](tests/exb2-host-vectors.md) 和 [四模式 SQL 夹具结果](tests/postgres-fixtures.md)。剩余验收从实际 runner 追到设备结果及 run_id。清理旧三模式写入、隐式重刷调用及失效入口，保留历史数据。按固件身份调度，同一固件的速率和重复放在烧录循环内层。依 [逐模块伪代码](references/medium.md) 把各项控制流和指标口径放到对应模块的 //! 注释。正式运行按 [中量矩阵](references/medium.md#中量矩阵) 逐格完成固定的 29 格。exec_bench 的四种执行模式全部只测 Metrics。case 只测 AOT Trace。同步更新代码仓库的 `xpost-test/procedures/medium.md`、预检、runner 和 PR 描述中的旧矩阵。按第 4 节清理旧 case 路径。以实验族、模式和变体为键，对账正式计划与实际数据库 run 的格集合及每格的 workload、流量条件和有效观测次数。case 正式只启动 AOT Trace，验收恰好一次成功 A→B 切换。历史记录和准备动作单列。保留失败和重试的原始记录。逐项报告缺项和超出计划的执行。目标全程 ≤60 分钟，列出固件烧录、冷装、预装、切换次数。增速率点或重复次数时，固件烧录数不增加。交付可执行的中量选择及启动命令。

**预检后直接启动。** 将 `run = ["medium"]` 接到真实调度。用 `code-review` 核对 v0.1.1 到设备采集、存储、查询的调用链和故障返回。预览正式 runner 的计划，逐格核对实验族、模式、变体及格内条件。按仓库 [数据库步骤](https://github.com/nan-mu/xpost/blob/5e435e083292cb7cb5d592c9406fdf76c791ee92/xpost-test/procedures/medium.md#database-and-review) 设置独立的 `[postgres].protocol_dbname`。复用已初始化的协议库，空库按 `protocol_v1.sql` 初始化。随后从 TR3000 完成 HTTP、镜像和协议库的只读预检。全部通过后由主代理立即使用 `./xpost-test/scripts/xpost-test.fish` 构建、上传并运行中量。把旧冒烟验收和排队指令从 `xpost-test/procedures/medium.md`、配置及 PR 说明中清掉。直接启动的授权覆盖本轮测试及增量报告。

## 6. 每项出结果就在 PR 展示

用 `pr` 在同一英文 PR 建立结果记录。Runner 每结束一个计划内的测试尝试，写入终态并立即输出该项结果和 run_id。写库失败也输出该次尝试及可用的证据位置。测试尝试以独立采集窗口、Trace 探针、EXB2 批次或 case 切换为单位。主代理在整轮执行期间消费这些结果。按 run_id 查库、分析并逐项更新 PR。第一项出结果时就更新，后续以同样方式发布。

每项写明实验、执行模式、采集变体、输入条件、重复或批次号、实际结果与单位、通过或失败状态和 run_id。保留失败、数据缺失及终止原因。写库报错时，先用原 run_id 查询。区分已有终态、只有未完成记录、查无记录或数据库不可达。在 PR 写明状态、错误和本地证据位置。从原尝试补写缺失结果，补写或导出本身不触发重跑。如果预检阻止了写入，报告被阻塞的条件及原因。PR 更新在该项计时窗结束后进行，运行中的其他独立测试可以继续。

每次发布时及结束对账前检查同一 PR 的新增反馈，按 run_id 对应原结果。定位失败或反馈指出的问题，修复后把受影响项放回同一中量计划的待跑队列。记录修复提交和新 run_id，关联原尝试。影响后续数据有效性或设备状态时先暂停受影响项。有独立执行条件的项继续。需要重启 Agent 或 runner 时，在测量窗外应用修复并重做受影响的只读预检。按同一计划恢复待跑项以及经确认需重试的项，复用仍有效的已完成结果。case 分开记录发布和证据导出状态。确认发布前失败时可重试。已经发布或结果未知时，先查设备状态和原始记录，从同次切换补导。证据不足时在 PR 报缺项，由维护者决定是否再做一次物理切换。

实测图在有对应结果时加入 PR。case 的两张图必须取自同一次成功切换，引用同一 run_id 和切换序号，明确主机与 DUT 的时钟边界。对照 runner 的终态输出与 PR 的逐项发布时间，核对结果随运行发布，并用原 run_id 追溯反馈及重试。核对恢复后的实际执行集合，防止重跑仍有效的结果或重复发布 case。结束时对账 29 格，报告缺项、总墙钟和设备动作次数。对已有验证，核对版本、摘要、入口和适用条件，等价时直接引用。受改动影响的验证才需补测。
