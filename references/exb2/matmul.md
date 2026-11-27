# matmul（EXB2 task 7）

对应 Rust 实现：`examples/exec_bench_hal/src/workloads/matmul.rs`。编译 Feature：`matmul`。这是新增矩阵乘法任务，不是原 C workload 的迁移。本轮 PR42 实现 HAL；同一契约将供后续 libbpf-mcu 的 AOT、JIT、rBPF 产物使用。

## 研究来源及实现选择

OpenAI 2026-10-02 预印本 [An Upper Bound of 9/4 for the Matrix Multiplication Exponent](https://github.com/openai/math/blob/main/preprints/Matrix-Multiplication-Nine-Fourths-October-2-2026/build/paper.tex) 的主定理给出对任意正 epsilon 的 O_epsilon(n^(9/4+epsilon)) 算术运算上界。它利用固定基维度 u 和精确秩分解长度 r 递归处理矩阵块；构造和常数依赖 epsilon。2026-11-24 的 [The matrix multiplication exponent is nine fourths](https://github.com/openai/math/tree/main/preprints/The-matrix-multiplication-exponent-is-nine-fourths-November-24-2026) 声称证明匹配下界；这是预印本的结论，不是 MCU 性能结论。Amir Shpilka 的 [Exposition of OpenAI’s Matrix Multiplication in Exponent 9/4 (and an explicit algorithm)](https://eccc.weizmann.ac.il/report/2026/281/)（2026-10-27）给出了显式构造，其基矩阵尺寸上界依赖 epsilon。以上不是一个已证实适用于 26×26 以内整数矩阵的简单程序。

本轮必须实现实际运行的矩阵乘法，不因研究构造复杂而省略任务。默认可采用经典三重循环或等价的原生优化实现（O(n^3) 算术操作），例如优化访问次序、局部累加和内存布局。Strassen 的递归七乘法具有 O(n^log2(7)) 渐近复杂度，可作为实现者评估的替代方法；小矩阵和紧张 SRAM 下不假定它更快。只有能给出确切的有限维构造、正确的整数运算实现、资源与实测证据时，才采用 9/4 研究相关方法。简要说明可实施性判断、实际算法和复杂度，不把研究上界当作本固件的性能指标。不要求额外实现多个算法、复现数学证明或开展大规模理论算法研究。

## 任务语义与设计思路

输入两个 n×n 的方阵 A、B，输出完整的 C=A×B。每个元素 C[i,j] = sum(k=0..n-1, A[i,k] * B[k,j])。输入为运行时提供的无符号整数 0..255，输入和输出元素编码为 little-endian u32。n=1..26；最大元素结果为 26×255²=1,690,650，u32 足够。这个精确整数任务并非对任意域的算术电路复杂度实验。

现有 EXB2 上下文容量 8258 B，64 B header，A 和 B 依次按行主序排列于 Input，C 位于 Scratch，均为连续的 n² 个 u32。Input bytes=8n²，Scratch bytes=4n²，总占用 64+12n²，n=26 时为 8176 B。n=27 无法容纳。先验证维度和所有长度，再访问缓冲区。使用 task 专属尺寸限制，不把原 copy/fletcher32 的 4096 B 上限错误应用于双矩阵输入。variant=0。

每个 round 从不可变的 A、B 计算完整 C；不依赖上一轮的 C，不将计算提前到准备阶段，不只算首末元素或返回预计算答案。C 使用独立 Scratch。完成后 result.lo=C[0,0]，result.hi=C[n-1,n-1]；共享 digest 按现有 EXB2 规则更新。完整 C 逐元素校验，不能仅检查 lo、hi、digest。选择布局和循环优化时记录实际算法及比较边界。无需改变原七项协议或性能输入规模。

## Agent 交互

Agent 通过对应 firmware profile 选择并烧录仅启用 matmul Feature 的 Firmware ELF。固件只处理 task 7，不以请求选择其他算法。Runner 按上述布局生成 A、B，使用当前 exec batch reset/write/invoke/read 和 Metrics 会话，等待执行结果并按现有计时语义记录。成功返回 HAL status=0，completed=rounds；完整 C 写入 Scratch。错误尺寸、长度、variant 或 task ID 按共享契约拒绝，不访问越界区域。输入值越界不应产生成功样本。Input、Padding 和 reserved 保持不变。

当前 PR42 在 HAL 的 standalone 和正式计划/预检查/调度/入库路径加入本任务。AOT、JIT、rBPF 的 matmul XDP ELF 尚未发布，不将该项列为已完成的四模式实验。具体采样网格和优化算法比较留待性能实验设计。

## 独立正确性向量与验证

n=2，A=[[1,2],[3,4]]，B=[[5,6],[7,8]]，按行主序编码。独立按矩阵定义得到 C=[[19,22],[43,50]]。Input bytes=32，Scratch bytes=16，rounds=3，seed=1，variant=0。Scratch 初始 0xa5，Padding 初始 0xcc。结果 status=0，completed=3，lo=19，hi=50，digest=0x91eee432（按共享 u32 环绕递推计算）。校验完整 C、保留的 A/B 字节及 Padding。

使用独立主机参考按行列定义计算每组预期，不复用待测 HAL 算法作为唯一 oracle。至少核对 n=1、上述非对称 2×2、零矩阵或单位矩阵、合法上界 n=26、非法 n=0/27、布局长度错误、错误 task 和越界元素。使用资源可承受的代表性动态输入确认每轮真实执行，检查内存容量与时钟边界；硬件条件不足时报告实际未验证项。这些是正确性/容量验证，不固定正式性能采样点或速度门槛。

本文件为任务书中文设计参考，提交到开发仓库 exb2/matmul.md 的文档使用英文，保留研究引用、公式和独立期望。
