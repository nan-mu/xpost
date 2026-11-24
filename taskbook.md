# XPOST #23 · 接入 libbpf-mcu v0.1.1，交付中量实验入口

在 `nan-mu/xpost` 独立 worktree 继续 [PR #23](https://github.com/nan-mu/xpost/pull/23) 的 `agent/experiment-matrix-v1`，base 为 `agent/rbpf-vm`。交付同一开放 PR 给维护者审核。GitHub 审阅基线为 `b5a44072ed57657c7482758412cccf1fd93df6fa`，本轮 Gitea 修订基线为 `8ab92efcc9ee3dee2dc3a4daf1739dee923ed3a6`。维护者审核就绪后在 T3 发令启动完整中量实验。

## 1. 从已纳入 #25、#26 的实现继续

用户已在工作分支整合 main 上两个改动。开工先核对本地工作分支和 GitHub head 的 ancestry 与实际文件。#25 主线合入提交为 `3986c4f17a54e7c9bd830d558c40c8c13a066660`，#26 为 `9cb03b795c8d45c5d107cf8e63b90dca6f1dcb56`。优先接续用户已整合的提交并记录 SHA。如果工作区只有已查到的旧远端 head，从该 head 在独立 worktree 接入这两个主线改动。保留用户已有的冲突处理。用 `code-review` 核对合并语义和正常调用链，复用 [合并预查](tests/main-integration.md) 的冲突定位。

保留 #25 将 exec SRAM 槽和 copy buffer 提到 12,288 B、实际固件 cold cell 16,384 B 的行为。Agent 冷装上限按本次实际固件与分区处理，网络等载体维持各自限制。保留 #26 主机编码按计算所得长度分配的行为。Compact ELF 格式本身取消人为整文件上限，实际镜像受 SRAM 槽约束，实际写入受分区和传输约束。合并后保留 #24 的设备身份复核及从同一份已核验字节写入的路径，避免引回 main 旧的本机 XDP build、配置回退或按路径重开文件。按 [配置说明](references/agent-profiles.md) 更新实际示例。将 [代码设计约定](references/code-design.md) 中适用内容纳入 XPOST 的长期设计文档，从现有架构说明引用。

## 2. Agent 从正式 Release 取件并接通正常入口

用 `implement` 接通 `profiles.toml` → HTTP/MCP 共用 Agent 操作 → 固件随装、独立冷安装及中量预检。固定 GitHub Release `nan-mu/libbpf-mcu v0.1.1`（`07a4ea9a81a7fedf7bef2d6ced8d7fcfcccfd113`）。12 个正式 7z 和 36 个部署容器已逐项核验，直接复用 [正式发布包检查](tests/release-v0.1.1.md)。包名取生产程序 stem。本地 profile 别名与文件名分离。按包根 v3 索引选 `llvm-aot`、`jit` 或 `ebpf` 对应的既有文件名，检查来源、摘要、格式、内部身份。HAL 只使用固件。沿用 [正式归档布局](https://github.com/nan-mu/libbpf-mcu/blob/v0.1.1/docs/artifacts.md#program-layout)。

将下载、索引校验、缓存和部署准备归入 `xpost-agent`，保留 `xpost-elf` 格式实现和 `xpost-serial` 设备写入边界。将旧 `verify-medium-release` 的清点能力迁移到 Agent 的只读预检，共用运行时取件逻辑。独立检查 12×3 项并与中量计划对账。固件随装在首次写入前准备好所有请求项，独立冷装在同一排他会话校验设备与分区。更新 CLI/HTTP/MCP 调用方、Cargo 依赖和锁文件、默认配置、测试与文档。删除 `xpost-artifact` crate、`build_root`、v1 index/manifest/report 和同级 profile 缓存回退，更新 AGENTS.md 与现行项目决策说明。保留 `fw.artifact` 作为已链接固件路径。移除中量入口对旧版的固定报错。用 `diagnosing-bugs` 从原普通入口追到真实写入点，保留失败原因而非固定门禁。

用 `tdd` 核对 Agent 新路径。用现有的 `just_tx` 与 `case_mark_idle` 展示空缓存、同输入复用缓存和本地别名选择。缺模式（含文件和索引行同时缺失）、摘要错或固件不匹配时，指出 tag、程序、模式与原因，首次设备写入前失败。旧版 0/36 与新版归档静态校验已有记录。执行端只实测尚未覆盖的 Agent 路径及本轮改动后的行为。

## 3. XPOST 负责实际定址、装载和切换

用 `implement` 接通目标固件 → 实际槽位、容量及 helper → Agent 准备镜像 → 板端 warmup → 发布入口。从实际链接 ELF、XPM1 及分区配置取布局。发布的 v0.1.1 原生 Compact ELF 已定位至 `0x40380200`，helper 为 `0x40380000`，所有容器未见 ELF relocation 节。地址、helper、map 与布局已经匹配时，验证并复用发布字节。需要换槽时由 Agent 以发布的 ELF 载荷为基础定址，复用能证明与该载荷对应的定位信息，更新地址相关字段与摘要。部署不重链接 .o，不重跑 JIT。libbpf-mcu `retarget` 的重建可作离线对照。由 XPOST 核查现有发布件可支持的定址方式，记录无法证明安全改写的具体程序、模式、地址和所缺信息，写入前明确失败。板端预装非活动 SRAM 槽成功后才发布入口。任一失败保持当前槽可运行。rBPF 沿用解释器校验和 map 绑定路径。

已在 QEMU 验证过三个 LLVM AOT 样例的零址到单槽改写，复用 [主机定址验证](tests/aot-single-slot.md)，跳过同范围复跑。用 `tdd` 核对新增 Agent 路径：对本轮支持的原生 AOT/JIT 定址，用同布局直接生成的装载字节和地址相关元数据作为独立预期，分别核对 A、B。记录实际发布字节在匹配槽位的接收结果，验证 case A/B 按槽进入预装、发布及失败状态，覆盖正式 AOT/JIT 与 rBPF 的实际装载，核对返回或包改写、map 和活动 generation。分开记录冷安装、warmup、publish。#25 与 v0.1.1 已有历史超限样本结果，复用 [容量与合并证据](tests/main-integration.md) 和 [发布镜像大小](tests/release-v0.1.1.md#镜像与容量)，只验证接入后的固件、运输及板端加载。缓存中的派生镜像与固件摘要、模式、helper 和目标槽绑定。

## 4. 接通中量实验和数据交付

迁移现行采集命名。用 `implement` 将正式低开销采集统一为 `metrics`，单次详细采集统一为 `trace`，沿用 [项目已有命名决定](https://github.com/nan-mu/xpost/blob/b5a44072ed57657c7482758412cccf1fd93df6fa/docs/user-decisions.md#2026-11-15-collection-name)。按 [命名迁移表](references/agent-profiles.md#采集命名迁移) 同步正式固件 profile、实际使用的 Cargo feature 入口、runner/config 分派、DB 新记录及文档。用 `code-review` 从实际入口检查旧 `cag`/`t` 形式是否退出正式路径，从 runner 的新名称各执行一条 Metrics、Trace 短探针，核对实际固件采集输出和新数据库记录的变体。将这两条检查并入本来要跑的采集冒烟，避免重复运行。保留历史数据、按需保留历史配置读取。采集变体、执行模式、上游产物名各用各自的字段。

用 `implement` 和 `tdd` 接通中量调度、实际固件的 perf prepare/arm/freeze/read/release、embeprom 指标、四模式写库及逐项查询。七个 EXB2 workload 使用 8258 B 缓冲、64 B 头，替换仍可达的旧 32 B map 与配置。复用 [v0.1.1 发布源码的七项 EXB2 主机向量](tests/exb2-host-vectors.md) 和 [四模式 SQL 夹具结果](tests/postgres-fixtures.md)。剩余验收从实际 runner 追到设备结果及 run_id。清理旧三模式写入、隐式重刷调用及失效入口，保留历史数据。按固件身份调度，同一固件的速率和重复放在烧录循环内层。依 [逐模块伪代码](references/medium.md) 把各项控制流和指标口径放到对应模块的 //! 注释。用 firewall、just_drop、just_tx、exec_bench、case 五族 × HAL/AOT/JIT/rBPF 四模式 × Metrics/Trace 两变体确定 40 格。只有四个 exec_bench Trace 格可选，其余 36 格必测。case 与 hot_swap 按伪代码贡献到同一实验族。以实验族、模式和变体为键逐格对账实际数据库 run，另列缺项、失败和重试。目标全程 ≤60 分钟，列出固件烧录、冷装、预装、切换次数。增速率点或重复次数时，固件烧录数不增加。短冒烟和正式中量分别启动。

用 `code-review` 核对 v0.1.1 到实际运行、存储、查询的调用链和故障返回。主代理亲自实测剩余验收。对于已做的验证，先核对产物版本及摘要、代码和运行入口等适用条件，等价时直接引用并跳过。旧版本和原型记录只覆盖明确标注的范围。版本、摘要或路径变化时更新对应 tests 记录，写明旧结果仍适用的部分、实际补测结果和未验证的入口。用 `pr` 在同一英文 PR 列实际版本、复用证据、剩余验证、启动命令及就绪情况。缺项逐项标明。维护者在 T3 发令后，每完成一项按 run_id 查库分析。发现异常则修复、更新英文 PR，再将受影响项入队，保留原始尝试。将实测图嵌入 PR，标明单位、条件、样本数和 run_id，分列主机回复间隔与 DUT 切换时间。报告总墙钟和设备动作次数。
