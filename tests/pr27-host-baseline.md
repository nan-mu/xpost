# PR27 首轮 Host 验收证据

来源：[PR #27 head 179ce44d8fe3227b2b80d1b4ddfb50f1e3f13b1a 的验收记录](https://github.com/nan-mu/xpost/blob/179ce44d8fe3227b2b80d1b4ddfb50f1e3f13b1a/.audit/taskbook/mcp/xpost/92e8a9b834c998841b6d378a9c7c5ece091412c4.md)。以下是该 PR 实施者的实测报告，任务书作者未重新运行。被测为 PR27 首轮实现，base main@9cb03b795c8d45c5d107cf8e63b90dca6f1dcb56；实际构建/fixture 的进一步身份见原报告。

| 已运行入口 | 报告结果 | 本轮复用边界 |
| --- | --- | --- |
| cargo build --offline --locked --manifest-path xpost-agent/Cargo.toml --target aarch64-apple-darwin --release | 正式二进制构建成功 | 删除生命周期和依赖后重建 |
| cargo test --offline --locked --manifest-path xpost-agent/Cargo.toml --target aarch64-apple-darwin | 11 passed | 重跑受影响的 Agent tests |
| cargo test --offline --locked --features dev,client,server（xpost-serial） | 71 passed，1 ignored | 未受改动影响的串口测试结果可引用；涉及 Board Info、清除时补测相应边界 |
| cargo +nightly test --offline --locked --target aarch64-unknown-linux-musl -p common | 10 existing + 1 TR3000 client fixture passed；Docker musl、TEST_SKIP_EBPF=1，仅主机测试 | HTTP 契约不变的部分可引用；对本轮改变的 busy/idle、health/错误及实际消费入口补测 |
| cargo metadata --locked --offline --no-deps / cargo tree --offline --locked | 单 workspace member 与 bin；无 rmcp、rmcp-macros、schemars | 检查删除后的 workspace 与依赖 |
| python3 xpost-agent/tests/cli_http.py <release-agent> | 26 个固定方法/路径/参数/响应样例通过；业务/HTTP/解码/redirect/连接错误非零退出 | 保留独立预期，补测 clear 和状态/切换失败，按新启动入口验证 |
| python3 xpost-agent/tests/lifecycle.py <fixture-a> <fixture-b> | 模拟 DUT，实际进程及 HTTP；不同 Worktree 锁竞争、HTTP 408 后仍 busy、退出/锁释放通过；前台 SIGINT 成功 | 只复用仍有效的前台、独占与实际操作保护方法。删除后台 start/stop/restart、双映像和 stale PID 等旧要求，改为从 serve 实测 |

原报告明确未进行真实 USB、flash、DB 或正式实验。Board Info 启动检查是本轮新增真机验收。旧测试替身不能证明物理 USB 可访问，也不能证明 PR23 的 Release/Perf/EXB2 业务可运行。
