//! WiFi STA: UDP :8080 echo + UDP :8081 VipMapOp control.

use crate::driver::FirewallDevice;
use crate::fw_cfg::{
    FW_CTRL_PORT, FW_ECHO_PORT, FW_LISTEN_PORT, FW_TR3000_IP, XDP_ENABLE, log_cfg,
};
use crate::patch::datapath;
use crate::patch::map::ops::MapOps;
use crate::patch::map::with_array_map;
use crate::vip_map_op::{self, KIND_CLEAR, KIND_SET, STATUS_BAD, STATUS_OK, VipMapOp, VipMapOpAck};
use embassy_futures::select::{Either, select};
use embassy_net::{
    IpAddress, IpEndpoint, Ipv4Address, StackResources,
    udp::{PacketMetadata, UdpSocket},
};
use esp_hal::{peripherals::WIFI, rng::Rng};
use esp_radio::wifi::{self, Config, ControllerConfig, sta::StationConfig};
use log::{error, info};
use static_cell::StaticCell;

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASSWORD");

/// Fixed UDP payload for echo to TR3000 (no magic / seq).
const ECHO_PAYLOAD: &[u8] = &[0x42];

static STACK_RESOURCES: StaticCell<StackResources<8>> = StaticCell::new();

static RX_META: StaticCell<[PacketMetadata; 16]> = StaticCell::new();
static RX_BUFFER: StaticCell<[u8; 1536]> = StaticCell::new();
static TX_META: StaticCell<[PacketMetadata; 16]> = StaticCell::new();
static TX_BUFFER: StaticCell<[u8; 1536]> = StaticCell::new();

static CTRL_RX_META: StaticCell<[PacketMetadata; 8]> = StaticCell::new();
static CTRL_RX_BUFFER: StaticCell<[u8; 512]> = StaticCell::new();
static CTRL_TX_META: StaticCell<[PacketMetadata; 8]> = StaticCell::new();
static CTRL_TX_BUFFER: StaticCell<[u8; 512]> = StaticCell::new();

#[inline]
pub fn init_sta(
    wifi: WIFI<'static>,
) -> (
    wifi::WifiController<'static>,
    embassy_net::Stack<'static>,
    embassy_net::Runner<'static, FirewallDevice<wifi::Interface<'static>>>,
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
            .with_ssid(SSID)
            .with_password(PASSWORD.into()),
    );

    info!("[Init][STA] ssid={}", SSID);
    let (controller, interfaces) =
        wifi::new(wifi, ControllerConfig::default().with_initial_config(sta)).unwrap();

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

/// Bind :8080 (echo) and :8081 (VipMapOp); multiplex with `select`.
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
    if let Err(e) = echo.bind(FW_LISTEN_PORT) {
        error!("[FWTEST] bind :{} failed: {:?}", FW_LISTEN_PORT, e);
        return;
    }
    info!("[FWTEST] UDP echo bound :{}", FW_LISTEN_PORT);

    let mut ctrl = UdpSocket::new(
        sta_stack,
        CTRL_RX_META.init([PacketMetadata::EMPTY; 8]),
        CTRL_RX_BUFFER.init([0; 512]),
        CTRL_TX_META.init([PacketMetadata::EMPTY; 8]),
        CTRL_TX_BUFFER.init([0; 512]),
    );
    if let Err(e) = ctrl.bind(FW_CTRL_PORT) {
        error!("[FWTEST] bind :{} failed: {:?}", FW_CTRL_PORT, e);
        return;
    }
    info!("[FWTEST] UDP ctrl bound :{}", FW_CTRL_PORT);

    recv_loop(echo, ctrl).await;
}

