# XPOST 实验架构重构 · PR #24 实施任务书

**先在 PR #24 完成本轮功能实现，只读验证通过后，将 #24 合入 PR #23。#24 不做真实烧录。** 实验依赖 `libbpf-mcu` 的新产物发布逻辑，它将替换旧的 `xpost-artifact` 相关流程。等上游能按新流程提供产物、其他依赖 PR 也就绪后，再在 #23 编写并运行具体测试。

## 一、在 #24 完成功能实现

接续 `nan-mu/xpost` 现有分支 `agent/xpost-artifact-contract`，合入目标是 #23 的 `agent/experiment-matrix-v1`。2026 年 11 月 23 日核对时，#24 的 head 是 `0ed29333`，#23 的 head 和 #24 的 base 都是 `2e402e0e`。接手时以两个 PR 的最新提交为准。[PR #24](https://github.com/nan-mu/xpost/pull/24) · [PR #23](https://github.com/nan-mu/xpost/pull/23)

- 沿 #24 的现有实现，从 Agent 的正常入口追到产物选择、身份校验和装载准备。补齐缺口，使调用方能追溯所选程序、模式、profile、固件摘要以及最终交给装载层的容器摘要。HAL 沿用自身固件路径；AOT、JIT 和 rBPF 使用对应的上游 XDP 产物。
- `libbpf-mcu` 负责生成并发布产物。XPOST 负责消费、校验并传递其身份，消费端不重新构建 XDP。现有 #24 固定的生产端版本、旧索引和缓存路径是迁移起点。上游尚未定稿的字段或文件布局，在 #23 的交接清单中标明；不要据旧流程推定新发布格式。[现有解析器](https://github.com/nan-mu/xpost/blob/0ed29333ba44937448032e1e92b964fe70c0dfda/xpost-artifact/src/lib.rs) · [Agent 调用入口](https://github.com/nan-mu/xpost/blob/0ed29333ba44937448032e1e92b964fe70c0dfda/xpost-agent/src/ops.rs)

## 二、#24 的合并条件

在待合并的 head 上阅读差异、配置和调用链，记录审阅的提交及对应文件或符号。在审阅记录中逐项回答以下问题。

1. 给定程序、模式、profile 和目标固件，代码从哪里取得候选产物，如何核对身份和摘要？
2. 最终传给装载层的是哪一份已校验的容器？缺件、摘要不符或固件不匹配时，代码在哪里阻止继续写入？
3. 哪些调用仍依赖旧 `xpost-artifact` 流程？哪些部分已经完成替换，哪些要等上游发布契约就绪后在 #23 对接？

发现实现缺口就在 #24 修正，并对新 head 重新只读核对。通过后将 #24 合入 #23。交接记录写明审阅提交、只读结论和留给 #23 的测试项。本阶段不连接设备、不调用 `flash_xdp`，也不把真机样本、Release 联调或矩阵结果列为合并条件。

## 三、依赖就绪后，在 #23 编写并运行具体测试

启动条件是 #24 已合入、`libbpf-mcu` 已按新流程发布所需产物、其他相关 PR 已就绪。在 #23 记录实际采用的发布版本和依赖提交，再从以下入口验证。

| 在 #23 验证的内容 | 可复核的结果 |
| --- | --- |
| 实际取件与烧录 | 从新发布渠道取得与 `firewall-cag` 匹配的 `firewall` AOT 容器。空缓存下执行真实冷装载，对照发布记录、实际烧录输入和设备固件，核对写前校验及本次 XDP 构建次数为零。 |
| 从设备到数据库 | 用已知的 firewall hit/drop 和 miss/pass 检查设备行为。独立采集 Metrics 与 Trace，沿固件、串口、`xpost-agent`、`xpost-test` 到 PostgreSQL，用同一个 run 查回产物身份、原始观测和分析结果。 |
| 实验矩阵 | 按原设计覆盖 `firewall`、`just_drop`、`just_tx`、`exec_bench`、`case`，区分 HAL、AOT、JIT、rBPF 及 Metrics、Trace。先做小批真机验证，按 40 个设计组合标记实测、未运行、阻塞或按设计可选；正式性能采样另行安排。 |

**包内入口。** 根目录 `taskbook.md` 规定本轮顺序。附件中的 `AGENT_TASK.md` 是 11 月 15 日的历史任务书。进入 #23 的测试阶段时，按测量规范、逐实验设计和 SQL 样例落实判据。[设计索引](XPOST_EXPERIMENTS_V1/XPOST_EXPERIMENTS_INDEX.md) · [测量规范](XPOST_EXPERIMENTS_V1/02_MEASUREMENT.md) · [40 项矩阵](XPOST_EXPERIMENTS_V1/03_MATRIX.csv) · [查询样例](XPOST_EXPERIMENTS_V1/sql/queries.sql)
