use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

/// Must match `HEAP_SIZE` in `src/patch/map.rs`.
const HEAP_SIZE: usize = 65536;
/// Reserved for talc metadata + registry HashMap overhead inside the pool.
const POOL_RESERVED: usize = 2048;

/// Highest `schema_version` this `build.rs` understands.
const MAX_CONFIG_SCHEMA: i64 = 1;

/// Userspace cfgs derived from enabled `[[tests]]`.
const US_CFGS: &[&str] = &[
    "vip_seed",
    "hot_load",
    "firewall_seed",
    "fw_rules_seed",
    "xdp_rx",
    "hw_selftest",
    "map_crud",
];

/// Known `[[tests]].name` values (router-probe registry alignment).
const KNOWN_TESTS: &[&str] = &[
    "firewall_echo",
    "xdp_forward_echo",
    "hot_load_ctrl",
    "fw_hash_test",
    "hw_selftest",
];

/// `(async_kw, await_suffix)` for `init_firmware_maps` under patch-sync features.
fn map_init_asyncness() -> (&'static str, &'static str) {
    let st = env::var_os("CARGO_FEATURE_PATCH_SYNC_ST").is_some();
    if st { ("", "") } else { ("async ", ".await") }
}

fn main() {
    linker_be_nice();

    println!("cargo:rerun-if-changed=esp32c3.bin");
    println!("cargo:rerun-if-changed=bpf.toml");
    println!("cargo:rerun-if-changed=mem_map.toml");
    println!("cargo:rerun-if-changed=config.toml");

    apply_userspace_config();
    generate_mem_map();
    generate_bpf_maps();
    generate_aot_map_bind();

    println!("cargo:rustc-link-arg=-Tlinkall.x");
}

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
}

