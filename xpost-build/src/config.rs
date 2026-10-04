//! Parse `config.toml` → `fw_cfg_data.rs` + enabled `fw_us_*` names.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::util::{escape_rust_str, parse_toml_ipv4, parse_toml_ipv4_value, parse_toml_u16};
use crate::{KNOWN_TESTS, MAX_CONFIG_SCHEMA, US_CFGS};

#[derive(Clone)]
enum SeedSpec {
    VipArray {
        map: String,
        index: u32,
        ip: [u8; 4],
        smoke: bool,
    },
    FirewallHash {
        map: String,
        src_ip: [u8; 4],
        dst_port: u16,
        action: u8,
    },
    /// HASH `fw_rules`: key = src IPv4 wire `u32`, value = u8 placeholder.
    FwRulesHash {
        map: String,
        src_ip: [u8; 4],
        action: u8,
    },
    /// ARRAY `firewall_array`: index + banned IPv4 (test0 BAN_IP).
    FirewallArray {
        map: String,
        index: u32,
        ip: [u8; 4],
    },
}

pub struct ConfigGen {
    pub enabled_us_cfgs: Vec<String>,
    pub warnings: Vec<String>,
}

/// Parse `config.toml` → enabled `fw_us_*` + `out_dir/fw_cfg_data.rs`.
pub fn generate(manifest_dir: &Path, out_dir: &Path) -> Result<ConfigGen> {
    let mut warnings = Vec::new();
    let path = manifest_dir.join("config.toml");
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let root: toml::Table = text
        .parse()
        .with_context(|| "failed to parse config.toml")?;

    let schema = root
        .get("schema_version")
        .and_then(|v| v.as_integer())
        .ok_or_else(|| anyhow::anyhow!("config.toml: missing required schema_version"))?;
    if schema < 1 {
        bail!("config.toml: schema_version must be >= 1, got {schema}");
    }
    if schema > MAX_CONFIG_SCHEMA {
        bail!(
            "config.toml: schema_version {schema} > MAX {MAX_CONFIG_SCHEMA} — upgrade xpost-build"
        );
    }

    let known_top = [
        "schema_version",
        "wifi",
        "probe",
        "dut",
        "tests",
        "arena",
        "helper",
        "slots",
    ];
    for (key, _) in &root {
        if !known_top.contains(&key.as_str()) {
            warnings.push(format!(
                "config.toml: ignoring unknown top-level key `{key}`"
            ));
        }
    }
    for key in ["arena", "helper", "slots"] {
        if root.get(key).is_some() {
            warnings.push(format!(
                "config.toml: [{key}] ignored — layout lives in layout.toml"
            ));
        }
    }

    let wifi = root
        .get("wifi")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow::anyhow!("config.toml: missing [wifi]"))?;
    let ssid = wifi
        .get("ssid")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("config.toml: [wifi].ssid required"))?;
    let password = wifi
        .get("password")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("config.toml: [wifi].password required"))?;
    for (key, _) in wifi {
        if key != "ssid" && key != "password" {
            warnings.push(format!("config.toml: ignoring unknown [wifi].{key}"));
        }
    }

    let probe = root
        .get("probe")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow::anyhow!("config.toml: missing [probe]"))?;
    let probe_ip = parse_toml_ipv4(probe, "ip", "probe");
    let reply_port = parse_toml_u16(probe, "reply_port", "probe", 18080);
    for (key, _) in probe {
        if key != "ip" && key != "reply_port" {
            warnings.push(format!("config.toml: ignoring unknown [probe].{key}"));
        }
    }

    let dut = root
        .get("dut")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow::anyhow!("config.toml: missing [dut]"))?;
    let listen_port = parse_toml_u16(dut, "listen_port", "dut", 8080);
    let ctrl_port = parse_toml_u16(dut, "ctrl_port", "dut", 8081);
    for (key, _) in dut {
        if key != "listen_port" && key != "ctrl_port" {
            warnings.push(format!("config.toml: ignoring unknown [dut].{key}"));
        }
    }

    let mut vip_seed = false;
    let mut hot_load = false;
    let mut firewall_seed = false;
    let mut fw_rules_seed = false;
    let mut firewall_array_seed = false;
    let mut hw_selftest = false;
    let mut map_crud = false;
    let mut net_test = false;
    let mut cold_boot = false;
    let mut seeds: Vec<SeedSpec> = Vec::new();

    // Empty / omitted [[tests]] is allowed (Phase A layout-only configs).
    let tests = root
        .get("tests")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    for (ti, entry) in tests.iter().enumerate() {
        let table = entry
            .as_table()
            .ok_or_else(|| anyhow::anyhow!("config.toml: [[tests]][{ti}] must be a table"))?;
        let name = table
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("config.toml: [[tests]][{ti}].name required"))?;
        let enabled = table
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        for (key, _) in table {
            if !matches!(key.as_str(), "name" | "enabled" | "seeds") {
                warnings.push(format!(
                    "config.toml: ignoring unknown [[tests]][{ti}].{key}"
                ));
            }
        }

        if !KNOWN_TESTS.contains(&name) {
            if enabled {
                bail!(
                    "config.toml: unknown enabled test `{name}`; known: {}",
                    KNOWN_TESTS.join(", ")
                );
            }
            warnings.push(format!(
                "config.toml: ignoring unknown disabled test `{name}`"
            ));
            continue;
        }

        if !enabled {
            continue;
        }

        match name {
            "hw_selftest" => hw_selftest = true,
            "hot_load_ctrl" => {
                hot_load = true;
                net_test = true;
            }
            "fw_hash_test" => {
                map_crud = true;
                hot_load = true;
                net_test = true;
            }
            "tail_call_abc" => {
                cold_boot = true;
                map_crud = true;
                hot_load = true;
                net_test = true;
            }
            "test0" => {
                map_crud = true;
                hot_load = true;
                net_test = true;
            }
            "drop_xdp" => {
                net_test = true;
            }
            "xdp_forward_echo" => net_test = true,
            "firewall_echo" => net_test = true,
            _ => {}
        }

        let Some(seed_arr) = table.get("seeds").and_then(|v| v.as_array()) else {
            continue;
        };
        for (si, seed_val) in seed_arr.iter().enumerate() {
            let st = seed_val.as_table().ok_or_else(|| {
                anyhow::anyhow!("config.toml: [[tests]] `{name}` seeds[{si}] must be a table")
            })?;
            let map = st.get("map").and_then(|v| v.as_str()).ok_or_else(|| {
                anyhow::anyhow!("config.toml: [[tests]] `{name}` seeds[{si}].map required")
            })?;
            match map {
                "vip_ip" => {
                    let index = st.get("index").and_then(|v| v.as_integer()).unwrap_or(0) as u32;
                    let ip = parse_toml_ipv4_value(
                        st.get("ip").ok_or_else(|| {
                            anyhow::anyhow!(
                                "config.toml: [[tests]] `{name}` seeds[{si}].ip required for vip_ip"
                            )
                        })?,
                        &format!("tests.{name}.seeds[{si}].ip"),
                    );
                    let smoke = st.get("smoke").and_then(|v| v.as_bool()).unwrap_or(false);
                    for (key, _) in st {
                        if !matches!(key.as_str(), "map" | "index" | "ip" | "smoke") {
                            warnings.push(format!(
                                "config.toml: ignoring unknown seeds[{si}].{key} in `{name}`"
                            ));
                        }
                    }
                    vip_seed = true;
                    seeds.push(SeedSpec::VipArray {
                        map: map.to_string(),
                        index,
                        ip,
                        smoke,
                    });
                }
                "firewall_rules" => {
                    let src_ip = parse_toml_ipv4_value(
                        st.get("src_ip").ok_or_else(|| {
                            anyhow::anyhow!(
                                "config.toml: [[tests]] `{name}` seeds[{si}].src_ip required for firewall_rules"
                            )
                        })?,
                        &format!("tests.{name}.seeds[{si}].src_ip"),
                    );
                    let dst_port = st
                        .get("dst_port")
                        .and_then(|v| v.as_integer())
                        .map(|n| {
                            assert!(
                                (0..=u16::MAX as i64).contains(&n),
                                "seeds[{si}].dst_port out of range"
                            );
                            n as u16
                        })
                        .unwrap_or(listen_port);
                    let action = st
                        .get("action")
                        .and_then(|v| v.as_integer())
                        .map(|n| {
                            assert!((0..=255).contains(&n), "seeds[{si}].action must be u8");
                            n as u8
                        })
                        .unwrap_or(1);
                    for (key, _) in st {
                        if !matches!(key.as_str(), "map" | "src_ip" | "dst_port" | "action") {
                            warnings.push(format!(
                                "config.toml: ignoring unknown seeds[{si}].{key} in `{name}`"
                            ));
                        }
                    }
                    firewall_seed = true;
                    seeds.push(SeedSpec::FirewallHash {
                        map: map.to_string(),
                        src_ip,
                        dst_port,
                        action,
                    });
                }
                "fw_rules" => {
                    let src_ip = parse_toml_ipv4_value(
                        st.get("src_ip").ok_or_else(|| {
                            anyhow::anyhow!(
                                "config.toml: [[tests]] `{name}` seeds[{si}].src_ip required for fw_rules"
                            )
                        })?,
                        &format!("tests.{name}.seeds[{si}].src_ip"),
                    );
                    let action = st
                        .get("action")
                        .and_then(|v| v.as_integer())
                        .map(|n| {
                            assert!((0..=255).contains(&n), "seeds[{si}].action must be u8");
                            n as u8
                        })
                        .unwrap_or(1);
                    for (key, _) in st {
                        if !matches!(key.as_str(), "map" | "src_ip" | "action") {
                            warnings.push(format!(
                                "config.toml: ignoring unknown seeds[{si}].{key} in `{name}`"
                            ));
                        }
                    }
                    fw_rules_seed = true;
                    seeds.push(SeedSpec::FwRulesHash {
                        map: map.to_string(),
                        src_ip,
                        action,
                    });
                }
                "firewall_array" => {
                    let index = st.get("index").and_then(|v| v.as_integer()).unwrap_or(0) as u32;
                    let ip = parse_toml_ipv4_value(
                        st.get("ip").ok_or_else(|| {
                            anyhow::anyhow!(
                                "config.toml: [[tests]] `{name}` seeds[{si}].ip required for firewall_array"
                            )
                        })?,
                        &format!("tests.{name}.seeds[{si}].ip"),
                    );
                    for (key, _) in st {
                        if !matches!(key.as_str(), "map" | "index" | "ip") {
                            warnings.push(format!(
                                "config.toml: ignoring unknown seeds[{si}].{key} in `{name}`"
                            ));
                        }
                    }
                    firewall_array_seed = true;
                    seeds.push(SeedSpec::FirewallArray {
                        map: map.to_string(),
                        index,
                        ip,
                    });
                }
                other => bail!(
                    "config.toml: [[tests]] `{name}` seeds[{si}].map `{other}` unsupported \
                     (known: vip_ip, firewall_rules, fw_rules, firewall_array)"
                ),
            }
        }
    }

    if hw_selftest && net_test {
        bail!("config.toml: hw_selftest cannot be enabled together with network tests");
    }
    if cold_boot && (vip_seed || firewall_seed || fw_rules_seed || firewall_array_seed) {
        bail!("config.toml: cold_boot (tail_call_abc) cannot combine with boot map seeds");
    }
    let seed_kinds = [vip_seed, firewall_seed, fw_rules_seed, firewall_array_seed]
        .into_iter()
        .filter(|x| *x)
        .count();
    if seed_kinds > 1 {
        bail!(
            "config.toml: vip_ip / firewall_rules / fw_rules seeds are mutually exclusive across enabled tests"
        );
    }

    let xdp_rx = net_test;

    let mut enabled = BTreeMap::new();
    enabled.insert("vip_seed", vip_seed);
    enabled.insert("hot_load", hot_load);
    enabled.insert("firewall_seed", firewall_seed);
    enabled.insert("fw_rules_seed", fw_rules_seed);
    enabled.insert("firewall_array_seed", firewall_array_seed);
    enabled.insert("map_crud", map_crud);
    enabled.insert("xdp_rx", xdp_rx);
    enabled.insert("hw_selftest", hw_selftest);
    enabled.insert("cold_boot", cold_boot);

    let mut enabled_us_cfgs = Vec::new();
    for name in US_CFGS {
        if *enabled.get(name).unwrap() {
            enabled_us_cfgs.push((*name).to_string());
        }
    }

    write_fw_cfg_data(
        out_dir,
        ssid,
        password,
        probe_ip,
        reply_port,
        listen_port,
        ctrl_port,
        xdp_rx,
        &seeds,
    )?;

    Ok(ConfigGen {
        enabled_us_cfgs,
        warnings,
    })
}

