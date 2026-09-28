//! test0 — docs/api driven boot seed + control negative/positive + abcd IP test.

use std::io::{self, Write};
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use anyhow::Result;
use rand::RngExt;
use test_common::PAYLOAD_HDR_LEN;
use test_common::hot_load_op::{
    HotLoadOp, KIND_DATAPATH, KIND_HOT_SWAP, KIND_READBACK, KIND_SWAP, KIND_UPLOAD, REGION_A,
    REGION_B, STORAGE_SRAM_A, STORAGE_SRAM_B,
};
use test_common::map_crud_op::{
    self, MapCrudOp, STATUS_BAD, STATUS_CONFLICT, STATUS_NOT_FOUND, STATUS_OK,
};

use crate::{config::CaseConfig, harness::Harness, tests::ProbeTest};
use test_report::{CaseReport, TestReport};

mod assets {
    #![allow(dead_code)] // DROP_BIN embedded for parity with hot_a assets; test0 uses PASS_BIN.
    include!(concat!(env!("OUT_DIR"), "/hot_a_assets.rs"));
}

pub struct Test0;

impl ProbeTest for Test0 {
    fn name(&self) -> &'static str {
        "test0"
    }

    fn run<'a>(
        &'a self,
        harness: &'a mut Harness,
        cases: &'a [CaseConfig],
    ) -> core::pin::Pin<Box<dyn core::future::Future<Output = Result<TestReport>> + Send + 'a>>
    {
        Box::pin(async move { run_test0(harness, cases).await })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    U,
    X,
    N,
}

impl Kind {
    fn idx(self) -> usize {
        match self {
            Kind::U => 0,
            Kind::X => 1,
            Kind::N => 2,
        }
    }
}

struct Peer {
    ip: Ipv4Addr,
    expect: Kind,
}

struct KindAccum {
    expected: u32,
    ok: u32,
    rtts_ms: Vec<f64>,
    /// TC L2 lengths of X bounces (empty for U/N).
    l2: Vec<u16>,
}

const PKTS_PER_PEER: usize = 16;
/// Gap between stimuli. Still no wait-for-reply; T0 stays per-packet at TC.
/// C3 STA RX queue is 16 DMA-backed frames. 2ms + per-TX UART `info!` overflowed
/// it (U 29/32, first-batch max RTT ~70ms). 4ms leaves slack for 1492B PASS+echo.
const IP_TEST_GAP: Duration = Duration::from_millis(4);
/// esp-radio WiFi Ethernet MTU is 1492 (L2 incl. 14B header, not Linux IP MTU 1500).
/// UDP payload = 1492 - 14 - 20 - 8.
const STIM_UDP_LEN: usize = 1450;
const STIM_PAD: usize = STIM_UDP_LEN - PAYLOAD_HDR_LEN;
const STIM_L2_LEN: usize = 14 + 20 + 8 + STIM_UDP_LEN;
const _: () = assert!(STIM_L2_LEN == 1492);
/// Must match `TX_SHRINK` in libbpf-mcu `firewall_array.c`.
const TX_SHRINK: usize = 64;
const BOUNCE_L2_LEN: usize = STIM_L2_LEN - TX_SHRINK;
const _: () = assert!(BOUNCE_L2_LEN == 1428);

fn gen_lan_ip(esp: Ipv4Addr, avoid: &[Ipv4Addr]) -> Ipv4Addr {
    let mut rng = rand::rng();
    let oct = esp.octets();
    loop {
        let host = rng.random_range(2..254u8);
        let ip = Ipv4Addr::new(oct[0], oct[1], oct[2], host);
        if ip != esp && !avoid.contains(&ip) {
            return ip;
        }
    }
}

