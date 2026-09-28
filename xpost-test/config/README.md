# xpost-test 配置

编译期嵌入；改完后需重编再 scp 到 TR3000。正式采集永远：

```bash
./xpost-test
```

无环境变量、无 CLI 参数。`default.toml` 的 `run = [...]` 只选**实验名**；实验内部步骤写死在代码里，不在 TOML 拆分。

## `run` 可选值

| 名 | 做什么 | 写哪些表 |
|----|--------|----------|
| `just_drop` | **正式** HAL + XDP：`kinds` 含 `c`（满速 drop 序列）与 `a`/`g`/`t`（固定 pps 多轮） | `just_drop_config`/`data`；`just_drop_run` + agg/gauge/trace_summary |
| `just_tx` | **正式** HAL + XDP 扫速 / ~5% 工作点（网格 400…4000 pps，步长 400） | `just_tx_config` + `just_tx_data` |
| `dev` | **调试** 仅 XDP：800/1600/2800/4000 × 1 × 8s；无 DB，看控制台 | — |

`dev` 全硬编码（无独立 TOML）；永不碰 Postgres / `just_tx_*` / `just_drop_*`。

正式实验的 `formal`：同名旧表 `DROP CASCADE` 后整表重建。DDL 见 [`../sql/`](../sql/)。

```toml
run = ["dev"]           # 日常改固件
# run = ["just_tx"]     # 正式
# run = ["just_drop", "just_tx"]
```

## 其它文件

- `default.toml`：`run`、postgres、agent、DUT、probe、profiles
- `just_drop.toml`：`arms` / `kinds`（`c|a|g|t`）/ `fixed_pps` / `trace_pps` / `instr_rounds` / `instr_window_secs`
- `just_tx.toml`：正式实验旋钮（调试时不要改）

## 功能 suite（可选）

`./xpost-test suite` 跑固定的 `test0`。也可用 `./xpost-test dev`。
