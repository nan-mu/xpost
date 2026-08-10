//! Seed `vip_l2_redirect` map: `vip_ip[0]` = FW_VIP_A_IP (wire-order u32).

use crate::fw_cfg::{FW_VIP_A_IP, ipv4_u32};
use crate::patch::map::ops::MapOps;
use crate::patch::map::{MAP_ID_VIP_IP, register_map_name, with_array_map};
use crate::patch::redirect::{XDP_PASS, XDP_TX, xdp_action_name};
use log::{error, info};

fn register_or_log(name: &'static str, id: crate::patch::map::ResourceId) -> bool {
    if register_map_name(name, id) {
        true
    } else {
        error!("[Seed][VIP] register_map_name({}) failed", name);
        false
    }
}

fn seed_vip(m: &mut crate::patch::map::BpfArrayMap) {
    let key = 0u32;
    let vip = ipv4_u32(FW_VIP_A_IP);
    let _ = MapOps::update(
        m,
        (&raw const key) as *const u8,
        (&raw const vip) as *const u8,
        0,
    );
}

/// Minimal eth+IPv4 frame (no L4): set `daddr` at IPv4 offset 16.
fn smoke_frame(daddr: [u8; 4]) -> [u8; 34] {
    let mut frame = [0u8; 34];
    frame[0..6].copy_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
    frame[6..12].copy_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
    frame[12] = 0x08;
    frame[13] = 0x00;
    frame[14] = 0x45; // ver/ihl
    frame[14 + 16..14 + 20].copy_from_slice(&daddr);
    frame
}

/// After seed: VIP daddr → `XDP_TX`, non-VIP → `XDP_PASS` (serial evidence without TR3000).
pub fn smoke_vip_xdp() {
    let mut vip_frame = smoke_frame(FW_VIP_A_IP);
    let vip_rc = crate::patch::xdp(&mut vip_frame);
    if vip_rc == XDP_TX {
        info!(
            "[Seed][VIP] smoke vip→{} ok (MAC swapped)",
            xdp_action_name(vip_rc)
        );
    } else {
        error!(
            "[Seed][VIP] smoke vip→{} want XDP_TX",
            xdp_action_name(vip_rc)
        );
    }

    let mut other = smoke_frame([192, 168, 110, 103]);
    let other_rc = crate::patch::xdp(&mut other);
    if other_rc == XDP_PASS {
        info!("[Seed][VIP] smoke sta→XDP_PASS ok");
    } else {
        error!(
            "[Seed][VIP] smoke sta→{} want XDP_PASS",
            xdp_action_name(other_rc)
        );
    }
}

/// Bind name and write `vip_ip[0]=FW_VIP_A_IP`.
///
/// Must complete before WiFi/XDP RX is up.
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn seed_vip_redirect() {
    if !register_or_log("vip_ip", MAP_ID_VIP_IP) {
        return;
    }
    match with_array_map("vip_ip", seed_vip).await {
        Ok(()) => {
            info!(
                "[Seed][VIP] vip_ip[0]={}.{}.{}.{}",
                FW_VIP_A_IP[0], FW_VIP_A_IP[1], FW_VIP_A_IP[2], FW_VIP_A_IP[3]
            );
            smoke_vip_xdp();
        }
        Err(e) => error!("[Seed][VIP] vip_ip seed failed: {:?}", e),
    }
}

/// Bind name and write `vip_ip[0]=FW_VIP_A_IP`.
///
/// Must complete before WiFi/XDP RX is up.
#[cfg(feature = "patch-sync-st")]
pub fn seed_vip_redirect() {
    if !register_or_log("vip_ip", MAP_ID_VIP_IP) {
        return;
    }
    match with_array_map("vip_ip", seed_vip) {
        Ok(()) => {
            info!(
                "[Seed][VIP] vip_ip[0]={}.{}.{}.{}",
                FW_VIP_A_IP[0], FW_VIP_A_IP[1], FW_VIP_A_IP[2], FW_VIP_A_IP[3]
            );
            smoke_vip_xdp();
        }
        Err(e) => error!("[Seed][VIP] vip_ip seed failed: {:?}", e),
    }
}