fn case_fail(test: &str, case: &str, err: impl Into<String>) -> CaseReport {
    let error = err.into();
    println!("ACT  {case:<22}  FAIL  {error}");
    let _ = io::stdout().flush();
    CaseReport {
        test: test.into(),
        case: case.into(),
        expect_echo: false,
        got_echo: false,
        expect_bounce: false,
        got_bounce: false,
        pass: false,
        t0_ns: None,
        t1_ns: None,
        delta_ns: None,
        seq: 0,
        src_ip: String::new(),
        dst_ip: String::new(),
        expect_sport: None,
        got_sport: None,
        expect_cell: None,
        got_cell: None,
        error: Some(error),
    }
}

fn print_act(name: &str, t0: Instant, op: &str, expect: &str, got: &str, ok: bool) {
    let dt = t0.elapsed().as_millis();
    let tag = if ok { "ok" } else { "FAIL" };
    println!(
        "ACT  {name:<22}  dt={dt:>4}ms  {op}  expect {expect}  got {}  {tag}",
        got.trim()
    );
    let _ = io::stdout().flush();
}

fn empty_accums() -> [KindAccum; 3] {
    [
        KindAccum {
            expected: 0,
            ok: 0,
            rtts_ms: Vec::new(),
            l2: Vec::new(),
        },
        KindAccum {
            expected: 0,
            ok: 0,
            rtts_ms: Vec::new(),
            l2: Vec::new(),
        },
        KindAccum {
            expected: 0,
            ok: 0,
            rtts_ms: Vec::new(),
            l2: Vec::new(),
        },
    ]
}

fn rtt_summary(rtts: &[f64]) -> String {
    if rtts.is_empty() {
        return "-".to_string();
    }
    let n = rtts.len() as f64;
    let min = rtts.iter().copied().fold(f64::INFINITY, f64::min);
    let max = rtts.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let avg = rtts.iter().sum::<f64>() / n;
    let var = if rtts.len() < 2 {
        0.0
    } else {
        rtts.iter()
            .map(|x| {
                let d = x - avg;
                d * d
            })
            .sum::<f64>()
            / (n - 1.0)
    };
    format!("min={min:.2}  avg={avg:.2}  max={max:.2}  var={var:.3} ms")
}

fn l2_summary(seen: &[u16], want: u16) -> String {
    if seen.is_empty() {
        return format!("l2=? want={want}");
    }
    let min = *seen.iter().min().unwrap();
    let max = *seen.iter().max().unwrap();
    if min == max && min == want {
        format!("l2={min}")
    } else if min == max {
        format!("l2={min} want={want}")
    } else {
        format!("l2={min}..{max} want={want}")
    }
}

fn print_ip_test(accums: &[KindAccum; 3]) {
    for kind in [Kind::U, Kind::X, Kind::N] {
        let a = &accums[kind.idx()];
        if a.expected == 0 {
            continue;
        }
        let tag = if a.ok == a.expected { "" } else { "  FAIL" };
        match kind {
            Kind::N => println!("N  {:>2}/{}{tag}", a.ok, a.expected),
            Kind::U => println!(
                "U  {:>2}/{}  rtt  {}{tag}",
                a.ok,
                a.expected,
                rtt_summary(&a.rtts_ms)
            ),
            Kind::X => println!(
                "X  {:>2}/{}  {}  rtt  {}{tag}",
                a.ok,
                a.expected,
                l2_summary(&a.l2, BOUNCE_L2_LEN as u16),
                rtt_summary(&a.rtts_ms)
            ),
        }
    }
    let _ = io::stdout().flush();
}

fn ip_pass(accums: &[KindAccum; 3]) -> bool {
    accums.iter().all(|a| a.ok == a.expected)
}

fn expect_of(peer: &Peer, passthrough: bool) -> Kind {
    if passthrough { Kind::U } else { peer.expect }
}

