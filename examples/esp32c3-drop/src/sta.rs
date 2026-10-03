//! WiFi STA: UDP :8080 echo; optional :8081 control plane.

use crate::fw_cfg::{XDP_ENABLE, dut, log_cfg, probe, wifi};
use crate::{init_log, trace_log};
use core::cell::RefCell;
use critical_section::Mutex;
use embassy_executor::Spawner;
use embassy_net::{
    IpAddress, IpEndpoint, Ipv4Address, Stack, StackResources,
    udp::{PacketMetadata, UdpSocket},
};
use esp_hal::{peripherals::WIFI, rng::Rng};
use esp_radio::wifi::{self as wifi_radio, Config, ControllerConfig, sta::StationConfig};
use log::error;
use static_cell::StaticCell;
use xpost::driver::FirewallDevice;

#[cfg(fw_us_hot_load)]
use crate::hot_load_op::{
    self, HotLoadOp, HotLoadOpAck, KIND_COPY, KIND_DATAPATH, KIND_HOT_SWAP, KIND_MOVE,
    KIND_READBACK, KIND_REBOOT, KIND_SWAP, KIND_UPLOAD, REGION_2, REGION_3, REGION_A, REGION_B,
    STORAGE_DRAM_0, STORAGE_DRAM_1, STORAGE_SRAM_2, STORAGE_SRAM_3, STORAGE_SRAM_A, STORAGE_SRAM_B,
};
#[cfg(fw_us_map_crud)]
use crate::map_crud_op::{self, MapCrudAck, MapCrudOp};
#[cfg(fw_us_map_crud)]
use xpost::map::ops::{MapError, MapOps};
#[cfg(fw_us_hot_load)]
use xpost::{SlotId, hot_swap_rx, read_slot, stage_raw_xdp, swap_rx, xdp_image};
#[cfg(fw_us_map_crud)]
use xpost::{
    datapath as dp,
    map::{
        MapCreateCmd, MapCtrlError, MapKind, create_map, destroy_map, register_map_addr,
        register_map_name, with_array_map as crud_with_array, with_hash_map,
    },
};

const ECHO_PAYLOAD: &[u8] = &[0x42];
const STIM_MAGIC: &[u8; 6] = b"FWSTIM";
const COLLECT_MAGIC: &[u8; 6] = b"FWCOL1";
const DUMP_MAGIC: &[u8; 6] = b"FWDMP1";
const COLLECT_RST_MAGIC: &[u8; 6] = b"FWRST1";
const STIM_HDR_LEN: usize = 14;
const COLLECT_HDR_LEN: usize = 9;
const COLLECT_CAP: usize = 256;
const RICH_REPLY_LEN: usize = 34;
/// Dump reply: magic(6) + n(2) + COLLECT_CAP bytes.
const DUMP_REPLY_LEN: usize = 6 + 2 + COLLECT_CAP;

/// Shared with ctrl task: dump/rst on :8081 (XDP always PASS).
static COLLECT_BUF: Mutex<RefCell<[u8; COLLECT_CAP]>> =
    Mutex::new(RefCell::new([0u8; COLLECT_CAP]));

#[inline]
fn collect_store(idx: usize, byte: u8) {
    if idx < COLLECT_CAP {
        critical_section::with(|cs| {
            COLLECT_BUF.borrow_ref_mut(cs)[idx] = byte;
        });
    }
}

#[inline]
fn collect_reset() {
    critical_section::with(|cs| {
        COLLECT_BUF.borrow_ref_mut(cs).fill(0);
    });
}

#[inline]
fn collect_copy_out(dst: &mut [u8]) -> usize {
    let n = dst.len().min(COLLECT_CAP);
    critical_section::with(|cs| {
        dst[..n].copy_from_slice(&COLLECT_BUF.borrow_ref(cs)[..n]);
    });
    n
}

static STACK: StaticCell<Stack<'static>> = StaticCell::new();
static STACK_RESOURCES: StaticCell<StackResources<8>> = StaticCell::new();

/// Socket depth for PASS frames that reach echo. Larger than this does not help
/// a 64-packet air burst: esp-radio holds DMA buffers in its RX queue until
/// smoltcp drains them, and the Wi-Fi heap is only 66320 bytes.
const ECHO_RX_PKTS: usize = 32;
const ECHO_RX_BYTES: usize = ECHO_RX_PKTS * 1536;
const ECHO_TX_PKTS: usize = 32;
const ECHO_TX_BYTES: usize = 8192;

static RX_META: StaticCell<[PacketMetadata; ECHO_RX_PKTS]> = StaticCell::new();
static RX_BUFFER: StaticCell<[u8; ECHO_RX_BYTES]> = StaticCell::new();
static TX_META: StaticCell<[PacketMetadata; ECHO_TX_PKTS]> = StaticCell::new();
static TX_BUFFER: StaticCell<[u8; ECHO_TX_BYTES]> = StaticCell::new();

#[cfg(fw_us_hot_load)]
const CTRL_RX_CAP: usize = 4608;
#[cfg(all(fw_us_map_crud, not(fw_us_hot_load)))]
const CTRL_RX_CAP: usize = 512;
#[cfg(any(fw_us_hot_load, fw_us_map_crud))]
const CTRL_TX_CAP: usize = 512;

