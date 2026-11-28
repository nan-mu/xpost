#![no_std]
#![no_main]

use embassy_executor::Spawner;
use exec_bench_hal::{Workload, firmware, workloads::prime::Prime};

// The Agent checks this symbol against the selected profile before a device write.
#[unsafe(no_mangle)]
static XPOST_EXB2_HAL_TASK: u8 = Prime::TASK_ID;

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    firmware::run::<Prime>(&XPOST_EXB2_HAL_TASK).await
}
