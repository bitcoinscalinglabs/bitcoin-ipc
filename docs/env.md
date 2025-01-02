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
cd  ~/Library/Application Support/Bitcoin/
```

bitcoin.conf:
```ini
regtest=1
server=1
rpcuser=yourusername
rpcpassword=yourpassword
```

2.3. Start Bitcoin Core daemon in regtest mode:

```sh
bitcoind --printtoconsole --regtest --maxtxfee=50 --mintxfee=0.001
```

To verify bitcoind is Running:
```sh
bitcoin-cli getblockchaininfo
```

### 3. Configure wallet

3.1.  Update `.env` in the project root

.env file:
```ini
RPC_USER=yourusername
RPC_PASS=yourpassword
RPC_URL=http://localhost:18443
WALLET_NAME=default
```

3.2. Set up a wallet

Manual Wallet Setup and Block Generation

3.2.1 Create a Wallet named default

```sh
bitcoin-cli createwallet "default"
```

3.2.2 (Optional - if wallet is not already loaded) Load the Wallet

```sh
bitcoin-cli loadwallet "default"
```

3.2.3 Generate 102 blocks

```sh
bitcoin-cli -regtest generatetoaddress 102 "$(bitcoin-cli -regtest getnewaddress)"
```

3.2.4 Verify block generation

```sh
bitcoin-cli -regtest getblockcount
```
The output should be 102.
