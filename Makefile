PROJECT := rsetup-next
CARGO_HOME ?= $(CURDIR)/debian/cargo-home

.DEFAULT_GOAL := build

.PHONY: build test lint fmt-check run tui serve deb-prepare deb clean

# `cargo fmt --all` also formats local path dependencies, which would drag in
# the pinned deviceinfo submodule. Format the workspace members explicitly and
# fail loudly when Cargo.toml gains a member that is missing from this list.
FMT_PACKAGES := rsetup-core rsetup-next

build:
	cargo build --workspace --locked

test:
	cargo test --workspace --locked
	node --test ui/*.test.mjs

lint: fmt-check
	cargo clippy --workspace --all-targets -- -D warnings

fmt-check:
	@members=$$(sed -n '/^members = \[/,/^\]/p' Cargo.toml | grep -c '"'); \
	 listed=$$(printf '%s\n' $(FMT_PACKAGES) | wc -l | tr -d ' '); \
	 if [ "$$members" != "$$listed" ]; then \
	   echo "Cargo.toml declares $$members workspace members but FMT_PACKAGES lists $$listed." >&2; \
	   echo "Add the new member to FMT_PACKAGES in the Makefile." >&2; \
	   exit 1; \
	 fi
	cargo fmt $(addprefix --package ,$(FMT_PACKAGES)) -- --check

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
