PROJECT := rsetup-next
CARGO_HOME ?= $(CURDIR)/debian/cargo-home
DESKTOP_MANIFEST := apps/desktop/src-tauri/Cargo.toml
DESKTOP_TARGET_DIR := target/desktop-ci

.DEFAULT_GOAL := build

.PHONY: build test lint fmt-check run tui serve deb-prepare deb clean desktop-test desktop-check

# `cargo fmt --all` also formats local path dependencies, which would drag in
# the pinned deviceinfo submodule. Format the workspace members explicitly and
# fail loudly when Cargo.toml gains a member that is missing from this list.
# `rsetup-desktop` is deliberately absent: it lives in apps/desktop/src-tauri,
# which Cargo.toml keeps in `workspace.exclude`, so it is not a workspace member
# and `cargo fmt --package rsetup-desktop` would not resolve. It is covered by
# the desktop-test/desktop-check targets below instead, which go through
# --manifest-path, and by the desktop-tauri-ipc CI job.
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

# The desktop crate is workspace-excluded, so `--package` cannot select it; go
# through its own manifest instead. These mirror the desktop-tauri-ipc CI job.
desktop-test:
	CARGO_TARGET_DIR=$(DESKTOP_TARGET_DIR) cargo test --locked --manifest-path $(DESKTOP_MANIFEST)

desktop-check:
	cargo fmt --manifest-path $(DESKTOP_MANIFEST) -- --check
	CARGO_TARGET_DIR=$(DESKTOP_TARGET_DIR) cargo clippy --locked --manifest-path $(DESKTOP_MANIFEST) --all-targets -- -D warnings

# Debian package builds are network-isolated. Populate the package-local Cargo
# cache first, then let dpkg-buildpackage enforce offline mode.
deb-prepare:
	CARGO_HOME=$(CARGO_HOME) cargo fetch --locked

deb: deb-prepare
	dpkg-buildpackage --build=binary --no-sign

clean:
	cargo clean
	dh clean --buildsystem=none
