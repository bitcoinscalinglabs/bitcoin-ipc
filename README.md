# bitcoin-ipc

# Bitcoin Regtest Setup Guide

This guide will walk you through the necessary steps needed to setup the environment to be able to run the bitcoin-ipc demo locally.

## Prerequisites

- Ensure you have the latest version of Bitcoin Core installed. You can download it from the [official website](https://bitcoin.org/en/download).

## Setup

1. **Install Bitcoin Core**

   Follow the installation instructions for your operating system provided on the [Bitcoin Core download page](https://bitcoin.org/en/download).

   On Linux you can install Bitcoin Core over a shell following the instuctions [here](https://bitcoin.org/en/full-node#linux-instructions).
   For example, you can install version 27.0 like this:
   ```sh
   wget https://bitcoin.org/bin/bitcoin-core-27.0/bitcoin-27.0-x86_64-linux-gnu.tar.gz
   tar xfz bitcoin-27.0-x86_64-linux-gnu.tar.gz
   sudo install -m 0755 -o root -g root -t /usr/local/bin bitcoin-27.0/bin/*
   sudo install -m 0755 -o root -g root -t /usr/local/bin bitcoin-27.0/bin/*
   ```

2. **Verify Installation**

   Open a terminal or command prompt and verify that `bitcoind` and `bitcoin-cli` are installed correctly:

   ```sh
   bitcoind --version
   bitcoin-cli --version
   ```

3. **Configuration** 
   
   3.1. **Ensure that the Bitcoin data directory exists. The default locations are:**

    <ul>
    <li>Linux: ~/.bitcoin/ </li> 
    <li> macOS: ~/Library/Application Support/Bitcoin/ </li>
    <li> Windows: %APPDATA%\Bitcoin\ </li>
    </ul>
   
   3.2 **Create or edit the bitcoin.conf file in the Bitcoin data directory and add the following configuration to enable regtest:**

   ```sh
   cd ~/.bitcoin/
   nano bitcoin.conf
   ```

    bitcoin.conf:
    ```ini
    regtest=1
    server=1
    rpcuser=yourusername
    rpcpassword=yourpassword
    ```
    
   3.3 **Starting Bitcoin Daemon in Regtest Mode**

    Start bitcoind in regtest mode:

    ```sh
    bitcoind --printtoconsole --regtest --maxtxfee=50 --mintxfee=0.001
    ```

    Verify bitcoind is Running:


    ```sh
    bitcoin-cli getblockchaininfo
    ```

    3.4 **Update .env**

    ```ini
    RPC_USER=yourusername
    RPC_PASS=yourpassword
    RPC_URL=http://localhost:18443
    WALLET_NAME=default
    ```

    Source the .env, in case you have updated it.

    ```sh
    . .env
    ```

    3.5 **Setting up a wallet (Optional - this is done in the code)**
    <details>
    <summary> Manual Wallet Setup and Block Generation</summary>
    
      3.5.1 **Create a Wallet named default**
            
    ```sh
    bitcoin-cli createwallet "default"
    ```

      3.5.2 **(Optional - if wallet is not already loaded) Load the Wallet**

    ```sh
    bitcoin-cli loadwallet "default"
    ```

      3.5.3 **Generate 101 blocks**

    ```sh
    bitcoin-cli -regtest generatetoaddress 101 "$(bitcoin-cli -regtest getnewaddress)"
    ```

    3.5.4 **Verify block generation**

    ```sh
    bitcoin-cli -regtest getblockcount
    ```
    The output should be 101.
    </details>



## Running the demo using the automated scripts

- On MacOS run the following command:
```sh
./scripts/demo_ubuntu.sh 
```

- On Ubuntu run the following command:
```sh
./scripts/demo_ubuntu.sh 
```

This will run `bitcoind`, `btc_monitor`, the `l1_manager`, and it will open a `subnet_interactor` terminal for every existing subnet.


## Running the demo manually
1. Make sure you have started the `bitcoin core` client 

```sh
bitcoind --printtoconsole --regtest --maxtxfee=50 --mintxfee=0.001
```

2. Start `btc_monitor`
```sh
cargo run --bin btc_monitor
```

3. Run the `l1_manager` binary to interact with L1 IPC
```sh
cargo run --bin l1_manager
```

4. Run the `subnet_interactor` binary to interact with a child subnet.
```sh
cargo run --bin subnet_interactor -- --subnet-name <subnet_name> 
```

5. Run a relayer that submits checkpoint transactions for a subnet periodically 
```sh
cargo run --bin relayer -- --subnet-name <subnet_name>
```

## Interacting with the L1 Manager
After the L1 Manager has been started, either using a script or manually, you can interact with it as follows:

1. Press 1 to Read the state where all subnets are listed.

2. Press 2 to create a child subnet, enter the name, required number of validators and a collateral (in satoshi) prompts.

3. Press 3 to join a subnet, first pick a subnet to join, then enter the prompts (ip address, validator public key and username) and watch the btc_monitor.
