# CI 40109395224 本地修复记录

## 范围与证据口径

- 目标运行：[CI 40109395224](https://github.com/xzl01/rsetup-next/actions/runs/40109395224)。该运行对应提交 `fcfd9c301c0de2e8086dccbcd56bb4733db06686`。
- 通过本地 `gh run view` 查询作业，再通过 `gh api repos/xzl01/rsetup-next/actions/jobs/<job-id>/logs` 获取原始日志。国外网络调用使用用户指定代理；本记录不包含凭据。
- 本文记录本地修复和验证，不将历史失败运行称为已变绿。未重新触发远端流水线，未自动 commit/push。
- 不修改任务业务行为、隧道密码学、数据库版本或真实 reboot 边界，不运行真实数据库写入与硬件操作。

## 根因与最小修复

| 作业 | 真实失败 | 修复 |
| --- | --- | --- |
| desktop-tauri-ipc | `pick_handle_random()` 中嵌套 `if let` / `if` 触发 Clippy `collapsible_if`，`-D warnings` 导致退出 101 | 按 Clippy 提示合并 let-chain；随机源成功/失败与回退路径不变，不抑制 lint |
| rust-and-ui | `inbound_event_burst_is_dropped_with_counter` 的 fake 在 Tokio 任务内用 `oneshot::Receiver::blocking_recv()`，必然触发 runtime panic；另外 64 条事件超过发送业务队列，造成无关的 `Busy` | fake 改用既有同步 Condvar Gate；用回调进入许可及有期限等待代替猜测；发送量不超过发送队列；以同流 echo 回包作接收处理屏障，精确断言丢弃数为 6 |
| debian-package arm64 | Trixie apt 提供 Rust 1.85.1，而源码使用较新 stable 功能；日志有 27 条 E0658，包括 let-chain / `slice::as_chunks` | 在 checkout 后、离线缓存预热与打包前安装 `dtolnay/rust-toolchain@stable`，与其他 Rust 作业一致；保留 Trixie、apt Build-Depends、离线缓存、静态链接与维护脚本门禁 |
| debian-package amd64 | 因 arm64 失败被矩阵取消，仅有 `context canceled` | 由共享 job 配置覆盖工具链修复；不能将被取消作业算作成功 |
| debian-12-runtime | 依赖打包失败而未运行 | 不修改或跳过此验收；仍需远端完整流水线复验 |

不能将“rsetup-core 使用的功能自 1.88 稳定”误写成“整个 workspace 的 MSRV 为 1.88”；其他源码还使用更新的标准库 API。本次不新增或降低 MSRV 声明。

### 压力测试额外发现

同一测试目标内的 `stuck_callback_exits_bounded_and_reports_cleaned_honestly` 使用固定 sleep、吞掉发送 `Busy`，不能证明接收通知队列已经被填满；单核全量并行下复现 `dropped_events == 0`。

修复仍仅限测试：等待回调实际进入，收紧接收队列容量，严格发送 5 条事件，通过同流 echo 回包屏障后再 abort；保留原退出期限、`cleaned == false`、回调追踪回收检查，并增加精确丢弃数 2 的断言。未修改生产 driver。

### echo 屏障的方向

事件从 board 发往 controller。测试由 controller 发出 echo 请求，其 **board 回包** 与前序事件进入 board 同一个 Data 流业务发送队列；回包经过同一半流 FIFO 到达 controller，controller 唯一 reader/owner 处理该 Response 后才唤醒请求等待者。因此回包到手可以证明前序事件已经经过接收 owner 的丢弃计数分支；不需要把请求方向反转。

## 本地验证

主代理在本次修复期间运行：

| 命令/范围 | 结果与边界 |
| --- | --- |
| `cargo test --offline --locked --workspace` | 2524 passed、0 failed、40 ignored；ignored 不计作完成 |
| `cargo clippy --offline --locked --workspace --all-targets -- -D warnings` | 退出 0 |
| `make fmt-check` | 退出 0，仓库正式格式门禁 |
| `cargo fmt --manifest-path apps/desktop/src-tauri/Cargo.toml -- --check` | 退出 0，桌面独立 workspace 格式 |
| `node --test scripts/tests/*.test.mjs` | 71 passed，包含新增 Debian stable 安装顺序合同；保留全部既有门禁 |
| `npm --prefix apps/controller-web test -- --run` | 736 passed |
| `npm --prefix apps/controller-web run typecheck` | 退出 0 |
| `node --test ui/*.test.mjs` | 102 passed |
| `cargo test --offline --locked -p rsetup-controller --test web_assets` | 88 passed，使用现有真实前端 dist |
| `cargo test --locked --manifest-path externals/devicesinfo/Cargo.toml -p deviceinfo` | 173 passed，含 doc tests；缺失依赖通过指定代理下载 |
| `make metrics-static-gates` | 退出 0 |
| `python3 scripts/check_controller_acceptance.py docs/testing/controller-v1-acceptance.md` | 格式检查退出 0，不提升任何验收状态 |
| `python3 -m unittest discover -s scripts/tests -p '*acceptance*.py'` | 28 passed |
| `cargo test --offline --locked -p rsetup-core native_fdtoverlay_validation_and_apply -- --ignored` | 1 passed，只操作隔离 fixture |
| `cargo test --offline --locked -p rsetup-core curl_benchmark_fixture -- --ignored` | 1 passed，只操作本地隔离 HTTP fixture |

隧道修复代理还执行定向 RED/GREEN、单核与多核压力验证：原 `inbound_event_burst` 在修改前单核可稳定复现；修复后保留 33 个 session_driver 用例并反复验证；额外 `stuck_callback` flake 在修改前单核并行第 7 轮复现，修改后同条件及定向复跑通过。没有通过忽略测试、只接受 `Busy` 或延长 sleep 来掩盖错误。

## 真实 Debian 静态编译补验

主代理使用现有 Debian 13 构建镜像，挂载真实 stable Rust 1.99.1，保持 `CARGO_HOME=/build/debian/cargo-home`，源代码与 registry 只读、网络关闭、输出仅写工作区 target。按 `debian/rules` 的 `cargo rustc --release --locked --offline -- -C target-feature=+crt-static` 分别编译六个二进制，全部退出 0：

- rsetup-next
- rsetup-next-helper
- rsetup-controller
- rsetup-controller-migrate
- rsetup-admin
- rsetup-board-agent

六个结果均通过 `readelf -d` 的无 `NEEDED` 依赖断言。此验证使用真实 Debian 编译/链接工具与真实 Rust 工具链，不使用空系统库或链接桩。

## 独立审查

新的 `glm-coding/glm-5.3-flash high` 只读代理复核最终差异，结论 **PASS，无 P1/P2**；已独立核实 echo 回包 FIFO 屏障方向与容量计算、工具链安装顺序以及验证边界。

一项非阻塞 P3 建议：新增配置合同中检查 apt 清单的正则覆盖范围较宽，后续可限定在单一 install 命令块，避免后续 `cargo fetch` 字样意外满足“apt 安装 cargo”的断言。本次真实安装清单仍逐字保留 `cargo` / `rustc`，未改变实际依赖安装行为。

最终完整工作区测试再次取得 2524 passed / 0 failed / 40 ignored。随后 Clippy 发现新测试的一处多余 u8 转型，已删除；重新执行 workspace Clippy、session_driver 33 项、仓库/桌面格式、diff 空白检查与脚本 71 项，全部退出 0。

## 未执行与不可外推范围

- 本机缺少真实 GTK/WebKit/DBus 开发库，未完成与 GitHub 相同依赖环境的 desktop Clippy/test 全链重跑。代理使用临时 pkg-config / 共享库桩的结果 **不作为正式构建或 CI 等价通过证据**；这些桩仅在忽略的 target 内，不进入源码或工作流。
- 未完整重跑两架构 `dpkg-buildpackage`、Lintian、维护脚本产物断言和 Debian 12 安装运行验收；amd64 的真实静态编译不替代 arm64 打包验收。
- stable 的 GitHub Actions 安装与 PATH 注入仍需真实远端运行验证；本地静态编译补验不宣称执行了 GitHub action 本身。
- 用户禁止未经授权 push，因此不推送或借重跑旧提交冒充修复版 CI 结果。
