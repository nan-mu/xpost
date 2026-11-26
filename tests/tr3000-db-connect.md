# TR3000 使用 `/root/.db` 连接 NAS PostgreSQL

## 验证对象与条件

维护者于 2026-11-27（UTC+8）在 TR3000 / ImmortalWrt 24.10.6 上执行独立客户端测试。设备使用 Fish、opkg；维护者手动配置 `/root/.db`，安装 PostgreSQL 客户端。测试对象是实际 NAS PostgreSQL 的 `experiments` 数据库，不是任何 XPOST runner 提交。实际 psql 版本未报告。

## 操作与观察

此前给出的测试入口如下（显式用 POSIX sh，避免 Fish 括号语法错误）：

```sh
sh -c 'set -a; . /root/.db && PGCONNECT_TIMEOUT=10 psql -X -w -v ON_ERROR_STOP=1 -c "SELECT current_database(), current_user, inet_server_addr(), inet_server_port(), now();"'
```

维护者回传的实际查询结果（终端折行已恢复）：

```text
current_database | current_user  | inet_server_addr | inet_server_port | now
experiments      | experiment_rw | 172.16.1.3       | 5432             | 2026-11-26 17:38:32.179938+00
(1 row)
```

维护者没有回传最终执行的完整命令行或退出码，但结果与所提供的查询匹配。此前 `opkg install psql` 返回 Unknown package；切换镜像源后客户端已能执行查询。没有提供或记录密码。

## 结论与复用边界

通过：TR3000 到 NAS 的数据库连接及 SQL 查询，连接目标和登录角色与任务约定一致。复用这一用户实测，不需要重新安装软件或再次采集同范围基线。

未验证：文件的实际 0600 权限、真实认证机制（密码是否为服务端唯一接受方式）、XPOST 的运行时 dotenv 解析、Protocol V1 初始化、写入权限、四模式实验执行。上述事项不可据此标记通过。网络、账号、目标数据库等相关条件发生变化时，报告本记录的适用性变化。