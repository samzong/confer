CARGO := cargo

BOLD  := \033[1m
CYAN  := \033[36m
GREEN := \033[32m
RESET := \033[0m

.DEFAULT_GOAL := help

# ── Build ────────────────────────────────────────────────────────────────────

.PHONY: build release

build: ## Debug build
	$(CARGO) build

release: ## Release build (LTO + strip)
	$(CARGO) build --release

# ── Quality ──────────────────────────────────────────────────────────────────

.PHONY: check audit fmt lint test

check: audit ## Full quality gate — dependency audit, format, lint, test
	@printf '\n$(BOLD)[2/4] Checking format$(RESET)\n'
	$(CARGO) fmt --all -- --check
	@printf '\n$(BOLD)[3/4] Running clippy$(RESET)\n'
	$(CARGO) clippy --all-targets --all-features -- -D warnings
	@printf '\n$(BOLD)[4/4] Running tests$(RESET)\n'
	$(CARGO) test --all-targets --all-features
	@printf '\n$(GREEN)  ✓ All checks passed$(RESET)\n\n'

audit: ## Check locked Rust dependencies for known vulnerabilities
	@printf '\n$(BOLD)[1/4] Auditing dependencies$(RESET)\n'
	$(CARGO) audit --file Cargo.lock

fmt: ## Format code
	$(CARGO) fmt --all

lint: ## Run clippy
	$(CARGO) clippy --all-targets --all-features -- -D warnings

test: ## Run tests
	$(CARGO) test --all-targets --all-features

# ── Install ──────────────────────────────────────────────────────────────────

.PHONY: install

install: ## Install the binary to ~/.cargo/bin
	$(CARGO) install --path . --locked --force

# ── Release ──────────────────────────────────────────────────────────────────

.PHONY: release-patch release-minor release-major

release-patch: ## Bump patch + commit + tag + push (append EXECUTE=1 to apply)
	$(CARGO) release patch $(if $(EXECUTE),--execute,)

release-minor: ## Bump minor + commit + tag + push (append EXECUTE=1 to apply)
	$(CARGO) release minor $(if $(EXECUTE),--execute,)

release-major: ## Bump major + commit + tag + push (append EXECUTE=1 to apply)
	$(CARGO) release major $(if $(EXECUTE),--execute,)

# ── Maintenance ──────────────────────────────────────────────────────────────

.PHONY: clean

clean: ## Remove build artifacts
	$(CARGO) clean

# ── Help ─────────────────────────────────────────────────────────────────────

.PHONY: help

help: ## Show available targets
	@awk 'BEGIN {FS = ":.*## "; printf "\n$(BOLD)Confer$(RESET) — local multi-agent rooms over MCP\n"} \
		/^# ── / {n = $$0; gsub(/(^# ── | ─+$$)/, "", n); printf "\n$(BOLD)%s$(RESET)\n", n} \
		/^[a-zA-Z_-]+:.*## / {printf "  $(CYAN)make %-13s$(RESET) %s\n", $$1, $$2} \
		END {printf "\n"}' $(MAKEFILE_LIST)
