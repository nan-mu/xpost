/* Compile THIS file with -DEB_TASK_ID=0..6. The selected kernel is included in
 * one translation unit, so no BPF-to-BPF calls or separate linker are needed.
 * Protocol parsing, batching and result publication live only in this file.
 */
#include "exec_protocol.h"

#ifndef EB_TASK_ID
#error "Set EB_TASK_ID: 0 noop, 1 prime, 2 log2, 3 copy, 4 fletcher32, 5 bsort, 6 strcmp"
#endif
#if EB_MAX_PRIME > 0xfffffffeu || EB_MAX_WORDS > 0x1fffffffu || \
    EB_MAX_SORT > 0x3fffffffu || EB_MAX_BYTES > 0x7ffffffeu
#error "Configured limits exceed protocol arithmetic bounds"
#endif

#if EB_TASK_ID == 0
#include "noop.h"
#elif EB_TASK_ID == 1
#include "prime.h"
#elif EB_TASK_ID == 2
#include "log2_int.h"
#elif EB_TASK_ID == 3
#include "copy.h"
#elif EB_TASK_ID == 4
#include "fletcher32.h"
#elif EB_TASK_ID == 5
#include "bsort.h"
#elif EB_TASK_ID == 6
#include "strcmp.h"
#else
#error "Unknown EB_TASK_ID"
#endif

EB_INLINE eb_u32 eb_finish(eb_u8 *mem, eb_u32 status, eb_u32 completed,
                           struct eb_result result, eb_u32 digest)
{
    eb_store32(mem + EB_O_COMPLETED, completed);
    eb_store32(mem + EB_O_RESULT_LO, result.lo);
    eb_store32(mem + EB_O_RESULT_HI, result.hi);
    eb_store32(mem + EB_O_DIGEST, digest);
    eb_store32(mem + EB_O_STATUS, status);
    return status;
}

