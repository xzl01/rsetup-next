# Git 提交信息规范

- 本项目后续所有 Git commit message 的标题与正文均使用简体中文，包括普通提交、合并提交和 revert 提交。
- 代码标识符、文件名、API 名称及必要的专业术语可保留英文；若使用 Conventional Commits，类型和作用域标记（如 `feat(controller):`）可保留英文，其后的描述须使用中文。
- 此规则适用于本仓库及其所有开发 worktree，由主代理和子代理共同遵守；实施计划中的英文提交信息示例不构成例外。
- 不为应用此规则重写已有提交历史，也不因此自动提交或推送。

# 本地开发授权与凭据保护

- `secret/` 只存放本地开发授权资料。当前人工授权仅限 `secret/mysql.json` 与 `secret/tidb.json` 此次指定的两个专用开发库；文件名不代表其中未来变更的内容或 `secret/` 其他文件自动获授权。使用前仍须确认实际目标是获授权、隔离且可丢弃的开发库，并具备对应写权限；不得扩展到其他目标或生产环境。拥有凭据不等于获得任意操作许可。
- 在上述两个已确认的专用开发库内，允许用于测试的表级建改删、迁移及可丢弃数据重置；绝不执行 `DROP DATABASE` 或以其他方式删除数据库本身。不能伪造真实备份、备份引用或确认记录；未满足真实备份等现有门禁条件时，不运行需要这些条件的迁移。
- 不得以任何形式提交或推送真实凭据：包括原文、复制件、编码或 Base64、派生配置，以及日志、报告、截图、测试 fixture 等载体。严禁使用 `git add -f` 绕过忽略规则。
- 不得将真实凭据回显到聊天、工具输出或普通日志；不得将 `secret/` 的内容写死在代码、命令参数或未被忽略的报告中。程序应从本地文件在进程内载入所需内容，禁止开启 shell tracing，禁止直接 `source` 不可信脚本。
- 开发 worktree 可以按用户授权读取主工作区原有 `secret/` 的绝对路径，无需复制秘密文件；不要在代码或文档中硬编码敏感服务地址或凭据路径的具体值。
- Git ignore 不影响已跟踪文件。提交前检查 tracked 文件、index 与 diff 是否包含真实秘密；发现已跟踪秘密时立即停止并报告，按受控流程清理及轮换，不擅自重写历史。
- 所有测试报告只保留去敏结果，不泄露真实值。不得因测试需要而将真实凭据变成 fixture、附件或其他可提交产物。

# Rust 格式检查口径

- 本仓的 Rust 格式门禁是 `make fmt-check`：它按 Makefile 中显式列出的 `FMT_PACKAGES` 逐包执行 `cargo fmt --package ... -- --check`，并先比对 `Cargo.toml` 的 members 数量与 `FMT_PACKAGES` 条目数；新增 workspace 成员时必须同步补全 `FMT_PACKAGES`，否则该目标直接报错。`make lint` 等于 `fmt-check` 加 `cargo clippy --workspace --all-targets -- -D warnings`，CI 执行的也是 `make fmt-check`。
- 不要直接用 `cargo fmt --all -- --check` 判定本仓格式是否合格：`--all` 会连本地 path 依赖一并格式化，把经 `[workspace.dependencies]` 引入的 `externals/devicesinfo` 这个钉住上游 commit 的第三方 submodule 纳入范围，其目录内的格式差异与本仓代码无关（`crates/` 下零差异，差异数量随上游 submodule 的钉住 commit 变化），CI 也不执行该命令。
- 不要试图用根目录 `rustfmt.toml` 的 `ignore` 豁免该目录：`ignore` 是 rustfmt 的 nightly-only 选项，本仓 CI 与本地均为 stable 工具链，该配置会被解析后静默丢弃并刷出大量 unstable 告警，格式化范围与退出码均不变。
- 不要为消除该差异去格式化 `externals/devicesinfo/` 内的文件：submodule 的改动落在 detached HEAD 上，无法随主仓提交，只会让主仓长期显示 `modified: externals/devicesinfo (modified content)`，其代码风格由上游仓库负责。
- 遇到 `cargo fmt --all` 报红时，应先确认差异是否落在 `externals/devicesinfo` 内；落在该目录内的差异不是本仓的格式缺陷，不要把它当成本仓问题去“修”。
