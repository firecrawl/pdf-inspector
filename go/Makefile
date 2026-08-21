# Convenience targets for the Go binding. Prefer `make native` over typing
# out the cargo invocation — kept here (rather than only in the README) so
# `go generate` (see pdfinspector/gen.go) has one canonical fallback command
# to point users at when no prebuilt native library is available for their
# platform.

.PHONY: native fmt lint test clean

native:
	cargo build --release

fmt:
	cargo fmt --manifest-path Cargo.toml -- --check

lint:
	cargo clippy --manifest-path Cargo.toml --release -- -D warnings

test: native
	cd pdfinspector && go test ./...

clean:
	cargo clean
