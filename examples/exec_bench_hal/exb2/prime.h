/* Trial division: primes in [2, n], inclusive. No generated data or helpers. */
EB_INLINE eb_u32 eb_kernel(const eb_u8 *in, eb_u8 *scratch, eb_u32 n,
                         eb_u32 variant, eb_u32 stride, struct eb_result *out,
                         const eb_u8 *end)
{
    eb_u32 count = 0, last = 0;
    (void)end; (void)in; (void)scratch; (void)variant; (void)stride;
    EB_LOOP
    for (eb_u32 x = 2; x <= n; ++x) {
        eb_u32 prime = 1;
        EB_LOOP
        for (eb_u32 d = 2; d <= x / d; ++d) {
            if (x % d == 0) { prime = 0; break; }
        }
        if (prime) { ++count; last = x; }
    }
    out->lo = count; out->hi = last;
    return EB_OK;
}
