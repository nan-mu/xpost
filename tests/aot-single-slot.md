# 三个 LLVM AOT 样例的主机与 QEMU 验证

任务书编写端在 Linux 验证。使用 libbpf-mcu v0.1.0 发布包内 `just_drop`、`firewall`、`just_tx` 的 C 源快照，Clang/LLD 19.1.7、QEMU 10.0.13。本次记录来自原先运行的 compare.py 和 results.txt。输入是本地生成、保留重定位记录的零址 ET_EXEC，并以关闭 linker relaxation 的同布局直接链接为字节对照。目标槽 0x40380200，helper 0x40380000。QEMU helper 为检查 map 地址及 helper id 的测试替身。

`compare.py` 从零址链接 ELF 取镜像，按保留的重定位改写并封装。三项所得 payload 与同布局、关闭 relaxation 的直接链接一致。三项改写镜像均在 QEMU 以退出 0 返回预期动作和数据。未改写直接执行时，firewall 与 just_tx 退出 -11，just_drop 无需改写仍通过。默认 relaxation 的直接链接对 firewall / just_tx 产生了不同字节长度，分别为 104 对 108、408 对 416。直接构建对照须固定相同布局选择。原始材料见 [单槽实验包](https://chatgpt.com/library?file_id=libfile_fe71f6f2c4b8819184cf80a638a97113&account_id=personal)。

2026-11-25 用 `cmp` 对照本地原型使用的三个 C 源快照和 [v0.1.1 正式包内源码](release-v0.1.1.md)，三份逐字节一致。以下是匹配的 SHA-256。

| 程序 | 源码 SHA-256 |
| --- | --- |
| just_drop | `670a4c20d4c21238ff3b782194a04d18d4d2a17ce0bf60e060cd7103e7296b80` |
| firewall | `f07a0f6a51914db826ce98904ff0803d3fafd2cf2816ca1bcf74a767aba33e90` |
| just_tx | `2a1e9a031315646b6e28e997a27f5c3d41e1e20ca091bb9afd4a2c19487b46cf` |

原 QEMU 执行记录仍是带重定位的单槽原型验证，源码一致性仅连接源文件身份。复用该方法证据，跳过相同原型复跑。正式 v0.1.1 Compact ELF 的可复用地址和重定位节检查见发布记录。Agent 新路径、JIT、真机和实际 A/B 切换按照正文验证。
