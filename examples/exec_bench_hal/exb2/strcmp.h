/* Two (n+1)-byte strings. Protocol checks the final terminators once.
 * Returns unsigned-byte difference as a 32-bit two's-complement value,
 * plus the actual number of examined byte pairs (excluding final terminator).
 */
EB_INLINE eb_u32 eb_kernel(const eb_u8 *in, eb_u8 *scratch, eb_u32 n,
                         eb_u32 variant, eb_u32 stride, struct eb_result *out,
                         const eb_u8 *end)
{
    (void)scratch; (void)variant;
    const eb_u8 *b = in + eb_offset(stride);
    out->lo = 0; out->hi = 0;
    EB_LOOP
    for (eb_u32 i = 0; i < EB_MAX_BYTES && i < n; ++i) {
        eb_u32 offset = eb_offset(i);
        const eb_u8 *left = in + offset;
        const eb_u8 *right = b + offset;
        EB_CHECK(left, 1);
        EB_CHECK(right, 1);
        eb_u32 a_byte = *left, b_byte = *right;
        out->hi = i + 1u;
        if (a_byte != b_byte) { out->lo = a_byte - b_byte; break; }
        if (a_byte == 0) break;
    }
    return EB_OK;
}