#[cfg(any(fw_us_hot_load, fw_us_map_crud))]
static CTRL_RX_META: StaticCell<[PacketMetadata; 8]> = StaticCell::new();
#[cfg(any(fw_us_hot_load, fw_us_map_crud))]
static CTRL_RX_BUFFER: StaticCell<[u8; CTRL_RX_CAP]> = StaticCell::new();
#[cfg(any(fw_us_hot_load, fw_us_map_crud))]
static CTRL_TX_META: StaticCell<[PacketMetadata; 8]> = StaticCell::new();
#[cfg(any(fw_us_hot_load, fw_us_map_crud))]
static CTRL_TX_BUFFER: StaticCell<[u8; CTRL_TX_CAP]> = StaticCell::new();

#[inline]
fn ipv4_octets(addr: IpAddress) -> [u8; 4] {
    let IpAddress::Ipv4(a) = addr;
    a.octets()
}

#[inline]
fn write_rich_echo(
    out: &mut [u8; RICH_REPLY_LEN],
    stim: &[u8],
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
) {
    if stim.len() >= STIM_HDR_LEN && stim.starts_with(STIM_MAGIC) {
        out[..STIM_HDR_LEN].copy_from_slice(&stim[..STIM_HDR_LEN]);
    } else {
        out[..STIM_HDR_LEN].fill(0);
    }
    out[14..18].copy_from_slice(&src_ip);
    out[18..22].copy_from_slice(&dst_ip);
    out[22..24].copy_from_slice(&src_port.to_le_bytes());
    out[24..26].copy_from_slice(&dst_port.to_le_bytes());
    let ts = embassy_time::Instant::now().as_micros() as u64;
    out[26..34].copy_from_slice(&ts.to_le_bytes());
}

#[inline]
pub fn echo_reply_payload(stim: &[u8]) -> &[u8] {
    if stim.len() >= STIM_HDR_LEN && stim.starts_with(STIM_MAGIC) {
        &stim[..STIM_HDR_LEN]
    } else {
        ECHO_PAYLOAD
    }
}

#[inline]
pub fn init_sta(
    wifi_periph: WIFI<'static>,
) -> (
    wifi_radio::WifiController<'static>,
    Stack<'static>,
    embassy_net::Runner<'static, FirewallDevice<wifi_radio::Interface<'static>>>,
) {
    init_log!("[Init][STA] initializing");
    log_cfg();
    if XDP_ENABLE {
        init_log!("[FWTEST] XDP path enabled");
    } else {
        init_log!("[FWTEST] XDP path bypassed");
    }

    let sta = Config::Station(
        StationConfig::default()
            .with_ssid(wifi::SSID)
            .with_password(wifi::PASSWORD.into()),
    );

    init_log!("[Init][STA] ssid={}", wifi::SSID);
    // Software RX queue holds PacketBuffers (DMA). Must stay strictly below
    // dynamic_rx_buf_num so the MAC keeps BA/DMA buffers. STA Wi-Fi already
    // uses ~47–57KB of the 66320-byte heap at defaults; leftover ~10KB fits
    // only a few 1492B TX copies (`esp_wifi_internal_tx` → 257 / ESP_ERR_NO_MEM).
    // Keep default tx_queue_size (3). Larger inflight of XDP_TX frames OOMs TX.
    let (controller, interfaces) = wifi_radio::new(
        wifi_periph,
        ControllerConfig::default()
            .with_rx_queue_size(16)
            .with_initial_config(sta),
    )
    .unwrap();

    let sta = interfaces.station;
    let mac = sta.mac_address();
    init_log!(
        "[Init][STA] mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5]
    );

    crate::wifi_zctx::init();
    init_log!("[Init][STA] wrapping FirewallDevice");
    let xdp_device = FirewallDevice::new(sta, XDP_ENABLE, Some(crate::wifi_zctx::HOOKS));

    let sta_config = embassy_net::Config::dhcpv4(Default::default());
    init_log!("[Init][STA] DHCPv4");

    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    init_log!("[Init][STA] net seed={}", seed);
    let resources = STACK_RESOURCES.init(StackResources::<8>::new());
    let (sta_stack, sta_runner) = embassy_net::new(xdp_device, sta_config, resources, seed);
    let stack = *STACK.init(sta_stack);

    (controller, stack, sta_runner)
}

#[inline]
pub fn start_udp_tasks(spawner: Spawner, stack: Stack<'static>) {
    spawner.spawn(echo_udp_task(stack).unwrap());
    #[cfg(any(fw_us_hot_load, fw_us_map_crud))]
    spawner.spawn(ctrl_udp_task(stack).unwrap());
        if let Some(n) = crate::drop_stats::try_handle(pkt, ack_buf) {
            return match ctrl.send_to(&ack_buf[..n], ack_dest).await {
                Ok(()) => true,
                Err(e) => {
                    error!("[CTRL] DROPST send failed: {:?}", e);
                    false
                }
            };
        }
        if let Some(n) = crate::perf_stats::try_handle(pkt, ack_buf) {
            return match ctrl.send_to(&ack_buf[..n], ack_dest).await {
                Ok(()) => true,
                Err(e) => {
                    error!("[CTRL] PERFST send failed: {:?}", e);
                    false
                }
            };
        }

    #[cfg(fw_us_hot_load)]
    spawner.spawn(crate::paper_timer::paper_datapath_timer_task().unwrap());
}

