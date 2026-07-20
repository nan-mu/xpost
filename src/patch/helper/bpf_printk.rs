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
pub(super) fn inner(
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
