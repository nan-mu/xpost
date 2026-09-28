//! Round-scoped stderr UI: checklist (start/end of a flash round) + live tx/rx bars + budget ETA.
//!
//! Init (config, clients, banner) is not checklist. Re-flash → `begin_round` clears temp output.

use std::cell::{Cell, RefCell};
use std::io::{self, IsTerminal, Write};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::agent::AgentClient;

const BAR_W: usize = 12;
const LABEL_COLS: usize = 36;
const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Max wait for UART `xpost ready` after flash (replaces fixed reboot sleep).
pub const READY_BUDGET: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
enum ItemState {
    Pending,
    Active,
    Done,
    Failed,
}

struct Item {
    label: String,
    state: ItemState,
    detail: String,
}

struct Bar {
    name: String,
    frac: f64,
    detail: String,
}

/// Budget-based remaining time: sum remaining fixed durations + estimated flashes.
#[derive(Clone, Debug, Default)]
pub struct BudgetEta {
    fixed_remaining: Duration,
    flashes_remaining: usize,
    flash_samples_ms: Vec<u64>,
    flash_started: Option<Instant>,
}

impl BudgetEta {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_fixed(&mut self, d: Duration) {
        self.fixed_remaining = self.fixed_remaining.saturating_add(d);
    }

    pub fn add_flashes(&mut self, n: usize) {
        self.flashes_remaining = self.flashes_remaining.saturating_add(n);
    }

    pub fn consume_fixed(&mut self, d: Duration) {
        self.fixed_remaining = self.fixed_remaining.saturating_sub(d);
    }

    pub fn begin_flash(&mut self) {
        self.flash_started = Some(Instant::now());
    }

    pub fn end_flash(&mut self, observed: Duration) {
        self.flash_started = None;
        self.flash_samples_ms.push(observed.as_millis() as u64);
        self.flashes_remaining = self.flashes_remaining.saturating_sub(1);
    }

    fn flash_mean(&self) -> Option<Duration> {
        if self.flash_samples_ms.is_empty() {
            return None;
        }
        let sum: u64 = self.flash_samples_ms.iter().sum();
        Some(Duration::from_millis(
            sum / self.flash_samples_ms.len() as u64,
        ))
    }

    /// (known_fixed, flash_estimate_if_known, flash_estimate_is_known)
    fn parts(&self) -> (Duration, Duration, bool) {
        let known = self.fixed_remaining;
        let mut flash = Duration::ZERO;
        let mut ok = true;

        let (current_left, pending_after) = if let Some(start) = self.flash_started {
            let elapsed = start.elapsed();
            let left = match self.flash_mean() {
                Some(mean) => mean.saturating_sub(elapsed),
                None => {
                    ok = false;
                    Duration::ZERO
                }
            };
            (left, self.flashes_remaining.saturating_sub(1))
        } else {
            (Duration::ZERO, self.flashes_remaining)
        };

        flash = flash.saturating_add(current_left);
        if pending_after > 0 {
            match self.flash_mean() {
                Some(mean) => flash = flash.saturating_add(mean.saturating_mul(pending_after as u32)),
                None => ok = false,
            }
        }
        (known, flash, ok)
    }

    pub fn fmt(&self) -> String {
        let (known, flash_est, flash_ok) = self.parts();
        let show_flash = self.flashes_remaining > 0 || self.flash_started.is_some();
        let total = if flash_ok {
            known.saturating_add(flash_est)
        } else {
            known
        };

        let mut s = format!("剩余 ≈ {}（已知 {}", fmt_short(total), fmt_short(known));
        if show_flash {
            if flash_ok {
                s.push_str(&format!(" + flash≈{}", fmt_short(flash_est)));
            } else {
                s.push_str(" + flash ?");
            }
        }
        s.push('）');

        if flash_ok || !show_flash {
            s.push_str(&format!(" · 约再过 {}", fmt_short(total)));
        } else {
            s.push_str(" · ETA 待首次烧录后");
        }
        s
    }
}

