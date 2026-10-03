use embassy_futures::select::Either;
use embassy_time::{Duration, Timer};
use esp_radio::wifi;
use log::error;

#[embassy_executor::task(pool_size = 2)]
pub async fn net_task(mut runner: embassy_net::Runner<'static, wifi::Interface<'static>>) {
    runner.run().await
}

#[embassy_executor::task(pool_size = 2)]
pub async fn connection(mut controller: wifi::WifiController<'static>) -> ! {
    crate::init_log!("[Init][WiFi] connection task start");
    loop {
        match controller.connect_async().await {
            Ok(_) => loop {
                let info = embassy_futures::select::select(
                    controller.wait_for_disconnect_async(),
                    controller.wait_for_access_point_connected_event_async(),
                )
                .await;
                match info {
                    Either::First(Ok(_)) => break,
                    Either::Second(Ok(_)) => {}
                    _ => {}
                }
            },
            Err(e) => {
                error!("[Init][WiFi] connect failed: {e:?}");
                Timer::after(Duration::from_millis(5000)).await;
            }
        }
    }
}