/// Parse `config.toml` → `fw_us_*` cfgs + `OUT_DIR/fw_cfg_data.rs`.
fn apply_userspace_config() {
    for name in US_CFGS {
        println!("cargo:rustc-check-cfg=cfg(fw_us_{name})");
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let path = manifest_dir.join("config.toml");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let root: toml::Table = text
        .parse()
        .unwrap_or_else(|e| panic!("failed to parse config.toml: {e}"));

    let schema = root
        .get("schema_version")
        .and_then(|v| v.as_integer())
        .unwrap_or_else(|| panic!("config.toml: missing required schema_version"));
    if schema < 1 {
        panic!("config.toml: schema_version must be >= 1, got {schema}");
    }
    if schema > MAX_CONFIG_SCHEMA {
        panic!(
            "config.toml: schema_version {schema} > MAX {MAX_CONFIG_SCHEMA} — upgrade firmware build.rs"
        );
    }

    let known_top = ["schema_version", "wifi", "probe", "dut", "tests"];
    for (key, _) in &root {
        if !known_top.contains(&key.as_str()) {
            println!("cargo:warning=config.toml: ignoring unknown top-level key `{key}`");
        }
    }

    let wifi = root
        .get("wifi")
        .and_then(|v| v.as_table())
        .unwrap_or_else(|| panic!("config.toml: missing [wifi]"));
    let ssid = wifi
        .get("ssid")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("config.toml: [wifi].ssid required"));
    let password = wifi
        .get("password")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("config.toml: [wifi].password required"));
    for (key, _) in wifi {
        if key != "ssid" && key != "password" {
            println!("cargo:warning=config.toml: ignoring unknown [wifi].{key}");
        }
    }

    let probe = root
        .get("probe")
        .and_then(|v| v.as_table())
        .unwrap_or_else(|| panic!("config.toml: missing [probe]"));
    let probe_ip = parse_toml_ipv4(probe, "ip", "probe");
    let reply_port = parse_toml_u16(probe, "reply_port", "probe", 18080);
    for (key, _) in probe {
        if key != "ip" && key != "reply_port" {
            println!("cargo:warning=config.toml: ignoring unknown [probe].{key}");
        }
    }

    let dut = root
        .get("dut")
        .and_then(|v| v.as_table())
        .unwrap_or_else(|| panic!("config.toml: missing [dut]"));
    let listen_port = parse_toml_u16(dut, "listen_port", "dut", 8080);
    let ctrl_port = parse_toml_u16(dut, "ctrl_port", "dut", 8081);
    for (key, _) in dut {
        if key != "listen_port" && key != "ctrl_port" {
            println!("cargo:warning=config.toml: ignoring unknown [dut].{key}");
        }
    }

    let mut vip_seed = false;
    let mut hot_load = false;
    let mut firewall_seed = false;
    let mut fw_rules_seed = false;
    let mut hw_selftest = false;
    let mut map_crud = false;
    let mut net_test = false;
    let mut seeds: Vec<SeedSpec> = Vec::new();

    let tests = root
        .get("tests")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("config.toml: at least one [[tests]] required"));

    for (ti, entry) in tests.iter().enumerate() {
        let table = entry
            .as_table()
            .unwrap_or_else(|| panic!("config.toml: [[tests]][{ti}] must be a table"));
        let name = table
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("config.toml: [[tests]][{ti}].name required"));
        let enabled = table
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        for (key, _) in table {
            if !matches!(key.as_str(), "name" | "enabled" | "seeds") {
                println!("cargo:warning=config.toml: ignoring unknown [[tests]][{ti}].{key}");
            }
        }

        if !KNOWN_TESTS.contains(&name) {
            if enabled {
                panic!(
                    "config.toml: unknown enabled test `{name}`; known: {}",
                    KNOWN_TESTS.join(", ")
                );
            }
            println!("cargo:warning=config.toml: ignoring unknown disabled test `{name}`");
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
            "xdp_forward_echo" => net_test = true,
            "firewall_echo" => net_test = true,
            _ => {}
        }

        let Some(seed_arr) = table.get("seeds").and_then(|v| v.as_array()) else {
            continue;
        };
        for (si, seed_val) in seed_arr.iter().enumerate() {
            let st = seed_val.as_table().unwrap_or_else(|| {
                panic!("config.toml: [[tests]] `{name}` seeds[{si}] must be a table")
            });
            let map = st.get("map").and_then(|v| v.as_str()).unwrap_or_else(|| {
                panic!("config.toml: [[tests]] `{name}` seeds[{si}].map required")
            });
            match map {
                "vip_ip" => {
                    let index = st.get("index").and_then(|v| v.as_integer()).unwrap_or(0) as u32;
                    let ip = parse_toml_ipv4_value(
                        st.get("ip").unwrap_or_else(|| {
                            panic!(
                                "config.toml: [[tests]] `{name}` seeds[{si}].ip required for vip_ip"
                            )
                        }),
                        &format!("tests.{name}.seeds[{si}].ip"),
                    );
                    let smoke = st.get("smoke").and_then(|v| v.as_bool()).unwrap_or(false);
                    for (key, _) in st {
                        if !matches!(key.as_str(), "map" | "index" | "ip" | "smoke") {
                            println!(
                                "cargo:warning=config.toml: ignoring unknown seeds[{si}].{key} in `{name}`"
                            );
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
                        st.get("src_ip").unwrap_or_else(|| {
                            panic!(
                                "config.toml: [[tests]] `{name}` seeds[{si}].src_ip required for firewall_rules"
                            )
                        }),
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
                            println!(
                                "cargo:warning=config.toml: ignoring unknown seeds[{si}].{key} in `{name}`"
                            );
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
                        st.get("src_ip").unwrap_or_else(|| {
                            panic!(
                                "config.toml: [[tests]] `{name}` seeds[{si}].src_ip required for fw_rules"
                            )
                        }),
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
                            println!(
                                "cargo:warning=config.toml: ignoring unknown seeds[{si}].{key} in `{name}`"
                            );
                        }
                    }
                    fw_rules_seed = true;
                    seeds.push(SeedSpec::FwRulesHash {
                        map: map.to_string(),
                        src_ip,
                        action,
                    });
                }
                other => panic!(
                    "config.toml: [[tests]] `{name}` seeds[{si}].map `{other}` unsupported \
                     (known: vip_ip, firewall_rules, fw_rules)"
                ),
            }
        }
    }

    if hw_selftest && net_test {
        panic!("config.toml: hw_selftest cannot be enabled together with network tests");
    }
    let seed_kinds = [vip_seed, firewall_seed, fw_rules_seed]
        .into_iter()
        .filter(|x| *x)
        .count();
    if seed_kinds > 1 {
        panic!(
            "config.toml: vip_ip / firewall_rules / fw_rules seeds are mutually exclusive across enabled tests"
        );
    }

    let xdp_rx = net_test;

    let mut enabled = BTreeMap::new();
    enabled.insert("vip_seed", vip_seed);
    enabled.insert("hot_load", hot_load);
    enabled.insert("firewall_seed", firewall_seed);
    enabled.insert("fw_rules_seed", fw_rules_seed);
    enabled.insert("map_crud", map_crud);
    enabled.insert("xdp_rx", xdp_rx);
    enabled.insert("hw_selftest", hw_selftest);

    for name in US_CFGS {
        if *enabled.get(name).unwrap() {
            println!("cargo:rustc-cfg=fw_us_{name}");
        }
    }

    write_fw_cfg_data(
        &out_dir,
        ssid,
        password,
        probe_ip,
        reply_port,
        listen_port,
        ctrl_port,
        xdp_rx,
        &seeds,
    );
}

fn write_fw_cfg_data(
    out_dir: &PathBuf,
    ssid: &str,
    password: &str,
    probe_ip: [u8; 4],
    reply_port: u16,
    listen_port: u16,
    ctrl_port: u16,
    xdp_rx: bool,
    seeds: &[SeedSpec],
) {
    let mut out = String::from("// @generated by build.rs from config.toml — do not edit\n\n");

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
        }
    }
    out.push_str("];\n");

    fs::write(out_dir.join("fw_cfg_data.rs"), out)
        .unwrap_or_else(|e| panic!("write fw_cfg_data.rs: {e}"));
}

