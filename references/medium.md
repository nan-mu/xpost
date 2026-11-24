## 中量矩阵

| 实验 | 执行模式 | 采集变体 | 格数 |
| --- | --- | --- | ---: |
| firewall、just_drop、just_tx | HAL、AOT、JIT、rBPF | Metrics、Trace | 24 |
| exec_bench | HAL、AOT、JIT、rBPF | Metrics | 4 |
| case（A/B 热切换） | AOT | Trace | 1 |
| 合计 | | | **29** |

中量执行整张表。每格包含下列有效观测。沿用 [9e73d84 的中量密度](https://github.com/nan-mu/xpost/blob/9e73d84a6ac248c90c6b1f55cab33335c27cecf5/xpost-test/procedures/medium.md#planned-matrix-and-time-budget)，应用本轮表中的执行模式与采集变体。

- 网络 Metrics 在 400、1400、4000 pps 各做两次，每次发送 5 秒。Trace 每个模式对 firewall 分别做一次 hit、miss，just_drop 和 just_tx 各做一次带标签探针。
- exec_bench 的每个模式覆盖七个程序，每个程序一个已记录的条件、两批。效率比较采用每批 Metrics，Trace 本轮不执行。
- case/AOT Trace 做一次 A→B 切换，解释切换期间的报文与控制阶段。准备探测在测量窗外。

`lookup` 和 `dev` 是独立诊断入口。中量使用表内 29 格完成真机验收，按 [逐项 PR 反馈](../taskbook.md#6-每项出结果就在-pr-展示) 发布每次实际结果。

```text
//! firewall:
//!   for fw in medium firmware groups:
//!     if device firmware differs: agent flash fw
//!     for mode in modes served by fw:
//!       bind firewall for mode; cold install and warm slot only if needed
//!       if fw supports planned Metrics:
//!         for rate in medium rates:
//!           for rep in medium repeats:
//!             reset blacklist to known hit/miss keys
//!             agent perf prepare
//!             enable policy
//!             agent perf arm
//!             send mixed hit/miss stream at rate for window
//!             wait bounded drain
//!             agent perf freeze
//!             disable policy
//!             read blacklist deltas and delivered legal tokens
//!             agent perf read
//!             agent perf release
//!             upload to db
//!       if fw supports planned Trace:
//!         for probe in {hit, miss}:
//!           reset blacklist to known hit/miss keys
//!           agent perf prepare
//!           enable policy
//!           agent perf arm
//!           send probe and wait for completion or timeout
//!           agent perf freeze
//!           disable policy
//!           read blacklist deltas and delivered legal tokens
//!           agent perf read
//!           agent perf release
//!           upload to db
//!
//! **数据处理**
//! 分别记录发送成功的合法、非法包数。合法交付率 = 匹配的唯一合法 token 数 / 合法发送数；非法命中比 = 黑名单命中计数增量 / 非法发送数。分母为零记 NULL，超过 1 保留并检查背景流量。Trace 分开记录 hit、miss。
```

```text
//! just_drop:
//!   for fw in medium firmware groups:
//!     if device firmware differs: agent flash fw
//!     for mode in modes served by fw:
//!       bind just_drop for mode; cold install and warm slot only if needed
//!       if fw supports planned Metrics:
//!         for rate in medium rates:
//!           for rep in medium repeats:
//!             check connectivity and pin neighbor before enabling DROP
//!             agent perf prepare
//!             enable policy
//!             agent perf arm
//!             send tagged probes at rate for window
//!             wait bounded drain
//!             agent perf freeze
//!             disable policy
//!             read normal DROP and fault counters
//!             agent perf read
//!             agent perf release
//!             upload to db
//!       if fw supports planned Trace:
//!         for probe in {tagged DROP probe}:
//!           check connectivity and pin neighbor before enabling DROP
//!           agent perf prepare
//!           enable policy
//!           agent perf arm
//!           send probe and wait for completion or timeout
//!           agent perf freeze
//!           disable policy
//!           read normal DROP and fault counters
//!           agent perf read
//!           agent perf release
//!           upload to db
//!
//! **数据处理**
//! 实际 offered pps = 成功提交的唯一探针数 / 主机实际发送秒数；ingress pps 与 completed-drop pps 各用归属本次探针的设备计数增量 / DUT 开窗秒数。单列 runtime fault；无法归属记 inconclusive。HAL 的完成点是 socket discard，XDP 是正常 DROP 动作完成。
```

```text
//! just_tx:
//!   for fw in medium firmware groups:
//!     if device firmware differs: agent flash fw
//!     for mode in modes served by fw:
//!       bind just_tx for mode; cold install and warm slot only if needed
//!       if fw supports planned Metrics:
//!         for rate in medium rates:
//!           for rep in medium repeats:
//!             start TX/RX capture and read initial rxcnt
//!             agent perf prepare
//!             enable policy
//!             agent perf arm
//!             send JTX1 frames with UUID and seq at rate for window
//!             wait bounded drain
//!             agent perf freeze
//!             disable policy
//!             read rxcnt and collect TX/RX records
//!             agent perf read
//!             agent perf release
//!             upload to db
//!       if fw supports planned Trace:
//!         for probe in {JTX1 probe}:
//!           start TX/RX capture and read initial rxcnt
//!           agent perf prepare
//!           enable policy
//!           agent perf arm
//!           send probe and wait for completion or timeout
//!           agent perf freeze
//!           disable policy
//!           read rxcnt and collect TX/RX records
//!           agent perf read
//!           agent perf release
//!           upload to db
//!
//! **数据处理**
//! 记录成功发送数、在排空截止前匹配的唯一有效回包数、重复、迟到及损坏数。observed loss = (sent − returned) / sent；sent 为零记 NULL。抓包和时钟合格时才报告 qualified loss。RTT 取同一主机时钟上唯一 TX 与首个有效 RX 的时间差，按模式、速率给出中位数、p95 和有效包数。
```

```text
//! exec_bench:
//!   for fw in medium firmware groups:
//!     if device firmware differs: agent flash fw
//!     for mode in modes served by fw:
//!       for workload in {noop, prime, log2, copy, fletcher32, bsort, strcmp}:
//!         select HAL entry or install and warm matching XDP ELF once
//!         for input, rounds in medium EXB2 conditions:
//!           for rep in medium repeats:
//!             initialize 8258-byte EXB2 buffer with 64-byte header
//!             agent perf prepare (Metrics)
//!             agent perf arm
//!             invoke workload once; workload executes rounds internally
//!             agent perf freeze
//!             check XDP_PASS or direct-entry OK and read EXB2 status, completed, result and digest
//!             compare input and padding; for copy and bsort compare entire scratch
//!             agent perf read
//!             agent perf release
//!             upload to db
//!
//! **数据处理**
//! 按模式、workload、输入及 rounds 保存每批 DUT ticks、completed 和正确性。status、完成数、输出均符合预期的批次计算 ticks / rounds；分布保留整批耗时。另列失败批数及具体不符字段。
```

## AOT 热切换

用一次 A→B Trace 展示发布前后的数据面和控制路径。冷安装、A/B 预装和双策略功能探测在计时窗外。报文期望和实测载荷见 [v0.1.1 case 双槽对照](../tests/case-v0.1.1.md)。旧版 `case` 曾执行单槽冷替换，`hot_swap` 曾执行 100 次往返切换。按 [任务书第 4 节](../taskbook.md#4-删除旧-case-实验路径) 接入现行单次 Trace。[旧 case.rs](https://github.com/nan-mu/xpost/blob/9e73d84a6ac248c90c6b1f55cab33335c27cecf5/xpost-test/common/src/exam/case.rs#L555) · [旧 hot_swap.rs](https://github.com/nan-mu/xpost/blob/9e73d84a6ac248c90c6b1f55cab33335c27cecf5/xpost-test/common/src/exam/hot_swap.rs#L122)

```text
//! case (AOT Trace):
//!   if device firmware differs: agent flash AOT Trace firmware
//!   if required images are not installed:
//!     cold install case_swap for A and case_mark_idle for B
//!   record cold installs and boot identity
//!   warm slot A and slot B once
//!   verify both policies with v0.1.1 JTX1 probes and CAS2 markers outside the timed window
//!   restore slot A as active and slot B as staged; record preparation actions
//!   verify both slots; record boot ID and both generations
//!   prepare Trace and start timestamped tcpdump capture with continuous tagged stimulus
//!   wait for stable A output; arm Trace
//!   publish prepared slot B once
//!   observe tagged B output within the bounded capture window
//!   freeze, read and release Trace
//!   include a bounded capture tail after the first B reply; stop capture
//!   verify same boot ID, expected slot B generation and DUT packet behavior
//!   store this switch, raw capture and trace with the same run_id
//!   render the tcpdump view and the event timeline from this run
//!
//! **数据处理**
//! UDP 目的端口为 8080。探针载荷为 32 字节 JTX1 头加 8 字节零尾部，共 40 字节。
//! 用每包的 JTX1 标签关联 DUT 回复。回复保留 JTX1 头和 IPv4 地址，互换 MAC，UDP 校验和清零。
//! JTX1 后的 8 字节是四字节大端版本号加 CAS2。A 槽 case_swap 回版本 2，B 槽 case_mark 回版本 1。
//! 按 v0.1.1 的完整报文判据核验 XDP_TX。已发布但状态或报文不符时，记录失败及实际状态。
//! 计时窗内没有冷装、重启或再次搬运。单列首次新回复后的旧回复、缺包和 quiesce_drops。
```

### case 两张交付图

1. **tcpdump 切换窗口。** 从实际抓包记录截取稳定 A 至首个 B 回复后有界观察窗结束的区间。显示抓包时间、探针序号或标签、DUT 回复及 CAS2 版本号。标出首个 B 回复及它之前的最后一个 A 回复，并单列尾窗内的迟到 A 回复。保留原始抓包记录，图中的回复可追溯到具体包。历史做法将每轮窗口写入 `case_obs.tcpdump`。[历史提交 c5ea22d](https://github.com/nan-mu/xpost/commit/c5ea22d77a266ba052f71982c603828126bcb49f)
2. **切换事件时间戳。** 从同一次 Trace 的实际事件绘制时间线，显示 DUT 的 control、quiesce、rebind（发生时）和 publish 边界，测试主机记录命令发出和收到确认，tcpdump 标出首个 B 回复及它之前的最后一个 A 回复。原始记录中已有的 Agent 接收时间按其来源时钟域展示。确认抓包与测试主机同一时基后合并到主机泳道，否则单列抓包泳道。各泳道在各自时钟域内取相对时间、标注单位。跨域延迟只在时钟已同步且可核对时计算。历史实现分别保存 `start`、`mcu_us`、`agent_rx`、`end`。其中 `mcu_us` 是板端放行新入口的时刻，阶段耗时从本次 Trace 的同域边界计算。[时间点来源](https://github.com/nan-mu/xpost/commit/2b2583a869cb921b56a302c7f23f3f346516b609)

两张图使用同一成功 run 的 run_id、切换序号和方向，随英文 PR 交付。保留可核对的抓包和 Trace 事件，缺失的必需边界按验收缺项报告。这是一次切换的证据，图表按单次观测解读。

```text
//! lookup:
//!   for fw in selected lookup diagnostic firmware groups:
//!     if device firmware differs: agent flash fw
//!     for mode in modes served by fw:
//!       bind just_tx once
//!       for lookup_timing in {off, on}:
//!         disable policy and set lookup timing
//!         run just_tx Metrics at 1400 pps with the same input and window
//!       bind firewall once
//!       for rate in selected {400, 1400, 4000} pps:
//!         for outcome in {hit, miss}:
//!           set known blacklist and send matching traffic in a helper_hist window
//!           freeze and export lookup counts and duration buckets to db
//!
//! **数据处理**
//! just_tx 配对比较开关前后的回包率与 RTT，并保存启用时的 array lookup 桶。firewall 对 hash hit/miss 分别报告样本数和 ticks 桶。零样本与零耗时分开。最小 just_drop 没有 lookup；删除旧诊断分支。诊断复用相同条件的 run。
```

```text
//! dev:
//!   for policy in {just_tx, just_drop, firewall}:
//!     if device firmware differs: agent flash corresponding HAL firmware
//!     prepare known inputs and enable policy
//!     send short stimulus and wait bounded drain
//!     disable policy and check expected dataplane outcome
//!
//! **数据处理**
//! 按策略记录诊断结果，写到诊断记录。
```
