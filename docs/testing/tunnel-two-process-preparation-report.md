# Task 7：真实两进程隧道验收**准备**报告

- 日期：2026-10-24（v1 准备轮 + v2 评审修复轮 §12 + v3 执行前阻断修复轮 §14）
- 范围：`scripts/tunnel_two_process.py`、`scripts/tests/test_tunnel_two_process.py`、本报告
- 状态：**准备 + 评审修复完成（脚本 + 离线自测）**。两进程验收本身**未执行**——不启动真实二进制、不连数据库、不访问网络；"准备完成"**不等于**两进程 PASS。
- 修订说明：v1（§2–§11）把三条**假绿**口径写成了合同（status 拿不到不判失败、proof claim 可被单层 true 满足、`complete=true` 允许在异常/写失败路径上出现）。这三条已由独立脚本独审判为 3 Major，修复已落地在脚本与测试里；本文凡属旧口径的句子都就地标注 **【historical，已由 §12 纠正】**，不删除原始错误表述，以免抹掉失败来源。

## 0. 结论

| 事项 | 结果 |
| --- | --- |
| 脚本就绪 | 是：真实 `rsetup-controller` + `rsetup-board-agent --connect` 全链路阶段与门禁已实现 |
| 离线自测 | **104 个单元测试** + 脚本 `--selftest`（15 项检查、16 行输出）全部通过，退出码 0（零网络、零数据库、零真实进程；实跑见 §8） |
| 独审 3 Major | **已修复并各带用例**：M1 typed status 必证、M2 proof/manifest 两层 claim 各自显式 true、M3 异常/中断/写失败路径不留 `complete=true`（逐条见 §12）；本轮未重写它们，也未做新一轮独审 |
| 执行前依赖 | §13 两处**输入合同**（库名连字符、Redactor 登记 `host/user/database`）；v3 §14 两处执行前**阻断**各带 2 例（`Credential.user` 依真实语法事实允许合法连字符、openssl PKCS#8 v1→ring v2 固定 shape 重排，§14.2 的真实 rlib 解析探针仍待主代理跑）；真实 proof 的 `restore_drill:false` 核对结论是"如实接受，不要求伪填 true"（§13.3） |
| 两进程验收 | **未执行**，本报告不声称 Pending/approve/Ready/revoke 任何真实证据已拿到 |
| 备份恢复演练 | **未做**，脚本如实以 `restore_drill=not_performed` 表达，不会抬成"已验证可恢复" |
| 前次遗留 | 原任务被误读成"JSON5 输入"，已收缩为标准 JSON 并同步改测试（见 §2） |

## 1. 改动清单

| 文件 | 行数（当前） | 性质 |
| --- | --- | --- |
| `scripts/tunnel_two_process.py` | 1930 | v1：按已核对的真实 API/合同收缩与纠正（删 160 行自制 JSON5 解释器，新增真实备份绑定与逐项 CAS 判定）。v2：独审 3 Major + 5 项次要的修复（M1 typed status 分类、M2 两层 claim 复核、M3 异常/中断/写失败收口、`Popen` fd 关闭、argparse/顶层固定错误码）；另加 §13 的输入合同最小修（`DATABASE_NAME_RE` 允许合法连字符并改 `fullmatch`、`Redactor.add_identifier`）。v3：§14 两处执行前阻断——独立 `USER_NAME_RE` 允许 user 合法连字符、`ed25519_pkcs8_v1_to_v2` 把 openssl v1 定长重排为 ring 接受的 v2。体量上升全部来自安全门禁，不是功能扩张 |
| `scripts/tests/test_tunnel_two_process.py` | 1557 | v1：68 个离线用例，临时目录从 `/tmp` 纠正到 `target/` 下。v2：100 个用例（+32：锁 M1/M2/M3 与独审次要项，另 6 例覆盖 §13 的两处输入合同修复）。v3：+4 例覆盖 §14 两处阻断（`user` 连字符 RED/GREEN + 非法形状对向、PKCS#8 v1→v2 逐字节一致 + 端到端） |
| `docs/testing/tunnel-two-process-preparation-report.md` | 本文件 | 设计口径、核对依据、离线证据、修复记录、执行前置条件 |

合成测试与探针产物只落在 `target/tunnel-two-process-prep/`（gitignored）。未触碰 Rust/Cargo/其它文件；未提交、未推送、未 clean；`secret/` 未读取、未复制、未出现在代码或本报告；未启动真实二进制、未连数据库、未访问网络、未生成 openssl key、未改动任何凭据或 proof。

## 2. 收缩：删除自制 JSON5 解释器

**原状态（根因）**：输入合同被读成"JSON5"，于是脚本自带 `_Json5` 递归下降解析器（约 160 行，支持 `//`/`/* */` 注释与尾随逗号），并有测试 `test_accepts_comments_and_trailing_commas` 断言这些非标准写法**必须通过**。同时 `Credential` 要求 `{"database": {...}, "admin": {...}}` 嵌套形状，与实际已授权 JSON 的**五个平铺字段**不符。

**现在的合同**：

- `load_json5` 已删除，改名 `load_json_document`＝`json.loads` + `object_pairs_hook` 重复键拒绝 + `parse_constant` 拒绝 `NaN`/`Infinity`；文件按 UTF-8 **严格解码**（坏字节 → `INPUT_NOT_UTF8` 类固定码，不 `replace`）。
- 注释、尾随逗号、单引号、裸键、重复键、非有限常量**全部拒绝**（标准 JSON 本就没有它们）；不再自带解释器。
- CLI 描述/help 与模块文档只说"标准 JSON"，不再出现 JSON5 字样。
- `Credential` 只接受 `host`/`port`/`user`/`password`/`database` 五个平铺字段（`port` 缺省 3306、`password` 可为空串），**已授权原文件不需任何变换**。

**测试变更及理由**：

| 变更 | 理由 |
| --- | --- |
| 删除 `Json5SubsetTests.test_accepts_comments_and_trailing_commas` | 该用例把误读固化成合同（断言注释/尾随逗号被接受） |
| 新增 `JsonDocumentTests.test_rejects_json5_forms` | 反向断言：7 种非标准写法一律 `INPUT_BAD_JSON` |
| 新增 `test_rejects_duplicate_keys_and_non_finite` | 重复键会静默覆盖秘密字段；`NaN` 不是合法 JSON 值 |
| `synthetic_credential()` 改五平铺字段；新增 `test_nested_database_object_is_rejected` | 与实际已授权文件一致，并锁死"不再兼容自造嵌套形状" |
| `test_credential_file_never_carries_admin` | 管理员凭据不得混进目标库 JSON（见 §3） |

## 3. 凭据与管理员来源合同

| 输入 | 字段 | 权限 | 失败码 |
| --- | --- | --- | --- |
| `--credential-file` | `host`,`port`,`user`,`password`,`database`（平铺，五个） | 0600、非链接、属主 euid | `CREDENTIAL_UNKNOWN_FIELD` / `CREDENTIAL_FIELD_MISSING` / `INPUT_FILE_MODE` |
| `--admin-file`（**独立显式私有文件**） | `username`，可选 `password`，可选 `new_password` | 0600 | `ADMIN_UNKNOWN_FIELD` / `ADMIN_FIELD_MISSING` |
| bootstrap sink（**controller 自己写出的**私有文件） | `<state-dir>/controller-bootstrap-secret.log`，一行 `admin: <password>` | 目录 0700、文件 0600、脚本按 `private_file()` 复验 | `BOOTSTRAP_SECRET_UNAVAILABLE` |
| `--state-dir` | 绝对路径、恰 0700；在仓库内必须 gitignored | — | `STATE_DIR_MODE` / `STATE_DIR_NOT_GITIGNORED` / `SYMLINK_NOT_ALLOWED` |

