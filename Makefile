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

.PHONY: build test lint fmt-check msrv-check run tui serve deb-prepare deb clean desktop-test desktop-check metrics-gates

# `cargo fmt --all` also formats local path dependencies, which would drag in
# the pinned deviceinfo submodule. Format the workspace members explicitly and
# fail loudly when Cargo.toml gains a member that is missing from this list.
# `rsetup-desktop` is deliberately absent: it lives in apps/desktop/src-tauri,
# which Cargo.toml keeps in `workspace.exclude`, so it is not a workspace member
# and `cargo fmt --package rsetup-desktop` would not resolve. It is covered by
# the desktop-test/desktop-check targets below instead, which go through
# --manifest-path, and by the desktop-tauri-ipc CI job.
FMT_PACKAGES := rsetup-core rsetup-next rsetup-controller rsetup-protocol rsetup-board-agent

build:
	cargo build --workspace --locked

test:
	cargo test --workspace --locked
	node --test ui/*.test.mjs

# 中控离线测试轨道：只跑前端 Vitest/typecheck/构建、Rust 资源测试与既有回归，
# 绝不初始化数据库。真实 MySQL/TiDB 用例属操作员逐引擎显式授权的 `--ignored`
# 轨道，不在本目标内；普通 CI 缺库 URL 时只记未验证，不在此隐式跑真库。
# 顺序是硬约束：web_assets 直接读取 apps/controller-web/dist 的真实构建字节，
# 前端必须先构建，Rust 资源测试才能拿到预期源。
.PHONY: controller-test-offline
controller-test-offline:
	npm --prefix apps/controller-web test -- --run
	npm --prefix apps/controller-web run typecheck
	npm --prefix apps/controller-web run build
	cargo test -p rsetup-controller --test web_assets --locked
	$(MAKE) test

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

# Prometheus 指标线的静态门禁（规范 §7.8 的九条命令）。第 5/6/7/9 条必须无输出、
# 第 8 条必须**有**输出——两条 grep 的期望相反是刻意的：前者拦手写常量时间折叠与
# 测试注入缝泄漏进生产入口，后者钉住比较确实落在 subtle 上。第 5 条的模式串刻意写成
# shell 拆分形式 'TO''DO|T-B-D|FIXME'，这样把命令写进文档不会自指命中。
# 第 1 条用仓库既有的 fmt-check（等价于 cargo fmt --all -- --check，但额外校验
# FMT_PACKAGES 未漏登记 workspace member）。
.PHONY: metrics-gates
metrics-gates: fmt-check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test -p rsetup-controller
	cargo test -p rsetup-controller --test production_composition
	@test -z "$$(grep -rnE 'TO''DO|T-B-D|FIXME' crates/rsetup-controller/src/metrics)" || { \
	  echo "metrics 门禁 5：src/metrics 下存在 TODO/T-B-D/FIXME 占位符" >&2; exit 1; }
	@test -z "$$(grep -rn 'build_production_router_with_metrics_source\|SqlxMetricsSnapshotSource::new' crates/rsetup-controller/src/main.rs)" || { \
	  echo "metrics 门禁 6：生产入口不得触碰测试注入缝或自建汇总源" >&2; exit 1; }
	@test -z "$$(grep -rn 'diff |= a\[i\]' crates/rsetup-controller/src/metrics)" || { \
	  echo "metrics 门禁 7：指标线不得手写常量时间折叠" >&2; exit 1; }
	@test -n "$$(grep -rn 'subtle::ConstantTimeEq\|use subtle::ConstantTimeEq' crates/rsetup-controller/src/metrics/auth.rs)" || { \
	  echo "metrics 门禁 8：常量时间比较必须落在 subtle 上" >&2; exit 1; }
	@test -z "$$(grep -nE 'SCRAPE_TIMEOUT.*\?' crates/rsetup-controller/src/metrics/collect.rs)" || { \
	  echo "metrics 门禁 9：连接借出后禁止 ? 早退（F1/I7）" >&2; exit 1; }
	@if git diff --cached --name-only | grep -Eq 'controller\.toml|^secret/'; then \
	  echo "metrics 门禁（§7.8 末段附加检查）：暂存区含配置文件实体或凭据目录" >&2; exit 1; fi
	@echo "metrics-gates：九条门禁全部符合预期（第 8 条已确认有输出）"

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
