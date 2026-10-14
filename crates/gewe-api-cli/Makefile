.PHONY: install-local test

install-local:
	cargo build --release -p gewe-api-cli
	install -m 755 ../../target/release/gewe-api $(HOME)/.local/bin/gewe-api

test:
	cargo test -p gewe-api-cli
	cargo clippy -p gewe-api-cli -- -D warnings
