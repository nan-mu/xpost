//! ESP32-C3 IBUS/DBUS dual-map terminal diagram (row-driven grid).

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Result, bail};

use crate::layout::{self, Layout};

/// Instruction bus base (ESP32-C3 SRAM window).
pub const IBUS_BASE: u32 = 0x4037_0000;
/// Data bus base (same physical SRAM).
pub const DBUS_BASE: u32 = 0x3FC8_0000;
/// Full SRAM window size (400 KiB).
#[allow(dead_code)]
pub const SRAM_LEN: u32 = 400 * 1024;

const BOX_INNER: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusType {
    IBus,
    DBus,
}

#[derive(Clone, Debug)]
pub struct Section {
    pub name: String,
    pub start_vma: u32,
    pub size: u32,
    pub bus: BusType,
}

impl Section {
    pub fn end_vma(&self) -> u32 {
        self.start_vma.saturating_add(self.size)
    }

    pub fn offset_start(&self) -> Result<u32> {
        let base = match self.bus {
            BusType::IBus => IBUS_BASE,
            BusType::DBus => DBUS_BASE,
        };
        if self.start_vma < base {
            bail!(
                "section `{}` VMA {:#x} below {:?} base {:#x}",
                self.name,
                self.start_vma,
                self.bus,
                base
            );
        }
        Ok(self.start_vma - base)
    }

    pub fn offset_end(&self) -> Result<u32> {
        Ok(self.offset_start()? + self.size)
    }
}

/// One printable grid row (columns aligned before `println!`).
#[derive(Clone, Debug, Default)]
pub struct RenderRow {
    pub ibus_addr: String,
    pub ibus_name: String,
    pub sram: String,
    pub dbus_name: String,
    pub dbus_addr: String,
}

/// Build dual-bus sections from a computed [`Layout`] (IBUS regions + DBUS aliases).
pub fn sections_from_layout(layout: &Layout) -> Result<Vec<Section>> {
    let mut ibus: Vec<(String, u32, u32)> = vec![(
        "helper".into(),
        layout.helper.base as u32,
        layout.helper.length as u32,
    )];

    for (i, r) in layout.hot_regions.iter().enumerate() {
        ibus.push((format!("slot{i}"), r.base as u32, r.length as u32));
    }

    let mut out = Vec::with_capacity(ibus.len() * 2);
    for (name, start, size) in ibus {
        let off = start
            .checked_sub(IBUS_BASE)
            .ok_or_else(|| anyhow::anyhow!("IBUS VMA {start:#x} below IBUS_BASE {IBUS_BASE:#x}"))?;
        out.push(Section {
            name: name.clone(),
            start_vma: start,
            size,
            bus: BusType::IBus,
        });
        out.push(Section {
            name,
            start_vma: DBUS_BASE + off,
            size,
            bus: BusType::DBus,
        });
    }
    Ok(out)
}

fn format_size(bytes: u32) -> String {
    if bytes >= 1024 && bytes.is_multiple_of(1024) {
        let kb = bytes / 1024;
        if kb >= 1024 && kb.is_multiple_of(1024) {
            format!("{} MB", kb / 1024)
        } else {
            format!("{kb} KB")
        }
    } else {
        format!("{bytes} Bytes")
    }
}

fn center_in(width: usize, text: &str) -> String {
    if text.len() >= width {
        return text.chars().take(width).collect();
    }
    let pad = width - text.len();
    let left = pad / 2;
    let right = pad - left;
    format!("{}{}{}", " ".repeat(left), text, " ".repeat(right))
}

fn boundary_box(kind: char) -> String {
    // kind: 't' top, 'm' mid, 'b' bottom
    let fill = "─".repeat(BOX_INNER);
    match kind {
        't' => format!("┌{fill}┐"),
        'b' => format!("└{fill}┘"),
        _ => format!("├{fill}┤"),
    }
}

fn content_box(label: &str) -> String {
    format!("│{}│", center_in(BOX_INNER, label))
}

fn fmt_addr(vma: u32) -> String {
    format!("{vma:#010x}")
}

