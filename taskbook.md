# XPOST · 放宽 exec_bench 的容器与槽位容量

在 `nan-mu/xpost` 从最新 `origin/main` 创建 `agent/exec-bench-container-capacity` 和独立 worktree。新 PR 的 base 为 **`main`**，交维护者审核。合并后从 `main` 交接。容量改动在这条独立分支完成，PR #23 后续从 `main` 接入。

## 目标

让原版 JIT 的 `log2`、`bsort` 和 `strcmp` 镜像能按 Compact ELF V0 打包，并符合 `exec_bench` 固件的 SRAM 槽和 flash 冷存储容量。这样 libbpf-mcu 发布时可以依赖已合入 XPOST `main` 的版本。

## 已确认的边界

- libbpf-mcu PR #6 在 `9883494` 报告三个原版 JIT payload 分别为 9300、10264 和 9348 B，当前批次为 33/36。容器大小还包括 ELF 头、元数据、对齐和摘要。 [实测记录](https://github.com/nan-mu/libbpf-mcu/pull/6)
- XPOST `main` 的核对基线为 `53c651af83135eabb0fba9a18e96e0cd1c329437`。编码器将完整容器限制在 8192 B，`exec_bench` 的 SRAM 槽和冷存储单元也各为 8192 B。冷存储分区长 64 KiB，构建规则要求单元容量能整除分区长度。 [编码器](https://github.com/nan-mu/xpost/blob/53c651af83135eabb0fba9a18e96e0cd1c329437/xpost-elf/src/encode.rs#L77) · [固件配置](https://github.com/nan-mu/xpost/blob/53c651af83135eabb0fba9a18e96e0cd1c329437/examples/exec_bench/Cargo.toml#L19) · [分区](https://github.com/nan-mu/xpost/blob/53c651af83135eabb0fba9a18e96e0cd1c329437/examples/exec_bench/partitions.csv) · [容量约束](https://github.com/nan-mu/xpost/blob/53c651af83135eabb0fba9a18e96e0cd1c329437/xpost-build/src/layout.rs#L583)

## 本轮改动与验收

1. **容器容量用 Matt 的 `diagnosing-bugs` 和 `implement`。** 从 `xpost-elf` 编码入口核对 8192 B 限制，调整完整容器上限并检查 `exec_bench` 使用的读取路径是否还有旧上限。保留 V0 字节布局和超限拒绝。先从现有测试入口用 `tdd` 检查一个 10264 B payload 的容器能正确编码并读回有效内容，同时保留超过新上限会被拒绝的小边界测试。此处可以使用现有测试夹具，验证的是容器尺寸和编解码。
2. **固件容量用 Matt 的 `implement`。** SRAM 槽按最大 payload 核定。完整容器用原版 JIT 的实际字节大小，或从现有编码器的元数据上限与固定开销推出的保守上界核定。最简夹具只用于验证编解码。可从 12 KiB SRAM 槽、16 KiB 容器上限和 16 KiB 冷存储单元试起，后者可在现有 64 KiB 分区内整除。以实际链接布局确认这些容量可用，并核对冷装载入口所需的并存数量。按现有构建方式生成 `exec_bench-native.elf` 和 `exec_bench-interp.elf`，核对链接槽长度与 `.xpost.mem` 一致，并从实际构建配置核对单元容量。若原尺寸的最大容器仍放不下，记录失败的层级和数据。保持原 JIT、程序语义和其他实验 profile 的容量配置，本轮以主机和链接产物验收。
3. **交付用 Matt 的 `code-review` 和 `pr`。** 核对最终 diff 与 `main` 的差异，给出完整容器实测大小或有来源的保守上界、链接槽长度、冷存储单元容量、两份 ELF 的来源、摘要和取得方式。附既有相关测试的实际结果。此 PR 的验收使用自身的源码与链接产物，libbpf-mcu Release 可在后续生成。

合并到 XPOST `main` 后，把已合入的 SHA、`xpost-elf` 容器上限和对应固件 ELF 交给 libbpf-mcu PR #6。PR #6 对齐本地容器检查和冷存储预算后重跑正式批次。PR #23 用正式发布的归档做消费端验收。
