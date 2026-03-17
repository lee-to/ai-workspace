SHELL := bash
.ONESHELL:
.SHELLFLAGS := -eu -o pipefail -c
.DELETE_ON_ERROR:
MAKEFLAGS += --warn-undefined-variables
MAKEFLAGS += --no-builtin-rules

.DEFAULT_GOAL := help

# --- Variables ---
VERSION ?= $(shell git describe --tags --always --dirty 2>/dev/null || echo "dev")
COMMIT  ?= $(shell git rev-parse --short HEAD 2>/dev/null || echo "unknown")
BINARY  := ai-workspace

.PHONY: help build release run test coverage lint fmt fmt-check audit check clean install uninstall

##@ General
help: ## Show this help
	@awk 'BEGIN {FS = ":.*##"; printf "Usage:\n  make \033[36m<target>\033[0m\n"} \
		/^[a-zA-Z_-]+:.*?## / {printf "  \033[36m%-15s\033[0m %s\n", $$1, $$2} \
		/^##@/ {printf "\n\033[1m%s\033[0m\n", substr($$0, 5)}' $(MAKEFILE_LIST)

##@ Development
build: ## Build in debug mode
	cargo build

release: ## Build in release mode
	cargo build --release

run: ## Run the binary (debug)
	cargo run

##@ Testing
test: ## Run all tests
	cargo test

coverage: ## Run tests with coverage report
	LLVM_COV=$(shell brew --prefix llvm)/bin/llvm-cov \
	LLVM_PROFDATA=$(shell brew --prefix llvm)/bin/llvm-profdata \
	cargo llvm-cov --html

##@ Code Quality
lint: ## Run clippy linter
	cargo clippy -- -D warnings

fmt: ## Format code
	cargo fmt

fmt-check: ## Check code formatting
	cargo fmt --check

audit: ## Run security audit on dependencies
	cargo audit

check: fmt-check lint test audit ## Run all checks (fmt + lint + tests + audit)

##@ Installation
install: ## Install binary to ~/.cargo/bin
	cargo install --path .

uninstall: ## Uninstall binary
	cargo uninstall ai-workspace

##@ Maintenance
clean: ## Clean build artifacts
	cargo clean
