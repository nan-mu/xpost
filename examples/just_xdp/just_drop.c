/* just_drop — count then drop; companion to just_tx (paper Listing 1 shape).
 *
 * Vendored AOT: examples/just_xdp/just_drop.elf
 * Matches just_drop_hal + xpost-test formal (`get map counter`):
 *   ARRAY `counter`, key = IPv4 src last octet (0..255), value = pkt count.
 *   Always XDP_DROP (no TX / no MAC swap). Non-IPv4: drop without counting.
 */
#include <bpf/bpf_endian.h>
#include <bpf/bpf_helpers.h>
#include "vmlinux.h"

#define ETH_P_IP 0x0800
#define ETH_P_8021Q 0x8100
#define ETH_P_8021AD 0x88A8

/* BPF vmlinux.h already has these. MCU vmlinux.h comments them out. */
#ifndef __bpf__
typedef __u64 u64;
struct vlan_hdr {
	__be16 h_vlan_TCI;
	__be16 h_vlan_encapsulated_proto;
};
#endif

struct bpf_map_def {
	unsigned int type;
	unsigned int key_size;
	unsigned int value_size;
	unsigned int max_entries;
	unsigned int map_flags;
};

struct bpf_map_def SEC("maps") counter = {
	.type = BPF_MAP_TYPE_ARRAY,
	.key_size = sizeof(u32),
	.value_size = sizeof(long),
	.max_entries = 256,
};

/* IPv4 src last octet, or -1 if truncated / not enough bytes. */
static int ipv4_src_last_octet(void *data, u64 nh_off, void *data_end)
{
	struct iphdr *iph = data + nh_off;
	__u8 *octets;

	if ((void *)(iph + 1) > data_end)
		return -1;
	octets = (__u8 *)&iph->saddr;
	return octets[3];
}

SEC("xdp1")
int xdp_prog1(struct xdp_md *ctx)
{
	void *data_end = (void *)(long)ctx->data_end;
	void *data = (void *)(long)ctx->data;
	struct ethhdr *eth = data;
	long *value;
	u16 h_proto;
	u64 nh_off;
	int last;
	u32 key;

	nh_off = sizeof(*eth);
	if (data + nh_off > data_end)
		return XDP_DROP;

	h_proto = eth->h_proto;

	if (h_proto == bpf_htons(ETH_P_8021Q) || h_proto == bpf_htons(ETH_P_8021AD)) {
		struct vlan_hdr *vhdr;

		vhdr = data + nh_off;
		nh_off += sizeof(struct vlan_hdr);
		if (data + nh_off > data_end)
			return XDP_DROP;
		h_proto = vhdr->h_vlan_encapsulated_proto;
	}

	if (h_proto == bpf_htons(ETH_P_IP)) {
		last = ipv4_src_last_octet(data, nh_off, data_end);
		if (last >= 0) {
			key = (__u32)last;
			value = bpf_map_lookup_elem(&counter, &key);
			if (value)
				*value += 1;
		}
	}

	return XDP_DROP;
}

char LICENSE[] SEC("license") = "GPL";