- 管理员来源未知（无 `--admin-file`）且要求 `--execute`：精确 **`NEEDS_CONTEXT`** 并拒绝执行。脚本**从不** `UPDATE users` 改口令、不重置既有 admin、不伪造 session。
- `--admin-file` 不给 `password` 时，走**真实首启 bootstrap 路径**：controller 在 schema 门禁之后调用 `bootstrap_admin`，用 `create_new`+0600 写出 sink（`crates/rsetup-controller/src/main.rs:260`、`crates/rsetup-controller/src/bootstrap.rs:33`，插入的 admin 带 `must_change_password=TRUE`）。脚本等到该文件出现、复验 0600/非链接后取用一次并登记进脱敏器。
- 真实登录回 `must_change_password=true` 而未给 `new_password`：`ADMIN_PASSWORD_CHANGE_REQUIRED` 失败关闭（绝不静默跳过、绝不伪造"已改密"）。

## 4. 备份门禁模型：真实验证 vs 如实登记

原 `BackupManifest` 要求 `engine/target_database/backup_file/sha256/size_bytes/restore_verified` 六个字段，其中 `restore_verified` 只能由人**伪填 true** 才能过——这正是不允许的做法。现改成"能验的真验、不能验的如实登记"：

| 维度 | 是否真实验证 | 证据 |
| --- | --- | --- |
| 完整性（gz 摘要） | ✅ | 一遍流式 `sha256(backup_path) == sha256_gz` |
| 完整性（gzip CRC + 解压流摘要） | ✅（manifest 声明 `sha256_raw` 时） | `zlib.decompressobj(GZIP_WBITS)` 逐块解压，trailer CRC 错 → `BACKUP_GZIP_CORRUPT`；raw 摘要不符 → `BACKUP_RAW_HASH_MISMATCH` |
| 引用卫生 | ✅ | 绝对路径、非符号链接（含祖先）、常规文件、group/other 可读位必须为 0 |
| 目标绑定 | ✅ | `target_database` 与凭据库名**字符串相等**（绑定的是"库相同"） |
| proof 绑定 | ✅ | 重算 `source_proof.path` 的 sha256 并复核其中的 `sha256_gz`/`target_database`/claims；改旧 manifest 或换 proof 都过不了 |
| `authoritative`/`consistent_snapshot` | ⚠️ 只如实校验（**两层各检一次**） | manifest 与 proof 里的同一 claim 都必须**显式为 JSON true**；任一层缺失/`false`/非布尔，或两层不一致即拒绝。**绝不默认 true**，也不从"有备份文件"推断；手写 manifest 把 proof 的 `false` 洗成 `true` 必拒（M2，见 §12.2） |
| 恢复演练 | ⚠️ 只如实登记 | `restore_drill` 字面量进 facts；`not_performed` → `restore_verified=false`（恒 false，不抬成已验证可恢复） |
| 备份自身 `metadata.schema_version` | ❌ 不参与门禁 | 备份记录的是 2、运行库是 4，**这是两回事**；要求备份 schema=4 会让合理重建后备份永远跑不过。该字段不进 canonical manifest |

**canonical 验收 manifest**（`manifest_version = rsetup-two-process-backup/v1`，字段名与实际 proof 对齐）：

必需 `manifest_version`,`engine`,`target_database`,`backup_path`,`sha256_gz`,`authoritative`,`consistent_snapshot`,`restore_drill`,`source_proof{path,sha256}`；可选 `sha256_raw`,`table_count`,`rows_total`。

派生方式（可信执行者**进程内**执行，脚本不读 `secret/`、不打印任何值）：

```
python3 scripts/tunnel_two_process.py \
  --derive-manifest-from-proof <真实 proof 路径> --engine tidb --manifest-out <私有目录>/manifest.json
```

派生只做：复制 proof 显式存在的键 + 记录 proof 文件 sha256 绑定 + `restore_drill:false → "not_performed"`。proof 写 `restore_drill:true` 或该字段缺失 → `BACKUP_RESTORE_DRILL_UNDECLARED`（一个裸 `true` 不构成可核对的演练凭据）。

## 5. 真实两进程阶段合同

| 阶段 | 真实谓词（不是固定 sleep） | 失败码 |
| --- | --- | --- |
| `identity` | openssl 生成两对 Ed25519，SPKI 前缀与 44 字节形状校验；`device_id` = 板端公钥 hex64 | `IDENTITY_PUBKEY_SHAPE` |
| `schema_gate` | 可选 `rsetup-controller-migrate --check`，只解析 `schema_version=<n>`；缺 `--migrate-bin` 即跳过并如实记 `schema_version_probe=false` | `SCHEMA_VERSION_NOT_V4` / `SCHEMA_VERSION_MISSING` / `SCHEMA_CHECK_REJECTED` |
| `controller_up` | `/healthz` 200（health 成立才把形状门禁记为 `controller_startup_healthz_observed`） | `WAIT_TIMEOUT_HEALTH` / `CONTROLLER_EXITED_BEFORE_READY` |
| `login` | 真实 `POST /api/v1/auth/login`，取 `data.csrf_token`；必要时真实改密后重登 | `LOGIN_FAILED` / `CSRF_TOKEN_MISSING` |
| `board_pending` | `GET /api/v1/approvals?decision=none&limit=200` 出现本 `device_id` | `WAIT_TIMEOUT_PENDING` |
| `approve` | `POST /api/v1/approvals/approve` 逐项 CAS：必须 `results[].ok == true` | `APPROVE_CAS_REJECTED` / `APPROVE_REJECTED` |
| `ready` | 板端私有 stderr 日志出现 `board-connect event=Ready ` | `WAIT_TIMEOUT_READY` |
| `status_poll` | `GET /api/v1/devices/{id}/status` 拿到按真实 DTO 校验通过的 **typed sample**（`snapshot` 对象 + `freshness ∈ {fresh,stale,error}` + `age_ms` 十进制字符串 + 非空 `metrics`）；`empty_cache`/403/404/5xx/`http_other`/transport/malformed **都不是样本** | `STATUS_RPC_NOT_OBSERVED`（失败关闭，见 §12.1） |
| `revoke` | `POST /api/v1/devices/{id}/revoke`（带 `expected_revision` + `confirm`） | `REVOKE_FAILED` |
| `board_stop` | 板端 exit 0 **且** `event=Stopped` **且** 落 `board-id/UNAUTHORIZED` | `BOARD_DID_NOT_STOP_PERMANENTLY` |
| `preflight` | 重启后 exit 78 **且** `event=PreflightFailed` **且** `event=AttemptStarted` 计数 **0**（零重连） | `PREFLIGHT_DID_NOT_REJECT` |
| `negative_badpin`（可选） | **先取证再判定**：先观察到本轮真的发起过连接尝试（`AttemptStarted` 或带事件退出），之后才开 5s 观察窗口；错 pin 板端不得 Ready、controller 不得落审批项 | `BAD_PIN_NO_ATTEMPT_EVIDENCE`（没取证≠通过）/ `BAD_PIN_ACCEPTED` |
| `shutdown` | `run()` 的 `finally` 无论正常、`GateError`、原生异常还是 `KeyboardInterrupt`/`SystemExit` 都会走一遍：SIGTERM → 有界 wait → SIGKILL → 每个子进程再 `wait()`，遗留子进程必须为 0 | `CHILD_PROCESS_LEFT_RUNNING` |

