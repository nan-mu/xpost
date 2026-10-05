# benchmark

阅读入口是 [report.md](report.md)。正文按 ARRAY、HASH、TX 组织实验设计、结果和解释。图片由 Python 生成。正文独立维护，绘图任务不改写正文，也不生成 Jupyter 或网页报告。

## 生成图片

在仓库根目录运行：

```bash
cd benchmark
pixi run figures
```

任务从 PostgreSQL 只读导出固定批次，再生成图片和 CSV。它使用 `report.toml` 指定的批次及网格。连接参数来自 `.env`，参见 `.env.example`。任务不刷写固件，也不发测试流量。

离线重画当前快照：

```bash
pixi run replay --snapshot outputs/snapshots/20261005T172042.719090Z
```

每张图提供 PNG、SVG、PDF。正文引用 PNG。文件位于 `outputs/report/figures/`。同目录的上一级保存逐点 CSV、Trace 原始事件、审计结果和来源记录。

运行分析检查：

```bash
pixi run check
```

## 数据和解释

`outputs/snapshots/<时间戳>/` 保存压缩原始行和清单。导出采用只读、可重复读事务。清单保存查询、参数、行数和 SHA-256。连接密码不进入清单。离线任务检查数据 SHA-256，再检查实验网格。

`report.toml` 为每个特性固定一个已完成批次。当前有 13 个发送速率。HASH 固定 200 条黑名单，并在同一窗口混发非法流与合法流。配置缺行、重复条件或中断批次会使任务失败。指标缺测保留为空，异常记录进入 `audit.json`。

更新批次时，应同时审阅 `report.md` 中的数值和结论。图片任务不会自动改写这些结论。当前重点负载点和 HASH 配对组写在绘图代码中。若实验网格改变，也应修改对应图件。

`outputs/` 不进入 Git。共享结果时，应保存报告、图片目录和原始快照。没有本地快照时，运行 `pixi run figures` 从数据库重新导出。

## 指标约定

- ARRAY 使用计数总量与主机发出数之比。它没有逐包匹配，比例可略高于 100%。
- HASH 的非法流命中计数相对非法发出数。合法流交付比为 `matched / 1024`，由虚拟 map `firewall_result` 回报。
- TX 的回包吞吐率为 `ok / 30`。Loss 为 `(sent - ok) / sent`。RTT 只包含成功回复。
- `rx_processing` 在 HAL 与 XDP 上覆盖不同片段。累计片段计时比例不等于整机 CPU 利用率。
- ARRAY 火焰时间线使用真实 Trace 区间。HAL 没有同类 Trace，不合成 HAL 时间线。

## 维护入口

- `report.md`：实验报告正文。
- `report.toml`：批次 UUID 和预期网格。
- `analysis/data.py`：只读导出与快照检查。
- `analysis/metrics.py`：指标公式、网格检查和事件配对。
- `analysis/figures.py`：图件定义与导出。
- `analysis/report.py`：生成图片、CSV、审计和来源记录。
- `tests/`：指标与数据质量检查。
- `utils/db.py`：保留的数据库查询工具。
- `backup/`：冻结归档，未经明确要求不得修改或删除。

旧的独立 flame/speedscope 导出工具仍使用旧数据结构。当前报告不调用它们。旧 notebook 与生成入口已删除。Python 环境暂时保留原有依赖，当前绘图流程不需要 Jupyter。

方法参考和科学可视化技能的引用见 [报告末尾](report.md)。
