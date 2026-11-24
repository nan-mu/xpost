# XPOST #25、#26 的已有证据及合并预查

## 变基前的远端与冲突证据

编写任务书时，以 GitHub PR API 和公开 Git ref 交叉确认 PR #23 head、`refs/pull/23/head` 和 `agent/experiment-matrix-v1` 都是 `b5a44072ed57657c7482758412cccf1fd93df6fa`。main 为 `9cb03b795c8d45c5d107cf8e63b90dca6f1dcb56`。该 main 已含 `3986c4f17a54e7c9bd830d558c40c8c13a066660`。旧 PR23 与 main 的 merge base 为 `ffc8082a99d2dc9ffec70f18a87cd6f86a05e96c`。本机 `git merge-tree --write-tree origin/pr23 origin/main` 返回 1，冲突位置为 `src/xdp/swap.rs`、`xpost-agent/src/ops.rs`、`xpost-agent/src/profiles.rs`。当前 #23 已推送至 `9e73d84a6ac248c90c6b1f55cab33335c27cecf5`。通过 `git merge-base --is-ancestor` 确认该提交包含 T3 已变基起点 `b473cf1` 以及 #25、#26 的 main 合入提交。上述冲突是对旧 ref 的模拟结果，仅用于追溯。后续改动核对当前代码继续保留下述行为。

## 容量与编码

- [XPOST #25](https://github.com/nan-mu/xpost/pull/25) 在 `6360e9e` 报告 native 和 interp 两个已链接 exec ELF，Rust nightly `8eb778d04`。两者 `xdp_slot`、`.xdp.hotload_0` 与 XPM1 同示 0x40380200、12,288 B，copy buffer 同为 12,288 B。冷表为 64 KiB，每项容量 16 KiB。该 PR 的主机测试通过，证据适用于其构建布局。
- [XPOST #26](https://github.com/nan-mu/xpost/pull/26) 在 `83c683c` 报告 17,001 B 镜像在旧主机上限报 `ContainerTooLarge`，主机按编码长度分配后编解码通过。八个 xpost-elf 主机测试及 RV32IMC 编译检查通过。代码检查确认合入版本只在嵌入式路径按固定缓冲容量限制，主机编码按计算长度分配。libbpf-mcu PR6 另外报告其生产 `pack` 与 Python 对同一镜像产生相同的 17,328 B 容器。

以上容量和编码实验由对应 PR 执行端完成。复用相同实现及布局的结果。接入 Agent 后核对本次真实链接固件、冷分区、transport 与预装路径。旧实验未覆盖这些集成调用。
