//! Parse serial `OK counters …` lines and build a JSON delta of changed keys only.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde_json::{Map, Number, Value};

/// `key` → JSON number or string (e.g. `map_lock=no-rt`).
pub type CounterMap = BTreeMap<String, Value>;

pub fn parse_counter_map(line: &str) -> Result<CounterMap> {
    let line = line.trim();
    let body = line
        .strip_prefix("OK counters ")
        .or_else(|| line.strip_prefix("OK COUNTERS "))
        .unwrap_or(line);
    let mut out = CounterMap::new();
    for part in body.split_whitespace() {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        if k.is_empty() {
            continue;
        }
        out.insert(k.to_string(), parse_value(v));
    }
    if out.is_empty() {
        anyhow::bail!("empty counters line: {line}");
    }
    Ok(out)
}

fn parse_value(v: &str) -> Value {
    if let Ok(n) = v.parse::<i64>() {
        return Value::Number(Number::from(n));
    }
    if let Ok(n) = v.parse::<u64>() {
        if n <= i64::MAX as u64 {
            return Value::Number(Number::from(n as i64));
        }
    }
    Value::String(v.to_string())
}

/// Keys whose value changed between `before` and `after`. Unchanged keys omitted.
/// Numeric keys store `after - before`; strings store `after`.
/// Missing-on-one-side counts as a change (uses the present side's value, or `null`).
pub fn counters_delta(before: &CounterMap, after: &CounterMap) -> Value {
    let mut obj = Map::new();
    let mut keys: BTreeMap<&str, ()> = BTreeMap::new();
    for k in before.keys() {
        keys.insert(k.as_str(), ());
    }
    for k in after.keys() {
        keys.insert(k.as_str(), ());
    }
    for k in keys.keys() {
        let b = before.get(*k);
        let a = after.get(*k);
        if b == a {
            continue;
        }
        match (b.and_then(Value::as_i64), a) {
            (Some(bn), Some(v)) if v.is_i64() => {
                let an = v.as_i64().unwrap_or(0);
                obj.insert((*k).to_string(), Value::Number(Number::from(an - bn)));
            }
            (_, Some(v)) => {
                obj.insert((*k).to_string(), v.clone());
            }
            (_, None) => {
                obj.insert((*k).to_string(), Value::Null);
            }
        }
    }
    Value::Object(obj)
}

pub async fn fetch_counters(agent: &crate::agent::AgentClient) -> Result<CounterMap> {
    let line = agent.counters().await.context("agent counters")?;
    parse_counter_map(&line)
}
