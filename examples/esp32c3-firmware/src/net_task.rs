use esp_radio::wifi;

#[embassy_executor::task(pool_size = 2)]
pub async fn net_task(mut runner: embassy_net::Runner<'static, wifi::Interface<'static>>) {
    runner.run().await
}