# XPOST #23 · 修复阻塞全量测试的错误并运行 `all`

由 T3 Code 中的 Codex 在 `nan-mu/xpost` 的独立 worktree 接续 [PR #23](https://github.com/nan-mu/xpost/pull/23)。审核 head 为 `dfb12acd354f4a2a6ca4c0dd49aaab1781a26c2f`。开工核对实际 head 和 issue 状态。保留当前 head 分支 `agent/experiment-matrix-v1` 及目标分支 `agent/rbpf-vm`，同一英文 PR 交付待审，维护者决定合并。本稿接续 Gitea `pr23@44635ee7af53f115e3fa2ad17fef50afee1512f0`，共享术语与用户决策来自 `main@ccc86c1e7252d4f19eb96a586cdab2c682d9b863`。

修复下面列出的错误。从 `xpost-test/profile.toml` 选择 `all`，通过现有 Fish 脚本在 TR3000 实际启动全量。用新 campaign 对账并出图。上一轮的 144 项中量记录保留为验证和诊断证据。[中量所用提交与实测数据](tests/premerge-evidence.md)

继续已有 T3 线程时，先重新读取本次交付的固定提交。本轮执行方法以各缺陷条目写明的操作和判据为准。执行者可按实际情况安排修复和验证的先后顺序。优先复用现有入口、检查与仍适用的证据，只在缺陷验收缺少证据时补相应检查。

## 1. 修复审核发现的全量运行错误

| 当前错误 | 修复要求与验证 |
| --- | --- |
| **`all` 无法启动。** 构建仍从 `config/default.toml` 读取 `run`。校验拒绝 `all`，调度只认中量入口和旧的独立选择。中量还有写死的 3 个速率、5 秒和 2 次重复。 | 将运行选择接到 `xpost-test/profile.toml` 的 `run = ["all"]`。改配置后重建，确认实际编入 TR3000 runner 的值已更新。编译后的 `./xpost-test` 无参数启动下节的计划。调度、只读就绪检查和对账共用计划。正式 `all` 使用现行四模式、发布包和协议库路径。从该入口核对计划展开数量。 [配置校验](https://github.com/nan-mu/xpost/blob/dfb12ac/xpost-test/common/src/config.rs#L336-L381) · [主入口](https://github.com/nan-mu/xpost/blob/dfb12ac/xpost-test/common/src/main.rs#L21-L44) |
| **只构建脚本会掩盖 Cargo 失败。** `build_in_devcontainer.fish` 在 Cargo 后直接输出产物路径，最终可能以 `echo` 的成功状态退出。 | 编译失败时返回原失败状态。执行者看到失败就停止上传。验证失败时不把上次构建留下的文件当成本轮 runner。 [只构建脚本](https://github.com/nan-mu/xpost/blob/dfb12ac/xpost-test/scripts/build_in_devcontainer.fish#L1-L6) |
| **长跑结束会被误判为失败。** 中量结束条件硬编码了 3600 秒，全量仅网络发包和观察就超过 4 小时。 | 全量按自身计划核对实际耗时、完成项及失败项。超过 3600 秒的完整合格全量记录仍可通过。实测记录构建、烧录、冷装、采集、分析入库及暂停，方便核算剩余时间。 [结束判定](https://github.com/nan-mu/xpost/blob/dfb12ac/xpost-test/common/src/exam/medium.rs#L226-L243) |
| **Just TX 的 PCAP 上限挡住有效长窗。** 当前读取上限是 16 MiB。4000 pps × 30 秒且全部回包时，82 B 帧加 PCAP 记录头约为 22.4 MiB。 | 让采集完整保留计划最大窗口的双向 PCAP，并能解析、落库和读回。准备超过 16 MiB 的完整捕获作为正例，截断捕获作为反例。按现行回复截止判丢包和 RTT，保留 #36 的尾块导出等待。 [抓包上限](https://github.com/nan-mu/xpost/blob/dfb12ac/xpost-test/common/src/exam/medium_network_capture.rs#L19) · [完整性判定](https://github.com/nan-mu/xpost/blob/dfb12ac/xpost-test/common/src/exam/medium_network_capture.rs#L148-L154) |
| **JIT/rBPF 可以提前放行。** 旧代码只检查 HAL 和 AOT 每项实验各有一条合格 Metrics，第一阶段的其他速率和 Trace 尚无结果也能通过。 | 按所选计划核对 HAL/AOT 的每个计划键都有终态记录，允许有明确原因的不可判定项。四项实验的 HAL 和 AOT 各须至少一项有效 Metrics 可出图。用缺 Trace、有 `running` 和仅有八条 Metrics 的记录确认它们在设备写入前被拒绝。执行者在第二阶段首次写入前核对同一 PR 已发布首阶段对账、图表及缺项原因。 [旧放行代码](https://github.com/nan-mu/xpost/blob/dfb12ac/xpost-test/common/src/exam/medium.rs#L797-L821) · [已验证的主机判定样例](tests/premerge-evidence.md#第一阶段放行) |
| **续跑可能沿用已经不可比的数据。** 现有身份记录有 `window_ns`，但缺少 tcpdump 尾块等待和分析版本。旧记录能在采集边界或判定改变后继续计为有效。 | 窗口、观察或抓包边界、捕获点、证据判定等影响可比性的条件变化时，续跑报告不兼容项，只重测受影响的条件。测试固定部署和 30 秒窗口，只改抓包边界或分析版本，旧记录不应直接复用。另核对同一部署的 5 秒窗口不会混入 30 秒全量，兼容的结果能被跳过。 [旧输入记录](https://github.com/nan-mu/xpost/blob/dfb12ac/xpost-test/common/src/exam/medium_network.rs#L126-L145) · [旧资格判断](https://github.com/nan-mu/xpost/blob/dfb12ac/xpost-test/common/src/exam/medium.rs#L647-L688) |

## 2. 修复 issue 中的错误并逐项核对状态

定位 [#37](https://github.com/nan-mu/xpost/issues/37) 的 HAL `just_tx` 回包聚合。旧 PCAP 中一个捕获记录含多枚独立探针，只提供一个接收时间戳。按原 run_id 读回失败 PCAP 和 400 pps 合格对照，沿 HAL 回包和主机捕获路径找出聚合位置，再修复已证实的原因。每个合格回复须有对应的独立时间戳，且 UUID、序号、地址、端口和负载均可从发送账本与原始捕获核对。旧聚合窗保留为不可判定。用全量 HAL `just_tx` 的 400、1400、4000 pps 正式窗口验证实际修复，结果各自关联新 run_id。复用[五个旧失败窗及同速率对照](tests/premerge-evidence.md#hal-just-tx)和[原始失败记录](tests/phased-baseline.md#hal-just-tx)。

用 `code-review` 对照下列错误和[已有验收证据](tests/premerge-evidence.md#issue-与证据)。若对应修复已随 PR 保留、验收条件仍相同，复用原证据并关闭 issue。若 `all` 仍走到受影响的旧路径，修复后再关闭。由配置的 GitHub MCP 标记为 `completed`，在英文 PR 列出关闭编号及证据。

| Issue | 要核对的错误与期望 |
| --- | --- |
| [#28](https://github.com/nan-mu/xpost/issues/28)、[#29](https://github.com/nan-mu/xpost/issues/29) | 无 Wi-Fi 的网络固件曾可通过预检。Firewall 曾把两类流量都没有 DUT 证据的窗判成有效。确认写入前拒绝错误固件，且零 DUT 证据不合格。 |
| [#30](https://github.com/nan-mu/xpost/issues/30)、[#31](https://github.com/nan-mu/xpost/issues/31)、[#32](https://github.com/nan-mu/xpost/issues/32) | Firewall 曾把背景键增量当成目标键异常。Trace 曾缺少标签对应的终点证据。DROP 曾无法区分正常丢弃和 fault。组合验证用已确认的真机样本。 |
| [#33](https://github.com/nan-mu/xpost/issues/33)、[#34](https://github.com/nan-mu/xpost/issues/34)、[#35](https://github.com/nan-mu/xpost/issues/35) | rBPF 冷载曾未解析 map 索引，合法 map value 曾被内存检查拒绝，带标签的 abort 曾仍获合格。用 HASH 和 ARRAY 真机回归，按 issue 的旧 run_id 核对替代分析。 |
| [#36](https://github.com/nan-mu/xpost/issues/36) | tcpdump 停得过早会丢尾块，即便内核报告零丢包。验证回复截止和抓包导出等待相互独立。 |
| [#38](https://github.com/nan-mu/xpost/issues/38)、[#39](https://github.com/nan-mu/xpost/issues/39) | 冷安装曾跨会话读回，损坏表也可能被误判为空。复用同会话校验、写入、读回以及写前拒绝损坏或多镜像表的证据。 |

三个速率的 HAL 正式回归通过后，附新 run_id 并关闭 #37。若修复无法验证，保持开放，列出失败 run_id 和证据缺口。无效条件继续列在总对账，可独立通过的数据按实际测量发布。

## 3. `all` 的数据量

三项网络实验的 Metrics 采用同一组 13 个目标速率，单位 pps。速率依次为 `400, 600, 800, 1000, 1200, 1400, 1600, 2000, 2400, 2800, 3200, 3600, 4000`。每速率每模式跑三轮，每窗发包 30 秒，观察 1 秒。保留目标速率、实际发包量、速率顺序和 seed。HAL、AOT、JIT、rBPF 各写入自己的协议 schema。

| 实验 | 计划尝试 | 每项采样 |
| --- | ---: | --- |
| `firewall` | 156 Metrics + 8 Trace | 四模式各跑 13 速率 × 3 轮；Trace 每模式一次 hit、一次 miss |
| `just_tx` | 156 Metrics + 4 Trace | 同上主网格；每模式一次带标签探针的 Trace |
| `just_drop` | 156 Metrics + 4 Trace | 同上主网格；每模式一次带标签探针的 Trace |
| `exec_bench` | 140 Metrics | 7 个现行 EXB2 workload × 4 模式 × 5 个独立 run；每 run 100 个批样本，共 14,000 批。保留当前固定输入、每批 3 rounds 和独立输出校验 |
| `case` | 1 Trace | AOT 一次 A→B。预装在测量之前，输出 tcpdump 图和事件时间戳图 |

合计 29 格、625 项计划尝试。468 个网络 Metrics 窗计划约 2700 万个探针，纯发包和观察为 **4 小时 1 分 48 秒**。部署、Trace、EXB2、分析和入库另计。输入依据和数量推导见[全量规模与当前代码](tests/premerge-evidence.md#全量规模与当前代码)。`lookup` 和 `dev` 是独立诊断入口。

## 4. 构建、运行、交付

1. 在 `xpost-test/profile.toml` 设置 `run = ["all"]`。在本轮 worktree 编译面向 TR3000 的 `aarch64-unknown-linux-musl` runner，核对构建版本与计划。现有只构建入口为 `./xpost-test/scripts/build_in_devcontainer.fish`。
2. 在接 DUT 的 Mac 上从本轮 worktree 启动前台 `xpost-agent`。使用仓库现有 HTTP 入口，确认 Board Info、恢复 App 模式和 TR3000 可达。沿用 `libbpf-mcu v0.1.1` 与 XDP ELF 身份校验。[启动说明](references/agent-profiles.md#前台-http-启动步骤)
3. 从仓库运行 `./xpost-test/scripts/xpost-test.fish`。脚本构建、上传 runner，并在 TR3000 执行无参数的 `./xpost-test`。由 `all` 运行全量，保持 HAL/AOT 后 JIT/rBPF 的中量顺序约定。每阶段按 `exec_bench`、`firewall`、`just_tx`、`just_drop` 顺序执行，`case` 单独运行一次。必要的设备写入按固件和装载身份分组。速率和重复切换复用同一部署。[现有 Fish 脚本](https://github.com/nan-mu/xpost/blob/dfb12ac/xpost-test/scripts/xpost-test.fish#L1-L9)
4. 在新 campaign 用现行四模式协议数据库逐项查结果。每项到达终态即按其 run_id 查库和更新 PR。中断后从数据库找回未发布项，标明补发时间。修复后只重排失败或缺失的条件，关联原 run_id。用 `pr` 记录计划与数据库对账、实际有效项、缺项原因、设备操作及耗时。图表引用实际入图的 run_id，沿用[已发布图表风格](https://github.com/nan-mu/xpost/pull/23#issuecomment-7159476484)，同时标注 5 秒中量和 30 秒全量的口径差别。

全量实际跑完后，把结果与 issue 状态交到同一英文 PR。若设备、采集或写库错误使正式运行受阻，记录实际完成量、失败条件、原 run_id 和续跑入口，留待维护者审核。
