/* Høiland-Jørgensen et al., "The eXpress Data Path", Listing 1 (Ch.4).
 * Listing body kept as in the paper; includes + parse_ipv6 complete the build.
 *
 * Vendored AOT: examples/just_xdp/just_tx.elf
 * Map: ARRAY `rxcnt`, key = IP protocol (UDP=17), value = pkt count.
 * UDP → swap MAC + XDP_TX; else XDP_DROP.
 */
#include <bpf/bpf_endian.h>
#include <bpf/bpf_helpers.h>
#include "vmlinux.h"

#define ETH_P_IP 0x0800
#define ETH_P_IPV6 0x86DD
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

/* map used to count packets; key is IP protocol, value is pkt count.
 * First-case ESP32-C3 deployment uses ARRAY (single core). PERCPU_ARRAY
 * remains a multicore extension and is not part of the first H′ instance. */
struct bpf_map_def SEC("maps") rxcnt = {
	.type = BPF_MAP_TYPE_ARRAY,
	.key_size = sizeof(u32),
	.value_size = sizeof(long),
	.max_entries = 256,
};

/* swaps MAC addresses using direct packet data access */
static void swap_src_dst_mac(void *data)
{
	unsigned short *p = data;
	unsigned short dst[3];

	dst[0] = p[0];
	dst[1] = p[1];
	dst[2] = p[2];
	p[0] = p[3];
	p[1] = p[4];
	p[2] = p[5];
	p[3] = dst[0];
	p[4] = dst[1];
	p[5] = dst[2];
}

static int parse_ipv4(void *data, u64 nh_off, void *data_end)
{
	struct iphdr *iph = data + nh_off;

	if ((void *)(iph + 1) > data_end)
		return 0;
	return iph->protocol;
}

/* Listed in paper Listing 1 but definition omitted from the PDF excerpt. */
static int parse_ipv6(void *data, u64 nh_off, void *data_end)
{
	__u8 *ip6h = data + nh_off;

	if ((void *)(ip6h + 40) > data_end)
		return 0;
	return ip6h[6];
}

SEC("xdp1") /* marks main eBPF program entry point */
int xdp_prog1(struct xdp_md *ctx)
{
	void *data_end = (void *)(long)ctx->data_end;
	void *data = (void *)(long)ctx->data;
	struct ethhdr *eth = data;
	int rc = XDP_DROP;
	long *value;
	u16 h_proto;
	u64 nh_off;
	u32 ipproto;

	nh_off = sizeof(*eth);
	if (data + nh_off > data_end)
		return rc;

	h_proto = eth->h_proto;

	/* check VLAN tag; could be repeated to support double-tagged VLAN */
	if (h_proto == bpf_htons(ETH_P_8021Q) || h_proto == bpf_htons(ETH_P_8021AD)) {
		struct vlan_hdr *vhdr;

		vhdr = data + nh_off;
		nh_off += sizeof(struct vlan_hdr);
		if (data + nh_off > data_end)
			return rc;
		h_proto = vhdr->h_vlan_encapsulated_proto;
	}

	if (h_proto == bpf_htons(ETH_P_IP))
		ipproto = parse_ipv4(data, nh_off, data_end);
	else if (h_proto == bpf_htons(ETH_P_IPV6))
		ipproto = parse_ipv6(data, nh_off, data_end);
	else
		ipproto = 0;

	/* lookup map element for ip protocol, used for packet counter */
	value = bpf_map_lookup_elem(&rxcnt, &ipproto);
	if (value)
		*value += 1;

	/* swap MAC addrs for UDP packets, transmit out this interface */
	if (ipproto == IPPROTO_UDP) {
		swap_src_dst_mac(data);
		rc = XDP_TX;
	}
	return rc;
}

char LICENSE[] SEC("license") = "GPL";
