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
//!         for variant in planned variants supported by fw:
//!           for input, rounds in medium EXB2 conditions:
//!             for rep in medium repeats:
//!               initialize 8258-byte EXB2 buffer with 64-byte header
//!               agent perf prepare
//!               agent perf arm
//!               invoke workload once; workload executes rounds internally
//!               agent perf freeze
//!               read EXB2 status, completed, result and digest
//!               if workload in {copy, bsort}: compare entire scratch
//!               agent perf read
//!               agent perf release
//!               upload to db
//!
//! **数据处理**
//! 按模式、workload、输入及 rounds 保存每批 DUT ticks、completed 和正确性。status、完成数、输出均符合预期的批次计算 ticks / rounds；分布保留整批耗时。另列失败批数及具体不符字段。
```

```text
//! case:
//!   for fw in medium firmware groups:
//!     if device firmware differs: agent flash fw
//!     for mode in modes served by fw:
//!       select static HAL policies or cold install case_swap and case_mark once
//!       for policy in {case_swap, case_mark}:
//!         select HAL policy or warm and activate target slot
//!         verify policy on wire; disable and drain before reading map baseline
//!         agent perf prepare
//!         enable policy
//!         agent perf arm
//!         send tagged case probes
//!         wait bounded drain
//!         agent perf freeze
//!         disable policy
//!         read observations and counter delta
//!         agent perf read
//!         agent perf release
//!         upload to db
//!
//! **数据处理**
//! 对 case_swap、case_mark 分别记录发出、匹配预期输出、错误版本、缺失的探针数及对应 map 增量。按策略报告正确输出数 / 成功发送数，并保留分子分母。每个 probe 按身份只计一次。
```

```text
//! hot_swap:
//!   for fw in medium firmware groups:
//!     if device firmware differs: agent flash fw
//!     for mode in modes served by fw:
//!       if mode is HAL: prepare two static policies
//!       else:
//!         cold install case_swap and case_mark if missing
//!         warm slot A and slot B once
//!       select A
//!       start continuous tagged stimulus and wait for stable output
//!       for variant in planned variants supported by fw:
//!         agent perf prepare
//!         agent perf arm
//!         for switch in medium alternating switches (Trace: one switch):
//!           select other HAL policy or switch to other prepared slot
//!           record switch ticks and packet observations
//!         agent perf freeze
//!         agent perf read
//!         agent perf release
//!         upload to db
//!       stop stimulus
//!
//! **数据处理**
//! 记录主机命令往返时间、同一 DUT 时钟的切换 ticks、quiesce 和 publish ticks。主机另算最后旧回复到首次新回复的间隔，单列 old_after_new、missing_probes 和 quiesce_drops。冷安装、warmup、已预装切换分项记录；HAL 标注静态策略选择。
```

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
//! 按策略报告 smoke 通过或失败；此入口不写正式实验库。
```
