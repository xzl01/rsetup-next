PROJECT := rsetup-next
CARGO_HOME ?= $(CURDIR)/debian/cargo-home
DESKTOP_MANIFEST := apps/desktop/src-tauri/Cargo.toml
DESKTOP_TARGET_DIR := target/desktop-ci

# The controller embeds its web UI at compile time: crates/rsetup-controller/build.rs
# generates the asset table that web.rs include!s, and it panics when
# apps/controller-web/dist is missing. That dist is a **build product, not a
# committed artifact**: vite generates it on every build, the file names under
# dist/assets/ carry a content hash, and the repository carries only the sources
# under apps/controller-web (the whole dist/ directory is git-ignored). Nothing in
# the tree therefore hardcodes a generated file name or hash.
# `build`, `test`, `lint`, `metrics-gates` and `controller-test-offline` all compile
# rsetup-controller, so they depend on web-dist-check: when dist/index.html is
# missing it runs `$(MAKE) web-build` for you, and when it is present it only checks
# existence. A clean checkout thus needs Node 22 + npm exactly once — the first
# cargo-facing target pays for the npm install and the vite build — while a warm tree
# re-running `make build` never re-invokes npm. The Debian package build and CI build
# the frontend explicitly for the same reason: debian/rules runs
# `npm --prefix apps/controller-web ci` and `npm --prefix apps/controller-web run build`
# before cargo, and CI does the same before it compiles the controller.
# The explicit rebuild entry point is `make web-build`, backed by two stamp files:
#   WEB_DEPS_STAMP — node_modules/.rsetup-npm-ci-stamp, the locked dependency
#     install. Its only prerequisites are package.json and package-lock.json, so
#     editing a component never re-runs `npm ci` (that used to cost a full
#     reinstall on every frontend change, because the single stamp also listed
#     the whole src tree as a prerequisite).
#   WEB_STAMP — dist/.rsetup-build-stamp, the `vite build` output, rebuilt when a
#     dist input changes. Both stamps live inside git-ignored paths (node_modules/
#     and dist/). NPM may be overridden with a pinned node wrapper, a mirror wrapper
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

# Rust 工具链口径：统一使用当前 stable（本地验证 rustc 1.99.1 / cargo 1.99.1），
# 不再单独维护最低版本门禁，也不固定某个具体小版本。

.DEFAULT_GOAL := build

.PHONY: build test lint fmt-check run tui serve deb-prepare deb clean desktop-test desktop-check metrics-gates metrics-static-gates web-build web-assets web-dist-check

# `cargo fmt --all` also formats local path dependencies, which would drag in
# the pinned deviceinfo submodule. Format the workspace members explicitly and
# fail loudly when Cargo.toml gains a member that is missing from this list.
# `rsetup-desktop` is deliberately absent: it lives in apps/desktop/src-tauri,
# which Cargo.toml keeps in `workspace.exclude`, so it is not a workspace member
# and `cargo fmt --package rsetup-desktop` would not resolve. It is covered by
# the desktop-test/desktop-check targets below instead, which go through
# --manifest-path, and by the desktop-tauri-ipc CI job.
FMT_PACKAGES := rsetup-core rsetup-next rsetup-controller rsetup-protocol rsetup-board-agent rsetup-tunnel

build: web-dist-check
	cargo build --workspace --locked

