# xpost-test 配置

编译期嵌入；改完后需重编再 scp 到 TR3000。正式采集永远：

```bash
./xpost-test
```

无环境变量、无 CLI 参数。`default.toml` 的 `run = [...]` 只选**实验名**；实验内部步骤写死在代码里，不在 TOML 拆分。

## `run` 可选值

| 名 | 做什么 |
|----|--------|
| `just_tx` | HAL + XDP：满速处理 PPS（`experiment_*`）→ 扫速丢包 / return PPS → 约 5% 丢包工作点 RTT（`just_tx_rtt_*`） |
| `just_drop` | HAL + XDP：满速处理 PPS（`experiment_*`，读 `drop`） |

示例：

```toml
run = ["just_tx"]
# run = ["just_drop", "just_tx"]
```

## 其它文件

- `default.toml`：`run`、postgres、agent、DUT、probe、profiles
- `just_tx.toml` / `just_drop.toml`：轮次、臂、`formal` 等旋钮（不决定「跑哪些步骤」）

## 功能 suite（可选）

`./xpost-test suite` 跑固定的 `test0`（与 `run` 无关）。case / IP 写在代码里，不在本目录配置。

调试单步仍可用子命令（如 `just-tx-rtt`），正式路径只用上面的实验名。
