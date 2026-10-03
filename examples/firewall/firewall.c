// Firewall XDP — hash-map blacklist with per-key hit counts.
//
// Canonical MCU build source: `libbpf-mcu/tests/xdp/firewall.c` (keep in sync).
// Packed into Compact ELF V0 (`firewall.elf`) for cold install.
//   IPv4 src in `blacklist` → count++ then XDP_DROP;
//   miss / non-IPv4 / ctrl UDP:8081 → PASS.

#include <linux/bpf.h>
#include <linux/if_ether.h>
#include <linux/ip.h>
#include <linux/udp.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_endian.h>

#define CTRL_PORT 8081

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 256);
	__type(key, __u32);
	__type(value, __u64);
} blacklist SEC(".maps");

SEC("xdp")
int firewall(struct xdp_md *ctx)
{
	void *data_end = (void *)(long)ctx->data_end;
	void *data = (void *)(long)ctx->data;
	struct ethhdr *eth = data;
	struct iphdr *ip;
	__u64 *count;

	if ((void *)(eth + 1) > data_end)
		return XDP_PASS;
	if (eth->h_proto != bpf_htons(ETH_P_IP))
		return XDP_PASS;

	ip = (void *)(eth + 1);
	if ((void *)(ip + 1) > data_end)
		return XDP_PASS;

	/* Let MapCrud control plane (UDP dport 8081) reach the netstack. */
	if (ip->protocol == IPPROTO_UDP) {
		struct udphdr *udp = (void *)ip + ip->ihl * 4;
		if ((void *)(udp + 1) <= data_end &&
		    udp->dest == bpf_htons(CTRL_PORT))
			return XDP_PASS;
	}

	count = bpf_map_lookup_elem(&blacklist, &ip->saddr);
	if (count) {
		(*count) += 1;
		return XDP_DROP;
	}
	return XDP_PASS;
}

char LICENSE[] SEC("license") = "GPL";