fn escape_rust_str(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\n' => "\\n".chars().collect::<Vec<_>>(),
            '\r' => "\\r".chars().collect::<Vec<_>>(),
            '\t' => "\\t".chars().collect::<Vec<_>>(),
            c => vec![c],
        })
        .collect()
}

fn parse_toml_u16(table: &toml::Table, key: &str, section: &str, default: u16) -> u16 {
    let Some(v) = table.get(key) else {
        return default;
    };
    let n = v
        .as_integer()
        .unwrap_or_else(|| panic!("config.toml: [{section}].{key} must be integer"));
    assert!(
        (0..=u16::MAX as i64).contains(&n),
        "config.toml: [{section}].{key} out of u16 range"
    );
    n as u16
}

fn parse_toml_ipv4(table: &toml::Table, key: &str, section: &str) -> [u8; 4] {
    let v = table
        .get(key)
        .unwrap_or_else(|| panic!("config.toml: [{section}].{key} required"));
    parse_toml_ipv4_value(v, &format!("{section}.{key}"))
}

fn parse_toml_ipv4_value(v: &toml::Value, ctx: &str) -> [u8; 4] {
    let s = v
        .as_str()
        .unwrap_or_else(|| panic!("config.toml: {ctx} must be dotted IPv4 string"));
    parse_dotted_ipv4_build(s, ctx)
}

fn parse_dotted_ipv4_build(s: &str, ctx: &str) -> [u8; 4] {
    let parts: Vec<&str> = s.split('.').collect();
    assert!(parts.len() == 4, "config.toml: {ctx}: bad IPv4 `{s}`");
    let mut out = [0u8; 4];
    for (i, p) in parts.iter().enumerate() {
        out[i] = p
            .parse::<u8>()
            .unwrap_or_else(|_| panic!("config.toml: {ctx}: bad octet `{p}`"));
    }
    out
}

