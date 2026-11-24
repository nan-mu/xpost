# EXB2 七项主机向量 · v0.1.1

2026-11-25 在 Linux 从 [libbpf-mcu v0.1.1 正式发布包](https://github.com/nan-mu/libbpf-mcu/releases/tag/v0.1.1) 取七个程序的 C 源快照。tag 对应 `07a4ea9a81a7fedf7bef2d6ced8d7fcfcccfd113`。12 个归档均以当前 GitHub 公布的 asset SHA-256 验证过。对七份源码另按归档中 v3 `index.toml` 核对路径及 SHA-256。归档摘要见 [发布包检查](release-v0.1.1.md)。

使用 GCC 14.2.0 以 `cc -std=gnu11 -O2 -fPIC -shared -DEB_HOST_ENTRY <程序>.c -o <程序>.so` 编译，在主机以 `exec_entry(void *, uint64_t)` 各调用一次。输入与独立预期取自 [XPOST #23 固定向量](https://github.com/nan-mu/xpost/blob/b5a44072ed57657c7482758412cccf1fd93df6fa/xpost-test/procedures/medium.md#exb2-qualification-vectors)。采用 8258 B 缓冲、64 B 小端头、rounds 3、variant 0、seed 1、初始 status `0xffffffff`，scratch 填 `a5`，尾部 padding 填 `cc`。按向量构造预期响应后比较完整缓冲。

实测 **7/7**。每项函数返回 0，缓冲中 status 为 0、completed 为 3，完整 8258 B 与预期一致。输入及 padding 保持原值，copy 和 bsort 的全部 scratch 匹配。

| 程序 | result low（hex） | result high（hex） | digest（hex） |
| --- | --- | --- | --- |
| noop | 00000000 | 00000000 | 91efd7d5 |
| prime | 00000008 | 00000013 | 91efb91a |
| log2 | 00000009 | 0000048a | 91fe55a4 |
| copy | 00000004 | 00000000 | 91efe859 |
| fletcher32 | 04050204 | 00000000 | 199d264a |
| bsort | 00000001 | 00000008 | 91efbaee |
| strcmp | ffffffff | 00000003 | 664ce84e |

复用本结果作为 v0.1.1 发布源码的主机 ABI 验收。后续仍用这些向量检查 Agent、发布容器在板上的四模式行为及写库。该记录的执行对象是主机 direct entry。
