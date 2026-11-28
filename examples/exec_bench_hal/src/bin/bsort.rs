#![no_std]
#![no_main]

use embassy_executor::Spawner;
use exec_bench_hal::{Workload, firmware, workloads::bsort::Bsort};

// The Agent checks this symbol against the selected profile before a device write.
#[unsafe(no_mangle)]
static XPOST_EXB2_HAL_TASK: u8 = Bsort::TASK_ID;

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    firmware::run::<Bsort>(&XPOST_EXB2_HAL_TASK).await
}
