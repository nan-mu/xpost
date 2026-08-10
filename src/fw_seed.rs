//! Userspace map seeding from enabled `[[tests.seeds]]` in `config.toml`.

use crate::fw_cfg::{SEEDS, SeedOp, ipv4_u32};
use crate::patch::map::ops::MapOps;
use crate::patch::map::{ResourceId, register_map_name};
use log::{error, info};

fn map_id_for(name: &str) -> Option<ResourceId> {
    match name {
        #[cfg(fw_us_vip_seed)]
        "vip_ip" => Some(crate::patch::map::MAP_ID_VIP_IP),
        #[cfg(fw_us_firewall_seed)]
        "firewall_rules" => Some(crate::patch::map::MAP_ID_FIREWALL_RULES),
        _ => None,
    }
}

#[cfg(fw_us_vip_seed)]
fn smoke_vip_xdp(vip: [u8; 4]) {
    use crate::patch::redirect::{XDP_PASS, XDP_TX, xdp_action_name};

    fn smoke_frame(daddr: [u8; 4]) -> [u8; 34] {
        let mut frame = [0u8; 34];
        frame[0..6].copy_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        frame[6..12].copy_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        frame[12] = 0x08;
        frame[13] = 0x00;
        frame[14] = 0x45;
        frame[14 + 16..14 + 20].copy_from_slice(&daddr);
        frame
    }

    let mut vip_frame = smoke_frame(vip);
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

#[cfg(fw_us_firewall_seed)]
#[repr(C)]
struct FwKey {
    src_ip: u32,
    dst_port: u16,
    pad: u16,
}

#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn run_seeds() {
    for op in SEEDS {
        match *op {
            SeedOp::ArrayIp {
                map,
                index,
                ip,
                smoke,
            } => {
                #[cfg(fw_us_vip_seed)]
                {
                    use crate::patch::map::with_array_map;
                    let Some(id) = map_id_for(map) else {
                        error!("[Seed] no map id for {}", map);
                        continue;
                    };
                    if !register_map_name(map, id) {
                        error!("[Seed] register_map_name({}) failed", map);
                        continue;
                    }
                    let key = index;
                    let val = ipv4_u32(ip);
                    match with_array_map(map, |m| {
                        MapOps::update(
                            m,
                            (&raw const key) as *const u8,
                            (&raw const val) as *const u8,
                            0,
                        )
                    })
                    .await
                    {
                        Ok(Ok(())) => {
                            info!(
                                "[Seed] {}[{}]={}.{}.{}.{}",
                                map, index, ip[0], ip[1], ip[2], ip[3]
                            );
                            if smoke {
                                smoke_vip_xdp(ip);
                            }
                        }
                        Ok(Err(e)) => error!("[Seed] {} update err: {:?}", map, e),
                        Err(e) => error!("[Seed] {} busy/missing: {:?}", map, e),
                    }
                }
                #[cfg(not(fw_us_vip_seed))]
                {
                    let _ = (map, index, ip, smoke);
                    error!("[Seed] ArrayIp present but vip_seed inactive");
                }
            }
            SeedOp::FirewallRule {
                map,
                src_ip,
                dst_port,
                action,
            } => {
                #[cfg(fw_us_firewall_seed)]
                {
                    use crate::patch::map::with_hash_map;
                    let Some(id) = map_id_for(map) else {
                        error!("[Seed] no map id for {}", map);
                        continue;
                    };
                    if !register_map_name(map, id) {
                        error!("[Seed] register_map_name({}) failed", map);
                        continue;
                    }
                    let key = FwKey {
                        src_ip: ipv4_u32(src_ip),
                        dst_port,
                        pad: 0,
                    };
                    match with_hash_map(map, |m| {
                        MapOps::update(
                            m,
                            (&raw const key) as *const u8,
                            (&raw const action) as *const u8,
                            0,
                        )
                    })
                    .await
                    {
                        Ok(Ok(())) => info!(
                            "[Seed] {} DROP {}.{}.{}.{}:{} action={}",
                            map, src_ip[0], src_ip[1], src_ip[2], src_ip[3], dst_port, action
                        ),
                        Ok(Err(e)) => error!("[Seed] {} update err: {:?}", map, e),
                        Err(e) => error!("[Seed] {} busy/missing: {:?}", map, e),
                    }
                }
                #[cfg(not(fw_us_firewall_seed))]
                {
                    let _ = (map, src_ip, dst_port, action);
                    error!("[Seed] FirewallRule present but firewall_seed inactive");
                }
            }
        }
    }
}

#[cfg(feature = "patch-sync-st")]
pub fn run_seeds() {
    for op in SEEDS {
        match *op {
            SeedOp::ArrayIp {
                map,
                index,
                ip,
                smoke,
            } => {
                #[cfg(fw_us_vip_seed)]
                {
                    use crate::patch::map::with_array_map;
                    let Some(id) = map_id_for(map) else {
                        error!("[Seed] no map id for {}", map);
                        continue;
                    };
                    if !register_map_name(map, id) {
                        error!("[Seed] register_map_name({}) failed", map);
                        continue;
                    }
                    let key = index;
                    let val = ipv4_u32(ip);
                    match with_array_map(map, |m| {
                        MapOps::update(
                            m,
                            (&raw const key) as *const u8,
                            (&raw const val) as *const u8,
                            0,
                        )
                    }) {
                        Ok(Ok(())) => {
                            info!(
                                "[Seed] {}[{}]={}.{}.{}.{}",
                                map, index, ip[0], ip[1], ip[2], ip[3]
                            );
                            if smoke {
                                smoke_vip_xdp(ip);
                            }
                        }
                        Ok(Err(e)) => error!("[Seed] {} update err: {:?}", map, e),
                        Err(e) => error!("[Seed] {} busy/missing: {:?}", map, e),
                    }
                }
                #[cfg(not(fw_us_vip_seed))]
                {
                    let _ = (map, index, ip, smoke);
                    error!("[Seed] ArrayIp present but vip_seed inactive");
                }
            }
            SeedOp::FirewallRule {
                map,
                src_ip,
                dst_port,
                action,
            } => {
                #[cfg(fw_us_firewall_seed)]
                {
                    use crate::patch::map::with_hash_map;
                    let Some(id) = map_id_for(map) else {
                        error!("[Seed] no map id for {}", map);
                        continue;
                    };
                    if !register_map_name(map, id) {
                        error!("[Seed] register_map_name({}) failed", map);
                        continue;
                    }
                    let key = FwKey {
                        src_ip: ipv4_u32(src_ip),
                        dst_port,
                        pad: 0,
                    };
                    match with_hash_map(map, |m| {
                        MapOps::update(
                            m,
                            (&raw const key) as *const u8,
                            (&raw const action) as *const u8,
                            0,
                        )
                    }) {
                        Ok(Ok(())) => info!(
                            "[Seed] {} DROP {}.{}.{}.{}:{} action={}",
                            map, src_ip[0], src_ip[1], src_ip[2], src_ip[3], dst_port, action
                        ),
                        Ok(Err(e)) => error!("[Seed] {} update err: {:?}", map, e),
                        Err(e) => error!("[Seed] {} busy/missing: {:?}", map, e),
                    }
                }
                #[cfg(not(fw_us_firewall_seed))]
                {
                    let _ = (map, src_ip, dst_port, action);
                    error!("[Seed] FirewallRule present but firewall_seed inactive");
                }
            }
        }
    }
}
