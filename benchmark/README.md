# benchmark（pixi）

xpost 性能实验的分析工作区：**复现步骤与文字在 Jupyter**，**数据只从 Postgres 现查**（`utils/`，无本地结果缓存）。

采集侧产物是 TR3000 上的 **`./xpost-test`**（编译期嵌入 `xpost-test/config/test.toml`），默认跑 `just-drop-pps`：经 Mac `xpost-agent` 烧录固件/`xdp.elf`，写入 schema **`formal`** 的 `experiment_*`（开跑前表为空）。

## 布局

```
benchmark/
  pixi.toml              # 环境与 tasks
  .env                   # DB 账号；PGSCHEMA=formal
  utils/                 # db / schema / ingest / experiment 分析
  just_drop_pps.ipynb    # HAL vs XDP PPS：复现流程 + 分析
  tests/                 # pytest（合成数据，不连真库）
```

## 快速开始

```bash
cd benchmark
cp .env.example .env   # PGSCHEMA=formal
pixi install
pixi run ensure-schema
pixi run lab           # 打开 just_drop_pps.ipynb
```

## 数据库

| 变量 | 含义 |
|------|------|
| `PGHOST` / `PGPORT` / `PGDATABASE` | 目标库 |
| `PGUSER` / `PGPASSWORD` | 账号 |
| `PGSCHEMA` | 正式实验用 `formal` |

表：`experiment_runs` / `counter_samples` / `sample_reads`。

可选旧 CSV：`pixi run ingest-pps -- <csv> <summary.json>`。

## 测试

```bash
pixi run test
```
