//! WiFi STA: UDP :8080 echo (pure HAL, no XDP).

use crate::fw_cfg::{dut, log_cfg, probe, wifi};
use crate::{init_log, trace_log};
use embassy_executor::Spawner;
use embassy_net::{
    IpAddress, IpEndpoint, Ipv4Address, Stack, StackResources,
    udp::{PacketMetadata, UdpSocket},
};
use esp_hal::{peripherals::WIFI, rng::Rng};
use esp_radio::wifi::{self as wifi_radio, Config, ControllerConfig, sta::StationConfig};
use log::error;
use static_cell::StaticCell;

const ECHO_PAYLOAD: &[u8] = &[0x42];
const STIM_MAGIC: &[u8; 6] = b"FWSTIM";
const STIM_HDR_LEN: usize = 14;
const RICH_REPLY_LEN: usize = 34;

static STACK: StaticCell<Stack<'static>> = StaticCell::new();
static STACK_RESOURCES: StaticCell<StackResources<8>> = StaticCell::new();

const ECHO_RX_PKTS: usize = 32;
const ECHO_RX_BYTES: usize = ECHO_RX_PKTS * 1536;
const ECHO_TX_PKTS: usize = 32;
const ECHO_TX_BYTES: usize = 8192;

static RX_META: StaticCell<[PacketMetadata; ECHO_RX_PKTS]> = StaticCell::new();
static RX_BUFFER: StaticCell<[u8; ECHO_RX_BYTES]> = StaticCell::new();
static TX_META: StaticCell<[PacketMetadata; ECHO_TX_PKTS]> = StaticCell::new();
static TX_BUFFER: StaticCell<[u8; ECHO_TX_BYTES]> = StaticCell::new();

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
pub fn init_sta(
    wifi_periph: WIFI<'static>,
) -> (
    wifi_radio::WifiController<'static>,
    Stack<'static>,
    embassy_net::Runner<'static, wifi_radio::Interface<'static>>,
) {
    init_log!("[Init][STA] initializing (bare HAL echo)");
    log_cfg();

    let sta = Config::Station(
        StationConfig::default()
            .with_ssid(wifi::SSID)
            .with_password(wifi::PASSWORD.into()),
    );

    init_log!("[Init][STA] ssid={}", wifi::SSID);
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

    let sta_config = embassy_net::Config::dhcpv4(Default::default());
    init_log!("[Init][STA] DHCPv4");

    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    init_log!("[Init][STA] net seed={}", seed);
    let resources = STACK_RESOURCES.init(StackResources::<8>::new());
    let (sta_stack, sta_runner) = embassy_net::new(sta, sta_config, resources, seed);
    let stack = *STACK.init(sta_stack);

    (controller, stack, sta_runner)
}

#[inline]
pub fn start_udp_tasks(spawner: Spawner, stack: Stack<'static>) {
    spawner.spawn(echo_udp_task(stack).unwrap());
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

    let mut echo_buf = [0u8; 1536];
    let mut rich_reply = [0u8; RICH_REPLY_LEN];

    loop {
        match echo.recv_from(&mut echo_buf).await {
            Ok((len, remote)) => {
                trace_log!("[FWTEST] stim from {} len={}", remote.endpoint, len);
                let pkt = &echo_buf[..len];
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
            Err(e) => error!("[FWTEST] echo recv: {:?}", e),
        }
    }
}
