//! WiFi STA bring-up with XDP RX wrap (`xpost::xdp::new`).

use embassy_net::{Stack, StackResources};
use esp_hal::{peripherals::WIFI, rng::Rng};
use esp_radio::wifi::{self as wifi_radio, Config, ControllerConfig, sta::StationConfig};
use static_cell::StaticCell;

static STACK: StaticCell<Stack<'static>> = StaticCell::new();
static STACK_RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();

const SSID: &str = "CMCC-dKji";
const PASSWORD: &str = "22332233";

pub fn init_sta(
    wifi_periph: WIFI<'static>,
) -> (
    wifi_radio::WifiController<'static>,
    Stack<'static>,
    embassy_net::Runner<'static, wifi_radio::Interface<'static>>,
) {
    crate::init_log!("[Init][STA] initializing (bpf-toml-drop)");

    let sta = Config::Station(
        StationConfig::default()
            .with_ssid(SSID)
            .with_password(PASSWORD.into()),
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
