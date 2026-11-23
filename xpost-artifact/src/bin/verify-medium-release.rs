//! Check the deployment inventory in extracted libbpf-mcu program archives.

use std::path::Path;
use std::process::ExitCode;

use xpost_artifact::{XdpMode, published::verify_program};

const PROGRAMS: &[&str] = &[
    "firewall",
    "just_drop",
    "just_tx",
    "noop",
    "prime",
    "log2",
    "copy",
    "fletcher32",
    "bsort",
    "strcmp",
    "case_swap",
    "case_mark",
];
const MODES: &[XdpMode] = &[XdpMode::Aot, XdpMode::Jit, XdpMode::Rbpf];

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let [root] = args.as_slice() else {
        eprintln!("Usage: verify-medium-release <extracted-release-directory>");
        return ExitCode::FAILURE;
    };
    let root = Path::new(root);
    let mut ready = 0;
    let required = PROGRAMS.len() * MODES.len();
    for program in PROGRAMS {
        match verify_program(&root.join(program), program) {
            Ok(deployments) => {
                for mode in MODES {
                    if let Some(d) = deployments.iter().find(|d| d.mode == *mode) {
                        println!(
                            "{program}/{} {} name={} size_bytes={} sha256={} image_bytes={} slot={:#x} helper={:#x}",
                            mode.as_db(),
                            d.path.display(),
                            d.embedded_name,
                            d.size_bytes,
                            d.sha256,
                            d.image_size,
                            d.slot_address,
                            d.helper_address
                        );
                        ready += 1;
                    } else {
                        eprintln!("{program}/{}: no XDP ELF in v3 index", mode.as_db());
                    }
                }
            }
            Err(err) => eprintln!("{program}: {err}"),
        }
    }
    if ready != required {
        eprintln!("Release blocked. Verified {ready}/{required} required deployments.");
        return ExitCode::FAILURE;
    }
    println!("Verified {ready} deployments. Firmware binding and slot checks are separate.");
    ExitCode::SUCCESS
}