#[embassy_executor::task]
async fn echo_udp_task(stack: Stack<'static>) {
    init_log!("[FWTEST] waiting for DHCP lease...");
    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        init_log!("[FWTEST] STA IP {} gw={:?}", cfg.address, cfg.gateway);
    }

    let mut echo = UdpSocket::new(
        stack,
        RX_META.init([PacketMetadata::EMPTY; ECHO_RX_PKTS]),
        RX_BUFFER.init([0; ECHO_RX_BYTES]),
        TX_META.init([PacketMetadata::EMPTY; ECHO_TX_PKTS]),
        TX_BUFFER.init([0; ECHO_TX_BYTES]),
    );
    if let Err(e) = echo.bind(dut::LISTEN_PORT) {
        error!("[FWTEST] bind :{} failed: {:?}", dut::LISTEN_PORT, e);
        return;
    }
    init_log!("[FWTEST] UDP echo bound :{}", dut::LISTEN_PORT);

    let echo_dest = IpEndpoint::new(
        IpAddress::Ipv4(Ipv4Address::new(
            probe::IP[0],
            probe::IP[1],
            probe::IP[2],
            probe::IP[3],
        )),
        probe::REPLY_PORT,
    );
    trace_log!(
        "[FWTEST] echo → {}.{}.{}.{}:{}",
        probe::IP[0],
        probe::IP[1],
        probe::IP[2],
        probe::IP[3],
        probe::REPLY_PORT
    );

    let mut echo_buf = [0u8; 1536];
    #[cfg(any(fw_us_hot_load, fw_us_map_crud))]
    let mut rich_reply = [0u8; RICH_REPLY_LEN];

    loop {
        match echo.recv_from(&mut echo_buf).await {
            Ok((len, remote)) => {
                trace_log!("[FWTEST] stim from {} len={}", remote.endpoint, len);
                let pkt = &echo_buf[..len];

                // Byte-stream collect only on :8080 (datapath under test).
                // Dump / reset go via :8081 so they are not starved by WiFi RX.
                if len >= COLLECT_HDR_LEN && pkt.starts_with(COLLECT_MAGIC) {
                    let idx = u16::from_le_bytes([pkt[6], pkt[7]]) as usize;
                    collect_store(idx, pkt[8]);
                    continue;
                }

                #[cfg(any(fw_us_hot_load, fw_us_map_crud))]
                {
                    let dst_ip = stack
                        .config_v4()
                        .map(|c| c.address.address().octets())
                        .unwrap_or([0; 4]);
                    write_rich_echo(
                        &mut rich_reply,
                        pkt,
                        ipv4_octets(remote.endpoint.addr),
                        dst_ip,
                        remote.endpoint.port,
                        dut::LISTEN_PORT,
                    );
                    if let Err(e) = echo.send_to(&rich_reply, echo_dest).await {
                        error!("[FWTEST] echo send failed: {:?}", e);
                    } else {
                        trace_log!("[FWTEST] echo sent ({} bytes)", RICH_REPLY_LEN);
                    }
                }
                #[cfg(not(any(fw_us_hot_load, fw_us_map_crud)))]
                {
                    let reply = echo_reply_payload(pkt);
                    if let Err(e) = echo.send_to(reply, echo_dest).await {
                        error!("[FWTEST] echo send failed: {:?}", e);
                    } else {
                        trace_log!("[FWTEST] echo sent ({} bytes)", reply.len());
                    }
                }
            }
            Err(e) => error!("[FWTEST] echo recv: {:?}", e),
        }
    }
}

