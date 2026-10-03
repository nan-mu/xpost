//! WiFi STA: ICMP drop counter + UDP ctrl `DROPST` (pure HAL, no XDP).

use crate::drop_stats;
use crate::fw_cfg::{dut, log_cfg, probe, wifi};
use crate::{init_log, trace_log};
use embassy_executor::Spawner;
use embassy_net::{
    IpAddress, IpEndpoint, Ipv4Address, Stack, StackResources,
    icmp::{Socket as IcmpSocket, PacketMetadata as IcmpPacketMetadata},
    udp::{PacketMetadata, UdpSocket},
};
use esp_hal::{peripherals::WIFI, rng::Rng};
use esp_radio::wifi::{self as wifi_radio, Config, ControllerConfig, sta::StationConfig};
use log::error;
use static_cell::StaticCell;

static STACK: StaticCell<Stack<'static>> = StaticCell::new();
static STACK_RESOURCES: StaticCell<StackResources<8>> = StaticCell::new();

const ICMP_RX_PKTS: usize = 32;
const ICMP_RX_BYTES: usize = ICMP_RX_PKTS * 128;
const CTRL_RX_CAP: usize = 256;
const CTRL_TX_CAP: usize = 64;

static ICMP_RX_META: StaticCell<[IcmpPacketMetadata; ICMP_RX_PKTS]> = StaticCell::new();
static ICMP_RX_BUF: StaticCell<[u8; ICMP_RX_BYTES]> = StaticCell::new();

static CTRL_RX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
static CTRL_RX_BUF: StaticCell<[u8; CTRL_RX_CAP]> = StaticCell::new();
static CTRL_TX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
static CTRL_TX_BUF: StaticCell<[u8; CTRL_TX_CAP]> = StaticCell::new();

#[inline]
pub fn init_sta(
    wifi_periph: WIFI<'static>,
) -> (
    wifi_radio::WifiController<'static>,
    Stack<'static>,
    embassy_net::Runner<'static, wifi_radio::Interface<'static>>,
) {
    init_log!("[Init][STA] initializing (drop_hal RX drop)");
    log_cfg();

    let sta = Config::Station(
        StationConfig::default()
            .with_ssid(wifi::SSID)
            .with_password(wifi::PASSWORD.into()),
    );

    let (controller, interfaces) = wifi_radio::new(
        wifi_periph,
        ControllerConfig::default()
            .with_rx_queue_size(16)
            .with_initial_config(sta),
    )
    .unwrap();

    let sta = interfaces.station;
    let sta_config = embassy_net::Config::dhcpv4(Default::default());

    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    let resources = STACK_RESOURCES.init(StackResources::<8>::new());
    let (sta_stack, sta_runner) = embassy_net::new(sta, sta_config, resources, seed);
    let stack = *STACK.init(sta_stack);

    (controller, stack, sta_runner)
}

#[inline]
pub fn start_udp_tasks(spawner: Spawner, stack: Stack<'static>) {
    spawner.spawn(icmp_drop_task(stack).unwrap());
    spawner.spawn(ctrl_task(stack).unwrap());
}

#[embassy_executor::task]
async fn icmp_drop_task(stack: Stack<'static>) {
    init_log!("[FWTEST] waiting for DHCP lease...");
    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        init_log!("[FWTEST] STA IP {} gw={:?}", cfg.address, cfg.gateway);
    }

    let mut icmp = IcmpSocket::new(
        stack,
        ICMP_RX_META.init([IcmpPacketMetadata::EMPTY; ICMP_RX_PKTS]),
        ICMP_RX_BUF.init([0; ICMP_RX_BYTES]),
    );
    if let Err(e) = icmp.bind(1) {
        error!("[FWTEST] icmp bind failed: {:?}", e);
        return;
    }
    init_log!("[FWTEST] ICMP drop socket bound");

    let mut buf = [0u8; 128];
    loop {
        match icmp.recv_from(&mut buf).await {
            Ok((len, remote)) => {
                trace_log!("[FWTEST] icmp drop from {} len={}", remote.endpoint, len);
                drop_stats::record_packet();
            }
            Err(e) => error!("[FWTEST] icmp recv: {:?}", e),
        }
    }
}

#[embassy_executor::task]
async fn ctrl_task(stack: Stack<'static>) {
    stack.wait_config_up().await;

    let mut ctrl = UdpSocket::new(
        stack,
        CTRL_RX_META.init([PacketMetadata::EMPTY; 4]),
        CTRL_RX_BUF.init([0; CTRL_RX_CAP]),
        CTRL_TX_META.init([PacketMetadata::EMPTY; 4]),
        CTRL_TX_BUF.init([0; CTRL_TX_CAP]),
    );
    if let Err(e) = ctrl.bind(dut::CTRL_PORT) {
        error!("[FWTEST] ctrl bind :{} failed: {:?}", dut::CTRL_PORT, e);
        return;
    }
    init_log!("[FWTEST] UDP ctrl bound :{}", dut::CTRL_PORT);

    let ack_dest = IpEndpoint::new(
        IpAddress::Ipv4(Ipv4Address::new(
            probe::IP[0],
            probe::IP[1],
            probe::IP[2],
            probe::IP[3],
        )),
        probe::REPLY_PORT,
    );

    let mut ctrl_buf = [0u8; CTRL_RX_CAP];
    let mut reply = [0u8; drop_stats::REPLY_LEN];

    loop {
        match ctrl.recv_from(&mut ctrl_buf).await {
            Ok((len, _)) => {
                let pkt = &ctrl_buf[..len];
                if let Some(n) = drop_stats::try_handle(pkt, &mut reply) {
                    if let Err(e) = ctrl.send_to(&reply[..n], ack_dest).await {
                        error!("[CTRL] DROPST send failed: {:?}", e);
                    }
                }
            }
            Err(e) => error!("[CTRL] recv: {:?}", e),
        }
    }
}