/// Per-flash-round UI. Banner is permanent; temp region clears on `begin_round` / `end_round`.
pub struct RoundUi {
    tty: bool,
    items: RefCell<Vec<Item>>,
    live_title: RefCell<Option<String>>,
    bars: RefCell<Vec<Bar>>,
    eta: RefCell<Option<String>>,
    painted_lines: Cell<usize>,
    spin_i: Cell<usize>,
}

impl RoundUi {
    pub fn banner(line: &str) {
        eprintln!("{line}");
    }

    pub fn new() -> Self {
        let tty = io::stderr().is_terminal();
        if !tty {
            // Live checklist / bars need a PTY (e.g. `ssh -t host ./xpost-test`).
            eprintln!("  （live UI 关闭：stderr 非 TTY，请用 ssh -t 交互跑）");
        }
        Self {
            tty,
            items: RefCell::new(Vec::new()),
            live_title: RefCell::new(None),
            bars: RefCell::new(Vec::new()),
            eta: RefCell::new(None),
            painted_lines: Cell::new(0),
            spin_i: Cell::new(0),
        }
    }

    /// Clear previous round's temporary region (checklist + live + eta).
    pub fn begin_round(&self) {
        self.clear_temp();
        self.items.borrow_mut().clear();
        *self.live_title.borrow_mut() = None;
        self.bars.borrow_mut().clear();
        *self.eta.borrow_mut() = None;
    }

    pub fn end_round(&self) {
        self.begin_round();
    }

    pub fn clear_temp_for_error(&self) {
        self.clear_temp();
    }

    pub fn set_eta(&self, line: &str) {
        *self.eta.borrow_mut() = Some(line.to_string());
        self.repaint();
    }

    pub fn check_start(&self, label: &str) {
        {
            let mut items = self.items.borrow_mut();
            for it in items.iter_mut() {
                if matches!(it.state, ItemState::Active) {
                    it.state = ItemState::Done;
                }
            }
            items.push(Item {
                label: label.to_string(),
                state: ItemState::Active,
                detail: String::new(),
            });
        }
        self.repaint();
        if !self.tty {
            eprintln!("  {label} …");
        }
    }

    pub fn check_note(&self, detail: &str) {
        {
            if let Some(it) = self
                .items
                .borrow_mut()
                .iter_mut()
                .rev()
                .find(|i| matches!(i.state, ItemState::Active))
            {
                it.detail = detail.to_string();
            }
        }
        self.repaint();
    }

    pub fn check_ok(&self, detail: &str) {
        // Only the Active item — never a Pending trailer (rev-find Pending stole flash ETA).
        let printed = {
            let mut items = self.items.borrow_mut();
            if let Some(it) = items
                .iter_mut()
                .rev()
                .find(|i| matches!(i.state, ItemState::Active))
            {
                it.state = ItemState::Done;
                if !detail.is_empty() {
                    it.detail = detail.to_string();
                }
                Some((it.label.clone(), it.detail.clone()))
            } else {
                None
            }
        };
        self.repaint();
        if !self.tty {
            if let Some((l, d)) = printed {
                eprintln!("  {l} √{}", fmt_detail(&d));
            }
        }
    }

    pub fn check_fail(&self, detail: &str) {
        {
            if let Some(it) = self
                .items
                .borrow_mut()
                .iter_mut()
                .rev()
                .find(|i| matches!(i.state, ItemState::Active))
            {
                it.state = ItemState::Failed;
                it.detail = detail.to_string();
            }
        }
        self.repaint();
        if !self.tty {
            eprintln!("  ✗ {detail}");
        }
    }

    pub fn check_pending(&self, label: &str) {
        self.items.borrow_mut().push(Item {
            label: label.to_string(),
            state: ItemState::Pending,
            detail: String::new(),
        });
        self.repaint();
    }

