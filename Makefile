.PHONY: prepare fmt lint build xcframework update publish-check

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
	ls -lh target/release/libmacmon.dylib

xcframework:
	cargo build -p macmon-lib --release --locked
	rm -rf dist/CMacmon.xcframework
	xcodebuild -create-xcframework \
		-library ./target/release/libmacmon.dylib \
		-headers ./crates/lib/include \
		-output ./dist/CMacmon.xcframework

update:
	@# cargo install cargo-edit
	cargo upgrade -i

publish-check:
	cargo package -p macmon-lib --list --allow-dirty
	cargo publish -p macmon-lib --dry-run --allow-dirty