fn generate_mem_map() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let map_path = manifest_dir.join("mem_map.toml");
    let text = fs::read_to_string(&map_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", map_path.display()));
    let root: toml::Table = text
        .parse()
        .unwrap_or_else(|e| panic!("failed to parse mem_map.toml: {e}"));

    let helper = root
        .get("helper")
        .and_then(|v| v.as_table())
        .expect("mem_map.toml: missing [helper]");
    let hot_a = root
        .get("hot_region_a")
        .and_then(|v| v.as_table())
        .expect("mem_map.toml: missing [hot_region_a]");
    let hot_b = root
        .get("hot_region_b")
        .and_then(|v| v.as_table())
        .expect("mem_map.toml: missing [hot_region_b]");

    let helper_base = parse_addr(helper, "base", "helper");
    let helper_len = parse_addr(helper, "length", "helper");
    let hot_a_base = parse_addr(hot_a, "base", "hot_region_a");
    let hot_a_len = parse_addr(hot_a, "length", "hot_region_a");
    let hot_b_base = parse_addr(hot_b, "base", "hot_region_b");
    let hot_b_len = parse_addr(hot_b, "length", "hot_region_b");

    assert!(
        helper_base.is_multiple_of(4)
            && hot_a_base.is_multiple_of(4)
            && hot_b_base.is_multiple_of(4),
        "mem_map bases must be 4-byte aligned"
    );
    assert!(
        helper_len > 0 && hot_a_len > 0 && hot_b_len > 0,
        "mem_map lengths must be > 0"
    );

    let helper_end = helper_base + helper_len;
    let hot_a_end = hot_a_base + hot_a_len;
    let hot_b_end = hot_b_base + hot_b_len;

    assert_eq!(
        helper_end, hot_a_base,
        "helper window must abut HOT_A (helper_end={helper_end:#x} HOT_A={hot_a_base:#x})"
    );
    assert_eq!(
        hot_a_end, hot_b_base,
        "HOT_A must abut HOT_B (HOT_A_end={hot_a_end:#x} HOT_B={hot_b_base:#x})"
    );
    assert_eq!(
        helper_len + hot_a_len + hot_b_len,
        0x2000,
        "helper+HOT_A+HOT_B must total 8KiB"
    );
    assert_eq!(
        hot_b_end, 0x4038_2000,
        "patch arena must end at 0x40382000 (.trap); got {hot_b_end:#x}"
    );

    let rs = format!(
        "// Auto-generated from mem_map.toml — do not edit.\n\
         \n\
         /// Absolute IRAM address of `helper_dispatcher`.\n\
         pub const HELPER_DISPATCHER_BASE: usize = {helper_base:#x};\n\
         /// Reserved IRAM window size for the helper trampoline.\n\
         pub const HELPER_WINDOW_LEN: usize = {helper_len:#x};\n\
         /// Absolute IRAM load / entry address for slot A (RX default).\n\
         pub const HOT_A_BASE: usize = {hot_a_base:#x};\n\
         /// Firmware-owned HOT_A buffer size.\n\
         pub const HOT_A_LEN: usize = {hot_a_len:#x};\n\
         /// Absolute IRAM load / entry address for slot B (hot-replace / tail_call).\n\
         pub const HOT_B_BASE: usize = {hot_b_base:#x};\n\
         /// Firmware-owned HOT_B buffer size.\n\
         pub const HOT_B_LEN: usize = {hot_b_len:#x};\n\
         /// Alias: slot A base (boot / RX entry).\n\
         pub const HOT_REGION_BASE: usize = HOT_A_BASE;\n\
         /// Alias: slot A length.\n\
         pub const HOT_REGION_LEN: usize = HOT_A_LEN;\n\
         \n\
         /// Panic if linker placed symbols off the mem_map contract.\n\
         pub fn assert_linked_addresses(helper_addr: usize, hot_a_addr: usize, hot_b_addr: usize) {{\n\
             assert_eq!(\n\
                 helper_addr, HELPER_DISPATCHER_BASE,\n\
                 \"helper_dispatcher linked at {{helper_addr:#x}}, expected {{HELPER_DISPATCHER_BASE:#x}}\"\n\
             );\n\
             assert_eq!(\n\
                 hot_a_addr, HOT_A_BASE,\n\
                 \"HOT_A linked at {{hot_a_addr:#x}}, expected {{HOT_A_BASE:#x}}\"\n\
             );\n\
             assert_eq!(\n\
                 hot_b_addr, HOT_B_BASE,\n\
                 \"HOT_B linked at {{hot_b_addr:#x}}, expected {{HOT_B_BASE:#x}}\"\n\
             );\n\
         }}\n"
    );
    fs::write(out_dir.join("mem_map.rs"), rs).expect("write mem_map.rs");

    // Absolute IRAM sections + DRAM skip for the patch arena (dual-map safe).
    let sections = format!(
        "/* Auto-generated from mem_map.toml — do not edit. */\n\
         SECTIONS\n\
         {{\n\
             .iram1.helper_dispatcher {helper_base:#x} :\n\
             {{\n\
                 . = ALIGN(4);\n\
                 KEEP(*(.iram1.helper_dispatcher))\n\
                 . = {helper_end:#x};\n\
             }} > IRAM\n\
         \n\
             .iram1.hotload_a {hot_a_base:#x} :\n\
             {{\n\
                 . = ALIGN(4);\n\
                 KEEP(*(.iram1.hotload_a))\n\
                 . = {hot_a_end:#x};\n\
             }} > IRAM\n\
         \n\
             .iram1.hotload_b {hot_b_base:#x} :\n\
             {{\n\
                 . = ALIGN(4);\n\
                 KEEP(*(.iram1.hotload_b))\n\
                 . = {hot_b_end:#x};\n\
             }} > IRAM\n\
         }}\n\
         \n\
         SECTIONS\n\
         {{\n\
             .rwdata_dummy_bs (NOLOAD) : ALIGN(4)\n\
             {{\n\
                 /* Skip DRAM aliases of helper + HOT_A + HOT_B (8KiB IRAM head). */\n\
                 . = . + SIZEOF(.iram1.helper_dispatcher);\n\
                 . = . + SIZEOF(.iram1.hotload_a);\n\
                 . = . + SIZEOF(.iram1.hotload_b);\n\
             }} > DRAM\n\
         }}\n\
         INSERT BEFORE .data;\n"
    );
    let sections_path = out_dir.join("custom-sections.x");
    fs::write(&sections_path, sections).expect("write custom-sections.x");
    println!("cargo:rustc-link-arg=-T{}", sections_path.display());
    // AOT `esp32c3*.ld` lives only in libbpf-mcu — firmware must not generate or sync it.
}

fn parse_addr(table: &toml::Table, key: &str, section: &str) -> usize {
    let v = table
        .get(key)
        .unwrap_or_else(|| panic!("mem_map.toml: [{section}] missing {key}"));
    if let Some(i) = v.as_integer() {
        assert!(i > 0, "mem_map.toml: [{section}].{key} must be positive");
        return i as usize;
    }
    if let Some(s) = v.as_str() {
        let s = s.trim();
        let n = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            usize::from_str_radix(hex, 16)
        } else {
            s.parse::<usize>()
        };
        return n.unwrap_or_else(|e| panic!("mem_map.toml: [{section}].{key} parse: {e}"));
    }
    panic!("mem_map.toml: [{section}].{key} must be integer or hex string");
}

