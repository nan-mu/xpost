//! WiFi STA: UDP :8080 echo; optional :8081 control plane.

use crate::driver::FirewallDevice;
use crate::fw_cfg::{XDP_ENABLE, dut, log_cfg, probe, wifi};
use embassy_net::{
    IpAddress, IpEndpoint, Ipv4Address, StackResources,
    udp::{PacketMetadata, UdpSocket},
};
use esp_hal::{peripherals::WIFI, rng::Rng};
use esp_radio::wifi::{self as wifi_radio, Config, ControllerConfig, sta::StationConfig};
use log::{error, info};
use static_cell::StaticCell;

#[cfg(fw_us_hot_load)]
use crate::hot_load_op::{
    self, HotLoadOp, HotLoadOpAck, KIND_COPY, KIND_MOVE, KIND_READBACK, KIND_SWAP, KIND_UPLOAD,
    REGION_A, REGION_B, STORAGE_DRAM_0, STORAGE_DRAM_1, STORAGE_SRAM_A, STORAGE_SRAM_B,
};
#[cfg(fw_us_map_crud)]
use crate::map_crud_op::{self, MapCrudAck, MapCrudOp};
#[cfg(fw_us_map_crud)]
use crate::patch::map::ops::MapOps;
#[cfg(fw_us_hot_load)]
use crate::patch::{SlotId, read_slot, stage_raw_xdp, swap_hot_xdp, xdp_image};
#[cfg(fw_us_map_crud)]
use crate::patch::{datapath as dp, map::with_array_map as crud_with_array, map::with_hash_map};
#[cfg(any(fw_us_hot_load, fw_us_map_crud))]
use embassy_futures::select::{Either, select};

const ECHO_PAYLOAD: &[u8] = &[0x42];
const STIM_MAGIC: &[u8; 6] = b"FWSTIM";
const STIM_HDR_LEN: usize = 14;

#[inline]
fn echo_reply_payload(stim: &[u8]) -> &[u8] {
    if stim.len() >= STIM_HDR_LEN && stim.starts_with(STIM_MAGIC) {
        &stim[..STIM_HDR_LEN]
    } else {
        ECHO_PAYLOAD
    }
}

static STACK_RESOURCES: StaticCell<StackResources<8>> = StaticCell::new();