    /// Activate an existing pending item with this label, or `check_start`.
    pub fn check_activate(&self, label: &str) {
        let mut found = false;
        {
            let mut items = self.items.borrow_mut();
            for it in items.iter_mut() {
                if matches!(it.state, ItemState::Active) {
                    it.state = ItemState::Done;
                }
            }
            if let Some(it) = items
                .iter_mut()
                .find(|i| matches!(i.state, ItemState::Pending) && i.label == label)
            {
                it.state = ItemState::Active;
                it.detail.clear();
                found = true;
            }
        }
        if !found {
            self.check_start(label);
        } else {
            self.repaint();
            if !self.tty {
                eprintln!("  {label} …");
            }
        }
    }

    /// Start / replace live measurement bars.
    pub fn live_set(&self, title: &str, bars: &[(&str, f64, &str)]) {
        *self.live_title.borrow_mut() = Some(title.to_string());
        {
            let mut v = self.bars.borrow_mut();
            v.clear();
            for (name, frac, detail) in bars {
                v.push(Bar {
                    name: (*name).to_string(),
                    frac: (*frac).clamp(0.0, 1.0),
                    detail: (*detail).to_string(),
                });
            }
        }
        self.repaint();
    }

    pub fn live_clear(&self) {
        *self.live_title.borrow_mut() = None;
        self.bars.borrow_mut().clear();
        self.repaint();
    }

    /// Clear live bars, print a permanent result line; checklist stays until `end_round`.
    pub fn permanent(&self, line: &str) {
        *self.live_title.borrow_mut() = None;
        self.bars.borrow_mut().clear();
        if self.tty {
            self.clear_temp();
        }
        eprintln!("{line}");
        if self.tty {
            self.repaint();
        }
    }

    fn clear_temp(&self) {
        let n = self.painted_lines.get();
        if self.tty && n > 0 {
            // Move up one logical line at a time and erase — works when DECAWM was
            // off during paint (no wrap), so logical lines == physical rows.
            for _ in 0..n {
                eprint!("\x1b[1A\x1b[2K");
            }
            let _ = io::stderr().flush();
        }
        self.painted_lines.set(0);
    }

    fn repaint(&self) {
        if !self.tty {
            return;
        }
        self.clear_temp();
        let mut lines = 0usize;
        let spin = SPINNER[self.spin_i.get() % SPINNER.len()];
        self.spin_i.set(self.spin_i.get() + 1);

        let items: Vec<(String, ItemState, String)> = self
            .items
            .borrow()
            .iter()
            .map(|it| (it.label.clone(), it.state, it.detail.clone()))
            .collect();
        let live_title = self.live_title.borrow().clone();
        let bars: Vec<(String, f64, String)> = self
            .bars
            .borrow()
            .iter()
            .map(|b| (b.name.clone(), b.frac, b.detail.clone()))
            .collect();
        let eta = self.eta.borrow().clone();

        // Disable autowrap so CJK labels don't consume extra physical rows and
        // break the cursor-up clear (common on ssh -t / Cursor terminals).
        eprint!("\x1b[?7l");

        for (label, state, detail) in &items {
            let mark = match state {
                ItemState::Done => "√".to_string(),
                ItemState::Failed => "✗".to_string(),
                ItemState::Pending => ".".to_string(),
                ItemState::Active => spin.to_string(),
            };
            eprintln!(
                "  {} {mark}{}",
                pad_cols(label, LABEL_COLS),
                fmt_detail(&truncate_cols(detail, 40))
            );
            lines += 1;
        }

        if let Some(title) = live_title.as_ref() {
            if !items.is_empty() {
                eprintln!();
                lines += 1;
            }
            eprintln!("  {title}");
            lines += 1;
            for (name, frac, detail) in &bars {
                eprintln!(
                    "  {:<4} {}  {}",
                    name,
                    glyph(*frac),
                    truncate_cols(detail, 48)
                );
                lines += 1;
            }
        }

        if let Some(eta) = eta.as_ref() {
            eprintln!("  {}", truncate_cols(eta, 72));
            lines += 1;
        }

        eprint!("\x1b[?7h");
        let _ = io::stderr().flush();
        self.painted_lines.set(lines);
    }
}

fn display_cols(ch: char) -> usize {
    // Enough for CJK checklist labels; ASCII = 1, most East-Asian = 2.
    if ch.is_ascii() {
        1
    } else {
        2
    }
}

