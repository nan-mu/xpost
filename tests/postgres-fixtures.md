# 四模式 PostgreSQL 读回夹具

XPOST #23 执行端在 `b5a44072ed57657c7482758412cccf1fd93df6fa` 使用临时 PostgreSQL，应用 [protocol_v1.sql](https://github.com/nan-mu/xpost/blob/b5a44072ed57657c7482758412cccf1fd93df6fa/xpost-test/sql/protocol_v1.sql)，为每种模式写一份 EXB2 夹具，使用 [medium_run.sql](https://github.com/nan-mu/xpost/blob/b5a44072ed57657c7482758412cccf1fd93df6fa/xpost-test/sql/medium_run.sql) 按 run_id 查询。四模式均查回身份、boot 和 epoch，合成计时为 50 ticks/round。还能查回 blocked 网络与 case 夹具。重复应用 DDL 拒绝已存在的对象。原始报告见 [#23](https://github.com/nan-mu/xpost/pull/23)。

本轮核对 `9e73d84a6ac248c90c6b1f55cab33335c27cecf5` 时，两个文件的 blob 与 `b5a44072` 相同。固定证据的 `protocol_v1.sql` blob 为 `da87a629ca0e92224a8cfe845e01ab2dab86899c`，`medium_run.sql` blob 为 `ab203fe3e19dd8be1bee7229617d99b997772d44`，两者与此次远端检查对应文件一致。本轮核对的是版本与证据来源，实际 PostgreSQL 运行结果沿用上述记录。仅在 DDL、查询和夹具相关条件等价时复用。实现后验证真实 runner 能将实际观测写入、查回并与每个条件对账。写入或查询发生行为变化时按实际受影响部分验证。