static RX_META: StaticCell<[PacketMetadata; 16]> = StaticCell::new();
static RX_BUFFER: StaticCell<[u8; 1536]> = StaticCell::new();
static TX_META: StaticCell<[PacketMetadata; 16]> = StaticCell::new();
static TX_BUFFER: StaticCell<[u8; 1536]> = StaticCell::new();

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
pub fn init_sta(
    wifi_periph: WIFI<'static>,
) -> (
    wifi_radio::WifiController<'static>,
    embassy_net::Stack<'static>,
    embassy_net::Runner<'static, FirewallDevice<wifi_radio::Interface<'static>>>,
) {
    info!("[Init][STA] initializing");
    log_cfg();
    if XDP_ENABLE {
        info!("[FWTEST] XDP path enabled");
    } else {
        info!("[FWTEST] XDP path bypassed");
    }

    let sta = Config::Station(
        StationConfig::default()
            .with_ssid(wifi::SSID)
            .with_password(wifi::PASSWORD.into()),
    );

    info!("[Init][STA] ssid={}", wifi::SSID);
    let (controller, interfaces) = wifi_radio::new(
        wifi_periph,
        ControllerConfig::default().with_initial_config(sta),
    )
    .unwrap();

    let sta = interfaces.station;
    let mac = sta.mac_address();
    info!(
        "[Init][STA] mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );

    info!("[Init][STA] wrapping FirewallDevice");
    let xdp_device = FirewallDevice::new(sta);

    let sta_config = embassy_net::Config::dhcpv4(Default::default());
    info!("[Init][STA] DHCPv4");

    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    info!("[Init][STA] net seed={}", seed);
    let resources = STACK_RESOURCES.init(StackResources::<8>::new());
    let (sta_stack, sta_runner) = embassy_net::new(xdp_device, sta_config, resources, seed);

    (controller, sta_stack, sta_runner)
}

#[inline]
pub async fn handler(sta_stack: embassy_net::Stack<'static>) {
    info!("[FWTEST] waiting for DHCP lease...");
    sta_stack.wait_config_up().await;
    if let Some(cfg) = sta_stack.config_v4() {
        info!("[FWTEST] STA IP {} gw={:?}", cfg.address, cfg.gateway);
    }

    let mut echo = UdpSocket::new(
        sta_stack,
        RX_META.init([PacketMetadata::EMPTY; 16]),
        RX_BUFFER.init([0; 1536]),
        TX_META.init([PacketMetadata::EMPTY; 16]),
        TX_BUFFER.init([0; 1536]),
    );
    if let Err(e) = echo.bind(dut::LISTEN_PORT) {
        error!("[FWTEST] bind :{} failed: {:?}", dut::LISTEN_PORT, e);
        return;
    }
    info!("[FWTEST] UDP echo bound :{}", dut::LISTEN_PORT);

    #[cfg(any(fw_us_hot_load, fw_us_map_crud))]
    {
        let mut ctrl = UdpSocket::new(
            sta_stack,
            CTRL_RX_META.init([PacketMetadata::EMPTY; 8]),
            CTRL_RX_BUFFER.init([0; CTRL_RX_CAP]),
            CTRL_TX_META.init([PacketMetadata::EMPTY; 8]),
            CTRL_TX_BUFFER.init([0; CTRL_TX_CAP]),
        );
        if let Err(e) = ctrl.bind(dut::CTRL_PORT) {
            error!("[FWTEST] bind :{} failed: {:?}", dut::CTRL_PORT, e);
            return;
        }
        info!(
            "[FWTEST] UDP ctrl bound :{} (rx_cap={})",
            dut::CTRL_PORT,
            CTRL_RX_CAP
        );
        recv_loop_with_ctrl(echo, ctrl).await;
    }

    #[cfg(not(any(fw_us_hot_load, fw_us_map_crud)))]
    recv_loop_echo_only(echo).await;
}

// ---------------------------------------------------------------------------
// MapCrudOp (generic map CRUD)
// ---------------------------------------------------------------------------

#[cfg(fw_us_map_crud)]
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
async fn apply_map_crud<'a>(op: MapCrudOp<'a>, val_out: &'a mut [u8; 256]) -> MapCrudAck<'a> {
    use map_crud_op::*;
    dp::disable();
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
        (KIND_DELETE, MAP_TYPE_ARRAY) => {
            crud_with_array(op.map_name, |m| MapOps::delete(m, op.key.as_ptr()))
                .await
                .map(|r| r.map(|_| &[][..]))
        }
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
        _ => {
            dp::enable();
            return MapCrudAck {
                kind: op.kind,
                status: STATUS_BAD,
                value: &[],
            };
        }
    };
    dp::enable();
    match result {
        Ok(Ok(val)) => {
            info!(
                "[CTRL][MAP] kind={} map={} ok val_len={}",
                op.kind,
                op.map_name,
                val.len()
            );
            MapCrudAck {
                kind: op.kind,
                status: map_crud_op::STATUS_OK,
                value: val,
            }
        }
        _ => {
            error!("[CTRL][MAP] kind={} map={} failed", op.kind, op.map_name);
            MapCrudAck {
                kind: op.kind,
                status: map_crud_op::STATUS_BAD,
                value: &[],
            }
        }
    }
}

