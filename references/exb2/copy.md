# copy（EXB2 task 3）

对应 Rust 实现：`examples/exec_bench_hal/src/workloads/copy.rs`。编译 Feature：`copy`。

## 原代码、任务语义与原生设计

原代码：[copy.h](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/exb2/copy.h)。

原内核逐字节从 Input 复制到紧随其后的 Scratch，n 范围 0..4096。每轮复制 n 字节；输出 lo=n、hi=0。源数据不变，目标 n 字节与源一致。

设计思路：可使用 Rust slice copy_from_slice 或目标适合的原生内存复制方式，不限制逐字节循环。必须实际产生目标数据。不能只返回 n，或让目标与源别名而没有完成独立复制。

## Agent 交互

Agent 通过 firmware profile 选择启用 `copy` Feature 的 Firmware ELF；固件启动后只执行该 workload。当前 EXB2 兼容请求的 task ID 为 3，其他 task ID 只用于拒绝错误请求，不用于选择算法。

- 请求参数：n 为复制字节数，Input bytes=n，Scratch bytes=n。
- Agent 使用 `exec batch reset`、`exec batch write` 上传输入，再发送 `exec batch invoke`。固件执行编译时选定的任务，按现有测量契约返回时间戳和状态。Agent 使用 `exec batch read` 读取结果；读取不再次执行任务。
- 成功时 HAL status=0，completed=rounds；lo、hi 和 digest 符合任务契约。Input 和 Padding 保持不变。Scratch 必须等于输入的 n 个字节；历史向量为 44 20 82 3c（hex）。

共享控制流程、缓冲区布局、计时边界和构建说明见本例 README；任务书交付中的公共参考为 `references/hal-exb2-design.md`。这些是当前兼容接口，不预先批准后续协议改版或正式采样参数。

## 已有正确性向量

来源：[xpost-test/common/src/exam/exb2.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/common/src/exam/exb2.rs)。rounds=3、seed=1、variant=0。Scratch 初始填充 0xa5，Padding 填充 0xcc。下表是独立于新 HAL 实现的历史期望，不是新的性能输入规模。

| n | Input | 期望 lo, hi（u32） | Digest |
| ---: | --- | --- | --- |
| 4 | 44 20 82 3c（hex） | 4, 0 | 0x91efe859 |

校验完整结果缓冲区及未修改字节。Scratch 必须等于输入的 n 个字节；历史向量为 44 20 82 3c（hex）。 新实现还需按任务语义检查代表性输入；本文未运行构建或实机实验。
