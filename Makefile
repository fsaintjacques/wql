CARGO_FLAGS ?=

.PHONY: build release test lint format check clean

build:
	cargo build $(CARGO_FLAGS)

release:
	cargo build --release $(CARGO_FLAGS)

test:
	cargo test $(CARGO_FLAGS)

lint:
	cargo clippy $(CARGO_FLAGS) -- -D warnings

format:
	cargo fmt

check: format lint test
	@echo "All checks passed."

clean:
	cargo clean