EB_INLINE eb_u32 eb_execute(eb_u8 *mem, eb_u32 byte_length, const eb_u8 *end)
{
    struct eb_result result = {0, 0};
    eb_u32 n, rounds, variant, input_bytes, scratch_bytes;
    eb_u32 required_input = 0, required_scratch = 0, stride = 0;
    if (byte_length != EB_CONTEXT_BYTES) return EB_SHORT_BUFFER;
    /* Header-sized invalid requests get a defined, empty response. */
    eb_finish(mem, EB_NOT_RUN, 0, result, 0);
    if (eb_load32(mem + EB_O_MAGIC) != EB_MAGIC)
        return eb_finish(mem, EB_BAD_MAGIC, 0, result, 0);
    if (eb_load16(mem + EB_O_VERSION) != EB_VERSION ||
        eb_load16(mem + EB_O_HEADER) != EB_HEADER_BYTES)
        return eb_finish(mem, EB_BAD_VERSION, 0, result, 0);
    if (eb_load32(mem + EB_O_TASK) != EB_TASK_ID)
        return eb_finish(mem, EB_BAD_TASK, 0, result, 0);
    if (eb_load32(mem + EB_O_RESERVED0) || eb_load32(mem + EB_O_RESERVED1))
        return eb_finish(mem, EB_BAD_LAYOUT, 0, result, 0);
    n = eb_load32(mem + EB_O_N);
    rounds = eb_load32(mem + EB_O_ROUNDS);
    variant = eb_load32(mem + EB_O_VARIANT);
    input_bytes = eb_load32(mem + EB_O_INPUT_BYTES);
    scratch_bytes = eb_load32(mem + EB_O_SCRATCH_BYTES);
    if (!rounds || rounds > EB_MAX_ROUNDS)
        return eb_finish(mem, EB_BAD_RANGE, 0, result, 0);

#if EB_TASK_ID == 0
    if (n) return eb_finish(mem, EB_BAD_RANGE, 0, result, 0);
#elif EB_TASK_ID == 1
    if (n > EB_MAX_PRIME) return eb_finish(mem, EB_BAD_RANGE, 0, result, 0);
#elif EB_TASK_ID == 2
    if (n > EB_MAX_WORDS) return eb_finish(mem, EB_BAD_RANGE, 0, result, 0);
    if (variant > 1) return eb_finish(mem, EB_BAD_VARIANT, 0, result, 0);
    required_input = n * (variant ? 8u : 4u);
#elif EB_TASK_ID == 3 || EB_TASK_ID == 4
    if (n > EB_MAX_BYTES) return eb_finish(mem, EB_BAD_RANGE, 0, result, 0);
    required_input = n;
#if EB_TASK_ID == 3
    required_scratch = n;
#endif
#elif EB_TASK_ID == 5
    if (n > EB_MAX_SORT) return eb_finish(mem, EB_BAD_RANGE, 0, result, 0);
    required_input = n * 4u; required_scratch = n * 4u;
#elif EB_TASK_ID == 6
    if (n > EB_MAX_BYTES) return eb_finish(mem, EB_BAD_RANGE, 0, result, 0);
    stride = n + 1u; required_input = 2u * stride;
#endif
#if EB_TASK_ID != 2
    if (variant) return eb_finish(mem, EB_BAD_VARIANT, 0, result, 0);
#endif
    if (input_bytes != required_input || scratch_bytes != required_scratch)
        return eb_finish(mem, EB_BAD_LAYOUT, 0, result, 0);
    eb_u32 remaining = byte_length - EB_HEADER_BYTES;
    if (required_input > remaining)
        return eb_finish(mem, EB_SHORT_BUFFER, 0, result, 0);
    if (required_scratch > remaining - required_input)
        return eb_finish(mem, EB_SHORT_BUFFER, 0, result, 0);
    /* Only form pointers after proving their offsets fit the supplied object. */
    eb_u8 *in = mem + EB_HEADER_BYTES;
    eb_u8 *scratch = in + eb_offset(required_input);
#if EB_TASK_ID == 6
    eb_u8 *a_end = in + eb_offset(n);
    eb_u8 *b_end = in + eb_offset(stride + n);
    EB_CHECK(a_end, 1);
    EB_CHECK(b_end, 1);
    if (*a_end != 0 || *b_end != 0)
        return eb_finish(mem, EB_BAD_LAYOUT, 0, result, 0);
#endif
    eb_u32 digest = eb_load32(mem + EB_O_SEED) ^ 0x811c9dc5u;
    EB_LOOP
    for (eb_u32 r = 0; r < rounds; ++r) {
        /* A volatile runtime scale prevents hoisting a pure kernel out of this
         * loop. The caller MUST NOT mutate the request during execution.
         * Bounding by the validated scale also bounds every kernel's storage.
         */
        __asm__ __volatile__("" ::: "memory");
        eb_u32 round_n = eb_load32_volatile(mem + EB_O_N);
        if (round_n > n)
            return eb_finish(mem, EB_BAD_RANGE, r, result, digest);
        eb_u32 status = eb_kernel(in, scratch, round_n, variant, stride, &result, end);
        if (status != EB_OK)
            return eb_finish(mem, status, r, result, digest);
        digest = (digest << 5) | (digest >> 27);
        digest ^= result.lo;
        digest += result.hi ^ r ^ 0x9e3779b9u;
        /* Make each round's stores visible at the compiler boundary. This is
         * not a CPU fence, cache flush, or per-byte volatile instrumentation.
         */
        __asm__ __volatile__("" : "+r"(digest) : : "memory");
    }
    return eb_finish(mem, EB_OK, rounds, result, digest);
}

#if defined(EB_XDP_ENTRY)
/* First two fields match the Linux xdp_md ABI. Existing firmware integrations
 * may define EB_EXTERNAL_XDP_CTX and pre-include their own vmlinux.h.
 */
#ifndef EB_EXTERNAL_XDP_CTX
struct xdp_md { eb_u32 data, data_end, data_meta, ingress_ifindex, rx_queue_index; };
#endif
__attribute__((section("xdp"), used))
int exec_entry(struct xdp_md *ctx)
{
    eb_u8 *data = (eb_u8 *)(unsigned long)ctx->data;
    eb_u8 *end = (eb_u8 *)(unsigned long)ctx->data_end;
    /* Explicit packet-pointer range check, followed by the fixed-size rule. */
    if (data + EB_CONTEXT_BYTES > end) return 0;
    if ((eb_u64)(end - data) != EB_CONTEXT_BYTES) return 0;
    return eb_execute(data, EB_CONTEXT_BYTES, end) == EB_OK ? 2 /* PASS */ : 0 /* ABORTED */;
}
char eb_license[] __attribute__((section("license"), used)) = "GPL";
#else
#ifndef EB_HOST_ENTRY
__attribute__((section("exec"), used))
#endif
eb_u64 exec_entry(void *memory, eb_u64 byte_length)
{
    /* Length itself is supplied by the host, not by the request header. */
    if (!memory || byte_length > 0xffffffffu) return EB_SHORT_BUFFER;
    eb_u8 *data = (eb_u8 *)memory;
    return eb_execute(data, (eb_u32)byte_length, data);
}
#endif