/// Row-driven render: 2N-1 rows from N unique physical boundaries.
pub fn render(sections: &[Section]) -> Result<Vec<RenderRow>> {
    if sections.is_empty() {
        bail!("no sections to render");
    }

    let mut bounds_set = BTreeSet::new();
    for s in sections {
        bounds_set.insert(s.offset_start()?);
        bounds_set.insert(s.offset_end()?);
    }
    let bounds: Vec<u32> = bounds_set.into_iter().collect();
    let n = bounds.len();
    if n < 2 {
        bail!("need at least two distinct boundaries");
    }

    let row_count = 2 * n - 1;
    let mut rows = vec![RenderRow::default(); row_count];

    // SRAM box column + capacity on content rows.
    for (bi, &off) in bounds.iter().enumerate() {
        let row_i = 2 * bi;
        let kind = if bi == 0 {
            't'
        } else if bi + 1 == n {
            'b'
        } else {
            'm'
        };
        rows[row_i].sram = boundary_box(kind);

        // Address columns on boundary rows when a section starts/ends here.
        let ibus_vma = IBUS_BASE.saturating_add(off);
        let dbus_vma = DBUS_BASE.saturating_add(off);
        let ibus_hit = sections.iter().any(|s| {
            s.bus == BusType::IBus && (s.start_vma == ibus_vma || s.end_vma() == ibus_vma)
        });
        let dbus_hit = sections.iter().any(|s| {
            s.bus == BusType::DBus && (s.start_vma == dbus_vma || s.end_vma() == dbus_vma)
        });
        if ibus_hit {
            rows[row_i].ibus_addr = fmt_addr(ibus_vma);
        }
        if dbus_hit {
            rows[row_i].dbus_addr = fmt_addr(dbus_vma);
        }

        if bi + 1 < n {
            let next = bounds[bi + 1];
            let slice = next - off;
            rows[row_i + 1].sram = content_box(&format_size(slice));
        }
    }

    // Place section names at TargetRow = (StartRow + EndRow) / 2.
    for s in sections {
        let start_off = s.offset_start()?;
        let end_off = s.offset_end()?;
        let start_bi = bounds
            .iter()
            .position(|&b| b == start_off)
            .ok_or_else(|| anyhow::anyhow!("missing start bound for {}", s.name))?;
        let end_bi = bounds
            .iter()
            .position(|&b| b == end_off)
            .ok_or_else(|| anyhow::anyhow!("missing end bound for {}", s.name))?;
        let start_row = 2 * start_bi;
        let end_row = 2 * end_bi;
        let target = if start_row == end_row {
            start_row
        } else {
            (start_row + end_row) / 2
        };
        match s.bus {
            BusType::IBus => rows[target].ibus_name = s.name.clone(),
            BusType::DBus => rows[target].dbus_name = s.name.clone(),
        }
    }

    Ok(rows)
}

/// Print aligned 5-column diagram to stdout.
pub fn print_diagram(rows: &[RenderRow]) {
    let w_addr = rows
        .iter()
        .map(|r| r.ibus_addr.len().max(r.dbus_addr.len()))
        .max()
        .unwrap_or(10)
        .max(10);
    let w_name = rows
        .iter()
        .map(|r| r.ibus_name.len().max(r.dbus_name.len()))
        .max()
        .unwrap_or(8)
        .max(8);
    let w_sram = rows
        .iter()
        .map(|r| r.sram.len())
        .max()
        .unwrap_or(BOX_INNER + 2);

    println!(
        "{:>w_addr$}  {:^w_name$}  {:^w_sram$}  {:^w_name$}  {:<w_addr$}",
        "IBUS",
        "IBUS seg",
        "SRAM",
        "DBUS seg",
        "DBUS",
        w_addr = w_addr,
        w_name = w_name,
        w_sram = w_sram,
    );
    for r in rows {
        println!(
            "{:>w_addr$}  {:^w_name$}  {:^w_sram$}  {:^w_name$}  {:<w_addr$}",
            r.ibus_addr,
            r.ibus_name,
            r.sram,
            r.dbus_name,
            r.dbus_addr,
            w_addr = w_addr,
            w_name = w_name,
            w_sram = w_sram,
        );
    }
}

/// Load board intent `layout.toml`, render, print.
pub fn run_map(manifest_dir: &Path) -> Result<()> {
    let layout = layout::compute_from_file(&manifest_dir.join("layout.toml"))?;
    let sections = sections_from_layout(&layout)?;
    let rows = render(&sections)?;
    print_diagram(&rows);
    Ok(())
}
