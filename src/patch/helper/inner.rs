//! Consolidated BPF helper implementations (formerly one-file-per-ID).

#![allow(unsafe_op_in_unsafe_fn)]

pub(super) mod ktime_get_ns {
    pub(crate) fn inner() -> usize {
        use embassy_time::Instant;
        Instant::now().as_micros() as usize * 1000
    }
}

pub(super) mod redirect {
    use crate::patch::redirect::{self as xdp_redir, XDP_REDIRECT};
    use log::error;

    /// `bpf_redirect(ifindex, flags)` — [`IFINDEX_WIFI`] only on C3; wired stub `-ENOTSUP`.
    pub(crate) fn inner(ifindex: usize, _flags: usize) -> usize {
        use crate::patch::ctx::IFINDEX_WIFI;
        let ifindex = ifindex as u32;
        if !xdp_redir::ifindex_supported(ifindex) {
            error!(
                "[helper]bpf_redirect: ifindex {} unsupported (only {}=wifi)",
                ifindex, IFINDEX_WIFI
            );
            return (-95i32) as usize; // -ENOTSUP
        }
        xdp_redir::set_pending(ifindex);
        XDP_REDIRECT as usize
    }
}

pub(super) mod redirect_map {
    use crate::patch::map::pool::POOL;
    use crate::patch::map::{MapKind, resolve_map_id};
    use crate::patch::redirect::{self as xdp_redir, XDP_REDIRECT};
    use log::error;

    /// `bpf_redirect_map(map, key, flags)` — `key` is the u32 index value (not a pointer).
    ///
    /// On miss / wrong kind: return `flags & 0xf` (Linux fallback action bits).
    pub(crate) fn inner(map: usize, key: usize, flags: usize) -> usize {
        let fallback = flags & 0xf;
        let Some(id) = resolve_map_id(map) else {
            return fallback;
        };

        let mut guard = match POOL.try_write() {
            Some(g) => g,
            None => return fallback,
        };
        let table = match guard.as_mut() {
            Some(t) => t,
            None => return fallback,
        };
        let res = match table.get_mut(&id) {
            Some(r) => r,
            None => return fallback,
        };
        if res.kind != Some(MapKind::Devmap) {
            error!("[helper]bpf_redirect_map: map id {} is not DEVMAP", id);
            return fallback;
        }

        let key_u32 = key as u32;
        let key_bytes = key_u32.to_ne_bytes();
        let m: &mut crate::patch::map::BpfArrayMap = unsafe { res.as_mut() };
        use crate::patch::map::ops::MapOps;
        let val_ptr = match MapOps::lookup(m, key_bytes.as_ptr()) {
            Ok(p) => p,
            Err(_) => return fallback,
        };
        // Unpopulated slot stays 0 — treat as wifi ifindex only when explicitly written;
        // Linux DEVMAP miss returns fallback; zero value is a valid ifindex (wifi).
        // Distinguish empty: ARRAY is zero-filled; userspace must write before redirect.
        // Value 0 is wifi — that is intentional for this MCU port table.
        let ifindex = unsafe { core::ptr::read_unaligned(val_ptr as *const u32) };

        if !xdp_redir::ifindex_supported(ifindex) {
            use crate::patch::ctx::IFINDEX_WIFI;
            error!(
                "[helper]bpf_redirect_map: ifindex {} unsupported (only {}=wifi)",
                ifindex, IFINDEX_WIFI
            );
            return (-95i32) as usize;
        }
        xdp_redir::set_pending(ifindex);
        XDP_REDIRECT as usize
    }
}

pub(super) mod tail_call {
    pub(crate) fn inner(_ctx: usize, _map: usize, _index: usize) -> usize {
        (-2i32) as usize
    }
}

pub(super) mod map_lookup_elem {
    pub(crate) fn inner(map: usize, key: usize) -> usize {
        crate::patch::map::helper_lookup(map, key)
    }
}

