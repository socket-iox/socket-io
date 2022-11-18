build_date = `date +%Y%m%d%H%M`
commit = `git rev-parse HEAD`
version = `git rev-parse --short HEAD`

.PHONY: clippy
build:
	cargo build --verbose
test:
	cargo test --release --verbose
clippy:
	cargo clippy --all && \
	cargo clippy --all --features "client" --no-default-features && \
	cargo clippy --all --features "server" --no-default-features