#[allow(clippy::too_many_arguments)]
fn write_fw_cfg_data(
    out_dir: &Path,
    ssid: &str,
    password: &str,
    probe_ip: [u8; 4],
    reply_port: u16,
    listen_port: u16,
    ctrl_port: u16,
    xdp_rx: bool,
    seeds: &[SeedSpec],
) -> Result<()> {
    let mut out = String::from("// @generated by xpost-build from config.toml — do not edit\n\n");

    out.push_str("pub mod wifi {\n");
    out.push_str(&format!(
        "    pub const SSID: &str = \"{}\";\n",
        escape_rust_str(ssid)
    ));
    out.push_str(&format!(
        "    pub const PASSWORD: &str = \"{}\";\n",
        escape_rust_str(password)
    ));
    out.push_str("}\n\n");

    out.push_str("pub mod probe {\n");
    out.push_str(&format!(
        "    pub const IP: [u8; 4] = [{}, {}, {}, {}];\n",
        probe_ip[0], probe_ip[1], probe_ip[2], probe_ip[3]
    ));
    out.push_str(&format!("    pub const REPLY_PORT: u16 = {reply_port};\n"));
    out.push_str("}\n\n");

    out.push_str("pub mod dut {\n");
    out.push_str(&format!(
        "    pub const LISTEN_PORT: u16 = {listen_port};\n"
    ));
    out.push_str(&format!("    pub const CTRL_PORT: u16 = {ctrl_port};\n"));
    out.push_str("}\n\n");

    out.push_str(&format!(
        "/// AOT XDP on WiFi RX (derived from enabled network [[tests]]).\n\
         pub const XDP_RX: bool = {xdp_rx};\n\n"
    ));

    out.push_str(
        "/// Boot-time map seed ops from enabled `[[tests.seeds]]`.\n\
         #[derive(Clone, Copy)]\n\
         pub enum SeedOp {\n\
             /// ARRAY map: key = index, value = IPv4 as wire-order `u32`.\n\
             ArrayIp {\n\
                 map: &'static str,\n\
                 index: u32,\n\
                 ip: [u8; 4],\n\
                 smoke: bool,\n\
             },\n\
             /// HASH `firewall_rules`: key = (src_ip, dst_port, pad), value = action.\n\
             FirewallRule {\n\
                 map: &'static str,\n\
                 src_ip: [u8; 4],\n\
                 dst_port: u16,\n\
                 action: u8,\n\
             },\n\
             /// HASH `fw_rules`: key = src IPv4 wire u32, value = u8.\n\
             FwRulesEntry {\n\
                 map: &'static str,\n\
                 src_ip: [u8; 4],\n\
                 action: u8,\n\
             },\n\
             /// ARRAY `firewall_array`: banned IPv4 at index.\n\
             FirewallArrayEntry {\n\
                 map: &'static str,\n\
                 index: u32,\n\
                 ip: [u8; 4],\n\
             },\n\
         }\n\n",
    );

    out.push_str("pub const SEEDS: &[SeedOp] = &[\n");
    for s in seeds {
        match s {
            SeedSpec::VipArray {
                map,
                index,
                ip,
                smoke,
            } => {
                out.push_str(&format!(
                    "    SeedOp::ArrayIp {{\n        map: \"{}\",\n        index: {index},\n        ip: [{}, {}, {}, {}],\n        smoke: {smoke},\n    }},\n",
                    escape_rust_str(map),
                    ip[0],
                    ip[1],
                    ip[2],
                    ip[3],
                ));
            }
            SeedSpec::FirewallHash {
                map,
                src_ip,
                dst_port,
                action,
            } => {
                out.push_str(&format!(
                    "    SeedOp::FirewallRule {{\n        map: \"{}\",\n        src_ip: [{}, {}, {}, {}],\n        dst_port: {dst_port},\n        action: {action},\n    }},\n",
                    escape_rust_str(map),
                    src_ip[0],
                    src_ip[1],
                    src_ip[2],
                    src_ip[3],
                ));
            }
            SeedSpec::FwRulesHash {
                map,
                src_ip,
                action,
            } => {
                out.push_str(&format!(
                    "    SeedOp::FwRulesEntry {{\n        map: \"{}\",\n        src_ip: [{}, {}, {}, {}],\n        action: {action},\n    }},\n",
                    escape_rust_str(map),
                    src_ip[0],
                    src_ip[1],
                    src_ip[2],
                    src_ip[3],
                ));
            }
            SeedSpec::FirewallArray { map, index, ip } => {
                out.push_str(&format!(
                    "    SeedOp::FirewallArrayEntry {{\n        map: \"{}\",\n        index: {index},\n        ip: [{}, {}, {}, {}],\n    }},\n",
                    escape_rust_str(map),
                    ip[0],
                    ip[1],
                    ip[2],
                    ip[3],
                ));
            }
        }
    }
    out.push_str("];\n");

    fs::write(out_dir.join("fw_cfg_data.rs"), out).context("write fw_cfg_data.rs")?;
    Ok(())
}
