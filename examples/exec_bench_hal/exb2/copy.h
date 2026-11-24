/* Explicit byte-copy kernel, not a call to the target libc memcpy. */
EB_INLINE eb_u32 eb_kernel(const eb_u8 *in, eb_u8 *scratch, eb_u32 n,
                         eb_u32 variant, eb_u32 stride, struct eb_result *out,
                         const eb_u8 *end)
{
    (void)variant; (void)stride;
    EB_LOOP
    for (eb_u32 i = 0; i < EB_MAX_BYTES && i < n; ++i) {
        eb_u32 offset = eb_offset(i);
        const eb_u8 *src = in + offset;
        eb_u8 *dst = scratch + offset;
        EB_CHECK(src, 1);
        EB_CHECK(dst, 1);
        *dst = *src;
    }
    out->lo = n; out->hi = 0;
    return EB_OK;
}