`--execute` 之外的一切（含 dry-run 计划）都不 `Popen`、不连库；dry-run 只跑本地门禁（含备份真实绑定）并打印 `spawned=false`。**dry-run 期间唯一允许的真实子进程是仓库内只读 `git check-ignore`**（确认 state 目录被 gitignored）；用例 `test_dry_run_launches_no_acceptance_process` 直接把 `ProcessGroup` 替换成一构造即抛的替身，证明**连进程组都不会被构造**（见 §12.5 次要项 3）。

## 6. 与现行 API/env 的核对依据

| 事实 | 源码位置 | 对本脚本的影响 |
| --- | --- | --- |
| `/healthz` 只在启动门禁后注册服务；`check_identity_schema` 在绑端口之前 | `crates/rsetup-controller/src/lib.rs:65`、`src/main.rs:628` | health 200 是形状门禁的**隐式**证据；`--check` 只是版本号，不报形状 |
| `migrate --check` 输出仅 `schema_version=<n>` 或 `schema_version=missing` | `crates/rsetup-controller/src/bin/migrate-controller.rs:180` | **修掉旧 bug**：原正则 `schema version[=: ]+([0-9]+)`（空格）永不匹配真实下划线输出 |
| 审批路由 6 条 + `revoke` | `crates/rsetup-controller/src/api/approvals.rs:617,618,624` | 路径合同 |
| 列表 `limit` 合法区间 `1..=200`，`decision` 仅 `none/approved/denied/revoked` | `api/approvals.rs:255`、`238-247` | **修掉旧 bug**：原脚本 `?limit=1000` 必被 400 |
| 逐项 CAS：HTTP 恒 200，`{"results":[{"device_id","ok","revision"}]}` | `api/approvals.rs:349-381`、`api/adapters.rs:172-177,381` | **修掉旧 bug**：原来只要 200 就算批准成功 |
| `revision` 出口为十进制**字符串**；入参同 | `api/mod.rs:177` | `_revision_of()` 只接受数字字符串 |
| 改密成功契约 `data.changed == true` | `src/http_auth.rs:802` | 收紧：不是"任何 200 都算改密成功" |
| `must_change_password` 为 true 时其余管理端点 403 | `src/http_device_status.rs:227` | 必须先真实改密再操作 |
| Host/Origin 精确匹配（逗号分隔 host 列表；Origin 精确） | `src/http_security.rs:69-84` | env 与请求头都按 `127.0.0.1:<port>` / `http://127.0.0.1:<port>` 逐字给 |
| metrics 配置路径 env 与 `[metrics] enabled=false` | `src/metrics/config.rs:18` | 关掉 metrics 避免无关启动失败（basic auth 默认要求） |
| controller env：`CONTROLLER_DATABASE_URL`/`_LISTEN_ADDRESS`/`_MANAGEMENT_CURSOR_SECRET_HEX`/`_CONFIG`/`_BOOTSTRAP_SECRET_LOG`/`_ALLOWED_HOSTS`/`_ALLOWED_ORIGIN`；transport `RSETUP_CONTROLLER_KEY_FILE`/`RSETUP_TUNNEL_LISTEN`/`RSETUP_TUNNEL_POLL_PERIOD_MS` | `src/main.rs:603`、`src/config.rs:36`、`src/transport/listener.rs:24-36,104-120,281` | env 装配逐字采用；`RSETUP_TUNNEL_POLL_PERIOD_MS=2000` 在 `bounded_duration` 合法域内（非零且 ≤ 上界） |
| board env：`RSETUP_CONTROLLER_ADDR`/`RSETUP_CONTROLLER_PUBLIC_KEYS`/`RSETUP_BOARD_KEY_FILE`/`RSETUP_BOARD_JOURNAL_DIR`/`RSETUP_BOARD_DEVICE_SN|MODEL|FW_VERSION|EXTRA_INFO` | `crates/rsetup-board-agent/src/`（同一组常量） | 设备描述符用合成值，不取真实机器标识 |
| 事件行格式 `board-connect event=<Kind> attempt=<n> stop=<..>`（stderr） | `crates/rsetup-board-agent/src/bin/rsetup-board-agent.rs:543` | 谓词按真实前缀 `board-connect event=Ready ` 匹配 |
| exit code：`Shutdown`/`Stopped` → 0，`Fault` → 78 | `src/connect.rs:463-467` | `board_stop` 期望 0、`preflight` 期望 78 |
| 未授权标记落在身份文件父目录，名固定 `UNAUTHORIZED` | `src/connect.rs:706`、`bin/rsetup-board-agent.rs:536` | `board_stop` 检查 `<state>/board-id/UNAUTHORIZED` |
| preflight 遇标记 → `PreflightFailed` 故障并退出（不进入重连循环） | `src/connect.rs:641-649` | "零重连"可用 `AttemptStarted` 计数客观判定 |

## 7. 秘密边界与进程所有权

- 只有固定 token 能进 console：`Console.emit()` 断言 key 是 `[a-z][a-z0-9_]{0,31}`、value 只能是 `bool`/`int`/`None`/本模块固定 token；DSN、口令、pin、cookie、CSRF、日志行一律进不了 stdout。测试 `test_secrets_never_reach_console` 逐串断言。
- DSN 只进子进程 **env**（percent-encode 后），不进 argv；`ProcessGroup.spawn` 固定 argv list、无 `shell=True`、无 `source`、无 tracing；子进程 stdout/stderr 以 `O_CREAT|O_EXCL|O_NOFOLLOW` 0600 落在 state 目录（同名文件已存在即 `STATE_LOG_EXISTS`，不复用旧证据）。
- HTTP 响应体只 append 到 `<state>/http.log`（0600）；cookie 由标准 `HTTPCookieProcessor` 持有（不落盘）。
- `two-process-summary.json` 只含布尔/计数/固定类别码/退出码，写前再过一遍 `Redactor.scrub()`。
- 子进程所有权：`ProcessGroup.shutdown_all()` 先对全部存活子进程 SIGTERM，有界 wait，仍存活者 SIGKILL，最后**每个子进程都再 wait() 一次**；`run()` 的 `finally` 与 `__exit__` 两条路径都走它。
- v2 补充：`Redactor` 除口令与整条 DSN 外还登记 `host`/`user`/`database`（§13.2，身份字段整段替换，不打碎固定类别码）；summary 改为"先删旧文件 → `O_EXCL` 写 `.part` → `os.replace` 原子替换"，写失败按 `SUMMARY_WRITE_FAILED` 非零退出（§12.3）；`Popen` 创建失败时两个日志 fd 必关（§12.4）；argparse 与顶层异常路径只出固定码，不回显参数或 traceback。

## 8. 离线自测证据

**先跑了旧测试再动手**（45 个用例全绿），但没有把旧绿当正确设计：正是这次运行暴露出 `test_accepts_comments_and_trailing_commas` 把误读固化、`test_admin_password_is_explicit_input` 依赖嵌套 `admin` 对象。

