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
    </p>

4. **Running the code**

4.1. Run the listener by executing

```sh
cargo run --bin btc_monitor
```


