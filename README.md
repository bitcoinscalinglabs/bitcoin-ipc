# Bitcoin IPC (InterPlanetary Consensus)

> TBD Project description

## Components

> The project is in early development and the components are subject to change.

### Provider

Exposes an HTTP endpoint with a JSON RPC to interact with Bitcoin. It's primarily intented to be used by the IPC Subnet Manager. It listens on the port configurable by `PROVIDER_PORT`, defaulting to 3030. It requires an Authorization header with a bearer token to be set to the value of `PROVIDER_AUTH_TOKEN`.

### Monitor

Monitors the Bitcoin network for new blocks and transactions. It processes transactions with IPC-related data. It saves the data in a local database, configurable by `DATABASE_URL` environment variable.

## Guides

- [Development Environment Setup](./docs/env.md)