async fn apply_vip_op(op: VipMapOp) -> VipMapOpAck {
    let key = 0u32;
    let new_vip = match op.kind {
        KIND_SET => op.vip_be,
        KIND_CLEAR => 0u32,
        _ => {
            return VipMapOpAck {
                kind: op.kind,
                vip_be: 0,
                status: STATUS_BAD,
            };
        }
    };

    // Stop AOT XDP so userspace may update ARRAY (driver PASSes frames meanwhile).
    datapath::disable();
    let applied = with_array_map("vip_ip", |m| {
        MapOps::update(
            m,
            (&raw const key) as *const u8,
            (&raw const new_vip) as *const u8,
            0,
        )
    })
    .await;
    datapath::enable();

    match applied {
        Ok(Ok(())) => {
            let octets = new_vip.to_ne_bytes();
            info!(
                "[CTRL] vip_ip[0]={}.{}.{}.{} kind={}",
                octets[0], octets[1], octets[2], octets[3], op.kind
            );
            VipMapOpAck {
                kind: op.kind,
                vip_be: new_vip,
                status: STATUS_OK,
            }
        }
        Ok(Err(e)) => {
            error!("[CTRL] vip update map err: {:?}", e);
            VipMapOpAck {
                kind: op.kind,
                vip_be: 0,
                status: STATUS_BAD,
            }
        }
        Err(e) => {
            error!("[CTRL] vip update busy/missing: {:?}", e);
            VipMapOpAck {
                kind: op.kind,
                vip_be: 0,
                status: STATUS_BAD,
            }
        }
    }
}

#[inline]
async fn recv_loop(echo: UdpSocket<'static>, ctrl: UdpSocket<'static>) {
    let echo_dest = IpEndpoint::new(
        IpAddress::Ipv4(Ipv4Address::new(
            FW_TR3000_IP[0],
            FW_TR3000_IP[1],
            FW_TR3000_IP[2],
            FW_TR3000_IP[3],
        )),
        FW_ECHO_PORT,
    );
    let ack_dest = echo_dest;
    info!(
        "[FWTEST] echo/ack → {}.{}.{}.{}:{}",
        FW_TR3000_IP[0], FW_TR3000_IP[1], FW_TR3000_IP[2], FW_TR3000_IP[3], FW_ECHO_PORT
    );

    let mut echo_buf = [0u8; 1536];
    let mut ctrl_buf = [0u8; 512];
    let mut ack_buf = [0u8; 64];

    loop {
        match select(echo.recv_from(&mut echo_buf), ctrl.recv_from(&mut ctrl_buf)).await {
            Either::First(Ok((len, remote))) => {
                info!("[FWTEST] stim from {} len={}", remote.endpoint, len);
                if let Err(e) = echo.send_to(ECHO_PAYLOAD, echo_dest).await {
                    error!("[FWTEST] echo send failed: {:?}", e);
                } else {
                    info!("[FWTEST] echo sent ({} bytes)", ECHO_PAYLOAD.len());
                }
            }
            Either::First(Err(e)) => error!("[FWTEST] echo recv: {:?}", e),
            Either::Second(Ok((len, remote))) => {
                info!("[CTRL] from {} len={}", remote.endpoint, len);
                let ack = match vip_map_op::decode_op(&ctrl_buf[..len]) {
                    Ok(op) => apply_vip_op(op).await,
                    Err(e) => {
                        error!("[CTRL] decode: {:?}", e);
                        VipMapOpAck {
                            kind: 0,
                            vip_be: 0,
                            status: STATUS_BAD,
                        }
                    }
                };
                if let Some(n) = vip_map_op::encode_ack(&ack, &mut ack_buf) {
                    if let Err(e) = ctrl.send_to(&ack_buf[..n], ack_dest).await {
                        error!("[CTRL] ack send failed: {:?}", e);
                    } else {
                        info!(
                            "[CTRL] ack kind={} status={} vip_be={:#x}",
                            ack.kind, ack.status, ack.vip_be
                        );
                    }
                } else {
                    error!("[CTRL] ack encode failed");
                }
            }
            Either::Second(Err(e)) => error!("[CTRL] recv: {:?}", e),
        }
    }
}
