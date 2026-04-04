CARGO_FLAGS ?=

.PHONY: build release test lint format check-format check-lint check clean

build:
	cargo build $(CARGO_FLAGS)

release:
	cargo build --release $(CARGO_FLAGS)

test:
	cargo test $(CARGO_FLAGS)

lint:
	cargo clippy $(CARGO_FLAGS) --fix --allow-dirty -- -D warnings

format:
	cargo fmt

check-format:
	cargo fmt --check

check-lint:
	cargo clippy $(CARGO_FLAGS) -- -D warnings

check: check-format check-lint test

clean:
	cargo clean