#[cfg(fw_us_map_crud)]
#[cfg(feature = "patch-sync-st")]
async fn apply_map_crud<'a>(op: MapCrudOp<'a>, val_out: &'a mut [u8; 256]) -> MapCrudAck<'a> {
    use map_crud_op::*;
    dp::disable();
    let result = match (op.kind, op.map_type) {
        (KIND_UPDATE, MAP_TYPE_HASH) => with_hash_map(op.map_name, |m| {
            MapOps::update(m, op.key.as_ptr(), op.value.as_ptr(), op.flags)
        })
        .map(|r| r.map(|_| &[][..])),
        (KIND_DELETE, MAP_TYPE_HASH) => {
            with_hash_map(op.map_name, |m| MapOps::delete(m, op.key.as_ptr()))
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
        }
        (KIND_UPDATE, MAP_TYPE_ARRAY) => crud_with_array(op.map_name, |m| {
            MapOps::update(m, op.key.as_ptr(), op.value.as_ptr(), op.flags)
        })
        .map(|r| r.map(|_| &[][..])),
        (KIND_DELETE, MAP_TYPE_ARRAY) => {
            crud_with_array(op.map_name, |m| MapOps::delete(m, op.key.as_ptr()))
                .map(|r| r.map(|_| &[][..]))
        }
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
        }
        _ => {
            dp::enable();
            return MapCrudAck {
                kind: op.kind,
                status: STATUS_BAD,
                value: &[],
            };
        }
    };
    dp::enable();
    match result {
        Ok(Ok(val)) => {
            info!(
                "[CTRL][MAP] kind={} map={} ok val_len={}",
                op.kind,
                op.map_name,
                val.len()
            );
            MapCrudAck {
                kind: op.kind,
                status: map_crud_op::STATUS_OK,
                value: val,
            }
        }
        _ => {
            error!("[CTRL][MAP] kind={} map={} failed", op.kind, op.map_name);
            MapCrudAck {
                kind: op.kind,
                status: map_crud_op::STATUS_BAD,
                value: &[],
            }
        }
    }
}

// ---------------------------------------------------------------------------
// HotLoadOp (SWAP + UPLOAD + COPY + MOVE + READBACK)
// ---------------------------------------------------------------------------

#[cfg(fw_us_hot_load)]
fn bad_hot_ack(op: &HotLoadOp<'_>) -> HotLoadOpAck {
    HotLoadOpAck {
        kind: op.kind,
        region: op.region,
        dram_slot: op.dram_slot,
        status: hot_load_op::STATUS_BAD,
        stashed_len: 0,
        loaded_len: 0,
        readback_len: 0,
        copied_len: 0,
    }
}

#[cfg(fw_us_hot_load)]
fn storage_to_slot(id: u32) -> Option<SlotId> {
    match id {
        STORAGE_SRAM_A => Some(SlotId::A),
        STORAGE_SRAM_B => Some(SlotId::B),
        _ => None,
    }
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
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
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
#[cfg(feature = "patch-sync-st")]
async fn read_storage(id: u32) -> Option<&'static [u8]> {
    if let Some(slot) = storage_to_slot(id) {
        read_slot(slot)
    } else if let Some(idx) = storage_to_dram_idx(id) {
        xdp_image(idx as u8)
    } else {
        None
    }
}

