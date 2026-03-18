# Local Docker deployment for bitcoin-ipc
# Uses docker-compose.local.yml

COMPOSE_FILE := docker-compose.local.yml

.PHONY: docker-ipc-build docker-build docker-up docker-down docker-restart docker-reset

# Build the IPC builder image (required before first docker-up).
# Run once, or when the IPC repo / Dockerfile.ipc changes.
docker-ipc-build:
	docker build -f docker-deploy-local/Dockerfile.ipc -t ipc-builder:latest .

# Build the bitcoin-ipc image (no ipc-builder rebuild).
docker-build:
	docker compose -f $(COMPOSE_FILE) build

# Build and start the container. Builds ipc-builder if missing.
docker-up:
	@docker image inspect ipc-builder:latest >/dev/null 2>&1 || $(MAKE) docker-ipc-build
	docker compose -f $(COMPOSE_FILE) up --build

# Stop and remove the container.
docker-down:
	docker compose -f $(COMPOSE_FILE) down

# Restart the container (e.g. after resetting .ipc or .bitcoin).
docker-restart:
	docker compose -f $(COMPOSE_FILE) restart

# Full reset: stop container, delete the root-data volume, and remove ~/.ipc on the host.
# Run 'make docker-up' afterward to start fresh.
docker-reset:
	docker compose -f $(COMPOSE_FILE) down -v
	rm -rf $(HOME)/.ipc
	@echo "Reset complete. Run 'make docker-up' to start fresh. Run `make docker-ipc-build` if you also want to rebuild the IPC repo."
