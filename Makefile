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

# Checkpoint tests across epoch lengths, one fresh devnet per cell: K = m·P + 15 for each period P in
# PERIODS and multiple m in MULTIPLES, and K = P when EQUAL=1. Needs the bitcoin-ipc image; the first
# cell builds fendermint:latest if it is missing. Logs per cell in $(OUT)/P<period>-K<K>/, one result
# line per cell in $(OUT)/matrix.tsv: make checkpoint-matrix OUT=<folder> HOST_ADDR=<host address>
PERIODS ?= 30 100
MULTIPLES ?= 1
EQUAL ?= 1
OUT ?=
HOST_ADDR ?= host.docker.internal
CHECKPOINT_CELLS := $(foreach p,$(PERIODS),$(foreach m,$(MULTIPLES),$(p):$(shell echo $$(($(m) * $(p) + 15))):$(m)) $(if $(filter 1,$(EQUAL)),$(p):$(p):-))

# One cell: $(1) period, $(2) K, $(3) multiple ("-" for K = P).
define checkpoint_cell
cell=$(OUT)/P$(1)-K$(2); mkdir -p $$cell; start=$$(date +%s); rc=0; \
echo "== period $(1), K $(2): fresh devnet"; \
docker compose -f $(COMPOSE_FILE) down -v; rm -rf $(HOME)/.ipc; \
docker ps -aq --filter name=^validator- | xargs -r docker rm -f >/dev/null; \
docker network ls --format '{{.Name}}' | grep '^b4-' | xargs -r docker network rm >/dev/null; \
build=0; docker image inspect fendermint:latest >/dev/null 2>&1 || build=1; \
BLOCK_INTERVAL=1 UE_ACTIVATION_HEIGHT=1 docker compose -f $(COMPOSE_FILE) up -d || rc=1; \
for i in $$(seq 120); do n=$$(docker exec bitcoin-ipc bitcoin-cli getblockcount 2>/dev/null); [ "$${n:-0}" -gt 101 ] && break; sleep 5; done; \
[ "$${n:-0}" -gt 101 ] || { echo "FAIL: Bitcoin block 101 not reached after 600 s" | tee -a $$cell/ue_scenario.out; rc=1; }; \
ready=""; [ $$rc -ne 0 ] || for i in $$(seq 60); do ready=1; for id in validator1 validator2 validator3 validator4 validator5 validator6 user1 user2; do \
  port=$$(sed -n 's/^PROVIDER_PORT=//p' $(HOME)/.ipc/$$id/.env 2>/dev/null); token=$$(sed -n 's/^PROVIDER_AUTH_TOKEN=//p' $(HOME)/.ipc/$$id/.env 2>/dev/null); \
  h=$$(curl -s --max-time 5 -X POST http://127.0.0.1:$$port/api -H 'Content-Type: application/json' -H "Authorization: Bearer $$token" \
    -d '{"jsonrpc":"2.0","id":1,"method":"getconfirmedcount","params":null}' | grep -oE '"result":[0-9]+' | cut -d: -f2); \
  [ "$${h:-0}" -gt 101 ] || { ready=""; break; }; done; [ -n "$$ready" ] && break; sleep 5; done; \
[ $$rc -ne 0 ] || [ -n "$$ready" ] || { echo "FAIL: provider of $$id reports no monitor height above 101 after 300 s" | tee -a $$cell/ue_scenario.out; rc=1; }; \
if [ $$rc -eq 0 ]; then for run in "--group setup" "--group deposits" "--step join_v5" "--step traffic" "--step removal" "--step checkpoint_invariants"; do \
  docker exec -e HOST_ADDR=$(HOST_ADDR) -e UE_CHECK_PERIOD=$(1) -e UE_EPOCH_LENGTH=$(2) -e UE_STEP_TIMEOUT=$(shell echo $$((12 * $(2) + 900))) -e UE_BUILD_IMAGE=$$build \
    bitcoin-ipc ue_scenario.sh $$run 2>&1 | tee -a $$cell/ue_scenario.out; r=$${PIPESTATUS[0]}; [ $$rc -ne 0 ] || rc=$$r; build=0; done; fi; \
cp -r $(HOME)/.ipc/logs $(HOME)/.ipc/test-round $$cell/; \
for c in bitcoin-ipc $$(docker ps -a --format '{{.Names}}' | grep '^validator-'); do docker logs $$c > $$cell/$$c.docker.log 2>&1; done; \
printf '%s\t%s\t%s' $(1) $(2) $(3) >> $(OUT)/matrix.tsv; \
for t in PASS FAIL OUT_OF_SCOPE DEFERRED; do v=$$(grep -P "^$$t\tTOTAL\t[0-9]+$$" $$cell/ue_scenario.out | tail -n 1 | cut -f3); printf '\t%s' "$${v:-?}" >> $(OUT)/matrix.tsv; done; \
printf '\t%s\t%ss\n' $$rc $$(($$(date +%s) - start)) >> $(OUT)/matrix.tsv
endef

checkpoint-matrix: SHELL := /bin/bash
checkpoint-matrix:
	@[ -n "$(OUT)" ] || { echo "set OUT=<folder>"; exit 1; }
	mkdir -p $(OUT) && printf 'period\tK\tmultiple\tPASS\tFAIL\tOUT_OF_SCOPE\tDEFERRED\texit\tduration\n' > $(OUT)/matrix.tsv
	$(foreach c,$(CHECKPOINT_CELLS),$(call checkpoint_cell,$(word 1,$(subst :, ,$(c))),$(word 2,$(subst :, ,$(c))),$(word 3,$(subst :, ,$(c))));)
	column -t -s "$$(printf '\t')" $(OUT)/matrix.tsv
