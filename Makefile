.DEFAULT_GOAL := help
.PHONY: help up down clean logs build run stop liveness test fmt lint check smoke e2e

WEAVIATE_URL     ?= http://localhost:8080
WEAVIATE_API_KEY ?= readonly-key
BIND_ADDR        ?= 127.0.0.1:3000

help: ## Show this help
	@grep -E '^[a-zA-Z0-9_-]+:.*## ' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*## "}; {printf "  \033[36m%-8s\033[0m %s\n", $$1, $$2}'

# --- Local Weaviate cluster -------------------------------------------------

up: ## Start the 3-node Weaviate cluster and wait until it's healthy
	docker compose up -d
	@echo "Waiting for Weaviate to be ready..."
	@for port in 8080 8081 8082; do \
		curl -sf --retry 60 --retry-connrefused --retry-all-errors --retry-delay 1 \
			-o /dev/null http://localhost:$$port/v1/.well-known/ready || exit 1; \
	done
	@docker compose wait weaviate-init >/dev/null
	@echo "Cluster ready."

down: ## Stop the cluster (keeps data)
	docker compose down

clean: ## Stop the cluster and delete its data volumes
	docker compose down -v

logs: ## Follow the cluster logs
	docker compose logs -f

# --- Service ----------------------------------------------------------------

build: ## Build the service
	cargo build

run: ## Run the service against the local cluster
	WEAVIATE_URL=$(WEAVIATE_URL) WEAVIATE_API_KEY=$(WEAVIATE_API_KEY) BIND_ADDR=$(BIND_ADDR) cargo run

stop: ## Stop the service started with `make run`
	@pkill -TERM -f 'target/(debug|release)/weaviate-monitor' && echo "Service stopped." || echo "Service is not running."

liveness: ## Call /api/liveness on the running service (start it first with `make run`)
	curl -sS -i http://$(BIND_ADDR)/api/liveness

# --- Tests ------------------------------------------------------------------

test: ## Run unit tests (no cluster needed)
	cargo test

fmt: ## Format the code
	cargo fmt

lint: ## Check formatting and run clippy
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

check: lint test ## Lint and unit tests (what CI should run)

smoke: build ## Smoke-test the running service against the cluster (needs `make up`)
	WEAVIATE_URL=$(WEAVIATE_URL) WEAVIATE_API_KEY=$(WEAVIATE_API_KEY) ./scripts/smoke.sh

e2e: up smoke ## Start the cluster, then run the smoke test
