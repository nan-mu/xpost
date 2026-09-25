//! Consolidated BPF helper implementations (this version's dispatched set).
//!
//! Deleted: redirect_map, tail_call, adjust_head, load/store_bytes,
//! dynptr, xpost_evt_wake. Deferred BPF ringbuf helpers live in
//! Deferred BPF ringbuf helpers live in `ext/ringbuf/` (not dispatched).
//!
//! `xdp_adjust_tail` is supported **shrink-only** (positive delta rejected).

#![allow(unsafe_op_in_unsafe_fn)]

pub(super) mod ktime_get_ns {
    pub(crate) fn inner() -> usize {
        use embassy_time::Instant;
        // RV32 BPF u64 return is low 32 bits only — publish monotonic µs (wrap ≈4294s).
        Instant::now().as_micros() as usize
    }
}

pub(super) mod map_lookup_elem {
    pub(crate) fn inner(map: usize, key: usize) -> usize {
        crate::map::helper_lookup(map, key)
    }
}

pub(super) mod map_update_elem {
    pub(crate) fn inner(map: usize, key: usize, value: usize, flags: usize) -> usize {
        crate::map::helper_update(map, key, value, flags)
    }
}

pub(super) mod map_delete_elem {
    pub(crate) fn inner(map: usize, key: usize) -> usize {
        crate::map::helper_delete(map, key)
    }
}

pub(super) mod csum_diff {
    /// Incremental checksum diff (Linux `bpf_csum_diff` subset).
    ///
    /// `from`/`to` are treated as byte buffers of the given sizes; seed is the
    /// running `__wsum`. Returns the folded 16-bit checksum as `usize`.
    ///
    /// N1 gap: buffer bounds are trusted from the verifier/loader contract.
    pub(crate) fn inner(
        from: usize,
        from_size: usize,
        to: usize,
        to_size: usize,
        seed: usize,
    ) -> usize {
        let mut sum = seed as u64;

        if from != 0 && from_size != 0 {
            sum = csum_add(sum, from as *const u8, from_size, true);
        }
        if to != 0 && to_size != 0 {
            sum = csum_add(sum, to as *const u8, to_size, false);
        }

        csum_fold(sum) as usize
    }

    fn csum_add(mut sum: u64, data: *const u8, mut size: usize, subtract: bool) -> u64 {
        let mut p = data;
        while size > 1 {
            let word = unsafe { u16::from_be_bytes([*p, *p.add(1)]) } as u64;
            let term = if subtract { (!word) & 0xffff } else { word };
            sum = sum.wrapping_add(term);
            p = unsafe { p.add(2) };
            size -= 2;
        }
        if size == 1 {
            let word = unsafe { u16::from_be_bytes([*p, 0]) } as u64;
            let term = if subtract { (!word) & 0xffff } else { word };
            sum = sum.wrapping_add(term);
        }
        sum
    }

    fn csum_fold(mut sum: u64) -> u16 {
        while (sum >> 16) != 0 {
            sum = (sum & 0xffff) + (sum >> 16);
        }
        !sum as u16
    }
}

pub(super) mod printk {
    use core::fmt::Write;
    use heapless::{String, Vec};
    use nom::{
        IResult, Parser,
        branch::alt,
        bytes::complete::{tag, take_until},
        combinator::{map, rest, value, verify},
    };

    const BPF_PRINTK_NARGS_SHIFT: usize = 24;
    const BPF_PRINTK_SIZE_MASK: usize = 0x00ff_ffff;
    /// Coarse upper bound; real bound must come from verifier/loader contract.
    const BPF_PRINTK_MAX_FMT_SIZE: usize = 256;

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum SpecType {
        Signed,
        Unsigned,
        Hex,
        Octal,
        Dot,
    }

    #[derive(Debug, PartialEq)]
    enum Token<'a> {
        Text(&'a [u8]),
        Spec(SpecType),
    }

    fn parse_spec_type(input: &[u8]) -> IResult<&[u8], SpecType> {
        alt((
            value(SpecType::Dot, tag(b"%pI4" as &[u8])),
            value(
                SpecType::Signed,
                alt((tag(b"%d" as &[u8]), tag(b"%i" as &[u8]))),
            ),
            value(SpecType::Unsigned, tag(b"%u" as &[u8])),
            value(
                SpecType::Hex,
                alt((
                    tag(b"%x" as &[u8]),
                    tag(b"%X" as &[u8]),
                    tag(b"%p" as &[u8]),
                )),
            ),
            value(SpecType::Octal, tag(b"%o" as &[u8])),
        ))
        .parse(input)
    }

