# benchmark（pixi）

xpost 性能实验的分析工作区：**文字与表格在 Jupyter**，**数据只从 Postgres 现查**（`utils/`，无本地结果缓存）。

## 布局

```
benchmark/
  pixi.toml          # 环境与 tasks
  .env               # DB 账号（gitignore；模板见 .env.example）
  utils/             # 通用 Python：db / 实验查询 / schema / ingest
  experiments/       # 每个实验一个 .ipynb
  results/           # 仅作历史种子或探针旁路产物；分析勿依赖
```

## 快速开始

```bash
cd benchmark
cp .env.example .env   # 填入 PGPASSWORD 等
pixi install
pixi run ensure-schema
# 若需把现有 just-drop-pps CSV 写入库（一次性）：
pixi run ingest-pps
pixi run lab           # 打开 experiments/just_drop_pps.ipynb
```

`utils.db.read_sql` / `utils.pps.*` 每次直连数据库，不写 parquet/CSV 缓存。

## 数据库

| 变量 | 含义 |
|------|------|
| `PGHOST` / `PGPORT` / `PGDATABASE` | 目标库 |
| `PGUSER` / `PGPASSWORD` | 账号 |
| `PGSCHEMA` | `public` 或 `formal` |

表（`just_drop_pps_*`）：

- `just_drop_pps_run` — 一次臂（`hal` / `xdp`）+ 窗口参数
- `just_drop_pps_round` — 轮次汇总（`overall_pps`、`host_pps`、…）
- `just_drop_pps_sample` — 周期采样时间序列

与探针：`./probe just-drop-pps --path …`（见 [`xpost-test/README.md`](../xpost-test/README.md)）。CSV 可用 `pixi run ingest-pps` 入库。

## 实验笔记本

| Notebook | 主题 |
|----------|------|
| [`experiments/just_drop_pps.ipynb`](experiments/just_drop_pps.ipynb) | just_drop vs just_drop_hal PPS |
