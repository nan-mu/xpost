# 任务书 · PR41 数据库校验收尾

开发仓库：nan-mu/xpost；继续 [PR #41](https://github.com/nan-mu/xpost/pull/41) 的 `agent/runner-db-runtime-config`，base 为 main。审查及当前已核对 head：`9754bcc384b7fd32094609a0dd14004a75ff65c0`。沿用独立 worktree，交付同一开放 PR，不合并。

任务书仓库：mcp/xpost，pr41；上一版任务书提交：`783824def4cfb283042b5bf57da0aae92c8651fb`；共享 main：`a98e1bb60d2625d25c7d317708d87014800933dd`，适用决定 runner-db-runtime-config、runner-schema-reconcile。

## Goal

完成 PR41 的数据库校验收尾。用户要求不要过于严格：以实验数据正常写入、基本完整性和可恢复性为目标，不构建通用 schema 迁移器或要求数据库与 DDL 的所有细节完全相同。

使用 Matt diagnosing-bugs、implement 和 tdd，先确认以下静态审查发现，再修复确实影响实验的缺口：

- [protocol_v1.rs](https://github.com/nan-mu/xpost/blob/9754bcc384b7fd32094609a0dd14004a75ff65c0/xpost-test/common/src/db/protocol_v1.rs)：新采集 prepare/reconcile 会检查规范对象，但只读 check/validate 只比较列、权限及部分全局对象。恢复采集可能接受缺少关键约束的表。例如 bench_artifact 的字节长度 CHECK 被删除时列定义不变，现有 validate 不会发现。此为静态推导，尚未实测。
- 同文件 reconcile 对保留表检查规范对象，但不拒绝额外对象。额外触发器可能修改实验记录；普通额外非唯一索引不影响数据语义，不应拒绝。

补足现有只读检查，使其能发现影响实际写入和数据关联的关键结构缺口，例如必要主键、外键及关键 CHECK。对额外对象仅处理具有明显实验数据风险的情形，例如会改变写入内容的非预期触发器；允许普通辅助索引及无害差异。无需证明任意 SQL 对象的语义等价，也不要求全量 catalog 对象匹配。不能安全处理时给出具体表和原因；不要自动删除额外对象。具体校验实现由执行端选择，优先复用现有定义及 catalog 查询，避免引入复杂的新解析器或第二套完整 DDL。

## 保留的约定

- /root/.db 在运行时读取 PGHOST、PGPORT、PGUSER、PGPASSWORD、PGDATABASE，root 所有且 0600，不泄露凭据；连接目标由配置指定，不硬编码库名。NAS postgres 历史库不操作。
- 唯一结构来源为 xpost-test/sql/protocol_v1.sql，使用 release，当前 132 张表（bench 12，各模式 hal/aot/jit/rbpf 30）。新采集补缺表，字段兼容时保留原表和数据，字段不兼容时允许仅重建目标表并丢弃该表原数据；依赖无法安全恢复时事务回滚。不得无界 CASCADE，不清空其他表，不修改非托管对象，不引入版本迁移链。
- 新采集 all、medium、standalone exec_bench 使用统一准备入口；db-preflight、all/medium-preflight、resume 保持只读，reconnect 不执行 DDL，plan/dev 不访问数据库。保留 schema 共享/排他保护、campaign 独占、guard 断开后停止受保护工作和重连保护。
- 保留已完成的旧入口清理、公共 Artifact 写入及其事务边界，专用 writer 维持原有语义。不要扩大到固件、AOT/JIT 重定位、其他实验重构或部署权限调整。

## 验收

使用现有隔离 PostgreSQL fixture 和正常 prepare/check/resume 入口，选择少量有区分力的用例：

1. 正常 schema 可准备和恢复；已有兼容数据仍可读写。增加一个普通非唯一索引不应导致拒绝。
2. 保持字段不变、移除一个对真实数据完整性重要的约束（例如上述 Artifact CHECK 或关键外键），只读检查与 resume 不得误报就绪；检查前后结构和数据不变。若新采集不能安全补齐该约束，明确拒绝即可。
3. 在托管表添加会改变记录内容的非预期写入触发器，验证采集入口不会在该风险未处理时继续写入；不得为通过检查自动删除该对象。
4. 对本轮受影响的数据库 fixture 和常规构建/测试执行必要复验。复用[上一轮审计](https://github.com/nan-mu/xpost/blob/9754bcc384b7fd32094609a0dd14004a75ff65c0/.audit/taskbook/mcp/xpost/783824def4cfb283042b5bf57da0aae92c8651fb.md)中条件仍等价的证据：132 表、字段重建、依赖回滚、锁、四模式 EXB2、Case、网络 writer、RO ACL；[NAS 权限观察](tests/nas-release-acl.md)仅作为权限基线。上述为已有执行报告而非本轮作者实测。仅在改动影响相应结论时复验，并注明复用范围。

无需重新跑硬件实验、连接真实 NAS 或变更真实数据。禁止读取输出真实凭据、修改真实数据库权限及固件。不要为纯命名差异、无害辅助索引或不涉及实验写入的扩展对象增加验收门槛。

## 独立模型验证与交付

让独立 Reviewer 使用 code-review，读取本任务书、上一版任务书（上述固定提交）、最终 PR head、规范 DDL、实际调用链及本轮测试证据，独立判断：正常入口是否保留原有行为；字段不变但关键约束缺失能否被发现；危险额外触发器是否可绕过；无害索引是否被误拒；只读路径是否保持无 DDL；是否无关扩展了修改范围。对原有目标引用仍适用的证据，不重复整套硬件/数据库验证。逐项给出通过、失败、阻塞或不适用及具体代码位置/测试证据，记录模型、角色和审查 SHA。缺独立验证能力则如实报告，不以自审冒充；可保持开放待审 PR，不标记完整验收通过。

按 task-brief-to-pr 继续当前 PR，使用 Matt pr 更新英文摘要、实际验收与未解决问题；若 head 变化先核对并保留他人修改。通过指定 GitHub MCP 交付同一开放 PR，禁止自动合并，等待用户审核。