fn generate_bpf_maps() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let bpf_toml_path = manifest_dir.join("bpf.toml");
    let text = fs::read_to_string(&bpf_toml_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", bpf_toml_path.display()));

    let root: toml::Table = text
        .parse()
        .unwrap_or_else(|e| panic!("failed to parse bpf.toml: {e}"));

    let maps_table = match root.get("maps").and_then(|v| v.as_table()) {
        Some(t) => t,
        None => {
            let (async_kw, _) = map_init_asyncness();
            write_generated(&format!(
                "// no [maps.*] entries in bpf.toml\n\
                 pub const FIRMWARE_MAP_COUNT: usize = 0;\n\
                 pub const FIRMWARE_HAS_ARRAY: bool = false;\n\
                 pub const FIRMWARE_HAS_HASH: bool = false;\n\
                 pub const FIRMWARE_HAS_DEVMAP: bool = false;\n\
                 pub const FIRMWARE_HAS_PROG_ARRAY: bool = false;\n\
                 pub const FIRMWARE_HAS_RINGBUF: bool = false;\n\
                 pub const SELFTEST_MAP_ID: ResourceId = 0;\n\
                 pub const SELFTEST_MAP_TYPE: &str = \"\";\n\
                 pub const SELFTEST_KEY_SIZE: usize = 0;\n\
                 pub const SELFTEST_VALUE_SIZE: usize = 0;\n\
                 pub {async_kw}fn init_firmware_maps() -> Result<(), allocator_api2::alloc::AllocError> {{ Ok(()) }}\n"
            ));
            return;
        }
    };

    // Stable order for deterministic codegen / ResourceId assignment.
    let maps: BTreeMap<&String, &toml::Value> = maps_table.iter().collect();

    let mut total_estimated: usize = 0;
    let mut map_specs = Vec::new();

    for (name, entry) in maps {
        let table = entry
            .as_table()
            .unwrap_or_else(|| panic!("[maps.{name}] must be a table"));

        let map_type = table
            .get("map_type")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("[maps.{name}] missing map_type"));
        let max_entries = table
            .get("max_entries")
            .and_then(|v| v.as_integer())
            .unwrap_or_else(|| panic!("[maps.{name}] missing max_entries"))
            as usize;
        let estimated_bytes = table
            .get("estimated_bytes")
            .and_then(|v| v.as_integer())
            .unwrap_or_else(|| panic!("[maps.{name}] missing estimated_bytes"))
            as usize;

        let value_fields = table
            .get("value")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("[maps.{name}] missing value"));
        let value_fields = parse_fields(value_fields, name, "value");

        let key_fields = match map_type {
            "BPF_MAP_TYPE_RINGBUF" => Vec::new(),
            _ => {
                let key = table
                    .get("key")
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| panic!("[maps.{name}] missing key"));
                parse_fields(key, name, "key")
            }
        };

        total_estimated = total_estimated
            .checked_add(estimated_bytes)
            .unwrap_or_else(|| panic!("estimated_bytes overflow summing maps"));

        map_specs.push(MapSpec {
            name: name.clone(),
            map_type: map_type.to_string(),
            max_entries,
            key_fields,
            value_fields,
        });
    }

    let required = total_estimated
        .checked_add(POOL_RESERVED)
        .unwrap_or_else(|| panic!("heap requirement overflow"));
    if required > HEAP_SIZE {
        panic!(
            "bpf.toml maps need {required} bytes (estimated={total_estimated} + reserved={POOL_RESERVED}) \
             but XDP_MAP heap is only {HEAP_SIZE} bytes — refuse to compile"
        );
    }

    let mut out = String::new();
    out.push_str("// @generated by build.rs from bpf.toml — do not edit\n");
    out.push_str(&format!(
        "// heap check: estimated={total_estimated} + reserved={POOL_RESERVED} <= HEAP_SIZE={HEAP_SIZE}\n"
    ));
    out.push_str(&format!(
        "pub const FIRMWARE_MAP_COUNT: usize = {};\n",
        map_specs.len()
    ));
    out.push_str(&format!(
        "pub const FIRMWARE_HAS_ARRAY: bool = {};\n",
        map_specs.iter().any(|s| s.map_type == "BPF_MAP_TYPE_ARRAY")
    ));
    out.push_str(&format!(
        "pub const FIRMWARE_HAS_HASH: bool = {};\n",
        map_specs.iter().any(|s| s.map_type == "BPF_MAP_TYPE_HASH")
    ));
    out.push_str(&format!(
        "pub const FIRMWARE_HAS_DEVMAP: bool = {};\n",
        map_specs
            .iter()
            .any(|s| s.map_type == "BPF_MAP_TYPE_DEVMAP")
    ));
    out.push_str(&format!(
        "pub const FIRMWARE_HAS_PROG_ARRAY: bool = {};\n",
        map_specs
            .iter()
            .any(|s| s.map_type == "BPF_MAP_TYPE_PROG_ARRAY")
    ));
    out.push_str(&format!(
        "pub const FIRMWARE_HAS_RINGBUF: bool = {};\n\n",
        map_specs
            .iter()
            .any(|s| s.map_type == "BPF_MAP_TYPE_RINGBUF")
    ));

    if let Some(first) = map_specs.first() {
        let key_size = c_layout_size(&first.key_fields);
        let value_size = c_layout_size(&first.value_fields);
        out.push_str("/// First map in bpf.toml (BTree order) — used by hw-selftest.\n");
        out.push_str("pub const SELFTEST_MAP_ID: ResourceId = 0;\n");
        out.push_str(&format!(
            "pub const SELFTEST_MAP_TYPE: &str = \"{}\";\n",
            first.map_type
        ));
        out.push_str(&format!(
            "pub const SELFTEST_KEY_SIZE: usize = {key_size};\n"
        ));
        out.push_str(&format!(
            "pub const SELFTEST_VALUE_SIZE: usize = {value_size};\n\n"
        ));
    }

    for (idx, spec) in map_specs.iter().enumerate() {
        let type_stem = snake_to_pascal(&spec.name);
        let val_ty = format!("{type_stem}Value");
        let id_const = format!("MAP_ID_{}", spec.name.to_ascii_uppercase());
        let key_size = c_layout_size(&spec.key_fields);
        let value_size = c_layout_size(&spec.value_fields);

        out.push_str(&format!(
            "/// ResourceId for maps.{} from bpf.toml\n",
            spec.name
        ));
        out.push_str(&format!("pub const {id_const}: ResourceId = {idx};\n"));
        out.push_str(&format!(
            "pub const {}_KEY_SIZE: usize = {key_size};\n",
            id_const
        ));
        out.push_str(&format!(
            "pub const {}_VALUE_SIZE: usize = {value_size};\n",
            id_const
        ));
        out.push_str(&format!(
            "pub const {}_MAX_ENTRIES: usize = {};\n\n",
            id_const, spec.max_entries
        ));

        match spec.map_type.as_str() {
            "BPF_MAP_TYPE_RINGBUF" => {
                emit_struct(&mut out, &val_ty, &spec.value_fields, false);
            }
            "BPF_MAP_TYPE_HASH"
            | "BPF_MAP_TYPE_ARRAY"
            | "BPF_MAP_TYPE_DEVMAP"
            | "BPF_MAP_TYPE_PROG_ARRAY" => {
                let key_ty = format!("{type_stem}Key");
                emit_struct(&mut out, &key_ty, &spec.key_fields, true);
                emit_struct(&mut out, &val_ty, &spec.value_fields, false);
            }
            other => panic!("[maps.{}] unsupported map_type `{other}`", spec.name),
        }
    }

    let (async_kw, await_suffix) = map_init_asyncness();
    out.push_str(
        "/// Create every map declared in bpf.toml into the talc resource pool.\n\
         ///\n\
         /// Call after [`init`] and before network-stack bring-up.\n\
         /// Create maps keyed by [`ResourceId`] (`MAP_ID_*`).\n\
         /// AOT helpers resolve `&map` pointers via [`bind_aot_map_addrs`].\n",
    );
    out.push_str(&format!(
        "pub {async_kw}fn init_firmware_maps() -> Result<(), allocator_api2::alloc::AllocError> {{\n"
    ));

    let needs_hash = map_specs.iter().any(|s| s.map_type == "BPF_MAP_TYPE_HASH");
    let needs_array = map_specs.iter().any(|s| s.map_type == "BPF_MAP_TYPE_ARRAY");
    let needs_devmap = map_specs
        .iter()
        .any(|s| s.map_type == "BPF_MAP_TYPE_DEVMAP");
    let needs_prog_array = map_specs
        .iter()
        .any(|s| s.map_type == "BPF_MAP_TYPE_PROG_ARRAY");
    let needs_ring = map_specs
        .iter()
        .any(|s| s.map_type == "BPF_MAP_TYPE_RINGBUF");
    if needs_hash || needs_array || needs_devmap || needs_prog_array || needs_ring {
        out.push_str("    use crate::patch::map::{");
        let mut parts = Vec::new();
        if needs_array {
            parts.push("create_bpf_array");
        }
        if needs_devmap {
            parts.push("create_bpf_devmap");
        }
        if needs_prog_array {
            parts.push("create_bpf_prog_array");
        }
        if needs_hash {
            parts.push("create_bpf_hash");
        }
        if needs_ring {
            parts.push("create_bpf_ringbuf");
        }
        parts.push("register_map_name");
        out.push_str(&parts.join(", "));
        out.push_str("};\n");
    }

    for spec in &map_specs {
        let id_const = format!("MAP_ID_{}", spec.name.to_ascii_uppercase());
        let key_size = c_layout_size(&spec.key_fields);
        let value_size = c_layout_size(&spec.value_fields);
        match spec.map_type.as_str() {
            "BPF_MAP_TYPE_HASH" => {
                out.push_str(&format!(
                    "    create_bpf_hash({id_const}, {key_size}, {value_size}, {}){await_suffix}?;\n",
                    spec.max_entries
                ));
            }
            "BPF_MAP_TYPE_ARRAY" => {
                out.push_str(&format!(
                    "    create_bpf_array({id_const}, {value_size}, {}){await_suffix}?;\n",
                    spec.max_entries
                ));
            }
            "BPF_MAP_TYPE_DEVMAP" => {
                out.push_str(&format!(
                    "    create_bpf_devmap({id_const}, {}){await_suffix}?;\n",
                    spec.max_entries
                ));
            }
            "BPF_MAP_TYPE_PROG_ARRAY" => {
                out.push_str(&format!(
                    "    create_bpf_prog_array({id_const}, {}){await_suffix}?;\n",
                    spec.max_entries
                ));
            }
            "BPF_MAP_TYPE_RINGBUF" => {
                // Linux: max_entries is the ring data area in bytes.
                out.push_str(&format!(
                    "    create_bpf_ringbuf({id_const}, {}){await_suffix}?;\n",
                    spec.max_entries
                ));
            }
            other => panic!("[maps.{}] unsupported map_type `{other}`", spec.name),
        }
        out.push_str(&format!(
            "    let _ = register_map_name(\"{}\", {id_const});\n",
            escape_rust_str(&spec.name)
        ));
    }

    out.push_str("    Ok(())\n}\n");

    write_generated(&out);
}