**临时目录口径纠正**：原测试与 `run_selftest()` 用 `tempfile.mkdtemp()`（默认 `/tmp`），越出本任务 scope。现在两个文件都不再 `import tempfile`；由脚本的 `prep_workspace()` 在 `<repo>/target/tunnel-two-process-prep/<用例名>-<pid>-<rand>` 下独占创建 0700 工作区，`tearDown`/`finally` 用 `remove_tree()` 删除。`target/` 已被 gitignore（`test_state_dir_under_repo_target_is_gitignored` 用真实 `git check-ignore` 验证）。

当前实跑（本轮；不设置 `TMPDIR`、不启动真实进程、不连库、不连网）：

```
$ python3 -m unittest scripts.tests.test_tunnel_two_process
Ran 104 tests in 0.218s
OK

$ python3 scripts/tunnel_two_process.py --selftest
stage=selftest json_plain=true
stage=selftest json_rejects_non_standard=true
stage=selftest redaction=true
stage=selftest backup_derive_bound=true
stage=selftest backup_wrong_target_blocks=true
stage=selftest backup_manifest_tamper_blocks=true
stage=selftest backup_file_tamper_blocks=true
stage=selftest symlink_state_rejected=true
stage=selftest status_needs_typed_sample=true
stage=selftest proof_two_layer_true_passes=true
stage=selftest proof_claim_laundering_blocks=true
stage=selftest unsafe_error_code_folded=true
stage=selftest kill_fallback_and_wait=true
stage=selftest term_is_enough=true
stage=selftest spawn_failure_closes_fds=true
stage=selftest ok=true
```

退出码均为 0；详细逐例日志 `target/tunnel-two-process-prep/unittest-104-v.log`，自检日志 `target/tunnel-two-process-prep/selftest-current.log`（`target/` 已 gitignored）。

当前覆盖分布（共 104 例）：`StageFlowTests` 20（阶段与谓词）、`BackupManifestTests` 17（备份绑定 + M2 两层 claim + 目标库标识符）、`PathHygieneTests` 11、`ControllerHttpContractTests` 11（假 opener，锁定路径/limit/CAS/CSRF 头 + M1 状态类别）、`StageExceptionContainmentTests` 8（M3）、`GateBlocksSpawnTests` 8（门禁不过时 state 目录保持空 = 未 spawn 的证据）、`CredentialTests` 10（含 §14.1 user 连字符 2 例）、`ProcessOwnershipTests` 6、`ConsoleRedactionTests` 5、`IdentityPkcs8Tests` 2（§14.2 PKCS#8 v1→v2）、`JsonDocumentTests` 3、`WaitPredicateTests` 2、`SelftestTests` 1。

**historical（v1 准备轮的运行，保留不覆盖）**：当时是 68 例 + 11 行自检，其中 `status_poll` 与 proof claim 的口径还是错的（见 §12）：

```
$ python3 -m unittest scripts.tests.test_tunnel_two_process
Ran 68 tests in 0.098s
OK

$ python3 scripts/tunnel_two_process.py --selftest   # 11 行，无 status/proof/异常三项
```

`FakePopen`/`FakeHttp`/`FakeRunner` **只服务于单测**，不是验收替身：脚本 `--execute` 路径里没有任何 fake 注入口。

## 9. 真正执行前还需要的条件

1. 预编译产物绝对路径：`rsetup-controller`、`rsetup-board-agent`，可选 `rsetup-controller-migrate`（`--check` 只读版本探针）。本脚本不构建，也不做 migration。
2. 目标库必须是**已迁移到 v4** 的已授权隔离开发库（已核对为可丢弃的开发库并具备写权限）。`schema_version != 4` 或 `missing` 时脚本直接拒绝，绝不隐式迁移。库名允许合法连字符（见 §13.1）；**换目标或改库名都不在选项里**。
3. 真实备份及其 proof（gzip 文件 + `sha256_gz`/`sha256_raw`/`target_database`/claims），并用 §4 的派生命令生成 canonical manifest。备份文件与 proof 都必须 0600/非链接（`test_proof_file_must_be_0600`）。proof 的字段合同必须**原样满足**，脚本不猜键名、不补默认值：
   - `authoritative`/`consistent_snapshot`：必须是**显式布尔**（`true` 才可能过；`false` 就如实拒）；
   - `restore_drill`：**布尔 `false` 是被明确允许的如实声明**，派生时如实映射成 canonical 字符串 `"not_performed"`（`facts.restore_drill_performed=false`、`restore_verified=false`）；写 `true` 或缺失 → `BACKUP_RESTORE_DRILL_UNDECLARED`。
   - 若真实 proof 的键名/类型与上面不同（例如 claims 写成字符串、或缺少 claims），正确做法是由**可信人工适配**把它显式转换成 §4 的 canonical 形状并在本报告外留痕，**不是**让脚本 `default` 或把 claim 洗成 true；脚本侧一律以 `BACKUP_FIELD_MISSING`/`BACKUP_PROOF_SHAPE` 拒绝。
4. 管理员来源二选一：`--admin-file`（已知口令，必要时带 `new_password`），或目标库尚未 bootstrap 时的 controller bootstrap sink 首启路径。
5. 一次运行一个**全新**私有 state 目录（0700、gitignored）：日志 `O_EXCL`、bootstrap sink 由 controller 以 `create_new` 写出，复用旧目录会失败——这是刻意的（不覆盖既有证据）。
6. 运行需 `--execute` + 逐字 `--ack ISOLATED_TARGET_AND_BACKUP_VERIFIED`；`--with-badpin-negative` 只在需要该可选负例时加。

## 10. 明确不测 / 不声称

- **不声称两进程验收已通过**：没有真实 controller/board 进程、没有真实库、没有真实备份。`stage_*` 的真实证据（Pending/CAS/Ready/Stopped/Preflight）只能来自 `--execute` 的私有 state 目录产物。
- **不测 GOAWAY/wire 级证据**：没有可采集的 wire 级 flush 证据来源，就不写"已验证 GOAWAY 排空"这类结论；脚本也不拿总耗时、"Ready 出现"替代它。
- **不用 Ready 当批准证据**、不用 Ready 时长当性能证据。**【historical，已由 §12.1 纠正】** 旧口径"status 拿不到样本时只记 `status_observed=false`，不推断"是假绿来源：现在拿不到 typed 样本就是 `STATUS_RPC_NOT_OBSERVED` 失败、非零退出、`complete=false`，既不推断也不放行。
- **不声称备份可恢复**：未做恢复演练，`restore_verified` 恒 `false`。
- 脚本内不含任何真实主机名、库名、口令、路径值；合成 fixture 值（`Synth-Pw/21?#42` 等）只存在于单测，且有测试断言其不出现在 console/argv/summary。

## 11. 已知限制与取舍

