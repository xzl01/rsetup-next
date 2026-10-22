PROJECT := rsetup-next
CARGO_HOME ?= $(CURDIR)/debian/cargo-home
DESKTOP_MANIFEST := apps/desktop/src-tauri/Cargo.toml
DESKTOP_TARGET_DIR := target/desktop-ci

# The controller embeds its web UI at compile time: crates/rsetup-controller/build.rs
# generates the asset table that web.rs include!s, and it panics when
# apps/controller-web/dist is missing. `apps/controller-web/dist` is **committed to the
# repository**, so a clean checkout builds the whole workspace with cargo alone — no
# Node, no npm, no network. Only two things are still expected of a machine that edits
# the frontend: Node 22 plus npm to run `make web-build`, and the discipline to commit
# the regenerated dist afterwards. CI rebuilds the frontend from source and fails when
# `git diff --exit-code -- apps/controller-web/dist` is not empty, so a stale dist
# cannot survive a pull request.
# `build`, `test`, `lint` and `metrics-gates` therefore depend on
# web-dist-check — a pure existence check of dist/index.html — and never on the npm
# build. The explicit rebuild entry point is `make web-build`, backed by two stamp
# files:
#   WEB_DEPS_STAMP — node_modules/.rsetup-npm-ci-stamp, the locked dependency
#     install. Its only prerequisites are package.json and package-lock.json, so
#     editing a component never re-runs `npm ci` (that used to cost a full
#     reinstall on every frontend change, because the single stamp also listed
#     the whole src tree as a prerequisite).
#   WEB_STAMP — dist/.rsetup-build-stamp, the `vite build` output, rebuilt when a
#     dist input changes. The stamp itself is git-ignored; the dist files it stands
#     for are not. NPM may be overridden with a pinned node wrapper, a mirror wrapper
#     or any other npm-compatible command.
WEB_DIR := apps/controller-web
WEB_DIST_INDEX := $(WEB_DIR)/dist/index.html
WEB_DEPS_STAMP := $(WEB_DIR)/node_modules/.rsetup-npm-ci-stamp
WEB_STAMP := $(WEB_DIR)/dist/.rsetup-build-stamp
NPM ?= npm
# Passed verbatim to `npm ci`, so an offline machine with a warm npm cache can do
# `make web-assets NPM_CI_FLAGS=--offline`. Empty by default.
NPM_CI_FLAGS ?=
# Everything `vite build` reads besides node_modules. Test files are excluded:
# Vitest runs them, `vite build` never bundles them, so touching a spec must not
# invalidate dist. The src directory itself is a prerequisite as well, so adding
# or deleting a file — including a file the find below cannot see yet — rebuilds.
# WEB_PUBLIC_DIR covers Vite's default publicDir; $(wildcard) makes it expand to
# nothing while the directory does not exist, which is the current state.
# The two directories are separate variables so the contract test can tell the
# directory prerequisite apart from the path inside $(shell find …).
WEB_SRC_DIR := $(WEB_DIR)/src
WEB_PUBLIC_DIR := $(wildcard $(WEB_DIR)/public)
# 目录项只覆盖新增/删除；public/ 下已有文件的内容改动由这份文件清单覆盖。
WEB_PUBLIC_FILES := $(shell if [ -d $(WEB_DIR)/public ]; then find $(WEB_DIR)/public -type f 2>/dev/null; fi)
WEB_SRCS := $(WEB_DIR)/index.html $(WEB_DIR)/vite.config.ts \
  $(wildcard $(WEB_DIR)/tsconfig*.json) \
  $(shell find $(WEB_SRC_DIR) -type f ! -name '*.test.ts' ! -name '*.test.js' 2>/dev/null) \
  $(WEB_SRC_DIR) \
  $(WEB_PUBLIC_DIR) $(WEB_PUBLIC_FILES)

# Rust 工具链口径：默认使用当前 stable（本地验证 rustc 1.99.1 / cargo 1.99.1）；
# MSRV 保持 1.85，由 msrv-check 用已安装的 1.85.0 单独验证，不把 1.85.0 设为默认。
# msrv-check 不信任 PATH：三个二进制都通过 rustup 按路径解析并断言版本，避免 PATH
# 里更新的 rustc/rustdoc 冒充 1.85。
MSRV_TOOLCHAIN ?= 1.85.0
MSRV_CARGO ?= $(shell rustup which --toolchain $(MSRV_TOOLCHAIN) cargo 2>/dev/null)
MSRV_RUSTC ?= $(shell rustup which --toolchain $(MSRV_TOOLCHAIN) rustc 2>/dev/null)
MSRV_RUSTDOC ?= $(shell rustup which --toolchain $(MSRV_TOOLCHAIN) rustdoc 2>/dev/null)
MSRV_RUSTC_PREFIX ?= 1.85.
# MSRV 用独立 target 目录，绝不与 stable 的产物混用。
MSRV_TARGET_DIR ?= target/msrv-ci

