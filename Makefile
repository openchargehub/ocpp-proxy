.PHONY: help test test-domain-coverage test-coverage fmt clippy run docker-build

help:
	@echo "make test                 Run the workspace test suite"
	@echo "make test-domain-coverage Domain line and region coverage"
	@echo "make test-coverage        Workspace line coverage"
	@echo "make fmt                  Format Rust sources"
	@echo "make clippy               Lint with warnings denied"
	@echo "make run                  Start the proxy on port 9000"
	@echo "make docker-build         Build the add-on image"

test:
	cargo test --workspace

test-domain-coverage:
	cargo +nightly llvm-cov -p ocpp-proxy-domain --branch --fail-under-lines 99 --fail-under-regions 99

test-coverage:
	cargo llvm-cov --workspace --fail-under-lines 90

fmt:
	cargo fmt --all

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

run:
	cargo run -p ocpp-proxy

docker-build:
	docker build --target addon -t ocpp-proxy .
