//! Shared whitelist bench constants (wl-rtt / wl-loss).

use std::net::Ipv4Addr;

use rand::RngExt;

/// Placeholder legit inter-packet gap (µs). Echo experiment may replace later.
pub const LEGIT_INTERVAL_US: u32 = 2_200;
/// Attack intensity α×100 → scaled into hping3 pps.
pub const ALPHA_PCTS: [u32; 5] = [0, 50, 100, 200, 400];
/// Nominal legit pps used to scale α (interval is fixed at 2200µs ≈ 454 pps).
pub const LEGIT_PPS: u32 = 1_000_000 / LEGIT_INTERVAL_US;

pub const WL_N: usize = 4;
pub const ATK_N: usize = 8;
pub const INTERFERER: &str = "hping3";

#[derive(Clone, Copy, Debug)]
pub struct AlphaTask {
    pub alpha_x100: u32,
}

pub fn alpha_tasks(smoke: bool) -> Vec<AlphaTask> {
    if smoke {
        vec![AlphaTask { alpha_x100: 0 }, AlphaTask { alpha_x100: 100 }]
    } else {
        ALPHA_PCTS
            .iter()
            .copied()
            .map(|alpha_x100| AlphaTask { alpha_x100 })
            .collect()
    }
}

pub fn hping_pps(alpha_x100: u32) -> u32 {
    ((u64::from(LEGIT_PPS) * u64::from(alpha_x100)) / 100) as u32
}

#[derive(Clone)]
pub struct Peers {
    pub ctrl: Ipv4Addr,
    pub wl: Vec<Ipv4Addr>,
    pub atk: Vec<Ipv4Addr>,
}

pub fn pick_peers(esp: Ipv4Addr) -> Peers {
    let mut avoid = vec![esp];
    let ctrl = gen_lan_ip(esp, &avoid);
    avoid.push(ctrl);
    let mut wl = Vec::with_capacity(WL_N);
    for _ in 0..WL_N {
        let ip = gen_lan_ip(esp, &avoid);
        avoid.push(ip);
        wl.push(ip);
    }
    let mut atk = Vec::with_capacity(ATK_N);
    for _ in 0..ATK_N {
        let ip = gen_lan_ip(esp, &avoid);
        avoid.push(ip);
        atk.push(ip);
    }
    Peers { ctrl, wl, atk }
}

fn gen_lan_ip(esp: Ipv4Addr, avoid: &[Ipv4Addr]) -> Ipv4Addr {
    let mut rng = rand::rng();
    let oct = esp.octets();
    loop {
        let host = rng.random_range(2..254u8);
        let ip = Ipv4Addr::new(oct[0], oct[1], oct[2], host);
        if ip != esp && !avoid.contains(&ip) {
            return ip;
        }
    }
}

pub fn ip_key(ip: Ipv4Addr) -> [u8; 4] {
    ip.octets()
}

pub fn fmt_dur(d: std::time::Duration) -> String {
    let s = d.as_secs();
    format!("{}h{:02}m{:02}s", s / 3600, (s % 3600) / 60, s % 60)
}