test: web-dist-check
	cargo test --workspace --locked
	node --test ui/*.test.mjs

# 前端产物门禁（dist 是生成物，不入库）：`rsetup-controller` 的 build.rs 在编译期读取
# apps/controller-web/dist 并生成 web.rs include! 的资源表，dist 缺失即 panic。
# dist 由 vite 现生成，文件名带内容 hash，仓库里只有 apps/controller-web 下的源码，
# 所以本目标承担「编译前产物必须在位」这一条：
#   dist/index.html 不存在 → 先 `$(MAKE) web-build`（npm ci + npm run build）把它构建出来；
#   dist/index.html 已存在 → 只打印一行确认，绝不重复跑 npm。
# 于是 build/test/lint/metrics-gates/controller-test-offline 在任何检出状态下都能直接
# 编译：干净检出的第一条 cargo 命令付一次前端构建，热工作区再跑 make 不会被 npm 拖慢。
# 改过 apps/controller-web 源码后想立刻看到效果，直接 `make web-build` 强制重建即可。
.PHONY: web-dist-check
web-dist-check:
	@if test -f $(WEB_DIST_INDEX); then \
	  echo "web-dist-check: 已检测到 $(WEB_DIST_INDEX)，不重复构建前端"; \
	else \
	  echo "web-dist-check: 缺少 $(WEB_DIST_INDEX)（dist 是不入库的构建产物），将自动构建前端…" >&2; \
	  $(MAKE) web-build; \
	fi
	@test -f $(WEB_DIST_INDEX) || { echo "web-dist-check: 自动构建后仍缺少 $(WEB_DIST_INDEX)；请手工运行 make web-build 查看前端构建错误" >&2; exit 1; }

# 显式的前端重建入口：装依赖 + vite build + 刷新 stamp。产物是 dist/ 下的生成物，
# 已被 .gitignore 忽略，任何时候都不需要提交。
.PHONY: web-build
web-build: web-assets

# 两级 stamp 的显式重建链路：
#   $(WEB_DEPS_STAMP) ← package.json/package-lock.json，配方 `npm ci $(NPM_CI_FLAGS)`；
#   $(WEB_STAMP)      ← $(WEB_DEPS_STAMP) + index.html/vite.config.ts/tsconfig*.json/
#                       src 下非测试文件/src 目录本身/public（存在时），配方只 `npm run build`。
# 依赖未变化时 make 两级都不重跑，改一个组件不会再触发一次 npm ci。
# 本目标需要 Node 22、npm（干净检出首次执行还需网络；本机 npm cache 已预热时可
# `make web-build NPM_CI_FLAGS=--offline`）。两个 stamp 文件与 dist/ 本身都被 .gitignore
# 忽略，所以手工准备 node_modules 与 dist 后 touch 两个 stamp 仍可让 make 认为已满足；
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
# dist 必须先存在，Rust 资源测试才能拿到预期源。dist 是生成物且不入库，所以本目标
# 先决于 web-dist-check——产物不在位时它会先 `$(MAKE) web-build` 把前端构建出来，
# 已在位时只确认一次存在性；随后再跑前端自身的两条门禁（test/typecheck）。
.PHONY: controller-test-offline
controller-test-offline: web-dist-check
	npm --prefix apps/controller-web test -- --run
	npm --prefix apps/controller-web run typecheck
	cargo test -p rsetup-controller --test web_assets --locked
	$(MAKE) test

# clippy --workspace 会编译 rsetup-controller，因此同样先决于 dist；
# 命令本身不变，只补先决条件，干净检出直接 `make lint` 不再撞上 build.rs 的 panic
# （dist 缺失时由 web-dist-check 先自动构建）。
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
# rsetup-controller，缺前端产物即在 build.rs 失败；该先决条件在 dist 缺失时会先自动
# 构建前端（build 产物不入库，见文件头），已存在则只做存在性检查。
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
	@rc=0; filelist=$$(find crates/rsetup-controller/src/main.rs crates/rsetup-controller/src/main \
		 -type f -name '*.rs' 2>/dev/null); rc=$$?; \
	 if [ $$rc -gt 1 ]; then echo "metrics 门禁 6：源文件枚举异常退出码 $$rc，Fail-Closed" >&2; exit 1; fi; \
	 if [ -z "$$filelist" ]; then echo "metrics 门禁 6：未找到 crates/rsetup-controller/src/main.rs，Fail-Closed" >&2; exit 1; fi; \
	 files=$$(printf '%s\n' "$$filelist" | sort); \
	 rc=0; hits=$$(grep -n 'build_production_router_with_metrics_source\|SqlxMetricsSnapshotSource::new' $$files) || rc=$$?; \
	 if [ $$rc -gt 1 ]; then echo "metrics 门禁 6：grep 异常退出码 $$rc，Fail-Closed" >&2; exit 1; fi; \
	 if [ -n "$$hits" ]; then echo "metrics 门禁 6：生产入口不得触碰测试注入缝或自建汇总源" >&2; printf '%s\n' "$$hits" >&2; exit 1; fi; \
	 rc=0; anchors=$$(grep -n 'build_production_router(' crates/rsetup-controller/src/main.rs) || rc=$$?; \
	 if [ $$rc -gt 1 ]; then echo "metrics 门禁 6：装配锚点 grep 异常退出码 $$rc，Fail-Closed" >&2; exit 1; fi; \
	 if [ $$rc -ne 0 ] || [ -z "$$anchors" ]; then echo "metrics 门禁 6：薄根 main.rs 必须保留 build_production_router( 装配点" >&2; exit 1; fi
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

run:
	cargo run -p $(PROJECT) -- status

tui:
	cargo run -p $(PROJECT) -- tui

serve:
	cargo run -p $(PROJECT) -- serve

# 板端测试代理与双端本地联调的便捷入口。
# board-agent/board-agent-once 对应 crates/rsetup-board-agent 的独立二进制
# src/bin/rsetup-board-agent.rs：前者产出可直接拷到板卡上运行的产物，后者只跑
# 一轮状态采集即退出，用于板端冒烟。
# controller-migrate 是中控数据库迁移 CLI（bin 名 rsetup-controller-migrate，源文件
# src/bin/migrate-controller.rs），与服务主二进制分离；controller-bin 编译中控主
# 二进制，它同样在编译期读取
# apps/controller-web/dist，因此与 build/test/lint 等入口目标一致，先决于
# web-dist-check（dist 是不入库的生成物：缺失时该先决条件会自动构建前端，已存在时只
# 做存在性检查）。
# dual-test 运行本地主控与板端的联合连通性与功能测试脚本。
.PHONY: board-agent board-agent-once controller-migrate controller-bin dual-test
board-agent:
	cargo build -p rsetup-board-agent --bin rsetup-board-agent

board-agent-once:
	cargo run -p rsetup-board-agent --bin rsetup-board-agent -- --once

controller-migrate:
	cargo build -p rsetup-controller --bin rsetup-controller-migrate

controller-bin: web-dist-check
	cargo build -p rsetup-controller --bin rsetup-controller

dual-test:
	python3 scripts/test_local_dual_run.py

# The desktop crate is workspace-excluded, so `--package` cannot select it; go
# through its own manifest instead. These mirror the desktop-tauri-ipc CI job.
desktop-test:
	CARGO_TARGET_DIR=$(DESKTOP_TARGET_DIR) cargo test --locked --manifest-path $(DESKTOP_MANIFEST)

desktop-check:
	cargo fmt --manifest-path $(DESKTOP_MANIFEST) -- --check
	CARGO_TARGET_DIR=$(DESKTOP_TARGET_DIR) cargo clippy --locked --manifest-path $(DESKTOP_MANIFEST) --all-targets -- -D warnings

# Debian package builds keep cargo network-isolated: populate the package-local Cargo
# cache first, then let dpkg-buildpackage enforce offline mode. The frontend is a
# separate prerequisite the package itself satisfies — debian/rules runs
# `npm --prefix apps/controller-web ci` and `run build` before cargo — so the build
# machine also needs npm with a reachable (or pre-warmed, see NPM_CI_FLAGS) npm cache.
deb-prepare:
	CARGO_HOME=$(CARGO_HOME) cargo fetch --locked

deb: deb-prepare
	dpkg-buildpackage --build=binary --no-sign

clean:
	cargo clean
	dh clean --buildsystem=none
