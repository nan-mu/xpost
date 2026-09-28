# benchmark（pixi 内核 + 外部 Jupyter）

**只做性能测试分析**：用常驻 Jupyter Lab 打开笔记本；分析依赖与 `utils/` 走 **pixi** 环境（通过 kernelspec `xpost-benchmark`）。烧录与发包在 TR3000 `./xpost-test` + Mac `xpost-agent`，不在本目录。

## 布局

```
benchmark/
  pixi.toml             # 分析依赖；task kernel 注册 Jupyter 内核
  .env                  # PGSCHEMA=formal
  utils/                # db / experiment 分析（PYTHONPATH=benchmark/）
  just_drop.ipynb
  just_tx.ipynb
  tests/
```

## 快速开始

```bash
# Mac（另开终端；HTTP /health /v1/* /mcp）
cargo xtask agent --listen 0.0.0.0:9240 --no-monitor

# TR3000
./xpost-test

# 一次性：装依赖 + DDL + 注册内核（给外部 Lab 用）
cd benchmark
cp .env.example .env   # PGSCHEMA=formal
pixi install && pixi run ensure-schema && pixi run kernel
```

Jupyter Lab **常驻**（本机已有即可，不必用 pixi 起）：

```bash
# 例：Homebrew Lab，root 指向仓库或任意目录均可
jupyter lab
```

打开 `just_tx.ipynb` / `just_drop.ipynb` 时选内核 **`xpost-benchmark (pixi)`**（笔记本已绑定该 name）。`utils` 经 kernelspec 的 `PYTHONPATH` 导入，不依赖 Lab 的启动 cwd。

改过 pixi 依赖或换了机器后重新执行 `pixi run kernel`。

## 数据库

| 变量 | 含义 |
|------|------|
| `PG*` | 连接 |
| `PGSCHEMA` | 正式性能数据用 `formal` |

表：`just_tx_config` / `just_tx_data`（扫速与 RTT）、`just_drop_config` / `just_drop_data`（满速 drop PPS）。DDL 见 `xpost-test/sql/`。

## 测试

```bash
pixi run test
```
