.PHONY: prepare fmt lint build update publish-check

prepare: fmt lint build

lint:
	cargo fmt --check
	cargo clippy --workspace --all-targets --all-features -- -D warnings
	cargo check --workspace --release --locked

fmt:
	cargo fmt

build:
	cargo build --workspace --release
	ls -lh target/release/macmon

update:
	@# cargo install cargo-edit
	cargo upgrade -i

publish-check:
	cargo package -p macmon-lib --list --allow-dirty
	cargo publish -p macmon-lib --dry-run --allow-dirty