/// Emit `aot_map_bind.rs`: AOT `&map` VMAs from `bpf.toml` `aot_addr` → ResourceId.
fn generate_aot_map_bind() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let out_path = out_dir.join("aot_map_bind.rs");
    let bpf_toml_path = manifest_dir.join("bpf.toml");

    let text = fs::read_to_string(&bpf_toml_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", bpf_toml_path.display()));
    let root: toml::Table = text
        .parse()
        .unwrap_or_else(|e| panic!("failed to parse bpf.toml: {e}"));

    let mut out = String::from(
        "// @generated by build.rs from bpf.toml aot_addr — do not edit\n\
         /// Bind absolute `.maps` addresses from the loaded AOT image to firmware map ids.\n\
         /// Call after copying the AOT bin into HOT_REGION and after [`init_firmware_maps`].\n\
         /// Addresses come from `[maps.*].aot_addr` (libbpf-mcu MCU report merge).\n\
         pub fn bind_aot_map_addrs() {\n\
             use crate::patch::map::register_map_addr;\n",
    );

    let mut bound = 0usize;
    if let Some(maps) = root.get("maps").and_then(|v| v.as_table()) {
        let maps: BTreeMap<&String, &toml::Value> = maps.iter().collect();
        for (name, entry) in maps {
            let table = entry
                .as_table()
                .unwrap_or_else(|| panic!("[maps.{name}] must be a table"));
            let Some(addr) = parse_optional_addr(table, "aot_addr", name) else {
                println!(
                    "cargo:warning=bpf.toml [maps.{name}] missing aot_addr — AOT helpers will not resolve &{name}"
                );
                continue;
            };
            if addr == 0 {
                println!("cargo:warning=bpf.toml [maps.{name}] aot_addr is 0 — skipped");
                continue;
            }
            let id_const = format!("MAP_ID_{}", name.to_ascii_uppercase());
            out.push_str(&format!("    register_map_addr(0x{addr:x}, {id_const});\n"));
            bound += 1;
            for alias in parse_addr_list(table, "aot_aliases", name) {
                if alias == 0 || alias == addr {
                    continue;
                }
                out.push_str(&format!(
                    "    register_map_addr(0x{alias:x}, {id_const});\n"
                ));
                bound += 1;
            }
        }
    }

    out.push_str("}\n");
    out.push_str(&format!(
        "/// Number of AOT map aliases bound from bpf.toml `aot_addr` (+ `aot_aliases`).\n\
         pub const AOT_MAP_BIND_COUNT: usize = {bound};\n"
    ));

    fs::write(&out_path, out).unwrap_or_else(|e| panic!("write {}: {e}", out_path.display()));
}

