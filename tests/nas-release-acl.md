# NAS release 权限观察

任务书作者通过 Magpie DBHub 的只读 SQL 查询真实 PostgreSQL 18.3，连接身份 experiment_ro，数据库 experiments。观察来自 PR41 审核会话，未执行写入。

查询 pg_namespace 中 dev、public、release 的所有者以及 has_schema_privilege('experiment_rw',oid,'USAGE'/'CREATE')，结果：dev 和 release owner 为 experiment_rw，USAGE=true、CREATE=true；public 两项均 false。experiment_ro 在三个 schema 的 CREATE 均 false。此前查询确认 release 有 USAGE、当时三者表数均为 0。

查询 pg_default_acl（defaclrole=experiment_rw）返回 release 的表默认 ACL {experiment_ro=r/experiment_rw,backup_release=r/experiment_rw}，序列默认 ACL {backup_release=r/experiment_rw}。证明当前 rw 可在 release 建表，且由该角色创建的表默认向上述角色授予 SELECT；不证明未来表实际写入、结构同步、外键恢复或其他对象权限。只复用权限基线，不在真实 NAS 重跑破坏性验收。