/* Little-endian 16-bit words; an odd final byte is padded with a zero high byte.
 * Fold at most every 360 words: large n never overflows the 32-bit accumulators.
 */
EB_INLINE eb_u32 eb_kernel(const eb_u8 *in, eb_u8 *scratch, eb_u32 n,
                         eb_u32 variant, eb_u32 stride, struct eb_result *out,
                         const eb_u8 *end)
{
    eb_u32 s1 = 0xffffu, s2 = 0xffffu, block = 0;
    (void)scratch; (void)variant; (void)stride;
    EB_LOOP
    for (eb_u32 i = 0; i < EB_MAX_BYTES / 2u && i < n / 2u; ++i) {
        const eb_u8 *word = in + eb_offset(2u * i);
        EB_CHECK(word, 2);
        s1 += eb_load16(word); s2 += s1;
        if (++block == 360u) {
            s1 = (s1 & 0xffffu) + (s1 >> 16);
            s2 = (s2 & 0xffffu) + (s2 >> 16);
            block = 0;
        }
    }
    if (n & 1u) {
        const eb_u8 *last = in + eb_offset(n - 1u);
        EB_CHECK(last, 1);
        s1 += *last; s2 += s1;
    }
    s1 = (s1 & 0xffffu) + (s1 >> 16);
    s2 = (s2 & 0xffffu) + (s2 >> 16);
    s1 = (s1 & 0xffffu) + (s1 >> 16);
    s2 = (s2 & 0xffffu) + (s2 >> 16);
    out->lo = (s2 << 16) | s1; out->hi = 0;
    return EB_OK;
}
