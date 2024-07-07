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


## Running the code

1. Run the listener by executing

```sh
cargo run --bin btc_monitor
```

You can leave the listener running on the background or separate shell.

2. Generate a bitcoin private/public keypair

```sh
cargo run --bin generate_keypair
```

The output will look like this:

```
private_key:
tprv8ZgxMBicQKsPeMg7q6BrYrcJBvVcQ6tR6R5PUWGrgx3fyGg9R6N9MEhhHrpeSZ65FzHVL95LCEU8r4nu6nEHAeELd632W3mGHM1ZFsPVGYU
public_key:
028cc08dacd6717da80a79f552197b23c61a2348c0aec6651d0150cf1512e53b21
```

3. Create a new IPC subnet
```sh
cargo run --bin create_child -- --name <name> --pk <subnetPK>
```
where `<name>` is the desired name for the new subnet and `<subnetPK>` is a valid bitcoin public key, such as the one in the output of `generate_keypair`.
This binary will create the necessary bitcoin transactions and submit them to the local bitcoin node.

When the transactions get finalized on the bitcoin network (the local testnet), the `btc_monitor` binary should detect them as IPC-related. You should see an output such as
```
transaction 8fd7027b33cbdaeeefd88b03effe8288a539c376240e167fe572551f785ff07f at block height 117 contains the keyword 'IPC:CREATE'
```

4. Join an existing IPC subnet
```sh
cargo run --bin join_child -- --ip <ip_address> --pk <subnetPK> --collateral <collateral>
```

Where `<ip_address>` is the ip address of the validator joining the sunet, `<subnetPK>` is a valid bitcoin public key that represents the subnet and `<collateral>`  is the collateral sent to the subnet address for joining the subnet specified in SATOSHI.

When the transactions get finalized on the bitcoin network (the local testnet), the `btc_monitor` binary should detect them as IPC-related. You should see an output such as
```
transaction e84de140c011a77106859026bbf7e5ffd01f644d7922e453556adf54478ae991 at block height 106 contains the keyword 'IPC:JOIN'
```