#[cfg(any(fw_us_hot_load, fw_us_map_crud))]
#[embassy_executor::task]
async fn ctrl_udp_task(stack: Stack<'static>) {
    stack.wait_config_up().await;

    let mut ctrl = UdpSocket::new(
        stack,
        CTRL_RX_META.init([PacketMetadata::EMPTY; 8]),
        CTRL_RX_BUFFER.init([0; CTRL_RX_CAP]),
        CTRL_TX_META.init([PacketMetadata::EMPTY; 8]),
        CTRL_TX_BUFFER.init([0; CTRL_TX_CAP]),
    );
    if let Err(e) = ctrl.bind(dut::CTRL_PORT) {
        error!("[FWTEST] bind :{} failed: {:?}", dut::CTRL_PORT, e);
        return;
    }
    init_log!(
        "[FWTEST] UDP ctrl bound :{} (rx_cap={})",
        dut::CTRL_PORT,
        CTRL_RX_CAP
    );

    let ack_dest = IpEndpoint::new(
        IpAddress::Ipv4(Ipv4Address::new(
            probe::IP[0],
            probe::IP[1],
            probe::IP[2],
            probe::IP[3],
        )),
        probe::REPLY_PORT,
    );
    trace_log!(
        "[FWTEST] ctrl ack → {}.{}.{}.{}:{}",
        probe::IP[0],
        probe::IP[1],
        probe::IP[2],
        probe::IP[3],
        probe::REPLY_PORT
    );

    let mut ctrl_buf = [0u8; CTRL_RX_CAP];
    let mut ack_buf = [0u8; CTRL_TX_CAP];
    let mut dump_reply = [0u8; DUMP_REPLY_LEN];

    loop {
        match ctrl.recv_from(&mut ctrl_buf).await {
            Ok((len, remote)) => {
                trace_log!("[CTRL] from {} len={}", remote.endpoint, len);
                let pkt = &ctrl_buf[..len];

                // Collect reset / dump on ctrl plane (always XDP PASS).
                if len >= 6 && pkt.starts_with(COLLECT_RST_MAGIC) {
                    collect_reset();
                    if let Err(e) = ctrl.send_to(COLLECT_RST_MAGIC, ack_dest).await {
                        error!("[CTRL] collect rst ack failed: {:?}", e);
                    }
                    continue;
                }
                if len >= 6 && pkt.starts_with(DUMP_MAGIC) {
                    let n = if len >= 8 {
                        let want = u16::from_le_bytes([pkt[6], pkt[7]]) as usize;
                        want.min(COLLECT_CAP)
                    } else {
                        COLLECT_CAP
                    };
                    dump_reply[..6].copy_from_slice(DUMP_MAGIC);
                    dump_reply[6..8].copy_from_slice(&(n as u16).to_le_bytes());
                    collect_copy_out(&mut dump_reply[8..8 + n]);
                    if let Err(e) = ctrl.send_to(&dump_reply[..8 + n], ack_dest).await {
                        error!("[CTRL] dump send failed: {:?}", e);
                    } else {
                        trace_log!("[CTRL] dump sent ({} bytes)", 8 + n);
                    }
                    continue;
                }

                let sent = handle_ctrl(pkt, &mut ack_buf, &ctrl, ack_dest).await;
                if !sent {
                    error!("[CTRL] no ack sent");
                }
            }
            Err(e) => error!("[CTRL] recv: {:?}", e),
        }
    }
}

// ---------------------------------------------------------------------------
// MapCrudOp (generic map CRUD)
// ---------------------------------------------------------------------------

#[cfg(fw_us_map_crud)]
fn intern_ctrl_map_name(name: &str) -> Option<&'static str> {
    match name {
        "rxcnt" => Some("rxcnt"),
        "rxcnt" => Some("rxcnt"),
        "jmp_table" => Some("jmp_table"),
        "fw_rules" => Some("fw_rules"),
        "sessions" => Some("sessions"),
        "firewall_array" => Some("firewall_array"),
        "ddos_wl" => Some("ddos_wl"),
        "ctrl_wl" => Some("ctrl_wl"),
        "rate_state" => Some("rate_state"),
        "params" => Some("params"),
        "stats" => Some("stats"),
        "decisions" => Some("decisions"),
        "decision_w" => Some("decision_w"),
        _ => None,
    }
}

#[cfg(fw_us_map_crud)]
fn wire_map_kind(t: u32) -> Option<MapKind> {
    use map_crud_op::*;
    match t {
        MAP_TYPE_HASH => Some(MapKind::Hash),
        MAP_TYPE_ARRAY => Some(MapKind::Array),
        MAP_TYPE_PROG_ARRAY => Some(MapKind::ProgArray),
        MAP_TYPE_RINGBUF => Some(MapKind::Ringbuf),
        _ => None,
    }
}

#[cfg(fw_us_map_crud)]
fn map_crud_bad<'a>(kind: u32, reason: &'a str) -> MapCrudAck<'a> {
    MapCrudAck {
        kind,
        status: map_crud_op::STATUS_BAD,
        value: &[],
        reason,
    }
}

#[cfg(fw_us_map_crud)]
fn map_crud_ok<'a>(kind: u32, value: &'a [u8]) -> MapCrudAck<'a> {
    MapCrudAck {
        kind,
        status: map_crud_op::STATUS_OK,
        value,
        reason: "",
    }
}

#[cfg(fw_us_map_crud)]
fn map_crud_not_found<'a>(kind: u32) -> MapCrudAck<'a> {
    MapCrudAck {
        kind,
        status: map_crud_op::STATUS_NOT_FOUND,
        value: &[],
        reason: "NotFound",
    }
}

#[cfg(fw_us_map_crud)]
fn map_crud_conflict<'a>(kind: u32, reason: &'a str) -> MapCrudAck<'a> {
    MapCrudAck {
        kind,
        status: map_crud_op::STATUS_CONFLICT,
        value: &[],
        reason,
    }
}

#[cfg(fw_us_map_crud)]
async fn apply_map_destroy<'a>(op: MapCrudOp<'a>) -> MapCrudAck<'a> {
    use map_crud_op::KIND_DESTROY_MAP;
    let Some(id) = xpost::map::resolve_map_name(op.map_name) else {
        return map_crud_not_found(KIND_DESTROY_MAP);
    };
    match destroy_map(id).await {
        Ok(()) => map_crud_ok(KIND_DESTROY_MAP, &[]),
        Err(MapCtrlError::InUse) => map_crud_conflict(KIND_DESTROY_MAP, "MAP_IN_USE"),
        Err(MapCtrlError::NotFound) => map_crud_not_found(KIND_DESTROY_MAP),
        Err(e) => {
            error!("[CTRL][MAP] DESTROY failed: {:?}", e);
            map_crud_bad(KIND_DESTROY_MAP, "")
        }
    }
}