pub(super) mod map_update_elem {
    pub(crate) fn inner(map: usize, key: usize, value: usize, flags: usize) -> usize {
        crate::patch::map::helper_update(map, key, value, flags)
    }
}

pub(super) mod map_delete_elem {
    pub(crate) fn inner(map: usize, key: usize) -> usize {
        crate::patch::map::helper_delete(map, key)
    }
}

pub(super) mod for_each_map_elem {
    pub(crate) fn inner(
        map: usize,
        callback_fn: usize,
        callback_ctx: usize,
        flags: usize,
    ) -> usize {
        crate::patch::map::helper_for_each_map_elem(map, callback_fn, callback_ctx, flags)
    }
}

pub(super) mod ringbuf_output {
    pub(crate) fn inner(map: usize, data: usize, size: usize, flags: usize) -> usize {
        crate::patch::map::helper_ringbuf_output(map, data, size, flags)
    }
}

pub(super) mod ringbuf_reserve {
    pub(crate) fn inner(map: usize, size: usize, flags: usize) -> usize {
        crate::patch::map::helper_ringbuf_reserve(map, size, flags)
    }
}

pub(super) mod ringbuf_submit {
    pub(crate) fn inner(data: usize, flags: usize) -> usize {
        let _ = flags;
        crate::patch::map::helper_ringbuf_submit(data)
    }
}

pub(super) mod ringbuf_discard {
    pub(crate) fn inner(data: usize, flags: usize) -> usize {
        let _ = flags;
        crate::patch::map::helper_ringbuf_discard(data)
    }
}

pub(super) mod ringbuf_query {
    pub(crate) fn inner(map: usize, flags: usize) -> usize {
        crate::patch::map::helper_ringbuf_query(map, flags)
    }
}

pub(super) mod ringbuf_reserve_dynptr {
    pub(crate) fn inner(map: usize, size: usize, flags: usize, ptr: usize) -> usize {
        crate::patch::map::helper_ringbuf_reserve_dynptr(map, size, flags, ptr)
    }
}

pub(super) mod ringbuf_submit_dynptr {
    pub(crate) fn inner(ptr: usize, flags: usize) -> usize {
        crate::patch::map::helper_ringbuf_submit_dynptr(ptr, flags)
    }
}

pub(super) mod ringbuf_discard_dynptr {
    pub(crate) fn inner(ptr: usize, flags: usize) -> usize {
        crate::patch::map::helper_ringbuf_discard_dynptr(ptr, flags)
    }
}

pub(super) mod xdp_adjust_head {
    pub(crate) fn inner(xdp_md: usize, delta: usize) -> usize {
        unsafe {
            match crate::patch::ctx::XdpMd::from_raw(xdp_md) {
                Some(ctx) => ctx.adjust_head(delta) as usize,
                None => (-22i32) as usize,
            }
        }
    }
}

pub(super) mod xdp_adjust_tail {
    pub(crate) fn inner(xdp_md: usize, delta: usize) -> usize {
        unsafe {
            match crate::patch::ctx::XdpMd::from_raw(xdp_md) {
                Some(ctx) => ctx.adjust_tail(delta) as usize,
                None => (-22i32) as usize,
            }
        }
    }
}

pub(super) mod xdp_load_bytes {
    pub(crate) fn inner(xdp_md: usize, offset: usize, buf: usize, len: usize) -> usize {
        unsafe {
            match crate::patch::ctx::XdpMd::from_raw(xdp_md) {
                Some(ctx) => ctx.load_bytes(offset, buf, len) as usize,
                None => (-22i32) as usize,
            }
        }
    }
}

pub(super) mod xdp_store_bytes {
    pub(crate) fn inner(xdp_md: usize, offset: usize, buf: usize, len: usize) -> usize {
        unsafe {
            match crate::patch::ctx::XdpMd::from_raw(xdp_md) {
                Some(ctx) => ctx.store_bytes(offset, buf, len) as usize,
                None => (-22i32) as usize,
            }
        }
    }
}