fn parse_optional_addr(table: &toml::Table, key: &str, map_name: &str) -> Option<usize> {
    let v = table.get(key)?;
    Some(parse_addr_value(v, key, map_name))
}

fn parse_addr_value(v: &toml::Value, key: &str, map_name: &str) -> usize {
    if let Some(i) = v.as_integer() {
        assert!(i >= 0, "[maps.{map_name}].{key} must be non-negative");
        return i as usize;
    }
    if let Some(s) = v.as_str() {
        let s = s.trim();
        let addr = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            usize::from_str_radix(hex, 16)
        } else {
            s.parse::<usize>()
        };
        return addr
            .unwrap_or_else(|e| panic!("[maps.{map_name}].{key}: invalid address `{s}`: {e}"));
    }
    panic!("[maps.{map_name}].{key}: expected integer or hex string");
}

/// Optional `[maps.*].aot_aliases = [0x…, …]` — extra AOT `.maps` VMAs for hot-siblings.
fn parse_addr_list(table: &toml::Table, key: &str, map_name: &str) -> Vec<usize> {
    let Some(v) = table.get(key) else {
        return Vec::new();
    };
    let Some(arr) = v.as_array() else {
        panic!("[maps.{map_name}].{key}: expected array of addresses");
    };
    arr.iter()
        .map(|e| parse_addr_value(e, key, map_name))
        .collect()
}