#[cfg(fw_us_map_crud)]
#[cfg(fw_us_map_crud)]
async fn apply_map_create<'a>(op: MapCrudOp<'a>) -> MapCrudAck<'a> {
    use map_crud_op::*;
    let Some(kind) = wire_map_kind(op.map_type) else {
        return map_crud_bad(op.kind, "");
    };
    if matches!(kind, MapKind::Hash | MapKind::Array) && op.key_size == 0 {
        return map_crud_bad(op.kind, "BadSpec");
    }
    let Some(name) = intern_ctrl_map_name(op.map_name) else {
        error!("[CTRL][MAP] CREATE unknown map_name={}", op.map_name);
        return map_crud_bad(op.kind, "");
    };
    if let Some(id) = xpost::map::resolve_map_name(name) {
        if op.aot_addr != 0 {
            if !register_map_addr(op.aot_addr as usize, id) {
                error!("[CTRL][MAP] CREATE alias bind failed");
                return map_crud_bad(op.kind, "");
            }
            trace_log!(
                "[CTRL][MAP] CREATE alias map={} id={} aot=0x{:x}",
                name,
                id,
                op.aot_addr
            );
            return map_crud_ok(KIND_CREATE, &[]);
        }
        error!("[CTRL][MAP] CREATE name already registered: {}", name);
        return map_crud_bad(op.kind, "");
    }
    let cmd = MapCreateCmd {
        kind,
        key_size: op.key_size as usize,
        value_size: op.value_size as usize,
        max_entries: op.max_entries as usize,
        id: None,
    };
    dp::disable();
    let result = create_map(cmd).await;
    match result {
        Ok(id) => {
            if !register_map_name(name, id) {
                error!("[CTRL][MAP] CREATE register_map_name failed");
                dp::enable();
                return map_crud_bad(op.kind, "");
            }
            if op.aot_addr != 0 && !register_map_addr(op.aot_addr as usize, id) {
                error!("[CTRL][MAP] CREATE register_map_addr failed");
                dp::enable();
                return map_crud_bad(op.kind, "");
            }
            // Leave datapath off so host can keep using :8081 (paper XDP
            // TX/DROPs control). Callers that need RX call DATAPATH enable.
            trace_log!(
                "[CTRL][MAP] CREATE map={} id={} aot=0x{:x} (dp left disabled)",
                name,
                id,
                op.aot_addr
            );
            map_crud_ok(KIND_CREATE, &[])
        }
        Err(e) => {
            dp::enable();
            error!("[CTRL][MAP] CREATE failed: {:?}", e);
            map_crud_bad(op.kind, "")
        }
    }
}

#[cfg(fw_us_map_crud)]
#[cfg(fw_us_map_crud)]
fn map_crud_from_result<'a>(
    kind: u32,
    map_name: &str,
    result: Result<Result<&'a [u8], MapError>, MapError>,
) -> MapCrudAck<'a> {
    match result {
        Ok(Ok(val)) => {
            trace_log!(
                "[CTRL][MAP] kind={} map={} ok val_len={}",
                kind,
                map_name,
                val.len()
            );
            map_crud_ok(kind, val)
        }
        Ok(Err(MapError::NotFound)) => map_crud_not_found(kind),
        Ok(Err(_)) => {
            error!("[CTRL][MAP] kind={} map={} failed", kind, map_name);
            map_crud_bad(kind, "")
        }
        Err(MapError::NotFound) => map_crud_not_found(kind),
        Err(MapError::Busy) => map_crud_bad(kind, "Busy"),
        Err(_) => {
            error!("[CTRL][MAP] kind={} map={} failed", kind, map_name);
            map_crud_bad(kind, "")
        }
    }
}

