//! Seed `firewall_rules` from an embassy task before WiFi bring-up.

use crate::patch::map::{
    FirewallRulesKey, MAP_ID_FIREWALL_RULES, register_map_name, with_hash_map,
};
use log::{error, info};

/// Wire-order octets as native `u32` (matches LE load of `iph->saddr`).
const fn ipv4(a: u8, b: u8, c: u8, d: u8) -> u32 {
    u32::from_ne_bytes([a, b, c, d])
}

/// Insert PASS (0) for 192.168.110.96 and DROP (1) for 100.112.108.92 on UDP 8080.
#[embassy_executor::task]
pub async fn seed_firewall_rules() {
    if !register_map_name("firewall_rules", MAP_ID_FIREWALL_RULES) {
        error!("[Seed][FW] register_map_name(firewall_rules) failed");
        return;
    }

    const DST_PORT: u16 = 8080;

    let allow = FirewallRulesKey {
        src_ip: ipv4(192, 168, 110, 96),
        dst_port: DST_PORT,
        pad: 0,
    };
    let deny = FirewallRulesKey {
        src_ip: ipv4(100, 112, 108, 92),
        dst_port: DST_PORT,
        pad: 0,
    };

    let ok = with_hash_map("firewall_rules", |m| {
        let r0 = m.insert_entry(allow, 0u8);
        let r1 = m.insert_entry(deny, 1u8);
        (r0, r1)
    })
    .await;

    match ok {
        Some((0, 0)) => info!("[Seed][FW] firewall_rules seeded (allow 96, deny 92 @ :8080)"),
        Some((r0, r1)) => error!("[Seed][FW] insert failed allow={} deny={}", r0, r1),
        None => error!("[Seed][FW] map firewall_rules not found"),
    }
}
