# PR #42 — EXB2 HAL 原生固件重构

目标仓库：nan-mu/xpost。基于 PR41 合入后的 main（已核实 bb6b906f0b39abe440107c3831da88d825bd325c），新 worktree，PR base 为 main，预估 PR42。共享决策基线：mcp/xpost main 87b4a88201e6f155f2b8d5d350a345157962f2ee。本版供维护者审核后交给 T3 的 Codex 执行；不自动启动实施或合并。

## Goal

HAL 代表同一任务的合理最佳原生 MCU 固件实现。重构原七项，并新增矩阵乘法 matmul，使八项 workload 各自独立编译、部署、执行、验证和记录。提供一个 Cargo 命令生成全部八份 Firmware ELF。以 [HAL EXB2 共享设计](references/hal-exb2-design.md) 及其链接的八份 workload 文档作为唯一技术契约和验收依据。

## 工作

- HAL-01（Matt implement、tdd）：按设计第 1–3、6 节删除原 C 编译和运行时算法分发路径；实现八项原生 Rust workload，每项一个编译 Feature。保留 exb2/ 目录及同名 Markdown/Rust 布局。新增 [matmul](references/exb2/matmul.md) 必须实际执行矩阵乘法；评估 9/4 研究的实现可行性，不把理论复杂度当作固件结果。
- HAL-02（Matt implement、tdd）：按设计第 4、6 节实现聚合 Cargo 构建及各 workload 独立产物，核对单项重建不会覆盖其他产物。实际命令和产物路径记录在 README。
- HAL-03（Matt implement、tdd）：按设计第 5、6 节接通 Agent profile、runner standalone 和 medium/all 的计划、预检查、部署、resume、结果验证及入库。只为新增 matmul 修改必要的 HAL 持久化契约，在隔离数据库验证。

## 文档语言

任务书和 Gitea Reference 使用中文，供维护者审核。所有新增或修改后提交至实际开发仓库的 Markdown、README、设计文档和审计报告均用英文。将本任务书参考转化为英文文档，保留原有技术语义、公式、源码和研究引用、正确性期望。PR 正文用英文。不要求翻译无关历史文件。

## 边界

- 既有超时诊断与镜像来源调查尚未完成，保留于 [原任务书历史提交](https://gitea.nas.nan-mu.cc/mcp/xpost/src/commit/101b6fc7e56ffc587d71a84f201d38bb9cc47648/taskbook.md)，不作为本轮验收前提；不将本轮 HAL 成功当作旧超时根因已解决。历史 baseline 附件已从当前交付移除。
- PR41 使用 NAS experiments.release，TR3000 从 /root/.db 读取配置。保留历史数据；不得删除或覆盖 NAS 现有表数据。诊断、资格验证使用独立身份。仅在隔离 fixture 核对新 matmul 持久化兼容性；无损生产入库受阻时报告，不擅自清库。
- libbpf-mcu PR7 已合并。matmul 的 AOT/JIT/rBPF 产物与新 Release 消费需后续 libbpf-mcu/XPost 工作。本轮实现 HAL matmul 的真实正常入口，非 HAL 保留原七项，不生成假 XDP 产物或宣布四模式对照完成。
- 不关闭 Issue #40，不展开其他实验，不启动全量正式 campaign；通信可靠性改版、统一计时及正式采样参数仍为后续设计。

## 独立模型验证要求

主代理把固定版本 [HAL EXB2 共享设计](references/hal-exb2-design.md) 连同其中链接的八份 workload 文档、最终开发 PR head、实际构建产物和原始验收证据交给已配置的独立 Reviewer/Tester。独立模型按共享设计第 6 节的 HAL-01、HAL-02、HAL-03 技术判据及各单项文档的独立期望直接验证真实调用链和行为。任务书不再重复定义技术判据；遇到不一致时，以设计文档为技术依据，以本文的范围、权限和交付边界为准。不得仅复述实现者结论。

逐项给出通过、失败、阻塞或不适用，附模型/角色、受审版本、入口、产物摘要、测试与证据。缺证据不能判通过；独立模型不得参与所审实现。主代理处理发现后核对最终 head，记录未完成项。缺独立验证或实机能力时明确报告实际阻塞，不冒充通过。

## 交付

使用 Matt code-review 和 pr。交付开放 PR，英文正文，等待维护者审核，无合并授权。记录各项实际验收与独立结论；未完成的工作不标为完成。
