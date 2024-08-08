use bitcoin_ipc::ipc_state::IPCState;
use std::process::{Command, Stdio};

fn main() {
    let subnets = IPCState::load_all().unwrap_or_else(|_| Vec::new());
    let mut subnet_names = Vec::new();

    subnets.iter().for_each(|subnet| {
        if subnet.has_required_validators() {
            subnet_names.push(subnet.get_name().clone());
        }
    });

    let mut args = vec![];
    args.extend(subnet_names.iter().map(String::as_str));

    if cfg!(target_os = "linux") {
        args.insert(0, bitcoin_ipc::DEMO_UBUNTU);
    } else if cfg!(target_os = "macos") {
        args.insert(0, bitcoin_ipc::DEMO_MACOS);
    } else {
        eprintln!("Unsupported operating system.");
    }

    let mut handle = Command::new("bash")
        .args(&args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("Failed to execute script");

    handle
        .wait()
        .expect("The script was not completed successfully");
}
