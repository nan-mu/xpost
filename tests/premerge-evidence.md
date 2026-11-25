# PR #23 合并前证据

评审对象为 GitHub [PR #23](https://github.com/nan-mu/xpost/pull/23) 的 `dfb12acd354f4a2a6ca4c0dd49aaab1781a26c2f`。该 head 的代码和审计已读回。GitHub 的预览合并 `d47b30e` 与该 head 有相同 Git tree。2026 年 11 月 25 日核对时，下表 issue 均为开放。没有直接连接正式数据库或 DUT 复跑下面引用的历史实测。

## 第一阶段放行

基线代码只检查每个实验模式至少一条有效 Metrics。它不检查剩余计划项或 Trace 的终态。正式 campaign `dc74be72-b3b7-4233-8050-6f038a7bdd4c` 的旧流程实际完成了第一阶段对账与图表发布，再运行 JIT/rBPF。两阶段各有 72 项计划尝试，分别有 67、72 项合格。[基线放行代码](https://github.com/nan-mu/xpost/blob/dfb12ac/xpost-test/common/src/exam/medium.rs#L797-L821) · [正式运行审计](https://github.com/nan-mu/xpost/blob/dfb12ac/.audit/taskbook/mcp/xpost/c5995078130b1b3c941f08b0d6989af337d461f2.md#L48-L73)

作者在上述 head 的本地临时工作树试改放行判定。使用现有计划产生 72 项终态键，保留 67 项合格。该样例通过。去掉 `firewall/hal/trace/miss/1` 后失败，并报告缺失键。仅把 HAL `just_tx` 的合格重复号换成计划外的第 3 轮后失败，并报告该模式没有可出图的有效结果。

本地命令为 `TEST_SKIP_EBPF=1 cargo +stable test --locked -p common --target x86_64-unknown-linux-gnu`。49 个单元测试通过，3 个 HTTP 集成测试通过，7 个测试跳过，退出码为 0。临时差异 SHA-256 为 `386aa9c607a61022d96c466b797e831da78904f1cbee5eb6d409e3af164fe354`，未推送。该验证覆盖主机条件判定。T3 需要在实际交付代码上验证正常入口的放行与拒绝。

## HAL Just TX

正式 campaign 的五个不可判定窗口都保有原 PCAP。作者读取了 PR 内的审计和样本清单。审计报告 PCAP 摘要一致、每个聚合负载块均可匹配独立发出的探针。单个聚合记录只保留一个接收时间戳。此前在 400 pps 有同速率合格对照。[正式审计](https://github.com/nan-mu/xpost/blob/dfb12ac/.audit/taskbook/mcp/xpost/c5995078130b1b3c941f08b0d6989af337d461f2.md#L75-L86)

| 速率 | 重复 | 原 run_id | 聚合接收记录 |
| --- | ---: | --- | ---: |
| 400 pps | 1 | `86345b3b-11a6-4606-8326-e36d0e92149c` | 1 |
| 1400 pps | 1 | `7d28d2bc-d0dd-4025-819b-e413f334694e` | 7 |
| 1400 pps | 2 | `4a5e1ed1-d006-47ae-a079-75e15f329558` | 6 |
| 4000 pps | 1 | `ee1a5ae3-2133-46c8-95e9-a0e8d0210597` | 46 |
| 4000 pps | 2 | `2bb9fd24-e01f-4b05-95c3-6e6588dd0ba4` | 13 |

## Issue 与证据

下表用来核对关闭条件。正式首阶段使用 `7a1817a`，HAL Just DROP 续跑使用 `4353aec`，第二阶段使用 `a3486e3`。继续核对相关代码与交付 head 是否等价。主机夹具与真机运行的证据范围分别见[本轮审计](https://github.com/nan-mu/xpost/blob/dfb12ac/.audit/taskbook/mcp/xpost/c5995078130b1b3c941f08b0d6989af337d461f2.md#L33-L92)。

| Issue | 已有执行证据与复核重点 |
| --- | --- |
| [#28](https://github.com/nan-mu/xpost/issues/28) | Agent CLI 夹具在设备擦写前拒绝缺 Wi-Fi 输入的网络 Firmware ELF。真机预检覆盖 11 个 HAL/AOT Firmware ELF 摘要。 |
| [#29](https://github.com/nan-mu/xpost/issues/29) | `9e19eb6` 的主机用例覆盖两个流量类别均无 DUT 证据的窗口。修复后的 400 pps 条件取得 DUT 证据。 |
| [#30](https://github.com/nan-mu/xpost/issues/30) | `900db29` 用原快照重新分析区分目标键和其他键。正式 rBPF 400 pps 记录 1800 次目标命中及 3 次其他键增量。 |
| [#31](https://github.com/nan-mu/xpost/issues/31) | 正式 campaign 的 Firewall hit/miss、Just TX 和 Just DROP Trace 在 AOT、JIT、rBPF 均合格。rBPF 的回包与正常丢弃有标签证据。 |
| [#32](https://github.com/nan-mu/xpost/issues/32) | 正式 rBPF Just DROP 记录 2000 个带标签到达、2000 次正常丢弃及零带标签 fault。四模式的 Just DROP Metrics 均合格。 |
| [#33](https://github.com/nan-mu/xpost/issues/33)、[#34](https://github.com/nan-mu/xpost/issues/34) | `a69f282` 修正冷加载 map 索引，`22f19e2` 修正 value 访问。正式 rBPF Firewall HASH 与 Just TX ARRAY 真机观测均有预期增量和零运行时 fault。 |
| [#35](https://github.com/nan-mu/xpost/issues/35) | 审计记录六个旧 rBPF Just TX 窗口已按带标签 fault 重判。正式回归合格。关闭前以 issue 所列旧 run_id 确认有效分析已替代原判定。 |
| [#36](https://github.com/nan-mu/xpost/issues/36) | `361eaf7` 延长超出回复截止后的抓包等待。AOT 和 JIT 板上复测恢复八个 1400/4000 pps Metrics 窗及两个 Trace。 |
| [#38](https://github.com/nan-mu/xpost/issues/38)、[#39](https://github.com/nan-mu/xpost/issues/39) | 历史串口夹具检查同会话校验与写入，并拒绝损坏或多镜像冷存储布局。正式两阶段累计确认 46 次冷安装。 |

## 全量规模与当前代码

在 `dfb12ac` 静态核对正常入口。当前 runner 构建所用的是 `config/default.toml`，其中 `run = ["medium"]`。`run = ["all"]` 不在校验允许的名称中。现有 Fish 脚本构建 `aarch64-unknown-linux-musl`、用 `scp` 安装到 TR3000 并执行 `./xpost-test`。当前中量将网络 Metrics 固定为 400、1400、4000 pps 各两轮、每窗 5 秒，单次执行上限 3600 秒。[配置校验](https://github.com/nan-mu/xpost/blob/dfb12ac/xpost-test/common/src/config.rs#L336-L381) · [脚本](https://github.com/nan-mu/xpost/blob/dfb12ac/xpost-test/scripts/xpost-test.fish) · [中量计划](https://github.com/nan-mu/xpost/blob/dfb12ac/xpost-test/common/src/exam/medium.rs#L54-L114) · [窗口常量](https://github.com/nan-mu/xpost/blob/dfb12ac/xpost-test/common/src/exam/medium_network.rs#L34-L42)

全量参数沿用 `XPOST_EXPERIMENTS_V1` 的网络主网格及 EXB2 正式采样数量。网络三个实验的正式设计各规定 13 个速率、四模式、三轮和每窗 30 秒 + 1 秒。`experiments/exec_bench.md` 规定每个 workload 和模式五个独立 run，每 run 100 批；程序表和每批输入以现行 EXB2 七个向量为准。`case` 沿用一次 AOT Trace 的后续决定，网络 Trace 沿用已验证的中量单探针用例。量的区别和算法是 `3 × 4 × 13 × 3 = 468` 个网络 Metrics 窗，`8 + 4 + 4 = 16` 个网络 Trace，`7 × 4 × 5 = 140` 个 EXB2 run 和 `140 × 100 = 14000` 批，以及一次 `case`。`468 × 31 s = 14508 s`，13 个速率相加为 25000 pps，故计划探针量为 `25000 × 30 × 3 × 4 × 3 = 27000000`。这些数字是计划量。[现行 EXB2 向量](https://github.com/nan-mu/xpost/blob/dfb12ac/xpost-test/procedures/medium.md#L168-L185) · [一次 Case Trace 的决定](https://github.com/nan-mu/xpost/blob/dfb12ac/docs/user-decisions.md#2026-11-25-formal-case-and-medium-scope)

按审计实测的 82 B 帧和 PCAP 每记录 16 B 头计算。4,000 pps 发 30 秒产生 120,000 个 TX。每个 TX 均有一个 RX 时，计划抓取约 `240000 × 98 B = 23520000 B`，约 22.4 MiB。已读到的捕获实现上限为 16 MiB。本条是容量推导，还没有对此规模运行真机复测。[帧长观察](https://github.com/nan-mu/xpost/blob/dfb12ac/.audit/taskbook/mcp/xpost/c5995078130b1b3c941f08b0d6989af337d461f2.md#L12-L30) · [PCAP 实现](https://github.com/nan-mu/xpost/blob/dfb12ac/xpost-test/common/src/exam/medium_network_capture.rs#L19-L60)
