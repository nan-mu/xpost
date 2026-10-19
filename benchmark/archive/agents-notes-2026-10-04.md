# AGENTS.md 中的 benchmark 历史说明

归档日期：2026-10-04。以下内容从根目录 AGENTS.md 移出，仅保留历史上下文。
benchmark 计划重构；这些说明不作为当前操作指令或重构约束，路径和导出流程未重新验证。

## 原火焰图导出说明

Flame export is **XDP-only** split path charts
(`export_flame_svg`: `just_tx-xdp-*-path-rx|path-tx.flame.*`;
x = `t_us` within episode). Prefer short `dev` capture →
`/tmp/xpost-dev-flame.report` → Mac `target/dev-flame.report`.

调试火焰图 Trace（**火焰轨**）：`run = ["dev"]`（硬编码：XDP × `just-tx-t` ×
100 pps × 2s × 1 轮；**无 DB**；report → `/tmp/xpost-dev-flame.report`；
Mac scp 到 `target/dev-flame.report` 后 `pixi run python -m utils.export_flame_svg`）。

## 原 Formal 采集与分析立场

采集：TR3000 `./xpost-test` → `formal.*`（DDL：`xpost-test/sql/formal.sql`）。`benchmark/`
**不负责**烧录 / 刺激 / USB；也不再维护「一实验一 notebook」的 Evaluation 路径。

- **实验**（`just_tx` / `just_drop` / 后续）只是取数窗口；Analysis / Evaluation 应按
  **指标与 xpost 功能问题**横切（例如 `hal_agg`/`xdp_agg` 同名段在 Pass vs Drop vs TX
  下是否同事），常跨多个 exam 对比。见 `benchmark/README.md`。
- 近期：先跑完全部 formal 实验；数据仅作轻量健全性检查。深度作图与章节写作延后，
  且应对齐 Methodology 的功能分节，而非实验名。
- 2026-10-04 当时的规则是保留 `benchmark/backup/`。该目录已从工作区删除。7z 在 git 标签 `archive/just-tx-formal-20260928`。
