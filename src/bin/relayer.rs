// pub fn get_parent(self) -> Self {
//     let mut parent_subnet: Option<Self> = None;

//     if self.url.eq("BTC") {
//         return self;
//     }

//     let url = self.url.clone();
//     let parent_url_length = url.split('/').count() - 1;
//     let parent_name = url
//         .split("/")
//         .nth(parent_url_length - 1)
//         .unwrap_or_default();

//     let mut parent_file_name = url
//         .split('/')
//         .take(parent_url_length)
//         .collect::<Vec<&str>>()
//         .join("/");

//     parent_file_name.push_str("/");
//     parent_file_name.push_str(parent_name);
//     parent_file_name.push_str(".json");

//     println!("Parent file name: {}", parent_file_name);

//     if let Ok(mut file) = File::open(parent_file_name) {
//         let mut json = String::new();
//         file.read_to_string(&mut json)
//             .expect("Failed to read parent file");
//         parent_subnet = serde_json::from_str(&json).expect("Failed to deserialize parent");
//     }
//     parent_subnet.clone().expect("Failed to load parent")
// }

use std::{thread, time::Duration};

use bitcoin_ipc::{ipc_lib, ipc_state::IPCState, subnet_simulator::SubnetState};

fn checkpoint() {
    loop {
        let subnets = IPCState::load_all().unwrap_or_else(|_| Vec::new());

        subnets.iter().for_each(|subnet| {
            if subnet.has_required_validators() {
                let hash = SubnetState::new().get_checkpoint();

                if let Ok(_) = ipc_lib::submit_checkpoint(hash, subnet.clone()) {
                    println!("Checkpoint for {} submitted successfully", subnet.get_url());
                }
            }
        });

        thread::sleep(Duration::from_secs(100));
    }
}

fn main() {
    checkpoint();
}
