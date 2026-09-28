//! Pluggable probe tests.

mod firewall_echo;
mod fw_hash_test;
mod hot_load_ctrl;
mod tail_call_abc;
mod test0;
mod xdp_forward_echo;

use anyhow::{Result, bail};

use crate::{config::CaseConfig, harness::Harness};
use test_report::TestReport;

pub use firewall_echo::FirewallEchoTest;
pub use fw_hash_test::FwHashTest;
pub use hot_load_ctrl::HotLoadCtrlTest;
pub use tail_call_abc::TailCallAbcTest;
pub use test0::Test0;
pub use xdp_forward_echo::XdpForwardEchoTest;

pub trait ProbeTest: Send + Sync {
    fn name(&self) -> &'static str;
    fn run<'a>(
        &'a self,
        harness: &'a mut Harness,
        cases: &'a [CaseConfig],
    ) -> core::pin::Pin<Box<dyn core::future::Future<Output = Result<TestReport>> + Send + 'a>>;
}

pub fn lookup(name: &str) -> Result<&'static dyn ProbeTest> {
    for t in registry() {
        if t.name() == name {
            return Ok(*t);
        }
    }
    bail!("unknown test '{name}'; known: {}", known_names().join(", "));
}

fn known_names() -> Vec<&'static str> {
    registry().iter().map(|t| t.name()).collect()
}

fn registry() -> &'static [&'static dyn ProbeTest] {
    &[
        &FirewallEchoTest,
        &XdpForwardEchoTest,
        &HotLoadCtrlTest,
        &FwHashTest,
        &TailCallAbcTest,
        &Test0,
    ]
}
