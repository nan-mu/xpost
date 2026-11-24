EB_INLINE eb_u32 eb_kernel(const eb_u8 *in, eb_u8 *scratch, eb_u32 n,
                         eb_u32 variant, eb_u32 stride, struct eb_result *out,
                         const eb_u8 *end)
{
    (void)end; (void)in; (void)scratch; (void)n; (void)variant; (void)stride;
    out->lo = 0; out->hi = 0;
    return EB_OK;
}