#[cfg(fw_us_map_crud)]
async fn apply_map_crud<'a>(op: MapCrudOp<'a>, val_out: &'a mut [u8; 256]) -> MapCrudAck<'a> {
    use map_crud_op::*;
    if op.kind == KIND_CREATE {
        return apply_map_create(op).await;
    }
    if op.kind == KIND_DESTROY_MAP {
        return apply_map_destroy(op).await;
    }
    if matches!(op.kind, KIND_UPDATE | KIND_DELETE | KIND_LOOKUP)
        && xpost::map::resolve_map_name(op.map_name).is_none()
    {
        return map_crud_not_found(op.kind);
    }
    if op.kind == KIND_DELETE && op.map_type == MAP_TYPE_ARRAY {
        return map_crud_bad(op.kind, "");
    }
    if matches!(op.kind, KIND_UPDATE | KIND_DELETE | KIND_LOOKUP) && dp::is_active() {
        return map_crud_bad(op.kind, "Busy");
    }
    let result = match (op.kind, op.map_type) {
        (KIND_UPDATE, MAP_TYPE_HASH) => with_hash_map(op.map_name, |m| {
            MapOps::update(m, op.key.as_ptr(), op.value.as_ptr(), op.flags)
        })
        .await
        .map(|r| r.map(|_| &[][..])),
        (KIND_DELETE, MAP_TYPE_HASH) => {
            with_hash_map(op.map_name, |m| MapOps::delete(m, op.key.as_ptr()))
                .await
                .map(|r| r.map(|_| &[][..]))
        }
        (KIND_LOOKUP, MAP_TYPE_HASH) => {
            with_hash_map(op.map_name, |m| match MapOps::lookup(m, op.key.as_ptr()) {
                Ok(ptr) => {
                    let val_len = op.value.len().max(1).min(256);
                    unsafe {
                        core::ptr::copy_nonoverlapping(ptr, val_out.as_mut_ptr(), val_len);
                    }
                    Ok(&val_out[..val_len] as &[u8])
                }
                Err(e) => Err(e),
            })
            .await
        }
        (KIND_UPDATE, MAP_TYPE_ARRAY) => crud_with_array(op.map_name, |m| {
            MapOps::update(m, op.key.as_ptr(), op.value.as_ptr(), op.flags)
        })
        .await
        .map(|r| r.map(|_| &[][..])),
        (KIND_LOOKUP, MAP_TYPE_ARRAY) => {
            crud_with_array(op.map_name, |m| match MapOps::lookup(m, op.key.as_ptr()) {
                Ok(ptr) => {
                    let val_len = op.value.len().max(1).min(256);
                    unsafe {
                        core::ptr::copy_nonoverlapping(ptr, val_out.as_mut_ptr(), val_len);
                    }
                    Ok(&val_out[..val_len] as &[u8])
                }
                Err(e) => Err(e),
            })
            .await
        }
        _ => return map_crud_bad(op.kind, ""),
    };
    map_crud_from_result(op.kind, op.map_name, result)
}

#[cfg(fw_us_map_crud)]
// ---------------------------------------------------------------------------
// HotLoadOp (SWAP + HOT_SWAP + UPLOAD + COPY + MOVE + READBACK)
// ---------------------------------------------------------------------------
#[cfg(fw_us_hot_load)]
fn hot_ack(
    op: &HotLoadOp<'_>,
    status: u32,
    reason: &'static str,
    stashed: u32,
    loaded: u32,
    readback: u32,
    copied: u32,
) -> HotLoadOpAck {
    HotLoadOpAck {
        kind: op.kind,
        region: op.region,
        dram_slot: op.dram_slot,
        status,
        stashed_len: stashed,
        loaded_len: loaded,
        readback_len: readback,
        copied_len: copied,
        sample_len: 0,
        sample: [0u8; 32],
        reason,
    }
}

#[cfg(fw_us_hot_load)]
fn bad_hot_ack(op: &HotLoadOp<'_>, reason: &'static str) -> HotLoadOpAck {
    hot_ack(op, hot_load_op::STATUS_BAD, reason, 0, 0, 0, 0)
}

#[cfg(fw_us_hot_load)]
fn storage_to_slot(id: u32) -> Option<SlotId> {
    let idx = match id {
        STORAGE_SRAM_A => 0u8,
        STORAGE_SRAM_B => 1,
        STORAGE_SRAM_2 => 2,
        STORAGE_SRAM_3 => 3,
        _ => return None,
    };
    SlotId::from_u8(idx)
}

#[cfg(fw_us_hot_load)]
fn region_to_slot(region: u32) -> Option<SlotId> {
    let idx = match region {
        REGION_A => 0u8,
        REGION_B => 1,
        REGION_2 => 2,
        REGION_3 => 3,
        _ => return None,
    };
    SlotId::from_u8(idx)
}

#[cfg(fw_us_hot_load)]
fn storage_to_dram_idx(id: u32) -> Option<usize> {
    match id {
        STORAGE_DRAM_0 => Some(0),
        STORAGE_DRAM_1 => Some(1),
        _ => None,
    }
}

#[cfg(fw_us_hot_load)]
async fn read_storage(id: u32) -> Option<&'static [u8]> {
    if let Some(slot) = storage_to_slot(id) {
        read_slot(slot)
    } else if let Some(idx) = storage_to_dram_idx(id) {
        xdp_image(idx as u8).await
    } else {
        None
    }
}

