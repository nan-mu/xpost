# benchmark（pixi）

xpost 性能实验的分析工作区：**复现步骤与文字在 Jupyter**，**数据只从 Postgres 现查**（`utils/`，无本地结果缓存）。

## 布局

```
benchmark/
  pixi.toml              # 环境与 tasks
  .env                   # DB 账号（gitignore；模板见 .env.example）
  utils/                 # db / schema / ingest / experiment 分析
  just_drop_pps.ipynb    # HAL vs XDP PPS：复现流程 + 分析
  tests/                 # pytest（合成数据，不连真库）
```

## 快速开始

```bash
cd benchmark
cp .env.example .env   # 填入 PGPASSWORD 等
pixi install
pixi run ensure-schema
pixi run lab           # 打开 just_drop_pps.ipynb
```

笔记本开头写清采集 → 入库 → 分析步骤。探针 CSV 是临时交接文件，不进仓库。

## 数据库

| 变量 | 含义 |
|------|------|
| `PGHOST` / `PGPORT` / `PGDATABASE` | 目标库 |
| `PGUSER` / `PGPASSWORD` | 账号 |
| `PGSCHEMA` | `public` 或 `formal` |

表（规范契约）：

- `experiment_runs` — 一轮运行元数据（`pair_id`、`run_kind`、`variant`、状态）
- `counter_samples` — 原始 `(time_ns, counter_value)` 快照
- `sample_reads` — 串口读诊断（三点组 / `τ` / 请求—响应时刻；可空）

入库：

```bash
pixi run ingest-pps -- /tmp/hal_pps.csv /tmp/hal_pps_summary.json
pixi run ingest-pps -- /tmp/xdp_pps.csv /tmp/xdp_pps_summary.json
```

探针：`./probe just-drop-pps --path … --out /tmp/….csv`（见 [`../xpost-test/README.md`](../xpost-test/README.md)）。

## 实验笔记本

| Notebook | 主题 |
|----------|------|
| [`just_drop_pps.ipynb`](just_drop_pps.ipynb) | just_drop vs just_drop_hal PPS |

## 测试

```bash
pixi run test
```
