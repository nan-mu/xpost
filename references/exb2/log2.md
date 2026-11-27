# log2（EXB2 task 2）

对应 Rust 实现：`examples/exec_bench_hal/src/workloads/log2.rs`。编译 Feature：`log2`。

## 原代码、任务语义与原生设计

原代码：[log2_int.h](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/exb2/log2_int.h)。

原内核以分段移位求整数 floor(log2(x))，协议规定 log2(0)=0。variant=0 输入 n 个 little-endian u32，variant=1 输入 n 个 little-endian u64。n 范围 0..1024，无 Scratch。输出 lo 为各 r_i 之和，hi 为从 0 开始、逐元素按 h = rotl32(h, 5) XOR (r_i + i) 更新的 u32 hash。这里的 hi 不是通用 EXB2 digest。

设计思路：可采用 Rust 的 leading_zeros、整数位操作及合适的读取方式，保持 0、32/64 位边界、字节序、顺序相关 hash 的精确语义。不引入浮点近似。

## Agent 交互

Agent 通过 firmware profile 选择启用 `log2` Feature 的 Firmware ELF；固件启动后只执行该 workload。当前 EXB2 兼容请求的 task ID 为 2，其他 task ID 只用于拒绝错误请求，不用于选择算法。

- 请求参数：n 为元素数量，variant=0 时 Input bytes=4n，variant=1 时 Input bytes=8n；Scratch bytes=0。
- Agent 使用 `exec batch reset`、`exec batch write` 上传输入，再发送 `exec batch invoke`。固件执行编译时选定的任务，按现有测量契约返回时间戳和状态。Agent 使用 `exec batch read` 读取结果；读取不再次执行任务。
- 成功时 HAL status=0，completed=rounds；lo、hi 和 digest 符合任务契约。Input 和 Padding 保持不变。无 Scratch。

共享控制流程、缓冲区布局、计时边界和构建说明见本例 README；任务书交付中的公共参考为 `references/hal-exb2-design.md`。这些是当前兼容接口，不预先批准后续协议改版或正式采样参数。

## 已有正确性向量

来源：[xpost-test/common/src/exam/exb2.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/common/src/exam/exb2.rs)。rounds=3、seed=1、variant=0。Scratch 初始填充 0xa5，Padding 填充 0xcc。下表是独立于新 HAL 实现的历史期望，不是新的性能输入规模。

| n | Input | 期望 lo, hi（u32） | Digest |
| ---: | --- | --- | --- |
| 4 | u32 [0, 1, 4, 255] | 9, 0x48a | 0x91fe55a4 |

校验完整结果缓冲区及未修改字节。无 Scratch。 新实现还需按任务语义检查代表性输入；本文未运行构建或实机实验。
