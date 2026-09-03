.PHONY: audit build check fmt install lint test

audit:
	cargo audit

build:
	cargo build

check: audit fmt lint test

fmt:
	cargo fmt --all -- --check

install:
	cargo install --path . --locked --force

lint:
	cargo clippy --all-targets --all-features -- -D warnings

test:
	cargo test --all-targets --all-features
