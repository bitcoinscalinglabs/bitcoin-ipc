# `ipc-cli`

> TBD better explain what is ipc-cli

For information about what `ipc-cli` is and how to use it, see the [ipc docs](https://docs.ipc.space/).

Clone the [bitcoinscalinglabs/ipc repo](https://github.com/bitcoinscalinglabs/ipc/tree/bitcoin) and checkout the `bitcoin` branch.

```sh
git clone git@github.com:bitcoinscalinglabs/bitcoin-ipc.git
cd bitcoin-ipc
git checkout bitcoin
```

See [Step 1](https://docs.ipc.space/quickstarts/deploy-a-subnet#step-1-prepare-your-system) to install the necessary dependencies and build the project.

You should now have the following binaries available:

```sh
./target/release/ipc-cli --version
./target/release/fendermint --version
```

For ease of use you could make aliases for these binaries:

```sh
alias ipc-cli="cargo run -q -p ipc-cli --release --"
alias fendermint="cargo run -q -p fendermint --release --"
```

You now have all of the necessary tools to [deploy a local subnet](./deploy-local.md).