fn pad_cols(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut cols = 0usize;
    for ch in s.chars() {
        let w = display_cols(ch);
        if cols + w > width {
            break;
        }
        out.push(ch);
        cols += w;
    }
    while cols < width {
        out.push(' ');
        cols += 1;
    }
    out
}

fn truncate_cols(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut cols = 0usize;
    for ch in s.chars() {
        let w = display_cols(ch);
        if cols + w > width {
            break;
        }
        out.push(ch);
        cols += w;
    }
    out
}

fn fmt_detail(d: &str) -> String {
    if d.is_empty() {
        String::new()
    } else {
        format!("  {d}")
    }
}

fn glyph(frac: f64) -> String {
    let filled = ((frac.clamp(0.0, 1.0) * BAR_W as f64).round() as usize).min(BAR_W);
    format!("[{}{}]", "#".repeat(filled), "-".repeat(BAR_W - filled))
}

pub fn fmt_short(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// Poll agent `/v1/dut_status` until UART shows `xpost ready`, or fall back to
/// `ping` after a short grace (XDP_DROP can delay/ starve the ready println).
pub async fn settle_checklist(ui: &RoundUi, agent: &AgentClient, budget: Duration) -> Result<()> {
    let label = format!("settle ≤{}s", budget.as_secs());
    ui.check_activate(&label);
    let start = Instant::now();
    let mut last = String::from("booting");
    /// Prefer UART ready; after this, ping is enough to leave settle.
    const PING_AFTER: Duration = Duration::from_secs(3);
    loop {
        match agent.dut_status().await {
            Ok(msg) => {
                last = msg.trim().to_string();
                if last.starts_with("ready") {
                    let left = budget.saturating_sub(start.elapsed());
                    ui.check_ok(&format!(
                        "{} ({} left)",
                        truncate_note(&last, 48),
                        fmt_short(left)
                    ));
                    return Ok(());
                }
                let left = budget.saturating_sub(start.elapsed());
                ui.check_note(&format!("{last} · {} left", fmt_short(left)));
            }
            Err(e) => {
                let left = budget.saturating_sub(start.elapsed());
                ui.check_note(&format!("dut_status err: {e:#} · {} left", fmt_short(left)));
            }
        }

        if start.elapsed() >= PING_AFTER {
            match agent.request("ping").await {
                Ok(m) if m.contains("OK ping") => {
                    ui.check_ok(&format!("ping-ok (uart={last})"));
                    return Ok(());
                }
                Ok(m) => ui.check_note(&format!("ping: {m}")),
                Err(e) => ui.check_note(&format!("ping: {e:#}")),
            }
        }

        if start.elapsed() >= budget {
            ui.check_fail(&format!("timeout; last={last}"));
            bail!("DUT not ready within {:?}; last status={last}", budget);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn truncate_note(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// Flash profile then wait agent idle, updating checklist + ETA flash timer.
pub async fn flash_idle(
    ui: &RoundUi,
    agent: &AgentClient,
    profile: &str,
    budget: Duration,
    eta: &mut BudgetEta,
) -> Result<Duration> {
    let label = format!("烧录 `{profile}`");
    ui.check_activate(&label);
    eta.begin_flash();
    ui.set_eta(&eta.fmt());
    let t0 = Instant::now();
    agent
        .flash_profile(profile)
        .await
        .with_context(|| format!("FlashProfile `{profile}`"))?;
    ui.check_note("waiting agent idle");
    let wait = agent.wait_idle(budget);
    tokio::pin!(wait);
    loop {
        tokio::select! {
            r = &mut wait => {
                r.context("wait agent idle after flash")?;
                break;
            }
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                ui.check_note(&format!("idle {}s", t0.elapsed().as_secs()));
                ui.set_eta(&eta.fmt());
            }
        }
    }
    let elapsed = t0.elapsed();
    eta.end_flash(elapsed);
    ui.check_ok(&fmt_short(elapsed));
    ui.set_eta(&eta.fmt());
    Ok(elapsed)
}
