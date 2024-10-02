use bitcoin_ipc::l1_manager::{self, CreateChildArgs, L1Manager, L1ManagerError};

fn main() {
    let mut manager = match L1Manager::new() {
        Ok(manager) => manager,
        Err(e) => {
            println!("Could not instantiate L1 Manager. Error:{}", e);
            return;
        }
    };

    let prompt = "Select an option:\n\
            1. Read state\n\
            2. Create child\n\
            3. Join child\n\
            4. Deposit\n\
            5. Exit";

    loop {
        let choice = match l1_manager::get_user_input(prompt) {
            Ok(c) => c,
            Err(_) => {
                println!("Invalid option. Please try again.");
                continue;
            }
        };
        let choice: usize = match choice.parse() {
            Ok(c) => c,
            Err(_) => {
                println!("Invalid option. Please try again.");
                continue;
            }
        };

        match choice {
            1 => match manager.update_and_get_subnets() {
                Ok(subnets) => {
                    subnets
                        .iter()
                        .for_each(|subnet| subnet.clone().print_state());
                }
                Err(_) => {
                    println!("An error occured while reading the state.");
                }
            },

            2 => match || -> Result<(), L1ManagerError> {
                let args: CreateChildArgs = L1Manager::parse_create_child_args()?;
                manager.create_child(args)
            }() {
                Ok(_) => {
                    println!("Transaction to create a child subnet has been submited to bitcoin, please wait for confirmation.");
                }
                Err(e) => {
                    println!("An error occured, child subnet was not created. Error: {e}");
                }
            },

            3 => match manager.join_child() {
                Ok(_) => {
                    println!("Transaction to join a child subnet has been submited to bitcoin, please wait for confirmation.");
                }
                Err(e) => {
                    println!("An error occured, child subnet was not joined. Error: {e}");
                }
            },

            4 => match manager.deposit() {
                Ok(_) => {
                    println!("Transaction to deposit funds has been submited to bitcoin, please wait for confirmation.");
                }
                Err(e) => {
                    println!("An error occured, funds were not deposited. Error: {e}");
                }
            },

            5 => break,

            _ => println!("Invalid option. Please try again."),
        }
        println!("===============")
    }
}
