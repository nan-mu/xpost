//! Userspace map seeding from enabled `[[tests.seeds]]` in `config.toml`.

#[allow(unused_imports)]
use crate::fw_cfg::ipv4_u32;
use crate::fw_cfg::{SEEDS, SeedOp};
use log::{error, info};
use xpost::map::ops::MapOps;
use xpost::map::{ResourceId, register_map_name};

fn map_id_for(name: &str) -> Option<ResourceId> {
    match name {
        #[cfg(fw_us_vip_seed)]
        "vip_ip" => Some(xpost::map::MAP_ID_VIP_IP),
        #[cfg(fw_us_firewall_seed)]
        "firewall_rules" => Some(xpost::map::MAP_ID_FIREWALL_RULES),
        #[cfg(fw_us_fw_rules_seed)]
        "fw_rules" => Some(xpost::map::MAP_ID_FW_RULES),
        #[cfg(fw_us_firewall_array_seed)]
        "firewall_array" => Some(xpost::map::MAP_ID_FIREWALL_ARRAY),
        _ => None,
    }
}

#[cfg(any(fw_us_vip_seed, fw_us_fw_rules_seed))]
fn smoke_ipv4_frame(saddr: [u8; 4], daddr: [u8; 4]) -> [u8; 34] {
    let mut frame = [0u8; 34];
    frame[0..6].copy_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
    frame[6..12].copy_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
    frame[12] = 0x08;
    frame[13] = 0x00;
    frame[14] = 0x45;
    frame[14 + 12..14 + 16].copy_from_slice(&saddr);
    frame[14 + 16..14 + 20].copy_from_slice(&daddr);
    frame
}

#[cfg(fw_us_vip_seed)]
fn smoke_vip_xdp(vip: [u8; 4]) {
    use xpost::action::{XDP_PASS, XDP_TX, xdp_action_name};

    let mut vip_frame = smoke_ipv4_frame([192, 168, 110, 96], vip);
    let vip_rc = xpost::xdp(&mut vip_frame);
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

    let mut other = smoke_ipv4_frame([192, 168, 110, 96], [192, 168, 110, 103]);
    let other_rc = xpost::xdp(&mut other);
    if other_rc == XDP_PASS {
        info!("[Seed][VIP] smoke sta→XDP_PASS ok");
    } else {
        error!(
            "[Seed][VIP] smoke sta→{} want XDP_PASS",
            xdp_action_name(other_rc)
        );
    }
}

#[cfg(fw_us_fw_rules_seed)]
fn smoke_fw_rules_xdp(hit_src: [u8; 4]) {
    use xpost::action::{XDP_PASS, XDP_TX, xdp_action_name};

    let mut hit = smoke_ipv4_frame(hit_src, [192, 168, 110, 103]);
    let hit_rc = xpost::xdp(&mut hit);
    if hit_rc == XDP_TX {
        info!(
            "[Seed][FW] smoke {}→{} ok",
            format_ip(hit_src),
            xdp_action_name(hit_rc)
        );
    } else {
        error!(
            "[Seed][FW] smoke {}→{} want XDP_TX",
            format_ip(hit_src),
            xdp_action_name(hit_rc)
        );
    }

    let miss_src = [192, 168, 110, 33];
    let mut miss = smoke_ipv4_frame(miss_src, [192, 168, 110, 103]);
    let miss_rc = xpost::xdp(&mut miss);
    if miss_rc == XDP_PASS {
        info!("[Seed][FW] smoke miss {}→XDP_PASS ok", format_ip(miss_src));
    } else {
        error!(
            "[Seed][FW] smoke miss {}→{} want XDP_PASS",
            format_ip(miss_src),
            xdp_action_name(miss_rc)
        );
    }
}

#[cfg(fw_us_fw_rules_seed)]
fn format_ip(ip: [u8; 4]) -> heapless::String<16> {
    use core::fmt::Write;
    let mut s = heapless::String::new();
    let _ = write!(s, "{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
    s
}

#[cfg(fw_us_firewall_seed)]
#[repr(C)]
struct FwKey {
    src_ip: u32,
    dst_port: u16,
    pad: u16,
}

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
                    use xpost::map::with_array_map;
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
                    use xpost::map::with_hash_map;
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
            SeedOp::FwRulesEntry {
                map,
                src_ip,
                action,
            } => {
                #[cfg(fw_us_fw_rules_seed)]
                {
                    use xpost::map::with_hash_map;
                    let Some(id) = map_id_for(map) else {
                        error!("[Seed] no map id for {}", map);
                        continue;
                    };
                    if !register_map_name(map, id) {
                        error!("[Seed] register_map_name({}) failed", map);
                        continue;
                    }
                    let key = ipv4_u32(src_ip);
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
                        Ok(Ok(())) => {
                            info!(
                                "[Seed] {} key={}.{}.{}.{} action={}",
                                map, src_ip[0], src_ip[1], src_ip[2], src_ip[3], action
                            );
                            smoke_fw_rules_xdp(src_ip);
                        }
                        Ok(Err(e)) => error!("[Seed] {} update err: {:?}", map, e),
                        Err(e) => error!("[Seed] {} busy/missing: {:?}", map, e),
                    }
                }
                #[cfg(not(fw_us_fw_rules_seed))]
                {
                    let _ = (map, src_ip, action);
                    error!("[Seed] FwRulesEntry present but fw_rules_seed inactive");
                }
            }
            SeedOp::FirewallArrayEntry { map, index, ip } => {
                #[cfg(fw_us_firewall_array_seed)]
                {
                    use xpost::map::with_array_map;
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
                        Ok(Ok(())) => info!(
                            "[Seed] {}[{}]={}.{}.{}.{}",
                            map, index, ip[0], ip[1], ip[2], ip[3]
                        ),
                        Ok(Err(e)) => error!("[Seed] {} update err: {:?}", map, e),
                        Err(e) => error!("[Seed] {} busy/missing: {:?}", map, e),
                    }
                }
                #[cfg(not(fw_us_firewall_array_seed))]
                {
                    let _ = (map, index, ip);
                    error!("[Seed] FirewallArrayEntry present but firewall_array_seed inactive");
                }
            }
        }
    }
}
