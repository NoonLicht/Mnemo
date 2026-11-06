# Mnemo — common development tasks.
#
#   make            # this help
#   make install    # install JS dependencies
#   make dev        # run the Tauri app in dev mode
#   make build      # build desktop bundles
#   make cli        # build the agent CLI
#   make test       # Rust test suite
#   make check      # TypeScript check + Rust check
#   make bench      # filename-search benchmark (needs a directory)
#   make reset      # delete all local Mnemo data (asks first)
#   make reset-index# delete only the index, keep settings
#   make clean      # remove build outputs (not your data)

SHELL := /bin/bash
.DEFAULT_GOAL := help

CARGO_TARGET := src-tauri/target
CLI_BIN := cli/target/release/mnemo-cli
BENCH_DIR ?= $(HOME)/Projects

.PHONY: help install dev build cli test check fmt bench clean reset reset-index data-dir

help: ## Show this help
	@printf '\nMnemo tasks:\n\n'
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}'
	@printf '\n'

install: ## Install JS dependencies (bun, or npm fallback)
	@command -v bun >/dev/null 2>&1 && bun install || npm install

dev: ## Run the desktop app in dev mode
	@command -v bun >/dev/null 2>&1 && bun run tauri dev || npm run tauri dev

build: ## Build desktop bundles (release)
	@command -v bun >/dev/null 2>&1 && bun run tauri build || npm run tauri build

cli: ## Build the agent CLI (release)
	cargo build --release --manifest-path cli/Cargo.toml
	@echo "built: $(CLI_BIN)"

test: ## Run the Rust test suite
	cargo test --manifest-path src-tauri/Cargo.toml

check: ## TypeScript typecheck + Rust check
	@command -v bun >/dev/null 2>&1 && bunx tsc --noEmit || npx tsc --noEmit
	cargo check --manifest-path src-tauri/Cargo.toml

fmt: ## Format Rust code
	cargo fmt --manifest-path src-tauri/Cargo.toml
	cargo fmt --manifest-path cli/Cargo.toml

bench: ## Run the filename-search benchmark (BENCH_DIR=...)
	cargo run --release --manifest-path src-tauri/Cargo.toml --example bench -- "$(BENCH_DIR)"

data-dir: ## Print where Mnemo stores config and index
	@if [ "$$(uname -s 2>/dev/null)" = "Darwin" ]; then \
		echo "$$HOME/Library/Application Support/mnemo"; \
	elif [ -n "$$APPDATA" ] && [ ! -d /etc ]; then \
		echo "$$APPDATA/mnemo"; \
	else \
		echo "$${XDG_CONFIG_HOME:-$$HOME/.config}/mnemo"; \
	fi

reset: ## Delete ALL local Mnemo data (config, index, models).
	@dir="$$( $(MAKE) --no-print-directory data-dir )"; \
	if [ -d "$$dir" ]; then \
		echo "Removing $$dir"; \
		rm -rf "$$dir"; \
		echo "Done. Mnemo will start fresh on next launch."; \
	else \
		echo "No Mnemo data found at $$dir"; \
	fi

reset-index: ## Delete only index/metadata (keeps settings).
	@dir="$$( $(MAKE) --no-print-directory data-dir )"; \
	if [ -d "$$dir" ]; then \
		rm -rf "$$dir/index" "$$dir/thumbs"; \
		rm -f "$$dir/metadata.db" "$$dir/metadata.db-wal" "$$dir/metadata.db-shm"; \
		echo "Index removed. Settings kept in $$dir/config.json"; \
	else \
		echo "No Mnemo data found at $$dir"; \
	fi

clean: ## Remove build outputs (Rust targets, dist, node_modules kept)
	rm -rf dist "$(CARGO_TARGET)" cli/target
	@echo "Build outputs removed."