async fn run_ip(
    harness: &mut Harness,
    peers: &[Peer],
    case: &str,
    passthrough: bool,
) -> CaseReport {
    match ip_test(harness, peers, passthrough).await {
        Ok(accums) => {
            print_ip_test(&accums);
            let x = &accums[Kind::X.idx()];
            if x.expected > 0 {
                let got = l2_summary(&x.l2, BOUNCE_L2_LEN as u16);
                let shrink_pass =
                    !x.l2.is_empty() && x.l2.iter().all(|&l| l as usize == BOUNCE_L2_LEN);
                print_act(
                    "shrink-tx",
                    Instant::now(),
                    "X bounce L2",
                    &format!("{BOUNCE_L2_LEN}"),
                    &got,
                    shrink_pass,
                );
            }
            let pass = ip_pass(&accums);
            CaseReport {
                test: "test0".into(),
                case: case.into(),
                expect_echo: true,
                got_echo: pass,
                expect_bounce: !passthrough,
                got_bounce: false,
                pass,
                t0_ns: None,
                t1_ns: None,
                delta_ns: None,
                seq: 0,
                src_ip: String::new(),
                dst_ip: String::new(),
                expect_sport: None,
                got_sport: None,
                expect_cell: None,
                got_cell: None,
                error: if pass {
                    None
                } else {
                    Some("U/X/N or bounce L2 shrink mismatch".into())
                },
            }
        }
        Err(e) => case_fail("test0", case, format!("{e:#}")),
    }
}

async fn ip_test(
    harness: &mut Harness,
    peers: &[Peer],
    passthrough: bool,
) -> Result<[KindAccum; 3]> {
    use rand::seq::SliceRandom;
    harness.reset_state().await;
    println!(
        "IP  stim L2={STIM_L2_LEN} udp={STIM_UDP_LEN}  bounce want L2={BOUNCE_L2_LEN} (adjust_tail -{TX_SHRINK})"
    );
    let mut jobs: Vec<usize> = (0..peers.len())
        .flat_map(|ri| std::iter::repeat(ri).take(PKTS_PER_PEER))
        .collect();
    jobs.shuffle(&mut rand::rng());
    let mut accums = empty_accums();
    let mut used = std::collections::HashSet::new();
    let mut planned: Vec<(usize, Kind, u64, Vec<u8>)> = Vec::with_capacity(jobs.len());
    {
        let mut rng = rand::rng();
        for ri in jobs {
            let expect = expect_of(&peers[ri], passthrough);
            accums[expect.idx()].expected += 1;
            let seq = loop {
                let s = rng.random::<u64>();
                if s != 0 && used.insert(s) {
                    break s;
                }
            };
            let mut pad = vec![0u8; STIM_PAD];
            rng.fill(&mut pad[..]);
            planned.push((ri, expect, seq, pad));
        }
    }

    let mut batch: Vec<(Kind, u64)> = Vec::with_capacity(planned.len());
    for (ri, expect, seq, pad) in planned {
        harness
            .send_stimulus_pad(peers[ri].ip, None, seq, &pad)
            .await?;
        batch.push((expect, seq));
        tokio::time::sleep(IP_TEST_GAP).await;
    }

    tokio::time::sleep(harness.pair_timeout()).await;

    for (expect, seq) in batch {
        let t0_ns = harness.wait_t0(seq, Some(Duration::ZERO)).await?;
        let echo = harness.wait_echo(seq, Some(Duration::ZERO)).await?;
        let bounce_meta = harness.wait_bounce_meta(seq, Some(Duration::ZERO)).await?;
        let bounce = bounce_meta.map(|(t, _)| t);
        let bounce_l2 = bounce_meta.map(|(_, l2)| l2);
        let got = match (echo.is_some(), bounce.is_some()) {
            (true, false) => Some(Kind::U),
            (false, true) => Some(Kind::X),
            (false, false) => Some(Kind::N),
            (true, true) => None,
        };
        if got == Some(Kind::X) {
            if let Some(l2) = bounce_l2 {
                accums[Kind::X.idx()].l2.push(l2);
            }
        }
        let shrink_ok =
            expect != Kind::X || bounce_l2.is_some_and(|l2| l2 as usize == BOUNCE_L2_LEN);
        if got == Some(expect) && shrink_ok {
            let slot = &mut accums[expect.idx()];
            slot.ok += 1;
            if expect != Kind::N {
                let t1 = if expect == Kind::U { echo } else { bounce };
                if let (Some(t0), Some(t1)) = (t0_ns, t1) {
                    if t1 >= t0 {
                        slot.rtts_ms.push((t1 - t0) as f64 / 1_000_000.0);
                    }
                }
            }
        }
    }
    Ok(accums)
}

