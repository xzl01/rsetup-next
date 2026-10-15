PROJECT := rsetup-next
CARGO_HOME ?= $(CURDIR)/debian/cargo-home
DESKTOP_MANIFEST := apps/desktop/src-tauri/Cargo.toml
DESKTOP_TARGET_DIR := target/desktop-ci

# The minimum supported Rust version and the toolchain that provides it. The
# msrv-check target below never trusts PATH for any of the three binaries:
# `cargo +1.85.0` only works through a rustup proxy, and a Homebrew or distro
# rustc (or rustdoc) earlier in PATH answers for 1.85 without being 1.85 — that
# is how a let-chain reached `main` while the gate reported green, and running
# `make msrv-check` with only RUSTC bound then failed in the doctest step
# because rustdoc was still the newer one. Resolve all three through rustup by
# path and assert the versions actually loaded.
MSRV_TOOLCHAIN ?= 1.85.0
MSRV_CARGO ?= $(shell rustup which --toolchain $(MSRV_TOOLCHAIN) cargo 2>/dev/null)
MSRV_RUSTC ?= $(shell rustup which --toolchain $(MSRV_TOOLCHAIN) rustc 2>/dev/null)
MSRV_RUSTDOC ?= $(shell rustup which --toolchain $(MSRV_TOOLCHAIN) rustdoc 2>/dev/null)
MSRV_RUSTC_PREFIX ?= 1.85.
# A gate that uses a second compiler gets its own target directory, so it never
# mixes artifacts built by the stable toolchain into this one.
MSRV_TARGET_DIR ?= target/msrv-ci

.DEFAULT_GOAL := build

.PHONY: build test lint fmt-check msrv-check run tui serve deb-prepare deb clean desktop-test desktop-check

# `cargo fmt --all` also formats local path dependencies, which would drag in
# the pinned deviceinfo submodule. Format the workspace members explicitly and
# fail loudly when Cargo.toml gains a member that is missing from this list.
# `rsetup-desktop` is deliberately absent: it lives in apps/desktop/src-tauri,
# which Cargo.toml keeps in `workspace.exclude`, so it is not a workspace member
# and `cargo fmt --package rsetup-desktop` would not resolve. It is covered by
# the desktop-test/desktop-check targets below instead, which go through
# --manifest-path, and by the desktop-tauri-ipc CI job.
FMT_PACKAGES := rsetup-core rsetup-next rsetup-controller rsetup-protocol

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

# Runs the workspace tests with the minimum supported toolchain. This mirrors the
# rust-msrv CI job: both binaries are resolved through rustup and their versions
# are asserted, so a newer compiler earlier in PATH cannot answer for 1.85.
msrv-check:
	@test -n "$(MSRV_CARGO)" && test -n "$(MSRV_RUSTC)" && test -n "$(MSRV_RUSTDOC)" || { \
	  echo "msrv-check: rustup has no $(MSRV_TOOLCHAIN) toolchain; run 'rustup toolchain install $(MSRV_TOOLCHAIN)'" >&2; \
	  exit 1; \
	}
	@cargo_version="$$("$(MSRV_CARGO)" --version)" || exit 1; \
	 rustc_version="$$("$(MSRV_RUSTC)" --version)" || exit 1; \
	 rustdoc_version="$$("$(MSRV_RUSTDOC)" --version)" || exit 1; \
	 echo "msrv-check: $$cargo_version"; \
	 echo "msrv-check: $$rustc_version"; \
	 echo "msrv-check: $$rustdoc_version"; \
	 case "$$cargo_version" in \
	   "cargo $(MSRV_TOOLCHAIN)"*) ;; \
	   *) echo "msrv-check: expected cargo $(MSRV_TOOLCHAIN), got $$cargo_version" >&2; exit 1 ;; \
	 esac; \
	 case "$$rustc_version" in \
	   "rustc $(MSRV_RUSTC_PREFIX)"*) ;; \
	   *) echo "msrv-check: expected rustc $(MSRV_RUSTC_PREFIX)x, got $$rustc_version" >&2; exit 1 ;; \
	 esac; \
	 case "$$rustdoc_version" in \
	   "rustdoc $(MSRV_RUSTC_PREFIX)"*) ;; \
	   *) echo "msrv-check: expected rustdoc $(MSRV_RUSTC_PREFIX)x, got $$rustdoc_version" >&2; exit 1 ;; \
	 esac
	CARGO_TARGET_DIR=$(MSRV_TARGET_DIR) RUSTC="$(MSRV_RUSTC)" RUSTDOC="$(MSRV_RUSTDOC)" \
	  "$(MSRV_CARGO)" test --workspace --locked

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