pub(super) mod dynptr_from_mem {
    pub(crate) fn inner(data: usize, size: usize, flags: usize, ptr: usize) -> usize {
        crate::patch::helper::dynptr::BpfDynptr::from_mem(data, size, flags, ptr)
    }
}

pub(super) mod dynptr_read {
    pub(crate) fn inner(dst: usize, len: usize, src: usize, offset: usize, flags: usize) -> usize {
        crate::patch::helper::dynptr::BpfDynptr::read(dst, len, src, offset, flags)
    }
}

pub(super) mod dynptr_write {
    pub(crate) fn inner(dst: usize, offset: usize, src: usize, len: usize, flags: usize) -> usize {
        crate::patch::helper::dynptr::BpfDynptr::write(dst, offset, src, len, flags)
    }
}

pub(super) mod dynptr_data {
    pub(crate) fn inner(ptr: usize, offset: usize, len: usize) -> usize {
        crate::patch::helper::dynptr::BpfDynptr::data(ptr, offset, len)
    }
}

pub(super) mod csum_diff {

    /// Incremental checksum diff (Linux `bpf_csum_diff` subset).
    ///
    /// `from`/`to` are treated as byte buffers of the given sizes; seed is the
    /// running `__wsum`. Returns the folded 16-bit checksum as `usize`.
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
            // One's-complement: invert only the 16-bit word, not the whole u64.
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
    use log::error;
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

    /// Returns `None` on parse failure or rejected `%s`.
    fn parse_bpf_format<'a>(input: &'a [u8]) -> Option<Vec<Token<'a>, 16>> {
        let mut results = Vec::<Token, 16>::new();
        let mut curr_input = input;

        while !curr_input.is_empty() {
            if curr_input.starts_with(b"%s") {
                error!("[helper]bpf_printk: %s is rejected");
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
                    error!("[helper]bpf_printk: parse error");
                    return None;
                }
            };

            if results.push(token).is_err() {
                error!("[helper]bpf_printk: too many format tokens");
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
                        Err(e) => {
                            error!(
                                "[helper]bpf_printk: utf8 decode failed in format text: {:?}",
                                e
                            );
                            ""
                        }
                    };
                    if write!(buf, "{}", text).is_err() {
                        error!("[helper]bpf_printk: buffer full, skip text");
                    }
                }
                Token::Spec(spec_type) => {
                    let Some(&val) = arg.next() else {
                        error!("[helper]bpf_printk: not enough arguments for format specifier");
                        continue;
                    };
                    let result = match spec_type {
                        SpecType::Signed => write!(buf, "{}", val as isize),
                        SpecType::Unsigned => write!(buf, "{}", val),
                        SpecType::Hex => write!(buf, "{:#x}", val),
                        SpecType::Octal => write!(buf, "{:#o}", val),
                        // BE-in-u32: 0xC0A80101 -> 192.168.1.1
                        SpecType::Dot => {
                            let v = val as u32;
                            write!(
                                buf,
                                "{}.{}.{}.{}",
                                (v >> 24) & 0xff,
                                (v >> 16) & 0xff,
                                (v >> 8) & 0xff,
                                v & 0xff
                            )
                        }
                    };
                    if result.is_err() {
                        error!("[helper]bpf_printk: buffer full, skip value");
                    }
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
            error!(
                "[helper]bpf_printk: soft assert failed: placeholders={} nargs={}",
                nspecs,
                args.len()
            );
            return Err(());
        }

        let msg = zip_format(&tokens, args);
        log::info!("XDP: {}", msg);
        Ok(())
    }

    /// `bpf_trace_printk`: unpack packed `fmt_len`, validate, format, emit.
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
            error!(
                "[helper]bpf_printk: invalid args: fmt_ptr={:#x} fmt_size={} nargs={}",
                fmt_ptr, fmt_size, nargs
            );
            return usize::MAX;
        }

        // SAFETY: fmt_ptr/fmt_size validated coarsely above; full readability
        // is assumed from the verifier/loader ABI contract.
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
