# Local Docker deployment for bitcoin-ipc
# Uses docker-compose.local.yml

COMPOSE_FILE := docker-compose.local.yml

.PHONY: local-build local-up local-down local-reset-fendermint local-reset-fendermint-deps local-reset-ipc-cli local-delete-data local-stop-validator local-start-validator checkpoint-matrix

# Build the bitcoin-ipc image (both bitcoin-ipc and ipc-cli stages built in parallel).
local-build:
	docker compose -f $(COMPOSE_FILE) build

# Start the container.
local-up:
	docker compose -f $(COMPOSE_FILE) up

# Stop and remove the container.
local-down:
	docker compose -f $(COMPOSE_FILE) down

# Delete the fendermint Docker image and clear the contracts cache.
# Fendermint will be rebuilt from mounted ../ipc when a spin-up script runs.
local-reset-fendermint:
	docker exec bitcoin-ipc rm -f /workspace/ipc/fendermint/.contracts-gen
	docker rmi fendermint:latest || true
	@echo "Fendermint image removed and contracts cache cleared."

# Rebuild fendermint deps cache (run after Cargo.lock changes in ../ipc).
local-reset-fendermint-deps:
	docker rmi fendermint-deps:latest || true
	@echo "Fendermint deps cache removed. Will be rebuilt on next fendermint build."

# Rebuild ipc-cli from the mounted ../ipc source inside the running container.
# Will not work if relayers are running, because they are using the ipc-cli binary.
local-reset-ipc-cli:
	docker exec bitcoin-ipc bash -c \
		"cd /workspace/ipc && cd contracts && ln -sf contracts src && make gen && cd /workspace/ipc && cargo build --release -p ipc-cli && cp target/release/ipc-cli /usr/local/bin/"
	@echo "ipc-cli rebuilt and installed."

# Reset all local data (Bitcoin chain, IPC config, etc.).
local-delete-data:
	docker compose -f $(COMPOSE_FILE) down -v
	rm -rf $(HOME)/.ipc
	@echo "Data reset complete."

# Stop one subnet A validator's L2 containers, keeping their data: make local-stop-validator N=1
local-stop-validator:
	docker stop -t 0 validator-$(N)-cometbft validator-$(N)-fendermint validator-$(N)-ethapi

# Start them again on their existing data: make local-start-validator N=1
local-start-validator:
	docker start validator-$(N)-fendermint
	docker start validator-$(N)-cometbft validator-$(N)-ethapi

# Checkpoint tests for several checkpoint periods and epoch lengths, described in
# src/easy_tester/ipc_tester/README.md: make checkpoint-matrix OUT=<folder> HOST_ADDR=<host address>
CHECKPOINT_PERIODS ?= 30 100
EPOCH_LENGTH_MULTIPLIERS ?= 1
INCLUDE_EQUAL_EPOCH_LENGTH ?= 1
OUT ?=
HOST_ADDR ?= host.docker.internal

checkpoint-matrix:
	CHECKPOINT_PERIODS="$(CHECKPOINT_PERIODS)" EPOCH_LENGTH_MULTIPLIERS="$(EPOCH_LENGTH_MULTIPLIERS)" \
	INCLUDE_EQUAL_EPOCH_LENGTH="$(INCLUDE_EQUAL_EPOCH_LENGTH)" OUT="$(OUT)" HOST_ADDR="$(HOST_ADDR)" \
	src/easy_tester/ipc_tester/checkpoint_matrix.sh