- 端口 `bind(0)` 探测存在 TOCTOU 窗口；脚本的处置是**失败即失败**，不静默换端口、换 target 或换协议。
- `--check` 只证明版本号；完整 v4 形状靠 controller 启动期硬门禁 + health 观察隐式成立，`facts.schema_shape_gate` 会把这两种状态区分开（`controller_startup` vs `controller_startup_healthz_observed`），不假报 "shape PASS"。
- 循环等待都是**等待真实谓词**（`wait_until`），由脚本自己的子进程/HTTP 状态驱动，不需要外部代理忙轮询。
- 状态阶段依赖**中控侧缓存被真实填充**（`crates/rsetup-controller/src/polling/snapshot.rs` 的 `SnapshotStore`）：缓存未填充时真实响应就是 `snapshot=null`/`freshness=unknown`，脚本按 `empty_cache` 如实分类并失败，不会因此判绿。
- 分类器要求样本 `metrics` 非空（真实 DTO 允许 `metrics: []`，见 `crates/rsetup-board-agent/src/host_source.rs:272` 的三个可选来源）。这是**刻意偏保守**：指标全不可用的样本被判 `malformed` → 失败关闭，绝不会反过来把无指标样本当证据。
- `complete=true` 需同时满足：全部必需阶段成功、无失败码、`status_observed is true`、无遗留子进程，且 summary 已成功落盘；任一不成立即 `false` + 非零退出。
- 行数：脚本 1891 行、测试 1462 行，高于 650–900 的目标。增量全部是评审修复带来的**安全门禁**（typed status 分类、两层 claim 复核、异常/中断/写失败收口、fd 关闭、固定错误码路径）与其配套用例，不是功能扩张；再压就只能删这些门禁本身，属于"为行数删安全门禁"，故保留。
- 备份 gate 只读一个已核对的 proof 形状；若 proof 键名与本 §4 列出的不同，`derive_acceptance_manifest` 会以 `BACKUP_FIELD_MISSING` 拒绝，**不做猜测式改名兼容**。
- `_write_summary` 用 `O_EXCL` 写 `two-process-summary.json.part` 再 `os.replace` 原子替换；`.part` 残留会让本次写失败并按 `SUMMARY_WRITE_FAILED` 非零退出（fail-closed，不会退回旧 summary 或返回 0）。
- 阶段内 `SystemExit`/`KeyboardInterrupt` 按"**不吞真实程序退出**"处理：先写完 summary（`RUN_INTERRUPTED`、`complete=false`）再向外传播，由解释码决定进程退出码；`run()` 自身在异常路径上从不返回 `EXIT_OK`。



## 12. 独审 3 Major 的修复记录（v2）

### 12.0 RED 依据与归因（先说清证据从哪来）

- 三个 Major 由**前一轮独立脚本独审**（记为 reviewer `6f82`）在旧脚本快照上**真实复现**；本轮不自称"本轮重新复现 RED"。
- 可复核的 RED 实物留在独审目录：`target/tunnel-e2e-script-review/`
  - `probe_nongate_error.py` 当时留下的工件 `probe-ws/state/two-process-summary.json` 内容就是 M3 的失败证据：非 `GateError` 的原生失败发生后仍写着 `"complete": true, "failure_codes": []`；
  - `probe_proof_claim.py` 的反例输入 `probe-ws2/proof.json`（`authoritative:false`）+ `probe-ws2/hand.json`（手写 `authoritative:true`）就是 M2 的洗白组合。
- **本轮不重建"语义回退副本"**（上一轮准备会话已用它验证新用例能抓缺陷；本轮按最小化要求不重做）。改完只把上述两个**既有最小探针原样复制**到 `target/tunnel-two-process-prep/probe-reuse/`（仅改写其工作区路径，不动旧工件）复跑一次，作为"同一反例在当前脚本上翻绿"的证据：
  - `probe-reuse/m3-postfix.log`：`failure_code=UNEXPECTED_STAGE_ERROR failed_stage=stage_identity`，summary `complete = False`、`failure_codes = ['UNEXPECTED_STAGE_ERROR']`；
  - `probe-reuse/m2-postfix.log`：`A derive rejected: BACKUP_CLAIM_NOT_TRUE`、`B handwritten manifest rejected: BACKUP_PROOF_MISMATCH`。
- M1 没有留下独立探针工件：其 RED 依据是独审复现 + v1 本报告 §5 里那行白纸黑字的旧口径（"拿不到只记 `status_observed=false`，不判失败"）。本轮的锁定手段是把 7 个失败类别全部参数化成必红用例（清单见 §12.1）。

### 12.1 M1：状态阶段"没看到样本也算过"

- **根因**：v1 把"HTTP 200 且 `data` 是对象"当成状态阶段成功，且 `status_observed=false` 不产生失败码。真实投影里 `200 + snapshot=null + freshness=unknown + age_ms=null` 是**空缓存的如实表示**（`crates/rsetup-controller/src/http_device_status.rs:324-343`），`data={}` 则根本不是样本；两条都会被旧口径记成"阶段通过"，链路可以继续走到 `complete=true`。
- **改法**（`classify_status_payload` + `ControllerHttp.status_probe` + `TwoProcessRun.stage_status_poll`）：
  1. 默认必须拿到**按真实 DTO 校验过的 typed sample**：`snapshot` 是对象、`freshness ∈ {fresh,stale,error}`、`age_ms` 是无符号十进制字符串、`schema_version` 是整数、`boot_id/agent_epoch/clock_epoch/clock_quality` 非空字符串、`sample_seq/sampled_monotonic_ns` 十进制字符串、`sampled_unix_ms` 有符号十进制字符串、`metrics` 非空且每项有 `key`（对齐 `http_device_status.rs:369-398`）；
  2. 固定分类而非异常文本：`present` / `empty_cache` / `http_403`（含 401）/ `http_404` / `http_5xx` / `http_other` / `transport` / `malformed`；
  3. 非 `present` 一律 `status_observed=false` + `raise GateError("STATUS_RPC_NOT_OBSERVED")` → `run()` 记失败码、`complete=false`、退出码 `EXIT_FAILED`；Ready 不能替代这条 RPC 证据，阶段顺序不变（status 失败即不 revoke）；
  4. 秘密边界：状态路由**不落响应体**（`record_body=False`），facts 只存固定 token；`must_change_password` 未真实改密时该路由真实回 403（`http_device_status.rs:227,293`），这条路径就是 `http_403` 而不是"没数据"。
- **新用例**：`test_status_probe_accepts_real_typed_sample`、`test_status_probe_reports_empty_cache_not_sample`、`test_status_probe_rejects_http200_without_data`、`test_status_probe_rejects_typed_sample_shape_violations`、`test_status_probe_fixed_categories`、`test_status_probe_never_returns_body_text`、`test_status_poll_absence_is_recorded_not_assumed`、`test_status_probe_failure_categories_are_all_red`、`test_status_probe_present_lets_the_chain_complete`；自检项 `status_needs_typed_sample`。

### 12.2 M2：proof 说 `false`、手写 manifest 写 `true` 就能洗白

