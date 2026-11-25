# XPOST #23 · 分阶段复测并发布实测图表

在 `nan-mu/xpost` 的独立 worktree 接续 [PR #23](https://github.com/nan-mu/xpost/pull/23)，保留 `agent/experiment-matrix-v1` → `agent/rbpf-vm`。代码基线为 `b824c58734fc4e1585ab195c22a196f0debbb10e`，开工核对实际 head。本次任务书接续 Gitea `pr23@b7af1ff1ce2b11838cbd7a57f4322b87952963bf`，共享术语和决策依据为 Gitea `main@ccc86c1e7252d4f19eb96a586cdab2c682d9b863` 的 `GLOSSARY.md` 和 `decisions.md`。本轮沿用直接执行及逐项报告授权，在同一英文 PR 交付并留待维护者审核。

## 1. 使用已合入的前台 Agent

#27 已合入 #23。合并基线含 HTTP/CLI、设备独占和关停保护，也保留 #23 的部署、EXB2 与 Perf 接口。按 [前台启动和验证入口](references/agent-profiles.md#前台-http-启动步骤) 在接 DUT 的 Mac 上启动本轮 worktree 构建的服务，确认启动时的 Board Info 成功、恢复 App 模式、TR3000 通过 HTTP 访问同一服务。沿用现有协议数据库。复用 [前台 Agent 验证](tests/phased-baseline.md#前台-agent) 中的启动、主机接口和写入约束证据。实际装载、采集和写库在本轮正式实验内验证。

Agent 负责设备访问、部署和采集控制。runner 负责发送、窗口、样本判定、入库和结果。沿用 v0.1.1 固定归档与 v3 索引，以实际 Firmware ELF、XDP ELF、SRAM 槽和冷存储单元验收本轮目标，EXB2 继续使用 8258 B 缓冲和 64 B 头。用 `code-review` 保留 [#38](https://github.com/nan-mu/xpost/issues/38) 的同会话校验、写入及读回和 [#39](https://github.com/nan-mu/xpost/issues/39) 的安全替换条件。复用 [容量证据](tests/main-integration.md)、[发布检查](tests/release-v0.1.1.md)、[EXB2 主机向量](tests/exb2-host-vectors.md) 和 [四模式 SQL 夹具](tests/postgres-fixtures.md)。

## 2. 按阶段运行同一套中量调度

用 `implement` 和 `tdd` 在现有中量入口选择阶段。正式运行、`medium-plan` 与 `medium-preflight` 共用所选计划。更新仓库程序注释及 `xpost-test/procedures/medium.md`，按 [中量矩阵和伪代码](references/medium.md) 核对：

1. **HAL/AOT。** 按 exec_bench → firewall → just_tx → just_drop 顺序，完成每项的 HAL 和 AOT。共 14 格、72 项计划尝试。每个实验出合格数据即更新 PR 图表。第一阶段结束时在 PR 对账 14 格，发布可用图表，并标出缺项及原因。
2. **JIT/rBPF。** 第一阶段结果与图表发布后，再按相同实验顺序运行 JIT 和 rBPF。共 14 格、72 项计划尝试。按相同口径追加图表，标注无效或缺失条件。

本轮暂缓 case、lookup、dev。阶段预检覆盖本阶段全部必需固件、部署、采集、TR3000 HTTP、抓包和协议库。让 Agent 返回当前阶段可独立判定的结果，避免完整 29 格报告的总状态遮蔽当前阶段。通过实际 HTTP client 验证两个场景：暂缓的 case 或第二阶段的 rBPF 目标有缺口时，完备的 HAL/AOT 计划仍可预检；HAL/AOT 必需目标有缺口时，在任何设备写入前拒绝。只读预检通过后直接用 `./xpost-test/scripts/xpost-test.fish` 启动该阶段。正式阶段承担真机验收。按实验顺序分段，再在每个实验内部按实际固件摘要编组。通过正式调度核对 just_tx 与 just_drop 共用摘要时的实际启动顺序。前一实验的首次尝试完成或置为阻塞后才进入下一实验，后续补跑关联原尝试。速率、重复和结果导出不触发烧录。恢复时先确认板上固件及装载身份，状态匹配就复用。统计两阶段与续跑的累计耗时和设备动作。中量目标仍为一小时内，单列诊断、暂停和重试时间。

## 3. 处理 issue 并验证可用样本

用 `diagnosing-bugs` 依 [已有问题证据](tests/phased-baseline.md#问题证据) 复用旧 run 和主机验证，再从实际入口执行尚缺的验收。

- **[#28](https://github.com/nan-mu/xpost/issues/28)。** 在首次写入前拒绝缺 Wi-Fi 输入的阶段网络固件。板上在同一中量固件组内确认数据面可用后才开计时窗，串口 `OK init` 不足以放行。用缺输入和正确输入两种现有构建条件验证判别。保留 [#29](https://github.com/nan-mu/xpost/issues/29) 的 firewall 零 DUT 证据判据。
- **[#36](https://github.com/nan-mu/xpost/issues/36)、[#37](https://github.com/nan-mu/xpost/issues/37)。** 保留 tcpdump 尾块导出等待以及原回复截止。先按 run_id 从生产协议库查回留存 PCAP，使用 [三个既有对照](tests/phased-baseline.md#hal-just-tx) 在各 run 内按 UUID 和序号匹配 TX/RX，比较捕获长度、IP/UDP 长度和首个差异字节。400 pps 使用已知通过窗做同速率对照。1400 pps 先核对同 run 的帧，若缺同速率通过窗，记录证据缺口。区分报文异常和解析判定。按对照修复可证实的原因。原因未确认时在 PR 保留缺项及可核验证据。修复判定后复用原始 PCAP 可重判的样本，只补跑当轮确有需要的条件。just_tx Metrics 的无回包、零 DUT 入站窗口记不可判定。主机发包与完整抓包不足以证明 DUT 全丢。
- **[#30](https://github.com/nan-mu/xpost/issues/30)–[#32](https://github.com/nan-mu/xpost/issues/32)、[#35](https://github.com/nan-mu/xpost/issues/35)。** 核对目标键与背景键分别计数，标签贯穿 Trace 至终点，DROP 区分正常完成、故障和暂停。网络窗口出现带标签的 XDP 故障时剔除，并按已有原始证据复核旧受影响样本。各阶段用正式测试验证对应模式的判据。
- **[#33](https://github.com/nan-mu/xpost/issues/33)、[#34](https://github.com/nan-mu/xpost/issues/34)。** 保留当前 rBPF 冷加载 map 索引解析及 map value 内存边界修复。第二阶段在 firewall 核对合法 HASH value 的命中增量，在 just_tx 核对 ARRAY backing storage 和正确回包。区分空查找、越界、解释器 abort 和正常丢包。两 issue 的真实回归在第二阶段验收。

## 4. 按 run_id 展示、对账和恢复

以合入 #27 的 Agent 建立本轮采集 campaign，并记录每次执行的实际提交、阶段和产物身份。旧 campaign 的原始数据保留供排障和重分析。本轮每项真机结果来自本轮 campaign。用 `tdd` 使绘图、续跑和最终对账共用同一有效结果判据。有效结果须匹配阶段、模式、变体、完整条件、重复序号和兼容的固件及部署身份，同时采用有明确替代关系的有效分析。检验原终态为 `complete` 但后续分析取消资格的样本，以及原不可判定后经同证据恢复的样本。续跑复用本轮仍有效的尝试，只执行缺失或失效项。写库失败先查原 run_id 并补记原证据。修复并重启时在采集窗外停用旧 Agent 或 runner，确认设备状态后从原 campaign 恢复。

用 `pr` 在同一个英文 PR 逐项更新结果。每个 `XPOST_RESULT` 一出，按 run_id 查库并报告实验、阶段、模式、变体、条件、重复号、指标及单位、有效性和原 run_id。失败和未入库也要及时报告。读 PR 新反馈，按原 run_id 关联修复与重试。受同一缺陷影响的项暂停，无依赖的当前阶段项可继续。第一阶段的 14 格都须有结果或明确缺项。四项实验的 HAL 与 AOT 都有可出图的真机结果，且图表和缺项已在 PR 可查后，才进入第二阶段。单个条件的缺项照实展示。第二阶段启动前通过自身的只读预检。

图表以合格样本为数据源，显示条件、单位、样本量及对应 run_id。exec_bench 按 workload 展示 DUT 每批用时和每轮耗时；firewall 展示合法交付比例和目标键命中比例；just_tx 展示合格丢包率和 RTT；just_drop 展示实际施加、DUT 入站与正常 DROP 的速率。区分目标 pps 与实际施加量，标出无效及缺失。第一阶段先展示 HAL/AOT，第二阶段附上 JIT/rBPF。结束时提交 28 格及 144 项计划尝试的结果对账、实际样本和每项失败原因，并核对 PR 的逐项发布时间。
