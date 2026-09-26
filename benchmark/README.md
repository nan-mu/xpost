# benchmark（pixi）

**只做性能测试分析**：Jupyter 现查 Postgres 中的 PPS / 稳定性 / 噪声模型脚手架。烧录与发包在 TR3000 `./xpost-test` + Mac `xpost-agent`，不在本目录。

## 布局

```
benchmark/
  pixi.toml
  .env                   # PGSCHEMA=formal
  utils/                 # db / experiment 分析
  just_drop.ipynb        # drop 路径性能复现 + 图
  just_tx.ipynb          # tx：扫速/RTT + 满速处理 PPS
  tests/
```

## 快速开始

```bash
# Mac（另开终端；HTTP /health /v1/* /mcp）
cargo xtask agent --listen 0.0.0.0:9240 --no-monitor

# TR3000
./xpost-test

# 分析
cd benchmark
cp .env.example .env   # PGSCHEMA=formal
pixi install && pixi run ensure-schema && pixi run lab
```

## 数据库

| 变量 | 含义 |
|------|------|
| `PG*` | 连接 |
| `PGSCHEMA` | 正式性能数据用 `formal` |

表：`experiment_runs` / `counter_samples` / `sample_reads`。

## 测试

```bash
pixi run test
```
