//! WiFi STA + firewall test: UDP :8080 → fixed echo to TR3000.

use crate::driver::FirewallDevice;
use crate::fw_cfg::{
    FW_ECHO_PORT, FW_LISTEN_PORT, FW_TR3000_IP, XDP_ENABLE, log_cfg,
};
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

/// Bind :8080; any datagram that reaches userspace triggers a fixed echo to TR3000.
#[inline]
pub async fn handler(sta_stack: embassy_net::Stack<'static>) {
    info!("[FWTEST] waiting for DHCP lease...");
    sta_stack.wait_config_up().await;
    if let Some(cfg) = sta_stack.config_v4() {
        info!("[FWTEST] STA IP {} gw={:?}", cfg.address, cfg.gateway);
    }

    let mut socket = UdpSocket::new(
        sta_stack,
        RX_META.init([PacketMetadata::EMPTY; 16]),
        RX_BUFFER.init([0; 1536]),
        TX_META.init([PacketMetadata::EMPTY; 16]),
        TX_BUFFER.init([0; 1536]),
    );
    if let Err(e) = socket.bind(FW_LISTEN_PORT) {
        error!(
            "[FWTEST] Failed to bind UDP socket to port {}: {:?}",
            FW_LISTEN_PORT, e
        );
        return;
    }
    info!("[FWTEST] UDP socket bound to port {}", FW_LISTEN_PORT);
    recv_loop(socket).await;
}

#[inline]
async fn recv_loop(udp_socket: UdpSocket<'static>) {
    let echo_dest = IpEndpoint::new(
        IpAddress::Ipv4(Ipv4Address::new(
            FW_TR3000_IP[0],
            FW_TR3000_IP[1],
            FW_TR3000_IP[2],
            FW_TR3000_IP[3],
        )),
        FW_ECHO_PORT,
    );
    info!(
        "[FWTEST] echo → {}.{}.{}.{}:{}",
        FW_TR3000_IP[0],
        FW_TR3000_IP[1],
        FW_TR3000_IP[2],
        FW_TR3000_IP[3],
        FW_ECHO_PORT
    );

    let mut rx_buffer = [0u8; 1536];
    loop {
        match udp_socket.recv_from(&mut rx_buffer).await {
            Ok((len, remote_endpoint)) => {
                info!(
                    "[FWTEST] stim from {} len={}",
                    remote_endpoint.endpoint, len
                );
                match udp_socket.send_to(ECHO_PAYLOAD, echo_dest).await {
                    Ok(()) => info!(
                        "[FWTEST] echo sent to {}.{}.{}.{}:{} ({} bytes)",
                        FW_TR3000_IP[0],
                        FW_TR3000_IP[1],
                        FW_TR3000_IP[2],
                        FW_TR3000_IP[3],
                        FW_ECHO_PORT,
                        ECHO_PAYLOAD.len()
                    ),
                    Err(e) => error!("[FWTEST] echo send failed: {:?}", e),
                }
            }
            Err(e) => {
                error!("[FWTEST] UDP receive error: {:?}", e);
            }
        }
    }
}