struct MapSpec {
    name: String,
    map_type: String,
    max_entries: usize,
    key_fields: Vec<(String, String)>,
    value_fields: Vec<(String, String)>,
}

fn parse_fields(block: &str, map_name: &str, kind: &str) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    for line in block.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let ty = parts
            .next()
            .unwrap_or_else(|| panic!("[maps.{map_name}] {kind}: bad field `{line}`"));
        let name = parts
            .next()
            .unwrap_or_else(|| panic!("[maps.{map_name}] {kind}: missing field name in `{line}`"));
        if parts.next().is_some() {
            panic!("[maps.{map_name}] {kind}: unexpected tokens in `{line}`");
        }
        let rust_ty = match ty {
            "u8" | "u16" | "u32" | "u64" | "i8" | "i16" | "i32" | "i64" => ty.to_string(),
            other => panic!("[maps.{map_name}] {kind}: unsupported C type `{other}`"),
        };
        fields.push((rust_ty, name.to_string()));
    }
    if fields.is_empty() {
        panic!("[maps.{map_name}] {kind}: empty field list");
    }
    fields
}

fn emit_struct(out: &mut String, name: &str, fields: &[(String, String)], is_key: bool) {
    out.push_str("#[repr(C)]\n");
    out.push_str("#[derive(Clone, Copy, Debug");
    if is_key {
        out.push_str(", PartialEq, Eq, Hash");
    }
    out.push_str(")]\n");
    out.push_str(&format!("pub struct {name} {{\n"));
    for (ty, field) in fields {
        out.push_str(&format!("    pub {field}: {ty},\n"));
    }
    out.push_str("}\n\n");
}

fn type_size_align(ty: &str) -> (usize, usize) {
    match ty {
        "u8" | "i8" => (1, 1),
        "u16" | "i16" => (2, 2),
        "u32" | "i32" => (4, 4),
        "u64" | "i64" => (8, 8),
        other => panic!("unsupported C type `{other}`"),
    }
}

fn align_up(off: usize, align: usize) -> usize {
    (off + align - 1) & !(align - 1)
}

fn c_layout_size(fields: &[(String, String)]) -> usize {
    if fields.is_empty() {
        return 0;
    }
    let mut off = 0usize;
    let mut max_align = 1usize;
    for (ty, _) in fields {
        let (sz, al) = type_size_align(ty);
        off = align_up(off, al);
        off += sz;
        max_align = max_align.max(al);
    }
    align_up(off, max_align)
}

fn snake_to_pascal(s: &str) -> String {
    s.split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect()
}

fn write_generated(contents: &str) {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join("bpf_maps.rs"), contents).expect("write bpf_maps.rs");
}

fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let kind = &args[1];
        let what = &args[2];

        match kind.as_str() {
            "undefined-symbol" => match what.as_str() {
                what if what.starts_with("_defmt_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `defmt` not found - make sure `defmt.x` is added as a linker script and you have included `use defmt_rtt as _;`"
                    );
                    eprintln!();
                }
                "_stack_start" => {
                    eprintln!();
                    eprintln!("💡 Is the linker script `linkall.x` missing?");
                    eprintln!();
                }
                what if what.starts_with("esp_rtos_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `esp-radio` has no scheduler enabled. Make sure you have initialized `esp-rtos` or provided an external scheduler."
                    );
                    eprintln!();
                }
                "embedded_test_linker_file_not_added_to_rustflags" => {
                    eprintln!();
                    eprintln!(
                        "💡 `embedded-test` not found - make sure `embedded-test.x` is added as a linker script for tests"
                    );
                    eprintln!();
                }
                "free"
                | "malloc"
                | "calloc"
                | "get_free_internal_heap_size"
                | "malloc_internal"
                | "realloc_internal"
                | "calloc_internal"
                | "free_internal" => {
                    eprintln!();
                    eprintln!(
                        "💡 Did you forget the `esp-alloc` dependency or didn't enable the `compat` feature on it?"
                    );
                    eprintln!();
                }
                _ => (),
            },
            _ => {
                std::process::exit(1);
            }
        }

        std::process::exit(0);
    }

    println!(
        "cargo:rustc-link-arg=--error-handling-script={}",
        std::env::current_exe().unwrap().display()
    );
}
