#ifndef EXEC_PROTOCOL_H
#define EXEC_PROTOCOL_H

/* EXB2, fixed-capacity little-endian protocol, independent of n and rounds. */
typedef unsigned char eb_u8;
typedef unsigned short eb_u16;
typedef unsigned int eb_u32;
#ifdef EB_EXTERNAL_XDP_CTX
typedef __u64 eb_u64;
#else
typedef unsigned long long eb_u64;
#endif
_Static_assert(sizeof(eb_u8) == 1 && sizeof(eb_u16) == 2 &&
               sizeof(eb_u32) == 4 && sizeof(eb_u64) == 8, "integer widths");

#define EB_MAGIC 0x32425845u /* bytes: E X B 2 */
#define EB_VERSION 2u
#define EB_HEADER_BYTES 64u
#define EB_NOT_RUN 0xffffffffu
#ifndef EB_CONTEXT_BYTES
#define EB_CONTEXT_BYTES 8258u
#endif
_Static_assert(EB_CONTEXT_BYTES >= EB_HEADER_BYTES &&
               EB_CONTEXT_BYTES <= 0xffffffffu, "context capacity");

enum eb_task {
    EB_NOOP = 0, EB_PRIME = 1, EB_LOG2 = 2, EB_COPY = 3,
    EB_FLETCHER32 = 4, EB_BSORT = 5, EB_STRCMP = 6
};
enum eb_status {
    EB_OK = 0, EB_BAD_MAGIC = 1, EB_BAD_VERSION = 2,
    EB_BAD_TASK = 3, EB_BAD_RANGE = 4, EB_BAD_LAYOUT = 5,
    EB_BAD_VARIANT = 6, EB_SHORT_BUFFER = 7
};
enum eb_offset {
    EB_O_MAGIC = 0, EB_O_VERSION = 4, EB_O_HEADER = 6,
    EB_O_TASK = 8, EB_O_N = 12, EB_O_ROUNDS = 16,
    EB_O_VARIANT = 20, EB_O_SEED = 24, EB_O_INPUT_BYTES = 28,
    EB_O_SCRATCH_BYTES = 32, EB_O_STATUS = 36, EB_O_COMPLETED = 40,
    EB_O_RESULT_LO = 44, EB_O_RESULT_HI = 48, EB_O_DIGEST = 52,
    EB_O_RESERVED0 = 56, EB_O_RESERVED1 = 60
};

#ifndef EB_MAX_ROUNDS
#define EB_MAX_ROUNDS 65536u
#endif
#ifndef EB_MAX_BYTES
#define EB_MAX_BYTES 4096u
#endif
#ifndef EB_MAX_WORDS
#define EB_MAX_WORDS 1024u
#endif
#ifndef EB_MAX_SORT
#define EB_MAX_SORT 128u
#endif
#ifndef EB_MAX_PRIME
#define EB_MAX_PRIME 8192u
#endif

#define EB_INLINE static __inline__ __attribute__((always_inline))
#if defined(__clang__)
#define EB_LOOP _Pragma("clang loop unroll(disable)")
#else
#define EB_LOOP
#endif

/* Clang can turn bounded scalar indexes into loop-carried packet pointers.
 * Keep the scalar bound and the pointer comparison visible to PREVAIL.
 * No validated offset in this protocol reaches the mask boundary. */
#if defined(__bpf__) && defined(EB_XDP_ENTRY)
_Static_assert(EB_CONTEXT_BYTES <= 0x4000u, "packet offsets fit the scalar mask");
EB_INLINE eb_u32 eb_offset(eb_u32 offset)
{
    __asm__ __volatile__("" : "+r"(offset));
    return offset & 0x3fffu;
}
#define EB_CHECK(ptr, width) do { \
    __asm__ __volatile__("" : "+r"(ptr)); \
    if ((ptr) + (width) > end) return EB_SHORT_BUFFER; \
} while (0)
#else
#define eb_offset(offset) (offset)
#define EB_CHECK(ptr, width) do { (void)(ptr); (void)end; } while (0)
#endif

EB_INLINE eb_u16 eb_load16(const eb_u8 *p)
{
    return (eb_u16)((eb_u16)p[0] | ((eb_u16)p[1] << 8));
}
EB_INLINE eb_u32 eb_load32(const eb_u8 *p)
{
    return (eb_u32)p[0] | ((eb_u32)p[1] << 8) |
           ((eb_u32)p[2] << 16) | ((eb_u32)p[3] << 24);
}
EB_INLINE eb_u32 eb_load32_volatile(const volatile eb_u8 *p)
{
    return (eb_u32)p[0] | ((eb_u32)p[1] << 8) |
           ((eb_u32)p[2] << 16) | ((eb_u32)p[3] << 24);
}
EB_INLINE void eb_store32(eb_u8 *p, eb_u32 x)
{
    p[0] = (eb_u8)x; p[1] = (eb_u8)(x >> 8);
    p[2] = (eb_u8)(x >> 16); p[3] = (eb_u8)(x >> 24);
}

/* Both results describe the LAST complete round. digest covers ALL rounds. */
struct eb_result { eb_u32 lo, hi; };

#if !defined(EB_XDP_ENTRY)
/* VM R1=memory, R2=byte length; native ABI uses the same two arguments. */
eb_u64 exec_entry(void *memory, eb_u64 byte_length);
#endif
#endif