#[cfg(fw_us_hot_load)]
async fn apply_hot_load(op: HotLoadOp<'_>) -> HotLoadOpAck {
    use crate::patch::datapath as dp2;

    match op.kind {
        KIND_SWAP => {
            let hot = match op.region {
                REGION_A => SlotId::A,
                REGION_B => SlotId::B,
                _ => return bad_hot_ack(&op),
            };
            #[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
            let result = swap_hot_xdp(op.dram_slot as u8, hot, op.payload).await;
            #[cfg(feature = "patch-sync-st")]
            let result = swap_hot_xdp(op.dram_slot as u8, hot, op.payload);

            match result {
                Ok((stashed, loaded)) => {
                    info!("[CTRL][HOT] swap stashed={} loaded={}", stashed, loaded);
                    HotLoadOpAck {
                        kind: op.kind,
                        region: op.region,
                        dram_slot: op.dram_slot,
                        status: hot_load_op::STATUS_OK,
                        stashed_len: stashed as u32,
                        loaded_len: loaded as u32,
                        readback_len: 0,
                        copied_len: 0,
                    }
                }
                Err(e) => {
                    error!("[CTRL][HOT] swap failed: {:?}", e);
                    bad_hot_ack(&op)
                }
            }
        }

        KIND_UPLOAD => {
            if op.payload.is_empty() {
                return bad_hot_ack(&op);
            }
            if let Some(idx) = storage_to_dram_idx(op.dst) {
                #[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
                let r = stage_raw_xdp(idx, op.payload).await;
                #[cfg(feature = "patch-sync-st")]
                let r = stage_raw_xdp(idx, op.payload);
                match r {
                    Ok(()) => {
                        info!("[CTRL][HOT] upload dram[{}] len={}", idx, op.payload.len());
                        HotLoadOpAck {
                            kind: op.kind,
                            region: 0,
                            dram_slot: idx as u32,
                            status: hot_load_op::STATUS_OK,
                            stashed_len: 0,
                            loaded_len: op.payload.len() as u32,
                            readback_len: 0,
                            copied_len: 0,
                        }
                    }
                    Err(e) => {
                        error!("[CTRL][HOT] upload failed: {:?}", e);
                        bad_hot_ack(&op)
                    }
                }
            } else if let Some(slot) = storage_to_slot(op.dst) {
                dp2::disable();
                use crate::patch::part::write_slot;
                let r = write_slot(slot, op.payload);
                dp2::enable();
                match r {
                    Ok(()) => {
                        info!(
                            "[CTRL][HOT] upload sram {:?} len={}",
                            slot,
                            op.payload.len()
                        );
                        HotLoadOpAck {
                            kind: op.kind,
                            region: 0,
                            dram_slot: 0,
                            status: hot_load_op::STATUS_OK,
                            stashed_len: 0,
                            loaded_len: op.payload.len() as u32,
                            readback_len: 0,
                            copied_len: 0,
                        }
                    }
                    Err(_) => bad_hot_ack(&op),
                }
            } else {
                bad_hot_ack(&op)
            }
        }

        KIND_COPY | KIND_MOVE => {
            let src_data = read_storage(op.src).await;
            let src_bytes = match src_data {
                Some(b) if !b.is_empty() => b,
                _ => {
                    error!("[CTRL][HOT] copy/move src={} empty", op.src);
                    return bad_hot_ack(&op);
                }
            };
            let copied = src_bytes.len();

            let ok = if let Some(idx) = storage_to_dram_idx(op.dst) {
                #[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
                let r = stage_raw_xdp(idx, src_bytes).await;
                #[cfg(feature = "patch-sync-st")]
                let r = stage_raw_xdp(idx, src_bytes);
                r.is_ok()
            } else if let Some(slot) = storage_to_slot(op.dst) {
                dp2::disable();
                use crate::patch::part::write_slot;
                let r = write_slot(slot, src_bytes);
                dp2::enable();
                r.is_ok()
            } else {
                false
            };

            if !ok {
                return bad_hot_ack(&op);
            }

            if op.kind == KIND_MOVE {
                if let Some(idx) = storage_to_dram_idx(op.src) {
                    #[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
                    let _ = crate::patch::remove_xdp(idx as u8).await;
                    #[cfg(feature = "patch-sync-st")]
                    let _ = crate::patch::remove_xdp(idx as u8);
                }
            }

            info!(
                "[CTRL][HOT] {} src={} dst={} bytes={}",
                if op.kind == KIND_COPY { "copy" } else { "move" },
                op.src,
                op.dst,
                copied
            );
            HotLoadOpAck {
                kind: op.kind,
                region: 0,
                dram_slot: 0,
                status: hot_load_op::STATUS_OK,
                stashed_len: 0,
                loaded_len: 0,
                readback_len: 0,
                copied_len: copied as u32,
            }
        }

        KIND_READBACK => {
            let src_data = read_storage(op.src).await;
            let rb_len = src_data.map(|b| b.len()).unwrap_or(0);
            info!("[CTRL][HOT] readback src={} len={}", op.src, rb_len);
            HotLoadOpAck {
                kind: op.kind,
                region: 0,
                dram_slot: 0,
                status: if rb_len > 0 {
                    hot_load_op::STATUS_OK
                } else {
                    hot_load_op::STATUS_BAD
                },
                stashed_len: 0,
                loaded_len: 0,
                readback_len: rb_len as u32,
                copied_len: 0,
            }
        }

        _ => bad_hot_ack(&op),
    }
}

// ---------------------------------------------------------------------------
// Control dispatcher
// ---------------------------------------------------------------------------