async fn run_test0(harness: &mut Harness, cases: &[CaseConfig]) -> Result<TestReport> {
    let ban_ip = cases
        .iter()
        .find(|c| !c.expect_bounce)
        .map(|c| c.src_ip)
        .unwrap_or(Ipv4Addr::new(10, 0, 0, 1));
    let tx_ip = cases
        .iter()
        .find(|c| c.expect_bounce)
        .map(|c| c.src_ip)
        .unwrap_or(Ipv4Addr::new(10, 0, 0, 2));

    let mut reports = Vec::new();
    let ctrl = gen_lan_ip(harness.esp_ip, &[harness.esp_ip, ban_ip, tx_ip]);
    let a = gen_lan_ip(harness.esp_ip, &[harness.esp_ip, ban_ip, tx_ip, ctrl]);
    let b = gen_lan_ip(harness.esp_ip, &[harness.esp_ip, ban_ip, tx_ip, ctrl, a]);
    let c = tx_ip;
    let d = ban_ip;

    println!(
        "ACT  {:<22}  dt={:>4}ms  BAN_IP={ban_ip}  TX_IP={tx_ip}  rx=a:firewall_array  maps=firewall_array",
        "boot-seed", 0
    );
    println!("a={a} (U)  b={b} (U)  c={c} (X)  d={d} (N)");
    let _ = io::stdout().flush();

    let peers = [
        Peer {
            ip: a,
            expect: Kind::U,
        },
        Peer {
            ip: b,
            expect: Kind::U,
        },
        Peer {
            ip: c,
            expect: Kind::X,
        },
        Peer {
            ip: d,
            expect: Kind::N,
        },
    ];
    let mut passthrough = false;
    let mut swapped_to_b = false;

    reports.push(run_ip(harness, &peers, "ip-test-boot-seed", passthrough).await);

    // delete-rx: UPLOAD empty to slot A (rx)
    let t0 = Instant::now();
    let op = HotLoadOp {
        kind: KIND_UPLOAD,
        region: 0,
        dram_slot: 0,
        src: 0,
        dst: STORAGE_SRAM_A,
        payload: &[],
    };
    match harness.hot_load_op(ctrl, op).await {
        Ok(ack) => {
            let got = format!("{} {}", ack.status, ack.reason);
            let pass = ack.status == STATUS_CONFLICT && ack.reason == "SLOT_IS_RX";
            print_act(
                "delete-rx",
                t0,
                "UPLOAD hot:a empty",
                "409 SLOT_IS_RX",
                &got,
                pass,
            );
            reports.push(CaseReport {
                test: "test0".into(),
                case: "delete-rx".into(),
                pass,
                expect_echo: false,
                got_echo: false,
                expect_bounce: false,
                got_bounce: false,
                t0_ns: None,
                t1_ns: None,
                delta_ns: None,
                seq: 0,
                src_ip: ctrl.to_string(),
                dst_ip: harness.esp_ip.to_string(),
                expect_sport: None,
                got_sport: None,
                expect_cell: None,
                got_cell: None,
                error: None,
            });
        }
        Err(e) => reports.push(case_fail("test0", "delete-rx", format!("{e:#}"))),
    }

    // delete-map-in-use
    let t0 = Instant::now();
    let op = MapCrudOp::simple(
        map_crud_op::KIND_DESTROY_MAP,
        "firewall_array",
        map_crud_op::MAP_TYPE_ARRAY,
        &[],
        &[],
        0,
    );
    match harness.map_crud_op(ctrl, op).await {
        Ok(ack) => {
            let got = format!("{} {}", ack.status, ack.reason);
            let pass = ack.status == STATUS_CONFLICT && ack.reason == "MAP_IN_USE";
            print_act(
                "delete-map-in-use",
                t0,
                "DESTROY_MAP firewall_array",
                "409 MAP_IN_USE",
                &got,
                pass,
            );
            reports.push(CaseReport {
                test: "test0".into(),
                case: "delete-map-in-use".into(),
                pass,
                expect_echo: false,
                got_echo: false,
                expect_bounce: false,
                got_bounce: false,
                t0_ns: None,
                t1_ns: None,
                delta_ns: None,
                seq: 0,
                src_ip: ctrl.to_string(),
                dst_ip: harness.esp_ip.to_string(),
                expect_sport: None,
                got_sport: None,
                expect_cell: None,
                got_cell: None,
                error: None,
            });
        }
        Err(e) => reports.push(case_fail("test0", "delete-map-in-use", format!("{e:#}"))),
    }

    // ctrl-negative / positive extras
    async fn check_map(
        harness: &mut Harness,
        ctrl: Ipv4Addr,
        name: &str,
        op: MapCrudOp<'_>,
        expect_status: u32,
        expect_reason: &str,
    ) -> CaseReport {
        let kind = op.kind;
        let t0 = Instant::now();
        match harness.map_crud_op(ctrl, op).await {
            Ok(ack) => {
                let got = format!("{} {}", ack.status, ack.reason);
                let expect = format!("{} {}", expect_status, expect_reason);
                let expect = expect.trim();
                let pass = ack.status == expect_status
                    && (expect_reason.is_empty() || ack.reason == expect_reason)
                    && ack.kind == kind;
                print_act(name, t0, name, expect, got.trim(), pass);
                CaseReport {
                    test: "test0".into(),
                    case: name.into(),
                    pass,
                    expect_echo: false,
                    got_echo: false,
                    expect_bounce: false,
                    got_bounce: false,
                    t0_ns: None,
                    t1_ns: None,
                    delta_ns: None,
                    seq: 0,
                    src_ip: ctrl.to_string(),
                    dst_ip: harness.esp_ip.to_string(),
                    expect_sport: None,
                    got_sport: None,
                    expect_cell: None,
                    got_cell: None,
                    error: None,
                }
            }
            Err(e) => case_fail("test0", name, format!("{e:#}")),
        }
    }

    reports.push(
        check_map(
            harness,
            ctrl,
            "array-element-delete",
            MapCrudOp::simple(
                map_crud_op::KIND_DELETE,
                "firewall_array",
                map_crud_op::MAP_TYPE_ARRAY,
                &0u32.to_ne_bytes(),
                &[],
                0,
            ),
            STATUS_BAD,
            "",
        )
        .await,
    );

    reports.push(
        check_map(
            harness,
            ctrl,
            "lookup-not-found",
            MapCrudOp::simple(
                map_crud_op::KIND_LOOKUP,
                "no_such_map",
                map_crud_op::MAP_TYPE_HASH,
                &[],
                &[],
                0,
            ),
            STATUS_NOT_FOUND,
            "NotFound",
        )
        .await,
    );

    reports.push(
        check_map(
            harness,
            ctrl,
            "create-badspec",
            MapCrudOp {
                kind: map_crud_op::KIND_CREATE,
                map_name: "sessions",
                map_type: map_crud_op::MAP_TYPE_HASH,
                key: &[],
                value: &[],
                flags: 0,
                key_size: 0,
                value_size: 1,
                max_entries: 8,
                aot_addr: 0,
            },
            STATUS_BAD,
            "BadSpec",
        )
        .await,
    );

    let key = 0u32.to_ne_bytes();
    reports.push(
        check_map(
            harness,
            ctrl,
            "update-busy",
            MapCrudOp::simple(
                map_crud_op::KIND_UPDATE,
                "firewall_array",
                map_crud_op::MAP_TYPE_ARRAY,
                &key,
                &ban_ip.octets(),
                0,
            ),
            STATUS_BAD,
            "Busy",
        )
        .await,
    );

    let t0 = Instant::now();
    let op = HotLoadOp {
        kind: KIND_SWAP,
        region: REGION_A,
        dram_slot: 0,
        src: 0,
        dst: 0,
        payload: &[],
    };
    reports.push(match harness.hot_load_op(ctrl, op).await {
        Ok(ack) => {
            print_act(
                "swap-empty",
                t0,
                "SWAP empty",
                "400",
                &format!("{} {}", ack.status, ack.reason),
                ack.status == STATUS_BAD,
            );
            CaseReport {
                test: "test0".into(),
                case: "swap-empty".into(),
                pass: ack.status == STATUS_BAD,
                expect_echo: false,
                got_echo: false,
                expect_bounce: false,
                got_bounce: false,
                t0_ns: None,
                t1_ns: None,
                delta_ns: None,
                seq: 0,
                src_ip: ctrl.to_string(),
                dst_ip: harness.esp_ip.to_string(),
                expect_sport: None,
                got_sport: None,
                expect_cell: None,
                got_cell: None,
                error: None,
            }
        }
        Err(e) => case_fail("test0", "swap-empty", format!("{e:#}")),
    });

    let t0 = Instant::now();
    let op = HotLoadOp {
        kind: KIND_HOT_SWAP,
        region: REGION_A,
        dram_slot: 0,
        src: 0,
        dst: 0,
        payload: &[],
    };
    reports.push(match harness.hot_load_op(ctrl, op).await {
        Ok(ack) => {
            print_act(
                "hot-swap-rx",
                t0,
                "HOT_SWAP rx",
                "400 NotRx",
                &format!("{} {}", ack.status, ack.reason),
                ack.status == STATUS_BAD,
            );
            CaseReport {
                test: "test0".into(),
                case: "hot-swap-rx".into(),
                pass: ack.status == STATUS_BAD,
                expect_echo: false,
                got_echo: false,
                expect_bounce: false,
                got_bounce: false,
                t0_ns: None,
                t1_ns: None,
                delta_ns: None,
                seq: 0,
                src_ip: ctrl.to_string(),
                dst_ip: harness.esp_ip.to_string(),
                expect_sport: None,
                got_sport: None,
                expect_cell: None,
                got_cell: None,
                error: None,
            }
        }
        Err(e) => case_fail("test0", "hot-swap-rx", format!("{e:#}")),
    });

    let t0 = Instant::now();
    let op = HotLoadOp {
        kind: KIND_UPLOAD,
        region: 0,
        dram_slot: 0,
        src: 0,
        dst: 99,
        payload: &[],
    };
    reports.push(match harness.hot_load_op(ctrl, op).await {
        Ok(ack) => {
            print_act(
                "invalid-storage",
                t0,
                "UPLOAD dst=99",
                "400",
                &format!("{} {}", ack.status, ack.reason),
                ack.status == STATUS_BAD,
            );
            CaseReport {
                test: "test0".into(),
                case: "invalid-storage".into(),
                pass: ack.status == STATUS_BAD,
                expect_echo: false,
                got_echo: false,
                expect_bounce: false,
                got_bounce: false,
                t0_ns: None,
                t1_ns: None,
                delta_ns: None,
                seq: 0,
                src_ip: ctrl.to_string(),
                dst_ip: harness.esp_ip.to_string(),
                expect_sport: None,
                got_sport: None,
                expect_cell: None,
                got_cell: None,
                error: None,
            }
        }
        Err(e) => case_fail("test0", "invalid-storage", format!("{e:#}")),
    });

    // READBACK rx
    let t0 = Instant::now();
    let op = HotLoadOp {
        kind: KIND_READBACK,
        region: 0,
        dram_slot: 0,
        src: STORAGE_SRAM_A,
        dst: 0,
        payload: &[],
    };
    reports.push(match harness.hot_load_op(ctrl, op).await {
        Ok(ack) => {
            print_act(
                "readback-rx",
                t0,
                "READBACK rx",
                "200 readback_len>0",
                &format!("{} len={}", ack.status, ack.readback_len),
                ack.status == STATUS_OK && ack.readback_len > 0,
            );
            CaseReport {
                test: "test0".into(),
                case: "readback-rx".into(),
                pass: ack.status == STATUS_OK && ack.readback_len > 0,
                expect_echo: false,
                got_echo: false,
                expect_bounce: false,
                got_bounce: false,
                t0_ns: None,
                t1_ns: None,
                delta_ns: None,
                seq: 0,
                src_ip: ctrl.to_string(),
                dst_ip: harness.esp_ip.to_string(),
                expect_sport: None,
                got_sport: None,
                expect_cell: None,
                got_cell: None,
                error: None,
            }
        }
        Err(e) => case_fail("test0", "readback-rx", format!("{e:#}")),
    });

    // UPLOAD pass-all into call slot B (rx stays A → no IP test)
    let t0 = Instant::now();
    let op = HotLoadOp {
        kind: KIND_UPLOAD,
        region: 0,
        dram_slot: 0,
        src: 0,
        dst: STORAGE_SRAM_B,
        payload: assets::PASS_BIN,
    };
    reports.push(match harness.hot_load_op(ctrl, op).await {
        Ok(ack) => {
            let pass = ack.status == STATUS_OK && ack.loaded_len > 0;
            print_act(
                "upload-call",
                t0,
                "UPLOAD pass → b",
                "200 loaded>0",
                &format!("{} len={}", ack.status, ack.loaded_len),
                pass,
            );
            CaseReport {
                test: "test0".into(),
                case: "upload-call".into(),
                pass,
                expect_echo: false,
                got_echo: false,
                expect_bounce: false,
                got_bounce: false,
                t0_ns: None,
                t1_ns: None,
                delta_ns: None,
                seq: 0,
                src_ip: ctrl.to_string(),
                dst_ip: harness.esp_ip.to_string(),
                expect_sport: None,
                got_sport: None,
                expect_cell: None,
                got_cell: None,
                error: None,
            }
        }
        Err(e) => case_fail("test0", "upload-call", format!("{e:#}")),
    });

    let t0 = Instant::now();
    let op = HotLoadOp {
        kind: KIND_HOT_SWAP,
        region: REGION_B,
        dram_slot: 0,
        src: 0,
        dst: 0,
        payload: &[],
    };
    reports.push(match harness.hot_load_op(ctrl, op).await {
        Ok(ack) => {
            let pass = ack.status == STATUS_OK;
            if pass {
                passthrough = true;
                swapped_to_b = true;
            }
            print_act(
                "hot-swap-pass",
                t0,
                "HOT_SWAP b (pass)",
                "200",
                &format!("{} {}", ack.status, ack.reason),
                pass,
            );
            CaseReport {
                test: "test0".into(),
                case: "hot-swap-pass".into(),
                pass,
                expect_echo: false,
                got_echo: false,
                expect_bounce: false,
                got_bounce: false,
                t0_ns: None,
                t1_ns: None,
                delta_ns: None,
                seq: 0,
                src_ip: ctrl.to_string(),
                dst_ip: harness.esp_ip.to_string(),
                expect_sport: None,
                got_sport: None,
                expect_cell: None,
                got_cell: None,
                error: None,
            }
        }
        Err(e) => case_fail("test0", "hot-swap-pass", format!("{e:#}")),
    });
    reports.push(run_ip(harness, &peers, "ip-test-hot-swap-pass", passthrough).await);

    if swapped_to_b {
        let t0 = Instant::now();
        let op = HotLoadOp {
            kind: KIND_HOT_SWAP,
            region: REGION_A,
            dram_slot: 0,
            src: 0,
            dst: 0,
            payload: &[],
        };
        reports.push(match harness.hot_load_op(ctrl, op).await {
            Ok(ack) => {
                let pass = ack.status == STATUS_OK;
                if pass {
                    passthrough = false;
                }
                print_act(
                    "hot-swap-restore",
                    t0,
                    "HOT_SWAP a (firewall)",
                    "200",
                    &format!("{} {}", ack.status, ack.reason),
                    pass,
                );
                CaseReport {
                    test: "test0".into(),
                    case: "hot-swap-restore".into(),
                    pass,
                    expect_echo: false,
                    got_echo: false,
                    expect_bounce: false,
                    got_bounce: false,
                    t0_ns: None,
                    t1_ns: None,
                    delta_ns: None,
                    seq: 0,
                    src_ip: ctrl.to_string(),
                    dst_ip: harness.esp_ip.to_string(),
                    expect_sport: None,
                    got_sport: None,
                    expect_cell: None,
                    got_cell: None,
                    error: None,
                }
            }
            Err(e) => case_fail("test0", "hot-swap-restore", format!("{e:#}")),
        });
        reports.push(run_ip(harness, &peers, "ip-test-hot-swap-restore", passthrough).await);
    }

    // DATAPATH disable → LOOKUP → enable
    let t0 = Instant::now();
    let op = HotLoadOp {
        kind: KIND_DATAPATH,
        region: 1,
        dram_slot: 0,
        src: 0,
        dst: 0,
        payload: &[],
    };
    reports.push(match harness.hot_load_op(ctrl, op).await {
        Ok(ack) => {
            let pass = ack.status == STATUS_OK;
            if pass {
                passthrough = true;
            }
            print_act(
                "datapath-disable",
                t0,
                "datapath-disable",
                "200",
                &format!("{}", ack.status),
                pass,
            );
            CaseReport {
                test: "test0".into(),
                case: "datapath-disable".into(),
                pass,
                expect_echo: false,
                got_echo: false,
                expect_bounce: false,
                got_bounce: false,
                t0_ns: None,
                t1_ns: None,
                delta_ns: None,
                seq: 0,
                src_ip: ctrl.to_string(),
                dst_ip: harness.esp_ip.to_string(),
                expect_sport: None,
                got_sport: None,
                expect_cell: None,
                got_cell: None,
                error: None,
            }
        }
        Err(e) => case_fail("test0", "datapath-disable", format!("{e:#}")),
    });
    reports.push(run_ip(harness, &peers, "ip-test-datapath-disable", passthrough).await);

    reports.push(
        check_map(
            harness,
            ctrl,
            "lookup-ban",
            MapCrudOp::simple(
                map_crud_op::KIND_LOOKUP,
                "firewall_array",
                map_crud_op::MAP_TYPE_ARRAY,
                &0u32.to_ne_bytes(),
                &[],
                0,
            ),
            STATUS_OK,
            "",
        )
        .await,
    );

    let t0 = Instant::now();
    reports.push(
        match harness
            .hot_load_op(
                ctrl,
                HotLoadOp {
                    kind: KIND_DATAPATH,
                    region: 2,
                    dram_slot: 0,
                    src: 0,
                    dst: 0,
                    payload: &[],
                },
            )
            .await
        {
            Ok(ack) => {
                let pass = ack.status == STATUS_OK;
                if pass {
                    passthrough = false;
                }
                print_act(
                    "datapath-enable",
                    t0,
                    "datapath-enable",
                    "200",
                    &format!("{}", ack.status),
                    pass,
                );
                CaseReport {
                    test: "test0".into(),
                    case: "datapath-enable".into(),
                    pass,
                    expect_echo: false,
                    got_echo: false,
                    expect_bounce: false,
                    got_bounce: false,
                    t0_ns: None,
                    t1_ns: None,
                    delta_ns: None,
                    seq: 0,
                    src_ip: ctrl.to_string(),
                    dst_ip: harness.esp_ip.to_string(),
                    expect_sport: None,
                    got_sport: None,
                    expect_cell: None,
                    got_cell: None,
                    error: None,
                }
            }
            Err(e) => case_fail("test0", "datapath-enable", format!("{e:#}")),
        },
    );
    reports.push(run_ip(harness, &peers, "ip-test-datapath-enable", passthrough).await);

    let ok = reports.iter().all(|c| c.pass);
    Ok(TestReport {
        name: "test0".into(),
        ok,
        cases: reports,
    })
}
