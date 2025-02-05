# Bitcoin Ipc — Development Environment Setup

This guide will walk you through the necessary steps needed to setup the environment to be able to run the bitcoin-ipc locally.

### 1. Install Bitcoin Core

Follow the installation instructions for your operating system provided on the [Bitcoin Core download page](https://bitcoin.org/en/download).

To verify the installation: Open a terminal or command prompt and verify that `bitcoind` and `bitcoin-cli` are installed correctly:
```sh
bitcoind --version
bitcoin-cli --version
```

### 2. Configure and start Bitcoin Core

2.1.  Ensure that the Bitcoin data directory exists. The default locations are:
```
- Linux: ~/.bitcoin/
- macOS: ~/Library/Application Support/Bitcoin/
- Windows: %APPDATA%\Bitcoin\
```

2.2. Create or edit the `bitcoin.conf` file in the Bitcoin data directory and add the following configuration to enable regtest:

```sh
# on macos
cd  ~/Library/Application\ Support/Bitcoin/
```

bitcoin.conf:
```ini
regtest=1
server=1
rpcuser=yourusername
rpcpassword=yourpassword
rpcallowip=127.0.0.1
fallbackfee=0.0002
listen=1
txindex=1
```

2.3. Start Bitcoin Core daemon in regtest mode:

```sh
bitcoind --printtoconsole
```

To verify bitcoind is running:
```sh
bitcoin-cli getblockchaininfo
```

### 3. Set up a wallet

Manual Wallet Setup and Block Generation

3.2 Create a Wallet named default

```sh
bitcoin-cli createwallet "default"
```

3.3 (Optional - if wallet is not already loaded) Load the Wallet

```sh
bitcoin-cli loadwallet "default"
```

3.4 Generate 102 blocks

```sh
bitcoin-cli generatetoaddress 102 "$(bitcoin-cli -rpcwallet=default getnewaddress)"
```

3.5 Verify block generation

```sh
bitcoin-cli getblockcount
```

The output should be 102.

3.6 Verify wallet balance

```sh
bitcoin-cli getbalance
```

The output should be `100.00000000`, 100 BTC.
