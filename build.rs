use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

/// Must match `HEAP_SIZE` in `src/patch/map.rs`.
const HEAP_SIZE: usize = 65536;
/// Reserved for talc metadata + registry HashMap overhead inside the pool.
const POOL_RESERVED: usize = 2048;

/// `(async_kw, await_suffix)` for `init_firmware_maps` under patch-sync features.
fn map_init_asyncness() -> (&'static str, &'static str) {
    let st = env::var_os("CARGO_FEATURE_PATCH_SYNC_ST").is_some();
    if st {
        ("", "")
    } else {
        ("async ", ".await")
    }
}

fn main() {
    linker_be_nice();

    println!("cargo:rerun-if-changed=esp32c3.bin");
    println!("cargo:rerun-if-changed=bpf.toml");
    println!("cargo:rerun-if-changed=mem_map.toml");

    dotenv_build::output(dotenv_build::Config::default()).unwrap();
    println!("cargo:rerun-if-changed=.env");

    generate_mem_map();
    generate_bpf_maps();
    generate_aot_map_bind();

    println!("cargo:rustc-link-arg=-Tlinkall.x");
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
    let hot = root
        .get("hot_region")
        .and_then(|v| v.as_table())
        .expect("mem_map.toml: missing [hot_region]");

    let helper_base = parse_addr(helper, "base", "helper");
    let helper_len = parse_addr(helper, "length", "helper");
    let hot_base = parse_addr(hot, "base", "hot_region");
    let hot_len = parse_addr(hot, "length", "hot_region");

    assert!(
        helper_base.is_multiple_of(4) && hot_base.is_multiple_of(4),
        "mem_map bases must be 4-byte aligned"
    );
    assert!(helper_len > 0 && hot_len > 0, "mem_map lengths must be > 0");
    assert!(
        helper_base.saturating_add(helper_len) <= hot_base
            || hot_base.saturating_add(hot_len) <= helper_base,
        "helper window and HOT_REGION overlap"
    );

    let helper_end = helper_base + helper_len;
    let hot_end = hot_base + hot_len;

    let rs = format!(
        "// Auto-generated from mem_map.toml — do not edit.\n\
         \n\
         /// Absolute IRAM address of `helper_dispatcher`.\n\
         pub const HELPER_DISPATCHER_BASE: usize = {helper_base:#x};\n\
         /// Reserved IRAM window size for the helper trampoline.\n\
         pub const HELPER_WINDOW_LEN: usize = {helper_len:#x};\n\
         /// Absolute IRAM load / entry address for AOT payload.\n\
         pub const HOT_REGION_BASE: usize = {hot_base:#x};\n\
         /// Firmware-owned HOT_REGION buffer size (full reserved page).\n\
         pub const HOT_REGION_LEN: usize = {hot_len:#x};\n\
         \n\
         /// Panic if linker placed symbols off the mem_map contract.\n\
         pub fn assert_linked_addresses(helper_addr: usize, hot_addr: usize) {{\n\
             assert_eq!(\n\
                 helper_addr, HELPER_DISPATCHER_BASE,\n\
                 \"helper_dispatcher linked at {{helper_addr:#x}}, expected {{HELPER_DISPATCHER_BASE:#x}}\"\n\
             );\n\
             assert_eq!(\n\
                 hot_addr, HOT_REGION_BASE,\n\
                 \"HOT_REGION linked at {{hot_addr:#x}}, expected {{HOT_REGION_BASE:#x}}\"\n\
             );\n\
         }}\n"
    );
    fs::write(out_dir.join("mem_map.rs"), rs).expect("write mem_map.rs");

    // Absolute IRAM sections + DRAM skip for the helper window (dual-map safe).
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
             .iram1.hotload_area {hot_base:#x} :\n\
             {{\n\
                 . = ALIGN(4);\n\
                 KEEP(*(.iram1.hotload_area))\n\
                 . = {hot_end:#x};\n\
             }} > IRAM\n\
         }}\n\
         \n\
         SECTIONS\n\
         {{\n\
             .rwdata_dummy_bs (NOLOAD) : ALIGN(4)\n\
             {{\n\
                 /* Skip DRAM aliases of helper + HOT_REGION (contiguous IRAM head). */\n\
                 . = . + SIZEOF(.iram1.helper_dispatcher);\n\
                 . = . + SIZEOF(.iram1.hotload_area);\n\
             }} > DRAM\n\
         }}\n\
         INSERT BEFORE .data;\n"
    );
    let sections_path = out_dir.join("custom-sections.x");
    fs::write(&sections_path, sections).expect("write custom-sections.x");
    println!("cargo:rustc-link-arg=-T{}", sections_path.display());
    // AOT `esp32c3.ld` lives only in libbpf-mcu — firmware must not generate or sync it.
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
        "pub const FIRMWARE_MAP_COUNT: usize = {};\n\n",
        map_specs.len()
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
        out.push_str(&format!("pub const SELFTEST_KEY_SIZE: usize = {key_size};\n"));
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

        out.push_str(&format!("/// ResourceId for maps.{} from bpf.toml\n", spec.name));
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
            "BPF_MAP_TYPE_HASH" | "BPF_MAP_TYPE_ARRAY" => {
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
    let needs_ring = map_specs.iter().any(|s| s.map_type == "BPF_MAP_TYPE_RINGBUF");
    if needs_hash || needs_array || needs_ring {
        out.push_str("    use crate::patch::map::{");
        let mut parts = Vec::new();
        if needs_array {
            parts.push("create_bpf_array");
        }
        if needs_hash {
            parts.push("create_bpf_hash");
        }
        if needs_ring {
            parts.push("create_bpf_ringbuf");
        }
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
            "BPF_MAP_TYPE_RINGBUF" => {
                // Linux: max_entries is the ring data area in bytes.
                out.push_str(&format!(
                    "    create_bpf_ringbuf({id_const}, {}){await_suffix}?;\n",
                    spec.max_entries
                ));
            }
            other => panic!("[maps.{}] unsupported map_type `{other}`", spec.name),
        }
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
                println!(
                    "cargo:warning=bpf.toml [maps.{name}] aot_addr is 0 — skipped"
                );
                continue;
            }
            let id_const = format!("MAP_ID_{}", name.to_ascii_uppercase());
            out.push_str(&format!(
                "    register_map_addr(0x{addr:x}, {id_const});\n"
            ));
            bound += 1;
        }
    }

    out.push_str("}\n");
    out.push_str(&format!(
        "/// Number of AOT map aliases bound from bpf.toml `aot_addr`.\n\
         pub const AOT_MAP_BIND_COUNT: usize = {bound};\n"
    ));

    fs::write(&out_path, out).unwrap_or_else(|e| panic!("write {}: {e}", out_path.display()));
}

fn parse_optional_addr(table: &toml::Table, key: &str, map_name: &str) -> Option<usize> {
    let v = table.get(key)?;
    if let Some(i) = v.as_integer() {
        assert!(i >= 0, "[maps.{map_name}].{key} must be non-negative");
        return Some(i as usize);
    }
    if let Some(s) = v.as_str() {
        let s = s.trim();
        let addr = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            usize::from_str_radix(hex, 16)
        } else {
            s.parse::<usize>()
        };
        return Some(addr.unwrap_or_else(|e| {
            panic!("[maps.{map_name}].{key}: invalid address `{s}`: {e}")
        }));
    }
    panic!("[maps.{map_name}].{key}: expected integer or hex string");
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
