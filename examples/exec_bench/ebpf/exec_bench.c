/*
 * exec_bench — XDP-ABI compute dispatcher for XPOST function-call benchmarks.
 *
 * Buffer layout (HAL twin must match):
 *   [0..4]  opcode  (LE u32)
 *   [4..8]  param   (LE u32; meaning depends on opcode)
 *   [8..]   payload / scratch
 *
 * Rebuild (host with clang + lld + xpost-compact-elf):
 *   ./examples/exec_bench/ebpf/build.sh
 *
 * Entry is the first instruction of the packed image (slot 0x40380200).
 */

typedef unsigned char u8;
typedef unsigned int u32;
typedef unsigned short u16;

#ifndef NULL
#define NULL ((void *)0)
#endif

enum {
	XDP_ABORTED = 0,
	XDP_DROP = 1,
	XDP_PASS = 2,
};

enum {
	OP_INCR = 0,
	OP_SQUARE = 1,
	OP_BITSWAP = 2,
	OP_FIB = 3,
	OP_BUF = 4,
	OP_MEMCPY = 5,
	OP_FLETCHER32 = 6,
	OP_BSORT = 7,
};

struct xdp_md {
	void *data;
	void *data_end;
	u32 data_meta;
	u32 ingress_ifindex;
	u32 rx_queue_index;
	u32 egress_ifindex;
};

static __attribute__((always_inline)) u32 load_u32(const u8 *p)
{
	return (u32)p[0] | ((u32)p[1] << 8) | ((u32)p[2] << 16) | ((u32)p[3] << 24);
}

static __attribute__((always_inline)) void store_u32(u8 *p, u32 v)
{
	p[0] = (u8)(v);
	p[1] = (u8)(v >> 8);
	p[2] = (u8)(v >> 16);
	p[3] = (u8)(v >> 24);
}

static __attribute__((always_inline)) u32 bitswap32(u32 x)
{
	x = ((x & 0x55555555u) << 1) | ((x >> 1) & 0x55555555u);
	x = ((x & 0x33333333u) << 2) | ((x >> 2) & 0x33333333u);
	x = ((x & 0x0f0f0f0fu) << 4) | ((x >> 4) & 0x0f0f0f0fu);
	x = ((x & 0x00ff00ffu) << 8) | ((x >> 8) & 0x00ff00ffu);
	return (x << 16) | (x >> 16);
}

static __attribute__((always_inline)) u32 fib_u32(u32 n)
{
	u32 a = 0, b = 1, i;
	if (n == 0)
		return 0;
	for (i = 1; i < n; i++) {
		u32 c = a + b;
		a = b;
		b = c;
	}
	return b;
}

static __attribute__((always_inline)) u32 fletcher32(const u8 *data, u32 len)
{
	u32 sum1 = 0xffffu, sum2 = 0xffffu;
	u32 i = 0;
	while (len > 1) {
		u32 words = len / 2;
		if (words > 360)
			words = 360;
		len -= words * 2;
		while (words--) {
			u16 w = (u16)data[i] | ((u16)data[i + 1] << 8);
			i += 2;
			sum1 += w;
			sum2 += sum1;
		}
		sum1 = (sum1 & 0xffffu) + (sum1 >> 16);
		sum2 = (sum2 & 0xffffu) + (sum2 >> 16);
	}
	if (len) {
		sum1 += data[i];
		sum2 += sum1;
		sum1 = (sum1 & 0xffffu) + (sum1 >> 16);
		sum2 = (sum2 & 0xffffu) + (sum2 >> 16);
	}
	sum1 = (sum1 & 0xffffu) + (sum1 >> 16);
	sum2 = (sum2 & 0xffffu) + (sum2 >> 16);
	return (sum2 << 16) | sum1;
}

/* SEC("xdp") equivalent — must be first in the packed image. */
__attribute__((section(".text.xdp"), used))
int xdp_exec_bench(struct xdp_md *ctx)
{
	u8 *data = (u8 *)ctx->data;
	u8 *end = (u8 *)ctx->data_end;
	u32 opcode, param, avail;
	u8 *payload;

	if (data + 8 > end)
		return XDP_ABORTED;

	opcode = load_u32(data);
	param = load_u32(data + 4);
	payload = data + 8;
	avail = (u32)(end - payload);

	switch (opcode) {
	case OP_INCR: {
		u32 x;
		if (avail < 4)
			return XDP_ABORTED;
		x = load_u32(payload);
		store_u32(payload, x + 1);
		return XDP_PASS;
	}
	case OP_SQUARE: {
		u32 x;
		if (avail < 4)
			return XDP_ABORTED;
		x = load_u32(payload);
		store_u32(payload, x * x);
		return XDP_PASS;
	}
	case OP_BITSWAP: {
		u32 x;
		if (avail < 4)
			return XDP_ABORTED;
		x = load_u32(payload);
		store_u32(payload, bitswap32(x));
		return XDP_PASS;
	}
	case OP_FIB: {
		if (avail < 4)
			return XDP_ABORTED;
		if (param > 47)
			param = 47;
		store_u32(payload, fib_u32(param));
		return XDP_PASS;
	}
	case OP_BUF: {
		u32 i, acc = 0;
		if (param > avail)
			return XDP_ABORTED;
		for (i = 0; i < param; i++) {
			payload[i] = (u8)(payload[i] + (u8)i);
			acc += payload[i];
		}
		if (avail >= param + 4)
			store_u32(payload + param, acc);
		return XDP_PASS;
	}
	case OP_MEMCPY: {
		u32 i;
		if (param == 0 || param > avail / 2)
			return XDP_ABORTED;
		for (i = 0; i < param; i++)
			payload[param + i] = payload[i];
		return XDP_PASS;
	}
	case OP_FLETCHER32: {
		u32 sum;
		if (param == 0 || param > avail || avail < param + 4)
			return XDP_ABORTED;
		sum = fletcher32(payload, param);
		store_u32(payload + param, sum);
		return XDP_PASS;
	}
	case OP_BSORT: {
		u32 n = param;
		u32 i, j;
		if (n < 2 || n > 64 || avail < n * 4)
			return XDP_ABORTED;
		for (i = 0; i < n - 1; i++) {
			for (j = 0; j < n - 1 - i; j++) {
				u32 a = load_u32(payload + j * 4);
				u32 b = load_u32(payload + (j + 1) * 4);
				if (a > b) {
					store_u32(payload + j * 4, b);
					store_u32(payload + (j + 1) * 4, a);
				}
			}
		}
		return XDP_PASS;
	}
	default:
		return XDP_ABORTED;
	}
}
