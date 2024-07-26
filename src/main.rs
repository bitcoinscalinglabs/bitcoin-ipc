use std::process::{Command, Stdio};
use std::thread;

use bitcoin_ipc::ipc_state::IPCState;

fn main() {
    // Step 1: Run bitcoind in a new terminal
    let _bitcoind_handle = thread::spawn(|| {
        Command::new("gnome-terminal")
            .arg("--title=bitcoind")
            .arg("--")
            .arg("bash")
            .arg("-c")
            .arg("bitcoind --printtoconsole --regtest --maxtxfee=50 --mintxfee=0.001; exec bash")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("Failed to start bitcoind");
    });

    // Wait a bit to ensure the previous steps are complete
    thread::sleep(std::time::Duration::from_secs(1));

    // Step 2: Run btc_monitor in a new terminal
    let _btc_monitor_handle = thread::spawn(|| {
        Command::new("gnome-terminal")
            .arg("--title=btc_monitor")
            .arg("--")
            .arg("bash")
            .arg("-c")
            .arg("cargo run --bin btc_monitor; exec bash")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("Failed to start btc_monitor");
    });

    // Wait a bit to ensure the previous steps are complete
    thread::sleep(std::time::Duration::from_secs(1));

    // Step 3: Run l1_manager in a new terminal
    let _l1_manager_handle = thread::spawn(|| {
        Command::new("gnome-terminal")
            .arg("--title=l1_manager")
            .arg("--")
            .arg("bash")
            .arg("-c")
            .arg("cargo run --bin l1_manager; exec bash")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("Failed to start l1_manager");
    });

    // Wait a bit to ensure the previous steps are complete
    thread::sleep(std::time::Duration::from_secs(1));

    let subnets = IPCState::load_all().unwrap_or_else(|_| Vec::new());

    subnets.iter().for_each(|subnet| {
        if subnet.has_required_validators() {
            let subnet_name = subnet.get_name().clone();
            let l1_name = bitcoin_ipc::L1_NAME.to_string();
            let _subnet_interactor_handle = thread::spawn(move || {
                Command::new("gnome-terminal")
                    .arg(format!("--title=subnet_interactor {}", subnet_name))
                    .arg("--")
                    .arg("bash")
                    .arg("-c")
                    .arg(format!(
                        "cargo run --bin subnet_interactor -- --url {}; exec bash",
                        format!("{}/{}", l1_name, subnet_name)
                    ))
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .expect("Failed to start subnet_interactor");
            });
            _subnet_interactor_handle
                .join()
                .expect("Failed to join subnet_interactor thread");
        }
    });

    // Step 4: Generate a keypair for the subnet
    let _generate_keypair_handle = thread::spawn(|| {
        Command::new("gnome-terminal")
            .arg("--title=generate_keypair")
            .arg("--")
            .arg("bash")
            .arg("-c")
            .arg("cargo run --bin generate_keypair; exec bash")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("Failed to generate keypair");
    });

    // Join threads to wait for them to complete
    _bitcoind_handle
        .join()
        .expect("Failed to join bitcoind thread");
    _btc_monitor_handle
        .join()
        .expect("Failed to join btc_monitor thread");
    _l1_manager_handle
        .join()
        .expect("Failed to join l1_manager thread");
    _generate_keypair_handle
        .join()
        .expect("Failed to join generate_keypair thread");
}
