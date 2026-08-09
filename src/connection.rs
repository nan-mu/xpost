use embassy_futures::select::Either;
use embassy_time::{Duration, Timer};
use esp_radio::wifi;
use log::{error, info};

#[embassy_executor::task(pool_size = 2)]
pub async fn net_task(mut runner: embassy_net::Runner<'static, wifi::Interface<'static>>) {
    runner.run().await
}

#[embassy_executor::task(pool_size = 2)]
pub async fn net_task_xdp(
    mut runner: embassy_net::Runner<
        'static,
        crate::driver::FirewallDevice<wifi::Interface<'static>>,
    >,
) {
    runner.run().await
}

#[embassy_executor::task]
pub async fn connection(mut controller: wifi::WifiController<'static>) -> ! {
    info!("[Init][WiFi] connection task start");
    loop {
        match controller.connect_async().await {
            Ok(_) => {
                // wait until we're no longer connected
                loop {
                    let info = embassy_futures::select::select(
                        controller.wait_for_disconnect_async(),
                        controller.wait_for_access_point_connected_event_async(),
                    )
                    .await;

                    match info {
                        Either::First(station_disconnected) => {
                            if let Ok(station_disconnected) = station_disconnected {
                                info!(
                                    "[Init][WiFi] station disconnected: {:?}",
                                    station_disconnected
                                );
                                break;
                            }
                        }
                        Either::Second(event) => {
                            if let Ok(event) = event {
                                match event {
                                    esp_radio::wifi::AccessPointStationEventInfo::Connected(
                                        access_point_station_connected_info,
                                    ) => {
                                        info!(
                                            "[Init][WiFi] AP station connected: {:?}",
                                            access_point_station_connected_info
                                        );
                                    }
                                    esp_radio::wifi::AccessPointStationEventInfo::Disconnected(
                                        access_point_station_disconnected_info,
                                    ) => {
                                        info!(
                                            "[Init][WiFi] AP station disconnected: {:?}",
                                            access_point_station_disconnected_info
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!("[Init][WiFi] connect failed: {e:?}");
                Timer::after(Duration::from_millis(5000)).await
            }
        }
    }
}
