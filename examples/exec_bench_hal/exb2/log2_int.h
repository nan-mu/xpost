/* Integer floor(log2), with log2(0)=0 by protocol definition. */
EB_INLINE eb_u32 eb_log2_32(eb_u32 v)
{
    eb_u32 r = 0;
    if (v >= 0x10000u) { v >>= 16; r += 16; }
    if (v >= 0x100u) { v >>= 8; r += 8; }
    if (v >= 0x10u) { v >>= 4; r += 4; }
    if (v >= 4u) { v >>= 2; r += 2; }
    if (v >= 2u) ++r;
    return r;
}
EB_INLINE eb_u32 eb_kernel(const eb_u8 *in, eb_u8 *scratch, eb_u32 n,
                         eb_u32 variant, eb_u32 stride, struct eb_result *out,
                         const eb_u8 *end)
{
    eb_u32 sum = 0, hash = 0;
    (void)scratch; (void)stride;
    EB_LOOP
    for (eb_u32 i = 0; i < EB_MAX_WORDS && i < n; ++i) {
        const eb_u8 *word = in + eb_offset(i * (variant ? 8u : 4u));
        EB_CHECK(word, 4);
        eb_u32 lo = eb_load32(word);
        eb_u32 r;
        if (variant) {
            const eb_u8 *high = word + 4;
            EB_CHECK(high, 4);
            eb_u32 hi = eb_load32(high);
            r = hi ? 32u + eb_log2_32(hi) : eb_log2_32(lo);
        } else {
            r = eb_log2_32(lo);
        }
        sum += r;
        hash = (hash << 5) | (hash >> 27);
        hash ^= r + i;
    }
    out->lo = sum; out->hi = hash;
    return EB_OK;
}
