# v0.1.1 case 的报文语义与 A/B 载荷

正式发布的两份 case AOT 载荷各为 432 B。同一程序在 A、B 地址直接链接所得的载荷均与该程序的发布载荷逐字节相同。QEMU 在这两个地址分别执行每份发布载荷，四次均返回 `XDP_TX` 并输出预期报文。

## 输入与方法

使用 [libbpf-mcu v0.1.1 正式附件](release-v0.1.1.md) 中的 `case_swap.7z` 和 `case_mark.7z`。核对归档的 SHA-256，再核对 v3 索引记录的 AOT 容器 ELF 和离线对照对象文件的 SHA-256。解析 Compact ELF，两份均无 map 描述项。

在 Linux 用 LLD 19.1.7 和该版本的 [RV32IMC 槽位链接脚本](https://github.com/nan-mu/libbpf-mcu/blob/07a4ea9a81a7fedf7bef2d6ced8d7fcfcccfd113/make/linker/rv32imc-slot.ld.tmpl) 各链接一次 A、B 地址。槽位地址为 `0x40380200` 和 `0x40381000`，对照长度 `0xe00`，helper 地址 `0x40380000`。将直接链接的装载区与发布容器的 `.xpost.image` 对照。对象文件只用于离线对照。

QEMU 10.0.13 RV32 用户态使用最小 harness 调用两处地址的发布载荷。输入是该版本的 [82 字节 case 输入夹具](https://github.com/nan-mu/libbpf-mcu/blob/07a4ea9a81a7fedf7bef2d6ced8d7fcfcccfd113/tests/fixtures/network/case_mark.input.bin)。逐字节对照板端函数返回值和完整帧。B 的期望使用 [case_mark 输出夹具](https://github.com/nan-mu/libbpf-mcu/blob/07a4ea9a81a7fedf7bef2d6ced8d7fcfcccfd113/tests/fixtures/network/case_mark.expected.bin)。A 的期望在同一帧中将版本字段从 1 改为 2，依据归档的 `case_swap.c`。两份发布源码仅 `CASE_VERSION` 不同。

## 结果与复用

| 程序 | A/B 直接链接 | QEMU 返回值 | 两槽报文尾部 |
| --- | --- | --- | --- |
| `case_swap` | 两处均等于发布载荷 | 两处均为 3（`XDP_TX`） | `00 00 00 02 43 41 53 32` |
| `case_mark` | 两处均等于发布载荷 | 两处均为 3（`XDP_TX`） | `00 00 00 01 43 41 53 32` |

```text
case_swap payload sha256  91a6278c06dca003a1a5c5029de84814d01f3a913fdde7d4a278b98083d536fd
case_mark payload sha256  7d74289e391827fedef4a6d56f62412323de79ab34dd87037721c2fb7a7464f5
```

输入 UDP 载荷是 32 B JTX1 头及 8 B 零尾部。回复互换源、目的 MAC，保留 IPv4 地址和 JTX1 头，将 UDP 校验和置零，并写入大端版本号和 `CAS2`。旧目的地址和 `rxcnt` 判据来自已退役程序。

复用上述载荷摘要和报文期望。证据覆盖这两份 AOT 发布载荷在所测布局中的字节与报文。Agent 重新封装的容器、实际固件布局、板上 warmup、槽位切换、Trace 采集和数据库写入仍按 [任务书](../taskbook.md) 验收。
