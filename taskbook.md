# PR #42 — EXB2 HAL 原生固件重构：合并前修订

目标仓库：[nan-mu/xpost PR #42](https://github.com/nan-mu/xpost/pull/42)，在现有 head 分支 `agent/hal-exb2-native-firmware` 的独立 worktree 继续，PR base 为 main，不新开 PR。审核依据：开发仓库 head [ef4bd520e2d1910000570579a5ac4bd351866ddb](https://github.com/nan-mu/xpost/tree/ef4bd520e2d1910000570579a5ac4bd351866ddb)。原始技术契约：[任务书 ac4f2937](https://gitea.nas.nan-mu.cc/mcp/xpost/src/commit/ac4f2937e8b71cbb855e24b03389fedec51386f7/taskbook.md)。共享约定以 [main 87b4a882](https://gitea.nas.nan-mu.cc/mcp/xpost/src/commit/87b4a88201e6f155f2b8d5d350a345157962f2ee/decisions.md) 中 EXB2 HAL、文档语言及数据库相关决定为准。

## Goal

保留八项独立、原生 Rust HAL 固件及已有 EXB2 行为，改为正常的 Rust 固件入口、模块声明和函数调用。完成修订后由 Codex 直接执行 HAL EXB2 全量性能实验并将结果写入 NAS；不等待人工启动。具体架构约束及验收见 [HAL EXB2 共享设计](references/hal-exb2-design.md) 第 4、6 节；八份 workload 文档继续定义各任务的独立语义。

## 实验执行授权

维护者明确要求本轮仅运行 HAL 的 EXB2 全量实验。执行方式：自动执行；实现、构建、必要隔离资格验证及设备/数据库预检查通过后，直接通过正常 Runner 启动 HAL EXB2 Full 采集、读回并分析，不等待维护者启动，也不要求 PR 先合并。仅授权 HAL EXB2 Full 及必要部署；不运行独立 Medium campaign、AOT/JIT/rBPF 或其他实验，不授权破坏性数据库操作或合并 PR。

## 工作与验收

- HAL-01：用 Matt `implement`、`tdd` 修订固件源代码组织。[noop 入口](https://github.com/nan-mu/xpost/blob/ef4bd520e2d1910000570579a5ac4bd351866ddb/examples/exec_bench_hal/src/bin/noop.rs)、[matmul 入口](https://github.com/nan-mu/xpost/blob/ef4bd520e2d1910000570579a5ac4bd351866ddb/examples/exec_bench_hal/src/bin/matmul.rs) 均通过 `include!("firmware.inc.rs")` 插入相同完整固件源码；[lib.rs](https://github.com/nan-mu/xpost/blob/ef4bd520e2d1910000570579a5ac4bd351866ddb/examples/exec_bench_hal/src/lib.rs) 通过 `cfg_attr(feature, path = ...)` 将不同源码映射至同一个 workload 模块。这两种方式都须删除，按设计第 4 节使用普通入口、模块和静态调用。八项算法、协议、独立文档及正确性向量保持不变。更新受影响的英文 README。
- HAL-02：现有聚合构建和产物隔离已有证据，但本轮更改固件入口及链接可达性，须按第 6 节重新核对聚合构建、单项重建、Feature 互斥、每个 ELF 的实际 Task ID 和独立产物身份；保持现有 bin/profile 及命令契约。
- HAL-03：现有 Agent、Runner、隔离入库及计划/resume 已经验证，保留其行为。由于本轮产生新 Firmware ELF，按第 6 节从正常 standalone Runner 对八个新固件执行受影响的真机资格验证，复用独立向量并核对完整响应、DUT 计时边界、固件摘要与隔离数据库读回；保留原每项两次的验证粒度。其余未受影响的主机测试、数据库兼容性和 medium/all 计划/resume 证据可复用，除非代码或适用条件变化。既有证据见 [Acceptance Audit](https://github.com/nan-mu/xpost/blob/ef4bd520e2d1910000570579a5ac4bd351866ddb/.audit/taskbook/mcp/xpost/ac4f2937e8b71cbb855e24b03389fedec51386f7.md)，旧 ELF 的 pilot 不能作为新 ELF 已运行的证据。

- EXB2-DATA（Matt `implement`、`tdd`，遇到实际故障用 `diagnosing-bugs`）：按共享设计第 7 节执行正式 NAS 数据采集。直接使用既有 FULL 采样参数，仅采集 HAL 的八项 workload；不运行其他 mode 或额外 Medium campaign。当前 standalone 固定 MEDIUM，增加显式选择 FULL 的最小配置，复用既有 run_mode 和 Sampling::FULL，默认行为不变。交付数据、覆盖率、时间统计和异常分析；仅实现或隔离验证不能判完成。

## 边界

本轮不改变 workload 算法、EXB2 ABI、计时契约、既有输入向量或 MEDIUM/FULL 采样参数，不添加运行时 workload 分发。非 HAL 保留原七项；matmul 的 AOT/JIT/rBPF 产物及不同输入规模的实验由后续工作承接。仅运行 HAL EXB2 Full，不执行非 HAL 实验、独立 Medium campaign、其他实验或历史超时专项调查；本轮实际采集故障需定位、记录并在授权范围内复试。不关闭 Issue #40。允许在 NAS 空 release schema 中通过现有 Runner 初始化表并追加实验数据；禁止删除、覆盖或擅自修改已有表及约束。开发仓库新增或修改的文档及 PR 正文用英文。

## 独立模型验证要求

主代理将原始任务书、当前任务书及 [共享设计](references/hal-exb2-design.md) 第 4、6、7 节和其中八份 workload 文档、最终 PR head、实际构建产物与测试证据交给现有独立 Reviewer/Tester。按 HAL-01、HAL-02、HAL-03 分别核对仍有效的原始目标与本轮变化，重点独立追踪各 bin 的正常 Rust 入口、具体 workload 的静态绑定及可达的代码。仅将 `include!` 移到另一个文件、换成源码拼接宏、保留 path 别名分发或生成八个运行时选择算法的通用固件，均不通过；八个 ELF 仅文件名不同也不通过。普通模块、函数及静态泛型复用是合理实现，不要求复制公共逻辑。

独立核对新固件构建及真机证据是否对应最终产物 SHA-256，正常 Runner 的输出是否符合八份文档的独立期望、是否保持原计时及入库身份契约。对 EXB2-DATA 独立通过 NAS 只读查询核实第 7 节的 HAL 八项 workload、采样量、run ID、真实产物身份、资格结果和数据统计；隔离 fixture、旧数据、Medium 和失败批次不得充当 Full 的合格结果。复用旧证据时说明适用范围。逐项报告通过、失败、阻塞或不适用，附代码位置、被审版本、实际模型/角色、产物及实测证据。缺独立验证或真机能力时明确标记阻塞，不冒充通过；修复后复核受影响项及最终 head。

## 交付

使用 Matt `code-review` 和 `pr` 更新同一个开放 PR #42，英文正文记录实际改动、新旧产物身份、资格验证、NAS 正式采集结果、缺失/失败项及独立结论。任务书、GLOSSARY.md、decisions.md 和导入的 references 等临时交接材料不进入最终开发仓库提交；保留必要验收审计及正式项目文档。等待维护者审核，无合并授权。