- **根因**：v1 只在 manifest 层要求 claim 显式 true，`_verify_proof_binding` 只比 `sha256_gz`/`target_database`，不读 proof 里的 claim。于是"真实 proof 声明 `authoritative=false` + 手写 canonical manifest `true`"能过门禁——把"派生通道"当成了唯一可信通道，却没有约束手写通道。
- **改法**（`BackupManifest.__init__` + `_verify_proof_binding`）：**两层各检一次且必须一致**——manifest 构造期要求两个 claim 显式 `is True`（缺失/`false`/非布尔 → `BACKUP_CLAIM_NOT_TRUE`/`BACKUP_FIELD_MISSING`）；`verify()` 内重算 proof 文件 sha256 后，再要求 proof 里同名 claim 是**显式布尔**（缺失/非布尔 → `BACKUP_PROOF_SHAPE`）且**两层都为 true**（任一为 false → `BACKUP_PROOF_MISMATCH`）。proof 还必须是 0600 私有文件（`INPUT_FILE_MODE`）。
- **新用例**：`test_proof_claim_false_laundered_by_handwritten_true_manifest_is_rejected`、`test_proof_claim_must_be_true_for_either_claim`（2 claim × `false/"true"/1/None`）、`test_proof_claim_missing_is_rejected`、`test_proof_hash_tampering_still_blocks`（修 M2 没把 hash 绑定弄松）、`test_two_layer_true_path_still_passes`（合法路径不被反向修成永假）、`test_proof_file_must_be_0600`；自检项 `proof_two_layer_true_passes`、`proof_claim_laundering_blocks`。
- **对真实 proof 的含义**：派生通道 `derive_acceptance_manifest` 只**复制** proof 里显式存在的键，从不补默认；真实 proof 若不满足这个形状，按 §9.3 由可信人工适配显式转换，脚本不 `default`、不洗 claims。

### 12.3 M3：异常/中断/写失败路径上 `complete=true` 与泄漏

- **根因**：v1 的 `run()` 只捕 `GateError`，`CalledProcessError`/`TimeoutExpired`/`FileExistsError` 等原生异常会**逃出去**，summary 仍按"没失败"写完（`complete=true`、`failure_codes=[]`，就是 §12.0 那份工件），而且异常文本可能带 argv/路径；`Ctrl-C` 走不通收尾，summary 写失败也不改变退出码。
- **改法**（`run()` + `_record_failure` + `_write_summary` + `ProcessGroup`）：
  1. 阶段循环三层捕获：`GateError` → `sanitize_code(error.code)`（非固定大写常量折成 `UNEXPECTED_STAGE_ERROR`，绝不回显文本）；`KeyboardInterrupt`/`SystemExit` → `RUN_INTERRUPTED` 且**收尾后继续向外传播**（不假继续）；其它 `Exception` → `UNEXPECTED_STAGE_ERROR`；
  2. `finally` 里**总是**执行 `stage_shutdown()`（owned children 一律 SIGTERM → 有界 wait → SIGKILL → 再 `wait()`），shutdown 自身异常同样只入固定类别；
  3. 失败只以 `failure_code=<固定码> failed_stage=<固定阶段名>` 一行进 stderr，无 `str(exc)`、无 traceback、无路径；
  4. summary **先 `os.unlink` 旧文件**再 `O_EXCL` 写 `.part` + `os.replace` 原子替换，写失败 → `SUMMARY_WRITE_FAILED` 且 `run()` 直接返回 `EXIT_FAILED`（既不返回 0，也不会留下上一轮的 `complete=true`）；`complete` 还要 `status_observed is True` 且无遗留子进程；
  5. 顶层同样收口：argparse 错误、派生 IO 错误、未预期异常都只出固定码。
- **新用例**：`test_called_process_error_in_identity_is_contained`、`test_timeout_expired_in_schema_gate_is_contained`、`test_file_exists_error_in_negative_journal_is_contained`、`test_keyboard_interrupt_is_not_swallowed`、`test_shutdown_exception_is_recorded`、`test_summary_write_failure_is_not_green`、`test_main_does_not_echo_unrecognized_argument`、`test_main_derive_io_error_is_fixed_code`（`StageExceptionContainmentTests` 共 8 例，均以 `LEAK_TOKEN` 断言 stderr/summary 无残留）；自检项 `unsafe_error_code_folded`。

### 12.4 独审次要项逐条（核过，未遗漏）

| 次要项 | 现状 | 证据 |
| --- | --- | --- |
| argparse 把未知参数原样回显 | `_FixedErrorParser.error()` 丢弃 message，只出 `gate_error=ARGUMENTS_INVALID` | `test_main_does_not_echo_unrecognized_argument` |
| `Popen` 抛异常时日志 fd 泄漏 | `ProcessGroup.spawn` 对两个 fd 都 `os.close`（含第二个日志打开失败的分支），并折成 `PROCESS_SPAWN_FAILED` | `test_popen_failure_closes_both_log_fds`、自检 `spawn_failure_closes_fds` |
| dry-run 允许 `git check-ignore` 但不得有验收进程 | dry-run 不构造 `ProcessGroup`；唯一子进程是仓库内只读 `git check-ignore` | `test_dry_run_launches_no_acceptance_process`、`test_state_dir_outside_repo_skips_git_check` |
| proof 0600 前提 | 读 proof 走 `load_private_json`→`private_file`（0600、非链接、属主 euid） | `test_proof_file_must_be_0600` |
| badpin 负例缺"本轮真的试过"前提 | 先等 `AttemptStarted`/Ready/带事件退出，再开 5s 窗口；没取证 → `BAD_PIN_NO_ATTEMPT_EVIDENCE`（不是通过） | `test_badpin_negative_without_attempt_evidence_is_not_a_pass` |

### 12.5 本轮精读结论

逐条读了 M1/M2/M3 的实现与全部用例后，**没有发现需要新修的必现缺陷**，也**没有用"把断言改松/把用例删掉"来规避回归**：新用例都是"必红方向"的断言（拒绝、非零退出、`complete=false`、固定类别），并各配一条"合法路径仍须通过"的对向用例（`test_two_layer_true_path_still_passes`、`test_status_probe_present_lets_the_chain_complete`、`test_status_probe_accepts_real_typed_sample`），防止修复被写成永假。M1/M2/M3 的既有落地代码**全部保留**；本轮对脚本与测试的进一步改动只有 §13 的两处输入合同最小修及其 6 个配套用例。

### 12.6 修复后仍未执行的边界（不因修复而改变）

- 没有真实 `--execute`：`present` 这条分支、Pending/CAS/Ready/Stopped/Preflight、备份文件的真实 gzip CRC 都还只是"离线可证"，不是验收证据；
- 真实 HTTP 状态样本依赖中控 polling 缓存被填充，脚本只等 30s 并如实分类；
- 恢复演练未做（`restore_verified` 恒 false）；GOAWAY/wire 级排空不测也不声称。

## 13. 执行前依赖的两处最小修（v2，配合真实授权目标形状）

可信执行侧已给出 fresh TiDB v4 真实 MVCC 备份 + 私有 canonical manifest + 新 admin 口令 + 空 state 目录，其中**授权库名含合法连字符**，暴露出两条输入合同缺陷。本轮只修输入范围与脱敏登记，不改任何凭据/proof、不换目标、不放宽其它维度。

### 13.1 数据库标识符允许合法连字符