#[cfg(fw_us_hot_load)]
async fn apply_hot_load(op: HotLoadOp<'_>) -> HotLoadOpAck {
    use hot_load_op::{DATAPATH_DISABLE, DATAPATH_ENABLE, DATAPATH_ENABLE_FOR};
    use xpost::datapath as dp2;
    use xpost::mount::try_rx_slot;
    use xpost::program::ProgramError;

    fn prog_reason(e: ProgramError) -> &'static str {
        match e {
            ProgramError::NotRx => "NotRx",
            ProgramError::EmptySlot => "EmptySlot",
            _ => "",
        }
    }

    match op.kind {
        KIND_SWAP => {
            if op.payload.is_empty() {
                return bad_hot_ack(&op, "");
            }
            let requested = if op.region == 0 {
                None
            } else {
                let Some(slot) = region_to_slot(op.region) else {
                    return bad_hot_ack(&op, "");
                };
                Some(slot)
            };
            let stash = if op.dram_slot == 0 {
                None
            } else {
                Some(op.dram_slot as u8)
            };
            match swap_rx(op.payload, requested, stash).await {
                Ok((stashed, loaded)) => {
                    trace_log!("[CTRL][HOT] swap stashed={} loaded={}", stashed, loaded);
                    hot_ack(
                        &op,
                        hot_load_op::STATUS_OK,
                        "",
                        stashed as u32,
                        loaded as u32,
                        0,
                        0,
                    )
                }
                Err(e) => {
                    error!("[CTRL][HOT] swap failed: {:?}", e);
                    hot_ack(&op, hot_load_op::STATUS_BAD, prog_reason(e), 0, 0, 0, 0)
                }
            }
        }

        KIND_HOT_SWAP => {
            let Some(dst) = region_to_slot(op.region) else {
                return bad_hot_ack(&op, "");
            };
            if try_rx_slot() == Some(dst) {
                return hot_ack(&op, hot_load_op::STATUS_BAD, "NotRx", 0, 0, 0, 0);
            }
            match hot_swap_rx(dst, op.payload).await {
                Ok(loaded) => hot_ack(&op, hot_load_op::STATUS_OK, "", 0, loaded as u32, 0, 0),
                Err(e) => {
                    error!("[CTRL][HOT] hot_swap failed: {:?}", e);
                    hot_ack(&op, hot_load_op::STATUS_BAD, prog_reason(e), 0, 0, 0, 0)
                }
            }
        }

        KIND_UPLOAD => {
            if let Some(slot) = storage_to_slot(op.dst) {
                use xpost::part::write_slot;
                use xpost::prog_table;
                if op.payload.is_empty() {
                    if try_rx_slot() == Some(slot) {
                        return hot_ack(
                            &op,
                            hot_load_op::STATUS_CONFLICT,
                            "SLOT_IS_RX",
                            0,
                            0,
                            0,
                            0,
                        );
                    }
                    dp2::disable();
                    prog_table::unload_slot(slot);
                    dp2::enable();
                    trace_log!("[CTRL][HOT] upload sram {:?} unload", slot);
                    return hot_ack(&op, hot_load_op::STATUS_OK, "", 0, 0, 0, 0);
                }
                dp2::disable();
                let r = write_slot(slot, op.payload).map(|()| {
                    prog_table::publish_tail_callee(slot);
                });
                dp2::enable();
                match r {
                    Ok(()) => {
                        trace_log!(
                            "[CTRL][HOT] upload sram {:?} len={}",
                            slot,
                            op.payload.len()
                        );
                        hot_ack(
                            &op,
                            hot_load_op::STATUS_OK,
                            "",
                            0,
                            op.payload.len() as u32,
                            0,
                            0,
                        )
                    }
                    Err(_) => bad_hot_ack(&op, ""),
                }
            } else if op.payload.is_empty() {
                bad_hot_ack(&op, "")
            } else if let Some(idx) = storage_to_dram_idx(op.dst) {
                match stage_raw_xdp(idx, op.payload).await {
                    Ok(()) => hot_ack(
                        &op,
                        hot_load_op::STATUS_OK,
                        "",
                        0,
                        op.payload.len() as u32,
                        0,
                        0,
                    ),
                    Err(e) => {
                        error!("[CTRL][HOT] upload dram failed: {:?}", e);
                        bad_hot_ack(&op, "")
                    }
                }
            } else {
                bad_hot_ack(&op, "")
            }
        }

        KIND_COPY | KIND_MOVE => {
            let src_data = read_storage(op.src).await;
            let src_bytes = match src_data {
                Some(b) if !b.is_empty() => b,
                _ => {
                    error!("[CTRL][HOT] copy/move src={} empty", op.src);
                    return bad_hot_ack(&op, "");
                }
            };
            let copied = src_bytes.len();

            let ok = if let Some(idx) = storage_to_dram_idx(op.dst) {
                stage_raw_xdp(idx, src_bytes).await.is_ok()
            } else if let Some(slot) = storage_to_slot(op.dst) {
                use xpost::part::write_slot;
                use xpost::prog_table;
                dp2::disable();
                let r = write_slot(slot, src_bytes).map(|()| {
                    prog_table::publish_tail_callee(slot);
                });
                dp2::enable();
                r.is_ok()
            } else {
                false
            };

            if !ok {
                return bad_hot_ack(&op, "");
            }

            if op.kind == KIND_MOVE {
                if let Some(idx) = storage_to_dram_idx(op.src) {
                    let _ = xpost::remove_xdp(idx as u8).await;
                }
            }

            trace_log!(
                "[CTRL][HOT] {} src={} dst={} bytes={}",
                if op.kind == KIND_COPY { "copy" } else { "move" },
                op.src,
                op.dst,
                copied
            );
            hot_ack(&op, hot_load_op::STATUS_OK, "", 0, 0, 0, copied as u32)
        }

        KIND_READBACK => {
            let src_data = read_storage(op.src).await;
            let rb_len = src_data.map(|b| b.len()).unwrap_or(0);
            trace_log!("[CTRL][HOT] readback src={} len={}", op.src, rb_len);
            hot_ack(
                &op,
                if rb_len > 0 {
                    hot_load_op::STATUS_OK
                } else {
                    hot_load_op::STATUS_BAD
                },
                "",
                0,
                0,
                rb_len as u32,
                0,
            )
        }

        KIND_DATAPATH => match op.region {
            DATAPATH_DISABLE => {
                dp2::disable();
                hot_ack(&op, hot_load_op::STATUS_OK, "", 0, 0, 0, 0)
            }
            DATAPATH_ENABLE => {
                dp2::enable();
                hot_ack(&op, hot_load_op::STATUS_OK, "", 0, 0, 0, 0)
            }
            DATAPATH_ENABLE_FOR => {
                if op.dram_slot == 0 {
                    return bad_hot_ack(&op, "EnableForMs");
                }
                crate::paper_timer::arm_disable_after_ms(op.dram_slot);
                crate::drop_stats::set_window_start_us();
                dp2::enable();
                hot_ack(&op, hot_load_op::STATUS_OK, "", 0, 0, 0, 0)
            }
            _ => bad_hot_ack(&op, ""),
        },

        KIND_REBOOT => hot_ack(&op, hot_load_op::STATUS_OK, "", 0, 0, 0, 0),

        _ => bad_hot_ack(&op, ""),
    }
}

