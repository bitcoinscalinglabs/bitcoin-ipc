# Local Docker deployment for bitcoin-ipc
# Uses docker-compose.local.yml

COMPOSE_FILE := docker-compose.local.yml

.PHONY: local-ipc-build local-build local-up local-down local-reset-fendermint local-reset-data

# Build the IPC builder image
# Run once, or when the IPC repo / Dockerfile.ipc changes.
local-ipc-build:
	docker build -f docker-deploy-local/Dockerfile.ipc -t ipc-builder:latest .

# Build the bitcoin-ipc image (not the ipc repo).
local-build:
	docker compose -f $(COMPOSE_FILE) build

# Start the container. Builds ipc-builder if missing.
local-up:
	@if ! docker image inspect ipc-builder:latest >/dev/null 2>&1; then $(MAKE) local-ipc-build; fi
	docker compose -f $(COMPOSE_FILE) up

# Stop and remove the container.
local-down:
	docker compose -f $(COMPOSE_FILE) down

# Delete the local fendermint Docker image and pull the latest IPC repo inside the
# bitcoin-ipc container. The next subnet spin-up will rebuild fendermint from the new code.
local-reset-fendermint:
	docker exec bitcoin-ipc git -C /workspace/ipc pull
	docker rmi fendermint:latest || true
	@echo "Fendermint image removed and workspace/ipc updated."

# Reset the local data (Bitcoin chain, IPC config, etc.).
# Run 'make docker-up' to start fresh.
local-reset-data:
	docker compose -f $(COMPOSE_FILE) down -v
	rm -rf $(HOME)/.ipc
	@echo "Data reset complete."
