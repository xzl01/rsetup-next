PROJECT := rsetup-next
CARGO_HOME ?= $(CURDIR)/debian/cargo-home

.DEFAULT_GOAL := build

.PHONY: build test lint run tui serve deb-prepare deb clean

build:
	cargo build --workspace --locked

test:
	cargo test --workspace --locked
	node --test ui/i18n.test.mjs

lint:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings

run:
	cargo run -p $(PROJECT) -- status

tui:
	cargo run -p $(PROJECT) -- tui

serve:
	cargo run -p $(PROJECT) -- serve

# Debian package builds are network-isolated. Populate the package-local Cargo
# cache first, then let dpkg-buildpackage enforce offline mode.
deb-prepare:
	CARGO_HOME=$(CARGO_HOME) cargo fetch --locked

deb: deb-prepare
	dpkg-buildpackage --build=binary --no-sign

clean:
	cargo clean
	dh clean --buildsystem=none
