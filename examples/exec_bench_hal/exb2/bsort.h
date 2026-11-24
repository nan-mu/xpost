/* Source stays immutable. Every round includes source->scratch restoration and
 * a full bubble sort, without a sorted-input early-exit optimization.
 */
EB_INLINE eb_u32 eb_kernel(const eb_u8 *in, eb_u8 *scratch, eb_u32 n,
                         eb_u32 variant, eb_u32 stride, struct eb_result *out,
                         const eb_u8 *end)
{
    (void)variant; (void)stride;
    EB_LOOP
    for (eb_u32 i = 0; i < EB_MAX_SORT && i < n; ++i) {
        eb_u32 offset = eb_offset(4u * i);
        const eb_u8 *src = in + offset;
        eb_u8 *dst = scratch + offset;
        EB_CHECK(src, 4);
        EB_CHECK(dst, 4);
        eb_store32(dst, eb_load32(src));
    }
    EB_LOOP
    for (eb_u32 i = 0; i < EB_MAX_SORT && i < n; ++i) {
        EB_LOOP
        for (eb_u32 j = 1; j < EB_MAX_SORT && j < n - i; ++j) {
            eb_u8 *left = scratch + eb_offset(4u * (j - 1u));
            eb_u8 *right = scratch + eb_offset(4u * j);
            EB_CHECK(left, 4);
            EB_CHECK(right, 4);
            eb_u32 a = eb_load32(left);
            eb_u32 b = eb_load32(right);
            if (a > b) {
                eb_store32(left, b);
                eb_store32(right, a);
            }
        }
    }
    out->lo = 0;
    out->hi = 0;
    if (n) {
        eb_u8 *last = scratch + eb_offset(4u * (n - 1u));
        EB_CHECK(scratch, 4);
        EB_CHECK(last, 4);
        out->lo = eb_load32(scratch);
        out->hi = eb_load32(last);
    }
    return EB_OK;
}
