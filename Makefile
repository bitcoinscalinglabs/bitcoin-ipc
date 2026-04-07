# Local Docker deployment for bitcoin-ipc
# Uses docker-compose.local.yml

COMPOSE_FILE := docker-compose.local.yml

.PHONY: local-build local-up local-down local-reset-fendermint local-reset-ipc-cli local-delete-data

# Build the bitcoin-ipc image (both bitcoin-ipc and ipc-cli stages built in parallel).
local-build:
	docker compose -f $(COMPOSE_FILE) build

# Start the container (builds if needed).
local-up:
	docker compose -f $(COMPOSE_FILE) up --build

# Stop and remove the container.
local-down:
	docker compose -f $(COMPOSE_FILE) down

# Delete the fendermint Docker image and clear the contracts cache.
# Fendermint will be rebuilt from mounted ../ipc when a spin-up script runs.
local-reset-fendermint:
	docker exec bitcoin-ipc rm -f /workspace/ipc/fendermint/.contracts-gen
	docker rmi fendermint:latest || true
	@echo "Fendermint image removed and contracts cache cleared."

# Rebuild ipc-cli from the mounted ../ipc source inside the running container.
local-reset-ipc-cli:
	docker exec bitcoin-ipc bash -c \
		"cd /workspace/ipc && cd contracts && ln -sf contracts src && make gen && cargo build --release -p ipc-cli && cp target/release/ipc-cli /usr/local/bin/"
	@echo "ipc-cli rebuilt and installed."

# Reset all local data (Bitcoin chain, IPC config, etc.).
local-delete-data:
	docker compose -f $(COMPOSE_FILE) down -v
	rm -rf $(HOME)/.ipc
	@echo "Data reset complete."