- **根因**：`NAME_RE = ^[A-Za-z0-9_][A-Za-z0-9_$]{0,63}$` 同时用于库名校验，含 `-` 的合法库名会被错拒成 `CREDENTIAL_FIELD_TYPE` / `BACKUP_FIELD_TYPE`，脚本根本跑不起来（阻断，不是假绿）。
- **改法**：新增 `DATABASE_NAME_RE = [A-Za-z0-9_][A-Za-z0-9_$-]{0,63}`，用于 `Credential.database` 与 `BackupManifest.target_database`。同时把 `database`/`target_database`/`user` 三处从 `pattern.match` 改成 `pattern.fullmatch` 并去掉 `^…$`：`^…$` 配 `match` 会放过**尾随换行**（`$` 允许在末尾 `\n` 前收尾），那等于把控制字符带进 DSN。**【historical，已由 §14.1 纠正】** 本句原写"`Credential.user` 维持原 `NAME_RE`（未出现连字符需求）"，该判断已作废——真实执行前的语法事实确认授权 DB 用户名与库名同值且含合法 `-`，`user` 现改用独立 `USER_NAME_RE` 允许连字符（见 §14.1）。
- **保持不变**：总长上限仍 64（1+63）；首字符不得是 `-`（避免被当命令行选项）；字符集不含空白/引号/分号/反斜杠/`NUL`/任何控制字符/非 ASCII；`dsn()` 的 path 段仍 `urllib.parse.quote(..., safe="")`（连字符是 unreserved 原样保留，`$` 变 `%24`，有断言）；脚本从不拼 SQL，因此不需要也不做反引号引用。
- **新用例**：`test_database_name_may_contain_hyphen`、`test_database_name_rejects_invalid_forms`（13 种非法形状）、`test_target_database_with_hyphen_binds_normally`（含"换库名仍 `BACKUP_TARGET_MISMATCH`"）、`test_target_database_rejects_invalid_forms`。
- **CLI 级冒烟**（合成、零 spawn，产物在 prep 目录）：`target/tunnel-two-process-prep/probe-reuse/dryrun_dash_db_smoke.py` → `target/tunnel-two-process-prep/probe-reuse/dryrun-dash-db.log`，输出 `derived target_database = rsetup-synth-v4 | restore_drill = not_performed` 与 `stage=plan spawned=false`、`dry-run exit = 0`、state 目录仍空。

### 13.2 Redactor 登记 host/user/database

- **根因**：只登记 `password` 与整条 DSN，裸 `user`/`database`/`host` 若随异常文本或落盘文本出现就不会被兜底替换。
- **改法**：`parse_credential`（dry-run 与 execute 共用入口）额外 `add_identifier(host/user/database)`；`Redactor` 分两类替换——秘密值仍是**无条件整串**替换，身份字段只做**整段**替换（前后非 `[A-Za-z0-9_]`），否则库名恰好是某个固定类别码的子串（如 `port` ⊂ `transport`）会把 summary 的固定 token 打碎成 `t[redacted]`，毁掉可核对的证据文本；长度下限 `MIN_REDACT_LEN=4` 以下的身份值不登记。两类都按长度降序替换，避免短值先把长值（DSN）撕成残留片段。
- **边界如实说明**：`<4` 字符的身份字段与"紧贴标识符字符"的裸值不会被这层兜住——console 本就只接受固定 token/布尔/整数（`Console.emit` 强校验），DSN 只进子进程 env 不进 argv，所以这两条由那些更强的约束承担，不靠 scrub。
- **新用例**：`test_credential_registers_host_user_database`（四个字段 + DSN + 含连字符库名都不残留）、`test_identifier_scrub_keeps_fixed_tokens`（固定类别码/JSON 结构不被打碎；短值不登记）；既有的 `test_redactor_scrubs_registered_values`、`test_summary_file_is_private_and_scrubbed`、`test_secrets_never_reach_console` 全部仍绿。

### 13.3 真实 proof 的 `restore_drill=false` 核对结果（未改代码）

按"不能为了 gate 要求伪填 true、不改旧 proof"复核：`derive_acceptance_manifest` 对 proof 的 `restore_drill` **明确接受布尔 `false`** 并如实映射为 canonical 字符串 `"not_performed"`（`facts.restore_drill_performed=false`、`restore_verified=false`）；写布尔 `true` 或缺失才拒（`BACKUP_RESTORE_DRILL_UNDECLARED`）——一个裸 `true` 不构成可核对的演练凭据。真实 proof 的 `authoritative`/`consistent_snapshot` 若不是显式布尔（例如缺键或写成字符串），脚本按 `BACKUP_FIELD_MISSING`/`BACKUP_PROOF_SHAPE` 拒，**需要可信 adapter 显式转换**，脚本不 default、不洗 claims（§9.3）。

## 14. 执行前阻断的两处修复（v3：`user` 合法连字符 + PKCS#8 v1→v2）

### 14.1 授权 DB 用户名允许合法连字符（`Credential.user`）

- **根因（真实执行前语法事实）**：可信 prepare 确认**授权 DB 用户名与库名同值且含合法 `-`**。§13.1 只放开了 `database`，`Credential.user` 仍用禁 dash 的 `NAME_RE.fullmatch`，这个既有合法账号必然被错拒成 `CREDENTIAL_FIELD_TYPE`（阻断）。主代理不给具体值、只给语法事实；**不可**改账户/新建用户名/换目标/改秘密 JSON 去迁就脚本。
- **改法**：新增**独立** `USER_NAME_RE = [A-Za-z0-9_][A-Za-z0-9_$-]{0,63}`（字符范围与 `DATABASE_NAME_RE` 相同，但独立 pattern 以保留政策边界），`Credential.user` 改用它；继续 `fullmatch`、总长 ≤64、首字符非 `-`、拒绝空白/引号/分号/`NUL`/控制字符/尾随换行。DSN userinfo 段仍 `urllib.parse.quote(user, safe="")`，不裸拼。
- **政策边界**：`user` 在此**只作 DSN userinfo 字段**，不是 SQL identifier；脚本从不拼 SQL，也**不做任何账号/服务器全局操作**（无 `CREATE/ALTER USER` 之类）。放宽的只有字符集，其它输入 shape 不扩大。
- **RED→GREEN**：RED = 旧 `NAME_RE` 下 `user="rsetup-synth-user"`（配 `database="rsetup-synth-db"`）直接抛 `CREDENTIAL_FIELD_TYPE`（先写必红用例，实测旧码 reject）；改 `USER_NAME_RE` 后 GREEN，`test_user_name_may_contain_hyphen` 构造 DSN 后 `urlsplit` 反读 `username==user`、`path==database`（**不实际连接**），且含 `$` 的用户名仍 `quote` 成 `%24`（非裸拼）。对向 `test_user_name_rejects_invalid_forms`：空白/`;`/单双引号/`\x00`/`\n`/`\t`/前导 `-`/非 ASCII/超 64/空 **仍** `CREDENTIAL_FIELD_TYPE`。短字段脱敏的 fixed-code 行为不变（既有 `test_credential_registers_host_user_database`、`test_identifier_scrub_keeps_fixed_tokens` 仍绿）。

### 14.2 openssl PKCS#8 v1 → ring 接受的 v2（身份密钥编码，`generate_ed25519_identity`）

