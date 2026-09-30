# nats-hub developer entrypoints. See CONTRIBUTING.md.
# All targets auto-apply the build env from scripts/dev/lib.sh
# (repo-local nats-server on PATH, BINDGEN fix when needed).

SHELL := /usr/bin/env bash
DEV   := scripts/dev
ENV   := source $(DEV)/lib.sh &&
PY    := .venv/bin/python

.PHONY: help doctor setup setup-extras setup-js build build-release test test-rust test-py lint fmt up clean-run

help: ## Show targets
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}'

doctor: ## Check toolchain; prints a fix for anything missing
	@$(DEV)/doctor.sh

setup: ## Install nats-server (.tools/bin) + Python venv (.venv). Idempotent.
	@$(DEV)/setup.sh

setup-extras: ## setup + optional SDKs (ACP, Cursor, Telegram, Discord)
	@$(DEV)/setup.sh --extras

setup-js: ## setup + node_modules for the JS Cline workers
	@$(DEV)/setup.sh --js

build: ## cargo build (all bins, incl. hub-tui)
	@$(ENV) cargo build --bins --features tui

build-release: ## cargo build --release
	@$(ENV) cargo build --release --bins --features tui

test: test-rust test-py ## Run all tests (Rust against a throwaway NATS + hub-server, then Python)

test-rust: ## Rust tests against an isolated nats-server + hub-server (random port)
	@$(DEV)/with_stack.sh cargo test --features tui

test-py: ## Python tests (pytest) against an isolated nats-server + hub-server
	@$(DEV)/with_stack.sh $(PY) -m pytest -q tests/python

lint: ## fmt check + clippy
	@$(ENV) cargo fmt --all -- --check && cargo clippy --all-targets --features tui

fmt: ## cargo fmt
	@cargo fmt --all

up: ## Local stack: nats-server + hub-server + visualizer + 2 echo workers (Ctrl+C stops)
	@$(DEV)/up.sh

clean-run: ## Delete local dev-stack state (.tools/run: DB, JetStream, logs)
	rm -rf .tools/run
