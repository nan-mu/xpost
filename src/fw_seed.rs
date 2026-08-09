//! Seed `firewall_rules`: DROP blocked source IP on UDP :8080.

use crate::fw_cfg::{FW_BLOCKED_SRC_IP, FW_LISTEN_PORT, ipv4_u32, log_cfg};
use crate::patch::map::{
    FirewallRulesKey, MAP_ID_FIREWALL_RULES, register_map_name, with_hash_map,
};
use log::{error, info};

/// Insert DROP (1) for `FW_BLOCKED_SRC_IP` on UDP listen port.
///
/// Must complete before WiFi/XDP RX is up; otherwise an empty `firewall_rules`
/// map fail-opens blocked traffic.
pub async fn seed_firewall_rules() {
    log_cfg();

    if !register_map_name("firewall_rules", MAP_ID_FIREWALL_RULES) {
        error!("[Seed][FW] register_map_name(firewall_rules) failed");
        return;
    }

    let drop_key = FirewallRulesKey {
        src_ip: ipv4_u32(FW_BLOCKED_SRC_IP),
        dst_port: FW_LISTEN_PORT,
        pad: 0,
    };

    match with_hash_map("firewall_rules", |m| {
        let t = m.table_mut();
        t.insert(drop_key.into(), 1u8.into());
    })
    .await
    {
        Ok(()) => info!(
            "[Seed][FW] drop rule blocked={}.{}.{}.{} :{}",
            FW_BLOCKED_SRC_IP[0],
            FW_BLOCKED_SRC_IP[1],
            FW_BLOCKED_SRC_IP[2],
            FW_BLOCKED_SRC_IP[3],
            FW_LISTEN_PORT
        ),
        Err(e) => error!("[Seed][FW] seed failed: {:?}", e),
    }
}