.DEFAULT_GOAL := build

.PHONY: build test lint fmt-check msrv-check run tui serve deb-prepare deb clean desktop-test desktop-check metrics-gates metrics-static-gates web-build web-assets web-dist-check

# `cargo fmt --all` also formats local path dependencies, which would drag in
# the pinned deviceinfo submodule. Format the workspace members explicitly and
# fail loudly when Cargo.toml gains a member that is missing from this list.
# `rsetup-desktop` is deliberately absent: it lives in apps/desktop/src-tauri,
# which Cargo.toml keeps in `workspace.exclude`, so it is not a workspace member
# and `cargo fmt --package rsetup-desktop` would not resolve. It is covered by
# the desktop-test/desktop-check targets below instead, which go through
# --manifest-path, and by the desktop-tauri-ipc CI job.
FMT_PACKAGES := rsetup-core rsetup-next rsetup-controller rsetup-protocol rsetup-board-agent

build: web-dist-check
	cargo build --workspace --locked

test: web-dist-check
	cargo test --workspace --locked
	node --test ui/*.test.mjs

# 前端产物门禁（dist 已入库）：`rsetup-controller` 的 build.rs 在编译期读取
# apps/controller-web/dist 并生成 web.rs include! 的资源表，dist 缺失即 panic。
# dist 的三个产物（index.html 与 assets/ 下的 js、css）现在随仓库提交，所以
# `build`/`test`/`lint`/`metrics-gates` 不再触发任何 npm：本目标只做
# 一次存在性检查，把「产物被误删/被 gitignore 重新吃掉」这类破坏在 cargo 编译
# 之前用一个可读的错误拦下来，干净检出与离线机器直接跑 cargo 也不再需要 Node。
# 改过 apps/controller-web 源码之后必须 `make web-build` 并把 dist 一起提交；
# CI 会重新构建前端并用 `git diff --exit-code -- apps/controller-web/dist` 比对。
.PHONY: web-dist-check
web-dist-check:
	test -f $(WEB_DIST_INDEX) || { echo "web-dist-check: dist 未入库/被删除，缺少 $(WEB_DIST_INDEX)；请运行 make web-build 重新生成并提交 dist" >&2; exit 1; }

# 显式的前端重建入口：只有改过 apps/controller-web 源码时才需要，装依赖 + vite
# build + 刷新 stamp，产物仍需人工提交入库（CI 只比对，不代提交）。
.PHONY: web-build
web-build: web-assets

# 两级 stamp 的显式重建链路，语义不变，只不再被编译类目标依赖：
#   $(WEB_DEPS_STAMP) ← package.json/package-lock.json，配方 `npm ci $(NPM_CI_FLAGS)`；
#   $(WEB_STAMP)      ← $(WEB_DEPS_STAMP) + index.html/vite.config.ts/tsconfig*.json/
#                       src 下非测试文件/src 目录本身/public（存在时），配方只 `npm run build`。
# 依赖未变化时 make 两级都不重跑，改一个组件不会再触发一次 npm ci。
# 本目标需要 Node 22、npm（干净检出首次执行还需网络；本机 npm cache 已预热时可
# `make web-build NPM_CI_FLAGS=--offline`）。stamp 文件本身被 .gitignore 忽略，
# 所以手工准备 node_modules 与 dist 后 touch 两个 stamp 仍可让 make 认为已满足；
# 只拷贝 node_modules 和 dist 而不 touch stamp 是无效的：make 只看 stamp 的时间戳。
.PHONY: web-assets
web-assets: $(WEB_STAMP)

$(WEB_DEPS_STAMP): $(WEB_DIR)/package.json $(WEB_DIR)/package-lock.json
	@mkdir -p $(dir $@)
	$(NPM) --prefix $(WEB_DIR) ci $(NPM_CI_FLAGS)
	@touch $@

$(WEB_STAMP): $(WEB_DEPS_STAMP) $(WEB_SRCS)
	@mkdir -p $(dir $@)
	$(NPM) --prefix $(WEB_DIR) run build
	@touch $@

# 中控离线测试轨道：只跑前端 Vitest/typecheck、Rust 资源测试与既有回归，
# 绝不初始化数据库。真实 MySQL/TiDB 用例属操作员逐引擎显式授权的 `--ignored`
# 轨道，不在本目标内；普通 CI 缺库 URL 时只记未验证，不在此隐式跑真库。
# 顺序是硬约束：web_assets 直接读取 apps/controller-web/dist 的真实构建字节，
# dist 必须先存在，Rust 资源测试才能拿到预期源。dist 入库后本目标不再自动
# `npm ci`/`run build`——它先由 web-dist-check 确认产物在位，再跑前端自身的两条
# 门禁（test/typecheck，需要本地 node_modules）；改了源码请先 `make web-build`。
.PHONY: controller-test-offline
controller-test-offline: web-dist-check
	npm --prefix apps/controller-web test -- --run
	npm --prefix apps/controller-web run typecheck
	cargo test -p rsetup-controller --test web_assets --locked
	$(MAKE) test

# clippy --workspace 会编译 rsetup-controller，因此同样先决于 dist；
# 命令本身不变，只补先决条件，干净检出直接 `make lint` 不再撞上 build.rs 的 panic。
lint: fmt-check web-dist-check
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

# Prometheus 指标线的完整门禁（规范 §7.8 的九条命令）：第 1 条用仓库既有的 fmt-check
# （等价于 cargo fmt --all -- --check，但额外校验 FMT_PACKAGES 未漏登记 workspace
# member），随后 clippy 与两条 cargo test；五条 grep 静态检查拆进独立目标
# metrics-static-gates 并由本目标依赖，CI 只想跑静态检查时可直接 `make metrics-static-gates`。
# web-dist-check 是后三条命令的先决条件：clippy 与两条 cargo test 都要编译
# rsetup-controller，缺前端产物即在 build.rs 失败；dist 入库后该检查不再跑 npm。
.PHONY: metrics-gates
metrics-gates: fmt-check metrics-static-gates web-dist-check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test -p rsetup-controller
	cargo test -p rsetup-controller --test production_composition
	@echo "metrics-gates：九条门禁全部符合预期（第 8 条已确认有输出）"

# 五条 grep 静态门禁（规范 §7.8 的第 5/6/7/9 条与末段附加检查）。第 5/6/7/9 条必须无输出、
# 第 8 条必须**有**输出——两条 grep 的期望相反是刻意的：前者拦手写常量时间折叠与
# 测试注入缝泄漏进生产入口，后者钉住比较确实落在 subtle 上。
# 本目标只读源码与 git（暂存区 + 已跟踪清单），不编译、不联网、不需要前端产物，可被 CI 单独执行。
# 秘密/配置实体检查查**两遍**：一遍 `git diff --cached --name-only`（暂存区，拦本地提交），
# 一遍 `git ls-files`（已跟踪清单，拦已经进仓库的东西）。CI 是干净检出，暂存区永远为空，
# 只查暂存区的门禁在 CI 里等于空转（这就是它此前形同虚设的原因）；已跟踪扫描让同一条规则
# 在没有 index 变更的流水线里仍然成立。本地提交前两者都查，任一命中即失败。
# Fail-Closed（F6）：每条负向门禁都显式检查 grep 的退出码——退出码 >1（目录缺失、
# 表达式非法等工具自身出错）与 git 命令失败一律直接报错退出，绝不因命令替换拿到
# 空输出而静默放行。第 5 条的模式串刻意写成 shell 拆分形式 'TO''DO|T''BD|FIX''ME'，
# 这样把命令写进文档不会自指命中；三个占位词全部拆分，其中 T''BD 修复旧模式
# 'T-B-D' 匹配不到真实 TBD 占位符的失配（模式不完整）。
.PHONY: metrics-static-gates
metrics-static-gates:
	@rc=0; hits=$$(grep -rnE 'TO''DO|T''BD|FIX''ME' crates/rsetup-controller/src/metrics) || rc=$$?; \
	 if [ $$rc -gt 1 ]; then echo "metrics 门禁 5：grep 异常退出码 $$rc（目录缺失或表达式非法），Fail-Closed" >&2; exit 1; fi; \
	 if [ -n "$$hits" ]; then echo "metrics 门禁 5：src/metrics 下存在 TODO/TBD/FIXME 占位符" >&2; printf '%s\n' "$$hits" >&2; exit 1; fi
	@rc=0; hits=$$(grep -rn 'build_production_router_with_metrics_source\|SqlxMetricsSnapshotSource::new' crates/rsetup-controller/src/main.rs) || rc=$$?; \
	 if [ $$rc -gt 1 ]; then echo "metrics 门禁 6：grep 异常退出码 $$rc（main.rs 缺失），Fail-Closed" >&2; exit 1; fi; \
	 if [ -n "$$hits" ]; then echo "metrics 门禁 6：生产入口不得触碰测试注入缝或自建汇总源" >&2; printf '%s\n' "$$hits" >&2; exit 1; fi
	@rc=0; hits=$$(grep -rn 'diff |= a\[i\]' crates/rsetup-controller/src/metrics) || rc=$$?; \
	 if [ $$rc -gt 1 ]; then echo "metrics 门禁 7：grep 异常退出码 $$rc（目录缺失或表达式非法），Fail-Closed" >&2; exit 1; fi; \
	 if [ -n "$$hits" ]; then echo "metrics 门禁 7：指标线不得手写常量时间折叠" >&2; printf '%s\n' "$$hits" >&2; exit 1; fi
	@rc=0; hits=$$(grep -rn 'subtle::ConstantTimeEq\|use subtle::ConstantTimeEq' crates/rsetup-controller/src/metrics/auth.rs) || rc=$$?; \
	 if [ $$rc -gt 1 ]; then echo "metrics 门禁 8：grep 异常退出码 $$rc（auth.rs 缺失或表达式非法），Fail-Closed" >&2; exit 1; fi; \
	 if [ $$rc -ne 0 ] || [ -z "$$hits" ]; then echo "metrics 门禁 8：常量时间比较必须落在 subtle 上" >&2; exit 1; fi
	@rc=0; hits=$$(grep -nE 'SCRAPE_TIMEOUT.*\?' crates/rsetup-controller/src/metrics/collect.rs) || rc=$$?; \
	 if [ $$rc -gt 1 ]; then echo "metrics 门禁 9：grep 异常退出码 $$rc（collect.rs 缺失或表达式非法），Fail-Closed" >&2; exit 1; fi; \
	 if [ -n "$$hits" ]; then echo "metrics 门禁 9：连接借出后禁止 ? 早退（F1/I7）" >&2; printf '%s\n' "$$hits" >&2; exit 1; fi
	@rc=0; staged=$$(git diff --cached --name-only) || rc=$$?; \
	 if [ $$rc -ne 0 ]; then echo "metrics 门禁（§7.8 末段附加检查）：git diff --cached 失败退出码 $$rc，Fail-Closed" >&2; exit 1; fi; \
	 rc=0; matched=$$(printf '%s\n' "$$staged" | grep -E 'controller\.toml|^secret/') || rc=$$?; \
	 if [ $$rc -gt 1 ]; then echo "metrics 门禁（§7.8 末段附加检查）：grep 异常退出码 $$rc，Fail-Closed" >&2; exit 1; fi; \
	 if [ $$rc -eq 0 ]; then echo "metrics 门禁（§7.8 末段附加检查）：暂存区含配置文件实体或凭据目录" >&2; printf '%s\n' "$$matched" >&2; exit 1; fi
	@rc=0; tracked=$$(git ls-files) || rc=$$?; \
	 if [ $$rc -ne 0 ]; then echo "metrics 门禁（§7.8 末段附加检查）：git ls-files 失败退出码 $$rc，Fail-Closed" >&2; exit 1; fi; \
	 rc=0; tracked_matched=$$(printf '%s\n' "$$tracked" | grep -E '(^|/)controller\.toml$$|^secret/') || rc=$$?; \
	 if [ $$rc -gt 1 ]; then echo "metrics 门禁（§7.8 末段附加检查）：已跟踪清单 grep 异常退出码 $$rc，Fail-Closed" >&2; exit 1; fi; \
	 if [ $$rc -eq 0 ]; then echo "metrics 门禁（§7.8 末段附加检查）：仓库已跟踪文件含配置文件实体或凭据目录" >&2; printf '%s\n' "$$tracked_matched" >&2; exit 1; fi
	@echo "metrics-static-gates：五条 grep 静态门禁全部符合预期（第 8 条已确认有输出）"

# 用已安装的 1.85.0 工具链跑 workspace 测试（MSRV 验证）。与 rust-msrv CI 作业对应：
# 三个二进制都按 rustup 路径解析并断言实际版本；dist 已入库，本目标只做存在性检查。
msrv-check: web-dist-check
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
