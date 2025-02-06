# Bitcoin Ipc — Overview

> [Video guide](https://drive.google.com/file/d/1LsLhgd2fWXCRSVqy8Cmh_TUnsvo9l-cS/view?usp=share_link) is available.

Now that we got the [bitcoin environment setup and ready](./env.md), let's discuss the components that interact with the Bitcoin Network. To generate the executable files, run the following command from the root of the project:

```sh
cargo build --release
```

The binaries will use the `.env` file in the current directory to read the environment variables.

Copy the `.env.example` file to `.env` and adjust the values to your needs, specifying the RPC credentials and the wallet name from the previous chapter.

```sh
cp .env.example .env
```

## Monitor

Monitors the Bitcoin network for new blocks and transactions. It processes transactions with IPC-related data. It saves the data in a local database, configurable by `DATABASE_URL` environment variable.

To run the monitor, execute the following command:

```sh
./target/release/monitor
```

## Provider

Exposes an HTTP endpoint with a JSON RPC to interact with Bitcoin, and read data from the local database. It's primarily intented to be used by the IPC cli, which we will introduce later. It listens on the port configurable by `PROVIDER_PORT`, defaulting to 3030. It requires an Authorization header with a bearer token to be match the value of `PROVIDER_AUTH_TOKEN` env var.

To run the provider, execute the following command:

```sh
./target/release/provider
```

> When running for the first time, make sure to run the monitor before the provider, since monitor will create the necessary database.

## Configuration

By default, the monitor and provider read the `.env` file in the current directory. For a full list of env variables see the [`.env.example`](../.env.example) file. You can specify a different file by passing a relative path to the `--env` flag, like so:

```sh
./target/release/monitor --env=custom.env
./target/release/provider --env=custom.env
```

## Interaction

With both monitor and provider running, it's time to [setup ipc-cli](./ipc-cli.md).
