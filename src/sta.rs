//! 初始化wifi sta

use crate::driver::FirewallDevice;
use embassy_net::{
    StackResources,
    udp::{PacketMetadata, UdpSocket},
};
use esp_hal::{peripherals::WIFI, rng::Rng};
use esp_radio::wifi::{self, Config, ControllerConfig, sta::StationConfig};
use log::info;
use static_cell::StaticCell;

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASSWORD");
const UDP_PORT: u16 = 8080;
const HEADER_SIZE: usize = 32;

static STACK_RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();
static RX_META: StaticCell<[PacketMetadata; 16]> = StaticCell::new();
static RX_BUFFER: StaticCell<[u8; 1536]> = StaticCell::new();
static TX_META: StaticCell<[PacketMetadata; 16]> = StaticCell::new();
static TX_BUFFER: StaticCell<[u8; 1536]> = StaticCell::new();

#[inline]
pub fn init_sta(
    wifi: WIFI<'static>,
) -> (
    wifi::WifiController<'static>,
    embassy_net::Stack<'static>,
    embassy_net::Runner<'static, FirewallDevice<wifi::Interface<'static>>>,
) {
    info!("STA Initialization");
    let sta = Config::Station(
        StationConfig::default()
            .with_ssid(SSID)
            .with_password(PASSWORD.into()),
    );

    info!("STA starting with ssid: {}", SSID);
    let (controller, interfaces) =
        wifi::new(wifi, ControllerConfig::default().with_initial_config(sta)).unwrap();

    let sta = interfaces.station;

    // WiFi RX frames are filtered by AOT XDP via FirewallDevice::check_packet.
    info!("Wrapping WiFi interface (XDP RX path enabled)");
    let xdp_device = FirewallDevice::new(sta);

    let sta_config = embassy_net::Config::dhcpv4(Default::default());
    info!("STA using DHCPv4");

    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    info!("STA starting with seed: {}", seed);
    let resources = STACK_RESOURCES.init(StackResources::<4>::new());
    let (sta_stack, sta_runner) = embassy_net::new(xdp_device, sta_config, resources, seed);

    (controller, sta_stack, sta_runner)
}

#[inline]
pub async fn handler(sta_stack: embassy_net::Stack<'static>) {
    info!("Waiting for DHCP lease...");
    sta_stack.wait_config_up().await;
    if let Some(cfg) = sta_stack.config_v4() {
        info!("STA got IP {} gw={:?}", cfg.address, cfg.gateway);
    }

    let mut socket = UdpSocket::new(
        sta_stack,
        RX_META.init([PacketMetadata::EMPTY; 16]),
        RX_BUFFER.init([0; 1536]),
        TX_META.init([PacketMetadata::EMPTY; 16]),
        TX_BUFFER.init([0; 1536]),
    );
    // 绑定到端口 8080
    if let Err(e) = socket.bind(UDP_PORT) {
        info!("Failed to bind UDP socket to port {}: {:?}", UDP_PORT, e);
        return;
    }
    info!("UDP socket bound to port {}", UDP_PORT);
    inner(socket).await;
}

#[inline]
async fn inner(udp_socket: UdpSocket<'static>) {
    let mut rx_buffer = [0u8; 1536];
    loop {
        match udp_socket.recv_from(&mut rx_buffer).await {
            Ok((len, remote_endpoint)) => {
                info!("Received {} bytes from {}", len, remote_endpoint);

                info!("{:?}", remote_endpoint);

                if len > 0 {
                    // 拆分为头部和载荷两个包
                    if len >= HEADER_SIZE {
                        let header = &rx_buffer[..HEADER_SIZE];
                        let payload = &rx_buffer[HEADER_SIZE..len];

                        // 先发送载荷包
                        if payload.len() > 0 {
                            match udp_socket.send_to(payload, remote_endpoint).await {
                                Ok(_) => {
                                    info!(
                                        "Sent payload ({} bytes) to {}",
                                        payload.len(),
                                        remote_endpoint
                                    );
                                }
                                Err(e) => {
                                    info!("Failed to send payload: {:?}", e);
                                }
                            }
                        }

                        // 再发送头部包
                        match udp_socket.send_to(header, remote_endpoint).await {
                            Ok(_) => {
                                info!("Sent header ({} bytes) to {}", HEADER_SIZE, remote_endpoint);
                            }
                            Err(e) => {
                                info!("Failed to send header: {:?}", e);
                            }
                        }
                    } else {
                        // 数据太小无法拆分，直接转发
                        match udp_socket.send_to(&rx_buffer[..len], remote_endpoint).await {
                            Ok(_) => {
                                info!("Forwarded packet ({} bytes) to {}", len, remote_endpoint);
                            }
                            Err(e) => {
                                info!("Failed to forward packet: {:?}", e);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                info!("UDP receive error: {:?}", e);
            }
        }
    }
}