- **根因（执行前阻断，非假绿）**：生产 `IdentityKey::from_pkcs8` → ring `Ed25519KeyPair::from_pkcs8`（ring 0.17.14 `src/ec/curve25519/ed25519/signing.rs:62-90`）走 `Version::V2Only`，**只**接受含 publicKey 的 PKCS#8 **v2**（`OneAsymmetricKey`，RFC 5958）并强制公私一致性；`openssl genpkey -algorithm ed25519` 产的是 **v1**（version=0、无 publicKey）。脚本原先把 openssl v1 直接写成 `identity.pk8`，controller/board 启动期 `load_or_generate→from_pkcs8` 必拒。**原独审 `6f82` 的"OpenSSL PKCS8 兼容"只是源码推断，不成立**——真实 stdin probe（openssl v1 DER → `from_pkcs8` false/exit1，无 DB/无起进程/无密钥回显）由主代理实测确证。
- **不放宽**：不改生产 `IdentityKey`、不把 `from_pkcs8` 换成 `from_pkcs8_maybe_unchecked`（会丢 public 一致性校验）。
- **改法**：新增固定 Ed25519 shape 转换器 `ed25519_pkcs8_v1_to_v2`（**纯定长切拼，不是通用 ASN.1 parser**）。以 ring 自身模板 `ed25519_pkcs8_v2_template.der`（`3051 0201 01 3005 06032b6570 0422 0420 ‖ 812100`，`private_key_index=0x10`，legacy `[1]`=0x81 包裹 public，ring `accept_legacy_ed25519_public_key_tag=true` 明确接受）为准：openssl v1 = `302e 0201 00 3005 06032b6570 0422 0420 ‖ seed`（48B，由同一结构去掉 version=1/public 段推出，非猜字节）；重排为 `ED25519_PKCS8_V2_PREFIX ‖ seed ‖ 812100 ‖ public`（83B）。`generate_ed25519_identity` 用 openssl 已派生的 public（SPKI 去前缀 32B）参与重排后写回**同一 0600 文件**；seed 只在内存内瞬存做字节拼接，**从不**进 argv/console/普通日志/报告。fail-closed：v1 非 48B 或前缀不符、public 非 32B → `IDENTITY_PKCS8_SHAPE`。
- **RED→GREEN（本轮 scope 内可证）**：新 `test_converter_ring_shape_and_rejects_bad` 读 ring 模板文件重算 `wrap_key_`，断言转换器输出与之**逐字节相同**（外层 SEQUENCE 定长 0x51、version=1、`[1]`=0x81 public、seed@16、public@51），并断言非法 v1 长度/前缀/非 32B public 全部 `IDENTITY_PKCS8_SHAPE`；`test_openssl_v1_reencoded_to_ring_v2` 端到端（假 openssl 产真实 48B v1）→ 返回 public 一致、落盘 83B v2、v1 不再在盘上。RED：改前 `generate_ed25519_identity` 不重排、落盘仍是 48B v1，该 83B 断言直接失败。ring 对**此字节布局**的接受由项目自身 Rust 测试 `generate_pkcs8 → from_pkcs8`（`tcp_stack.rs:22-23`、`handshake.rs:36-38`、`identity.rs:47/120`）已证明，不是"换命令声称兼容"。
- **仍待主代理核（超出本 subagent scope）**：用 `target/` 下 Rust probe 链接真实 `rsetup_tunnel` rlib + openssl 临时 key 进程内 pipe 实测 `from_pkcs8` 接受本 v2——需编译 + 起进程，落在"真实进程/改 Rust/Cargo/外路径"之外，本 subagent 未执行。主代理的 v1→false 探针已就位，预期 v2→接受。**此步完成前，§14.2 只声称"转换器与 ring 模板逐字节一致"，不声称"已在真实 rlib 上解析通过"。**

## 15. 本轮结论与仍然存在的阻断

**已完成**：M1/M2/M3 实现与新用例逐条精读（结论 §12.5）；本报告旧假绿口径就地标注并纠正、补齐 3 Major 的根因/新用例名/执行证据/RED 归因/未执行边界（§12）；两处执行前输入合同最小修及其 6 个用例（§13）；v3 两项执行前阻断修复各带 2 例（§14：`Credential.user` 允许合法连字符、openssl PKCS#8 v1→ring v2 固定 shape 重排）；`python3 -m unittest scripts.tests.test_tunnel_two_process` → **Ran 104 tests … OK**（v2 的 100 例全保留 + §14 的 4 例），`python3 scripts/tunnel_two_process.py --selftest` → **16 行、exit 0**（日志见 §8）。**§14 的 v1→false 根因由主代理真实 stdin probe 确证，但 §14.2 的"v2 在真实 rlib 上解析通过"探针属主代理 scope，本 subagent 未跑**；本节结论不把"转换器逐字节一致"等同于"已在真实 rlib 解析通过"，也不等于 3 Major 已独立复核通过。

**仍然阻断真实验收的条件（一条也没被本轮满足）**：

1. **真实 `--execute` 尚未开始**：本轮零真实进程、零数据库、零网络。所有 `stage_*` 的真证据只能来自执行后的私有 state 目录产物；**本报告不构成两进程验收 PASS**。
2. 执行前仍需 §9 的 6 项前置逐项就位（预编译产物、v4 库、备份+proof+canonical manifest、管理员来源、全新 0700 state 目录、逐字 `--ack`）。
3. 按既定流程，**脚本独审通过后才允许 execute**；本轮修复未经过新一轮独审，不应被当作"已复核"。
4. 真实 proof 若与 §9.3 的字段合同不符（claims 非显式布尔等），需可信人工适配并留痕，脚本不会代为默认。

**本轮明确没做**（避免扩工程）：没有重建语义回退副本或批量 mutation 框架；没有改动生产状态或任何凭据/proof；没有 commit/push/clean；没有读取 `secret/`；没有触碰 Rust/Cargo。

## 16. 主代理真实身份解析器核验（2026-10-24）

本轮 writer 的 scope 不含该实际进程探针，以下由主代理独立执行，不改写其边界声明：

- 链接当前构建的 `rsetup_tunnel`，探针只调用生产 `IdentityKey::from_pkcs8`。
- 真实 `openssl genpkey` 临时 Ed25519 v1 DER 及其派生 SPKI 全部通过进程 pipe／内存传递，
  不把私钥写入源码、命令参数、普通日志或聊天。
- 同一临时密钥：原 v1 **拒绝（exit 1）**；调用脚本固定 shape 转换后的 v2
  **接受（exit 0）**。ring 的严格入口仍会核对公私钥一致性，未使用 unchecked 解析。
- 主代理同时独立复跑当前 **104 项 unittest 通过、16 项 selftest 通过**。

§14.2 所列真实解析器探针现已完成；这只解除密钥编码兼容的执行前疑问，
不等价于真实 controller／board 两进程验收通过。新一轮脚本复审结果见 §17。

## 17. 修复的独立复审（30d7c60b）

**三个 Major 全部 ADDRESS，无新阻断，脚本侧独审通过，可进入真实 execute。**
reviewer 独立跑 **104 项 unittest、16 项 selftest、55 项合成 probe**，全部通过；只读核查
状态样本必须观测、proof 双层显式真且一致、原生异常和取消的失败 summary／子进程收尾，
以及用户名／库名 fullmatch、PKCS#8 固定编码。真实解析器探针来源为 §16 主代理执行。
reviewer 未读私有凭据、未连库、未启动生产进程，不冒称真实两进程成功。

非阻断边界继续保留：合法空 metrics 被脚本保守错拒；stale/error 的“存在样本”分类不等同
健康新样本；HOST／admin 用户名等剩余正则尚可放过尾随换行（执行者须核真实输入）；
bad-pin 提前退出的尝试证据弱于文案。本轮实际核心验收暂不启用可选 bad-pin 负例，
不外推签名拒绝、零副作用或线上 GOAWAY 的实际 flush 时点。

新的可信执行任务已获准依次核验 §9 全部前置、dry-run、真实 execute；最终必须同时核
进程退出、summary、真实状态样本 freshness 和数据库副作用，不能仅凭脚本 exit 0 报整体
通过。该执行结果仍待独立回执，本文当前只记录准备及复审完成。
