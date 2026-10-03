//! WiFi STA + XDP drop bench: UDP :8081 stats only.

use crate::drop_stats;
use crate::fw_cfg::{dut, log_cfg, probe, wifi};
use embassy_executor::Spawner;
use embassy_net::{
    IpAddress, IpEndpoint, Ipv4Address, Stack, StackResources,
    udp::{PacketMetadata, UdpSocket},
};
use esp_hal::{peripherals::WIFI, rng::Rng};
use esp_radio::wifi::{self as wifi_radio, Config, ControllerConfig, sta::StationConfig};
use log::error;
use static_cell::StaticCell;

static STACK: StaticCell<Stack<'static>> = StaticCell::new();
static STACK_RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();

const CTRL_RX_CAP: usize = 64;
const CTRL_TX_CAP: usize = 64;
static CTRL_RX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
static CTRL_RX_BUF: StaticCell<[u8; CTRL_RX_CAP]> = StaticCell::new();
static CTRL_TX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
static CTRL_TX_BUF: StaticCell<[u8; CTRL_TX_CAP]> = StaticCell::new();

pub fn init_sta(
    wifi_periph: WIFI<'static>,
) -> (
    wifi_radio::WifiController<'static>,
    Stack<'static>,
    embassy_net::Runner<'static, wifi_radio::Interface<'static>>,
) {
    crate::init_log!("[Init][STA] initializing (drop_xdp)");
    log_cfg();

    let sta = Config::Station(
        StationConfig::default()
            .with_ssid(wifi::SSID)
            .with_password(wifi::PASSWORD.into()),
    );

    let (controller, interfaces) = xpost::xdp::new(
        wifi_periph,
        ControllerConfig::default()
            .with_rx_queue_size(16)
            .with_initial_config(sta),
    )
    .unwrap();

    let sta = interfaces.station;
    let mac = sta.mac_address();
    crate::init_log!(
        "[Init][STA] mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5]
    );

    let sta_config = embassy_net::Config::dhcpv4(Default::default());
    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    let resources = STACK_RESOURCES.init(StackResources::<4>::new());
    let (sta_stack, sta_runner) = embassy_net::new(sta, sta_config, resources, seed);
    let stack = *STACK.init(sta_stack);

    (controller, stack, sta_runner)
}

pub fn start_udp_tasks(spawner: Spawner, stack: Stack<'static>) {
    spawner.spawn(ctrl_udp_task(stack).unwrap());
}

#[embassy_executor::task]
async fn ctrl_udp_task(stack: Stack<'static>) {
    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        crate::init_log!("[drop_xdp] STA IP {} gw={:?}", cfg.address, cfg.gateway);
    }

    let mut ctrl = UdpSocket::new(
        stack,
        CTRL_RX_META.init([PacketMetadata::EMPTY; 4]),
        CTRL_RX_BUF.init([0; CTRL_RX_CAP]),
        CTRL_TX_META.init([PacketMetadata::EMPTY; 4]),
        CTRL_TX_BUF.init([0; CTRL_TX_CAP]),
    );
    if let Err(e) = ctrl.bind(dut::CTRL_PORT) {
        error!("[drop_xdp] bind :{} failed: {:?}", dut::CTRL_PORT, e);
        return;
    }
    crate::init_log!("[drop_xdp] UDP ctrl bound :{}", dut::CTRL_PORT);

    let ack_dest = IpEndpoint::new(
        IpAddress::Ipv4(Ipv4Address::new(
            probe::IP[0],
            probe::IP[1],
            probe::IP[2],
            probe::IP[3],
        )),
        probe::REPLY_PORT,
    );

    let mut rx = [0u8; CTRL_RX_CAP];
    let mut tx = [0u8; CTRL_TX_CAP];
    loop {
        if let Some(done) = drop_stats::take_bench_done_notify() {
            if let Some(n) = drop_stats::encode_bench_done(&done, &mut tx) {
                if let Err(e) = ctrl.send_to(&tx[..n], ack_dest).await {
                    error!("[drop_xdp] bench_done notify failed: {:?}", e);
                }
            }
        }
        match embassy_time::with_timeout(
            embassy_time::Duration::from_millis(50),
            ctrl.recv_from(&mut rx),
        )
        .await
        {
            Ok(Ok((len, _remote))) => {
                if len == drop_stats::REQ_LEN && rx[0] == drop_stats::OP_RESET {
                    drop_stats::handle_reset().await;
                    continue;
                }
                if len == drop_stats::REQ_LEN && rx[0] == drop_stats::OP_STOP {
                    drop_stats::stop_and_seal();
                    continue;
                }
                if let Some(n) = drop_stats::try_handle(&rx[..len], &mut tx) {
                    if let Err(e) = ctrl.send_to(&tx[..n], ack_dest).await {
                        error!("[drop_xdp] stats reply failed: {:?}", e);
                    }
                }
            }
            Ok(Err(e)) => error!("[drop_xdp] ctrl recv: {:?}", e),
            Err(_) => {}
        }
    }
}
