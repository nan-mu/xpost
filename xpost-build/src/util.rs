//! Shared helpers for toml field / address parsing and Rust string escaping.

pub fn escape_rust_str(s: &str) -> String {
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

pub fn parse_toml_u16(table: &toml::Table, key: &str, section: &str, default: u16) -> u16 {
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

pub fn parse_toml_ipv4(table: &toml::Table, key: &str, section: &str) -> [u8; 4] {
    let v = table
        .get(key)
        .unwrap_or_else(|| panic!("config.toml: [{section}].{key} required"));
    parse_toml_ipv4_value(v, &format!("{section}.{key}"))
}

pub fn parse_toml_ipv4_value(v: &toml::Value, ctx: &str) -> [u8; 4] {
    let s = v
        .as_str()
        .unwrap_or_else(|| panic!("config.toml: {ctx} must be dotted IPv4 string"));
    parse_dotted_ipv4(s, ctx)
}

pub fn parse_dotted_ipv4(s: &str, ctx: &str) -> [u8; 4] {
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

pub fn parse_addr_value(v: &toml::Value, key: &str, map_name: &str) -> usize {
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
