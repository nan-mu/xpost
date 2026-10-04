#![no_std]
#![no_main]

use aya_ebpf::{
    bindings::TC_ACT_PIPE,
    helpers::bpf_ktime_get_ns,
    macros::{classifier, map},
    maps::{Array, RingBuf},
    programs::TcContext,
};
use aya_log_ebpf::debug;
use abi::{
    FilterConfig, KIND_BOUNCE_INGRESS, KIND_ECHO_INGRESS, KIND_STIM_EGRESS, MAGIC_STIM, StampEvent,
};

const ETH_HLEN: usize = 14;
const ETH_P_IP: u16 = 0x0800;
const IPPROTO_UDP: u8 = 17;
const UDP_HLEN: usize = 8;

#[map]
static CONFIG: Array<FilterConfig> = Array::with_max_entries(1, 0);

#[map]
static EVENTS: RingBuf = RingBuf::with_byte_size(4 * 1024 * 1024, 0);

#[classifier]
pub fn test_egress(ctx: TcContext) -> i32 {
    match try_stamp(&ctx, true) {
        Ok(ret) => ret,
        Err(ret) => ret,
    }
}

#[classifier]
pub fn test_ingress(ctx: TcContext) -> i32 {
    match try_stamp(&ctx, false) {
        Ok(ret) => ret,
        Err(ret) => ret,
    }
}

#[inline(always)]
fn magic_eq(got: &[u8; 6], want: &[u8; 6]) -> bool {
    got[0] == want[0]
        && got[1] == want[1]
        && got[2] == want[2]
        && got[3] == want[3]
        && got[4] == want[4]
        && got[5] == want[5]
}

#[inline(always)]
fn load_stim_seq(ctx: &TcContext, payload_off: usize) -> Result<(u8, u64), i32> {
    let b0 = ptr_at::<u8>(ctx, payload_off)?;
    let m1 = ptr_at::<u8>(ctx, payload_off + 1)?;
    let m2 = ptr_at::<u8>(ctx, payload_off + 2)?;
    let m3 = ptr_at::<u8>(ctx, payload_off + 3)?;
    let m4 = ptr_at::<u8>(ctx, payload_off + 4)?;
    let m5 = ptr_at::<u8>(ctx, payload_off + 5)?;
    let magic = [b0, m1, m2, m3, m4, m5];
    if !magic_eq(&magic, &MAGIC_STIM) {
        return Err(TC_ACT_PIPE);
    }
    let mut b = [0u8; 8];
    b[0] = ptr_at::<u8>(ctx, payload_off + 6)?;
    b[1] = ptr_at::<u8>(ctx, payload_off + 7)?;
    b[2] = ptr_at::<u8>(ctx, payload_off + 8)?;
    b[3] = ptr_at::<u8>(ctx, payload_off + 9)?;
    b[4] = ptr_at::<u8>(ctx, payload_off + 10)?;
    b[5] = ptr_at::<u8>(ctx, payload_off + 11)?;
    b[6] = ptr_at::<u8>(ctx, payload_off + 12)?;
    b[7] = ptr_at::<u8>(ctx, payload_off + 13)?;
    Ok((b0, u64::from_le_bytes(b)))
}