#[cfg(any(fw_us_hot_load, fw_us_map_crud))]
#[inline]
async fn recv_loop_with_ctrl(echo: UdpSocket<'static>, ctrl: UdpSocket<'static>) {
    let echo_dest = IpEndpoint::new(
        IpAddress::Ipv4(Ipv4Address::new(
            probe::IP[0],
            probe::IP[1],
            probe::IP[2],
            probe::IP[3],
        )),
        probe::REPLY_PORT,
    );
    let ack_dest = echo_dest;
    info!(
        "[FWTEST] echo/ack → {}.{}.{}.{}:{}",
        probe::IP[0],
        probe::IP[1],
        probe::IP[2],
        probe::IP[3],
        probe::REPLY_PORT
    );

    let mut echo_buf = [0u8; 1536];
    let mut ctrl_buf = [0u8; CTRL_RX_CAP];
    let mut ack_buf = [0u8; CTRL_TX_CAP];

    loop {
        match select(echo.recv_from(&mut echo_buf), ctrl.recv_from(&mut ctrl_buf)).await {
            Either::First(Ok((len, remote))) => {
                info!("[FWTEST] stim from {} len={}", remote.endpoint, len);
                let reply = echo_reply_payload(&echo_buf[..len]);
                if let Err(e) = echo.send_to(reply, echo_dest).await {
                    error!("[FWTEST] echo send failed: {:?}", e);
                } else {
                    info!("[FWTEST] echo sent ({} bytes)", reply.len());
                }
            }
            Either::First(Err(e)) => error!("[FWTEST] echo recv: {:?}", e),
            Either::Second(Ok((len, remote))) => {
                info!("[CTRL] from {} len={}", remote.endpoint, len);
                let sent = handle_ctrl(&ctrl_buf[..len], &mut ack_buf, &ctrl, ack_dest).await;
                if !sent {
                    error!("[CTRL] no ack sent");
                }
            }
            Either::Second(Err(e)) => error!("[CTRL] recv: {:?}", e),
        }
    }
}

#[cfg(any(fw_us_hot_load, fw_us_map_crud))]
async fn handle_ctrl(
    pkt: &[u8],
    ack_buf: &mut [u8],
    ctrl: &UdpSocket<'static>,
    ack_dest: IpEndpoint,
) -> bool {
    #[cfg(fw_us_hot_load)]
    {
        if let Some(k) = hot_load_op::peek_kind(pkt) {
            if matches!(
                k,
                KIND_SWAP | KIND_UPLOAD | KIND_COPY | KIND_MOVE | KIND_READBACK
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
                        }
                    }
                };
                return match hot_load_op::encode_ack(&ack, ack_buf) {
                    Some(n) => match ctrl.send_to(&ack_buf[..n], ack_dest).await {
                        Ok(()) => {
                            info!("[CTRL][HOT] ack status={}", ack.status);
                            true
                        }
                        Err(e) => {
                            error!("[CTRL][HOT] ack send: {:?}", e);
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
                        info!("[CTRL][MAP] ack status={}", ack.status);
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

    #[allow(unreachable_code)]
    {
        let _ = (pkt, ack_buf, ctrl, ack_dest);
        error!("[CTRL] unknown op");
        false
    }
}

#[cfg(not(any(fw_us_hot_load, fw_us_map_crud)))]
#[inline]
async fn recv_loop_echo_only(echo: UdpSocket<'static>) {
    let echo_dest = IpEndpoint::new(
        IpAddress::Ipv4(Ipv4Address::new(
            probe::IP[0],
            probe::IP[1],
            probe::IP[2],
            probe::IP[3],
        )),
        probe::REPLY_PORT,
    );
    info!(
        "[FWTEST] echo → {}.{}.{}.{}:{}",
        probe::IP[0],
        probe::IP[1],
        probe::IP[2],
        probe::IP[3],
        probe::REPLY_PORT
    );

    let mut echo_buf = [0u8; 1536];
    loop {
        match echo.recv_from(&mut echo_buf).await {
            Ok((len, remote)) => {
                info!("[FWTEST] stim from {} len={}", remote.endpoint, len);
                let reply = echo_reply_payload(&echo_buf[..len]);
                if let Err(e) = echo.send_to(reply, echo_dest).await {
                    error!("[FWTEST] echo send failed: {:?}", e);
                } else {
                    info!("[FWTEST] echo sent ({} bytes)", reply.len());
                }
            }
            Err(e) => error!("[FWTEST] echo recv: {:?}", e),
        }
    }
}