#[cfg(any(fw_us_hot_load, fw_us_map_crud))]
async fn handle_ctrl(
    pkt: &[u8],
    ack_buf: &mut [u8],
    ctrl: &UdpSocket<'static>,
    ack_dest: IpEndpoint,
) -> bool {
    if let Some(n) = crate::drop_stats::try_handle(pkt, ack_buf) {
        return match ctrl.send_to(&ack_buf[..n], ack_dest).await {
            Ok(()) => true,
            Err(e) => {
                error!("[CTRL] DROPST send failed: {:?}", e);
                false
            }
        };
    }
    if let Some(n) = crate::perf_stats::try_handle(pkt, ack_buf) {
        return match ctrl.send_to(&ack_buf[..n], ack_dest).await {
            Ok(()) => true,
            Err(e) => {
                error!("[CTRL] PERFST send failed: {:?}", e);
                false
            }
        };
    }

    #[cfg(fw_us_hot_load)]
    {
        if let Some(k) = hot_load_op::peek_kind(pkt) {
            if matches!(
                k,
                KIND_SWAP
                    | KIND_HOT_SWAP
                    | KIND_UPLOAD
                    | KIND_COPY
                    | KIND_MOVE
                    | KIND_READBACK
                    | KIND_DATAPATH
                    | KIND_REBOOT
            ) {
                let ack = match hot_load_op::decode_op(pkt) {
                    Ok(op) => apply_hot_load(op).await,
                    Err(e) => {
                        error!("[CTRL][HOT] decode: {:?}", e);
                        HotLoadOpAck {
                            kind: k,
                            region: REGION_A,
                            dram_slot: 1,
                            status: hot_load_op::STATUS_BAD,
                            stashed_len: 0,
                            loaded_len: 0,
                            readback_len: 0,
                            copied_len: 0,
                            sample_len: 0,
                            sample: [0u8; 32],
                            reason: "",
                        }
                    }
                };
                return match hot_load_op::encode_ack(&ack, ack_buf) {
                    Some(n) => match ctrl.send_to(&ack_buf[..n], ack_dest).await {
                        Ok(()) => {
                            trace_log!("[CTRL][HOT] ack status={}", ack.status);
                            if k == KIND_REBOOT {
                                embassy_time::Timer::after(embassy_time::Duration::from_millis(
                                    100,
                                ))
                                .await;
                                esp_hal::system::software_reset();
                            }
                            true
                        }
                        Err(e) => {
                            error!("[CTRL][HOT] ack send: {:?}", e);
                            if k == KIND_REBOOT {
                                embassy_time::Timer::after(embassy_time::Duration::from_millis(
                                    100,
                                ))
                                .await;
                                esp_hal::system::software_reset();
                            }
                            false
                        }
                    },
                    None => {
                        error!("[CTRL][HOT] ack encode failed");
                        false
                    }
                };
            }
        }
    }

    #[cfg(fw_us_map_crud)]
    {
        if let Ok(op) = map_crud_op::decode_op(pkt) {
            let mut val_tmp = [0u8; 256];
            let ack = apply_map_crud(op, &mut val_tmp).await;
            return match map_crud_op::encode_ack(&ack, ack_buf) {
                Some(n) => match ctrl.send_to(&ack_buf[..n], ack_dest).await {
                    Ok(()) => {
                        trace_log!("[CTRL][MAP] ack status={}", ack.status);
                        true
                    }
                    Err(e) => {
                        error!("[CTRL][MAP] ack send: {:?}", e);
                        false
                    }
                },
                None => {
                    error!("[CTRL][MAP] ack encode failed");
                    false
                }
            };
        }
    }

    error!("[CTRL] unknown op");
    false
}