#[inline(always)]
fn try_stamp(ctx: &TcContext, egress: bool) -> Result<i32, i32> {
    let eth_type = u16::from_be(ptr_at::<u16>(ctx, 12)?);
    if eth_type != ETH_P_IP {
        return Ok(TC_ACT_PIPE);
    }

    let ihl_ver = ptr_at::<u8>(ctx, ETH_HLEN)?;
    let ihl = ((ihl_ver & 0x0f) as usize) * 4;
    if ihl < 20 {
        return Ok(TC_ACT_PIPE);
    }

    let proto = ptr_at::<u8>(ctx, ETH_HLEN + 9)?;
    if proto != IPPROTO_UDP {
        return Ok(TC_ACT_PIPE);
    }

    let src = ptr_at::<u32>(ctx, ETH_HLEN + 12)?;
    let dst = ptr_at::<u32>(ctx, ETH_HLEN + 16)?;
    let udp_off = ETH_HLEN + ihl;
    let sport = u16::from_be(ptr_at::<u16>(ctx, udp_off)?);
    let dport = u16::from_be(ptr_at::<u16>(ctx, udp_off + 2)?);

    let cfg = CONFIG.get(0).ok_or(TC_ACT_PIPE)?;

    let payload_off = udp_off + UDP_HLEN;
    let q0 = ptr_at::<u8>(ctx, payload_off)?;
    let q1 = ptr_at::<u8>(ctx, payload_off + 1)?;
    let q2 = ptr_at::<u8>(ctx, payload_off + 2)?;
    let q3 = ptr_at::<u8>(ctx, payload_off + 3)?;
    let stim_off = if q0 == b'Q' && q1 == b'U' && q2 == b'I' && q3 == b'C' {
        payload_off + 8
    } else {
        payload_off
    };

    let (kind, seq) = if egress
        && dport == cfg.echo_port
        && (dst == cfg.esp_ip_be || (cfg.vip_ip_be != 0 && dst == cfg.vip_ip_be))
    {
        let (_, seq) = load_stim_seq(ctx, stim_off)?;
        (KIND_STIM_EGRESS, seq)
    } else if !egress && cfg.vip_ip_be != 0 && dst == cfg.vip_ip_be && dport == cfg.echo_port {
        let (_, seq) = load_stim_seq(ctx, stim_off)?;
        (KIND_BOUNCE_INGRESS, seq)
    } else if !egress && dst == cfg.esp_ip_be && dport == cfg.echo_port && src != cfg.esp_ip_be {
        // L2-only XDP_TX (paper Listing 1): MAC swapped, IP/ports unchanged.
        let (_, seq) = load_stim_seq(ctx, stim_off)?;
        (KIND_BOUNCE_INGRESS, seq)
    } else if !egress && src == cfg.esp_ip_be && sport == cfg.echo_port && dport != cfg.reply_port {
        // L3/L4-swap bounce: sport becomes echo_port after port swap.
        let (_, seq) = load_stim_seq(ctx, stim_off)?;
        (KIND_BOUNCE_INGRESS, seq)
    } else if !egress && src == cfg.esp_ip_be && dport == cfg.reply_port {
        let (_, seq) = load_stim_seq(ctx, stim_off)?;
        (KIND_ECHO_INGRESS, seq)
    } else {
        return Ok(TC_ACT_PIPE);
    };

    let ktime_ns = unsafe { bpf_ktime_get_ns() };
    if let Some(mut slot) = EVENTS.reserve::<StampEvent>(0) {
        slot.write(StampEvent {
            kind,
            _pad0: 0,
            udp_sport: sport,
            l2_len: ctx.len() as u16,
            _pad1: 0,
            ktime_ns,
            seq,
        });
        slot.submit(0);
        if kind == KIND_STIM_EGRESS {
            debug!(ctx, "stim_egress seq={} ns={}", seq, ktime_ns);
        } else if kind == KIND_BOUNCE_INGRESS {
            debug!(ctx, "bounce_ingress seq={} ns={}", seq, ktime_ns);
        } else {
            debug!(
                ctx,
                "echo_ingress seq={} sport={} ns={}", seq, sport, ktime_ns
            );
        }
    }

    Ok(TC_ACT_PIPE)
}

#[inline(always)]
fn ptr_at<T>(ctx: &TcContext, offset: usize) -> Result<T, i32>
where
    T: Copy,
{
    ctx.load(offset).map_err(|_| TC_ACT_PIPE)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &::core::panic::PanicInfo) -> ! {
    loop {}
}

#[unsafe(link_section = "license")]
#[unsafe(no_mangle)]
static LICENSE: [u8; 13] = *b"Dual MIT/GPL\0";