    fn parse_text_token<'a>(input: &'a [u8]) -> IResult<&'a [u8], Token<'a>> {
        verify(map(take_until(b"%" as &[u8]), Token::Text), |t| {
            if let Token::Text(s) = t {
                !s.is_empty()
            } else {
                false
            }
        })
        .parse(input)
    }

    fn parse_single_percent<'a>(input: &'a [u8]) -> IResult<&'a [u8], Token<'a>> {
        map(tag(b"%" as &[u8]), |_| Token::Text(b"%")).parse(input)
    }

    fn parse_bpf_format<'a>(input: &'a [u8]) -> Option<Vec<Token<'a>, 16>> {
        let mut results = Vec::<Token, 16>::new();
        let mut curr_input = input;

        while !curr_input.is_empty() {
            if curr_input.starts_with(b"%s") {
                return None;
            }

            let (next_input, token) = match alt((
                map(parse_spec_type, Token::Spec),
                parse_text_token,
                parse_single_percent,
                map(verify(rest, |r: &[u8]| !r.is_empty()), Token::Text),
            ))
            .parse(curr_input)
            {
                Ok(a) => a,
                Err(_e) => {
                    return None;
                }
            };

            if results.push(token).is_err() {
                return None;
            }
            curr_input = next_input;
        }

        Some(results)
    }

    fn zip_format(tokens: &[Token<'_>], args: &[usize]) -> String<256> {
        let mut buf: String<256> = String::new();
        let mut arg = args.iter();
        for token in tokens {
            match token {
                Token::Text(s) => {
                    let text = match core::str::from_utf8(s) {
                        Ok(s) => s,
                        Err(_e) => "",
                    };
                    let _ = write!(buf, "{}", text);
                }
                Token::Spec(spec_type) => {
                    let Some(&val) = arg.next() else {
                        continue;
                    };
                    let _ = match spec_type {
                        SpecType::Signed => write!(buf, "{}", val as isize),
                        SpecType::Unsigned => write!(buf, "{}", val),
                        SpecType::Hex => write!(buf, "{:#x}", val),
                        SpecType::Octal => write!(buf, "{:#o}", val),
                        SpecType::Dot => {
                            if val == 0 {
                                write!(buf, "0.0.0.0")
                            } else {
                                // N1 gap: `%pI4` arg readability trusted from verifier.
                                let o = unsafe { core::ptr::read_unaligned(val as *const [u8; 4]) };
                                write!(buf, "{}.{}.{}.{}", o[0], o[1], o[2], o[3])
                            }
                        }
                    };
                }
            }
        }
        buf
    }

    fn emit(fmt: &[u8], args: &[usize]) -> Result<(), ()> {
        let tokens = parse_bpf_format(fmt).ok_or(())?;

        let nspecs = tokens
            .iter()
            .filter(|t| matches!(t, Token::Spec(_)))
            .count();
        if nspecs != args.len() {
            return Err(());
        }

        let msg = zip_format(&tokens, args);
        log::info!("XDP: {}", msg);
        Ok(())
    }

    #[inline(always)]
    pub(crate) fn inner(
        fmt_ptr: usize,
        fmt_len: usize,
        arg1: usize,
        arg2: usize,
        arg3: usize,
    ) -> usize {
        let nargs = fmt_len >> BPF_PRINTK_NARGS_SHIFT;
        let fmt_size = fmt_len & BPF_PRINTK_SIZE_MASK;

        if fmt_ptr == 0 || fmt_size == 0 || fmt_size > BPF_PRINTK_MAX_FMT_SIZE || nargs > 3 {
            return usize::MAX;
        }

        let fmt = unsafe { core::slice::from_raw_parts(fmt_ptr as *const u8, fmt_size) };

        let result = match nargs {
            0 => emit(fmt, &[]),
            1 => emit(fmt, &[arg1]),
            2 => emit(fmt, &[arg1, arg2]),
            3 => emit(fmt, &[arg1, arg2, arg3]),
            _ => return usize::MAX,
        };

        match result {
            Ok(()) => 0,
            Err(()) => usize::MAX,
        }
    }
}

pub(super) mod xdp_adjust_tail {
    //! `bpf_xdp_adjust_tail` — **shrink only** (negative / zero delta).
    //!
    //! Positive delta (grow into headroom) is rejected. Frame storage is the
    //! in-place RX buffer; there is no scratch grow path.

    use crate::xdp::ingress::ctx::XdpMd;

    const EINVAL: usize = (-22isize) as usize;

    pub(crate) fn inner(ctx: usize, delta: usize) -> usize {
        if ctx == 0 {
            return EINVAL;
        }
        let delta = delta as i32;
        if delta > 0 {
            return EINVAL;
        }
        if delta == 0 {
            return 0;
        }
        let md = unsafe { &mut *(ctx as *mut XdpMd) };
        let data = md.data as usize;
        let end = md.data_end as usize;
        if end < data {
            return EINVAL;
        }
        let shrink = (-delta) as usize;
        let len = end - data;
        if shrink > len {
            return EINVAL;
        }
        md.data_end = (end - shrink) as *mut u8;
        0
    }
}
