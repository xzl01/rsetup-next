# 双引擎真实数据库验收报告（2026-10-22）

> **核验范围**：使用本地 `secret/` 中已授权的两个专用开发库（MySQL Community 8.0.46、TiDB v8.5.8），串行执行 controller 全部 `#[ignore]` 真库用例。
> **凭据与目标脱敏**：本报告不记录主机地址、账号、口令、连接串或备份引用具体值；这些只经进程内环境变量传递。
> **禁止事项遵守情况**：全程未执行 `DROP DATABASE`；仅对两个可丢弃开发库执行表级 `DROP TABLE` 重置与迁移；未伪造备份、未伪造通过。

## 1. 执行方式

- 每个用例独立进程串行执行，`--ignored --exact <name> --test-threads=1`；
- 每个 identity 用例执行前重置库为 0 表（表级 `DROP TABLE`，保留数据库本身）；
- 用例级超时 60–300s，超时即杀进程并记 `TIMEOUT`，不重试掩盖；
- 指标与迁移用例在 v4 结构上执行。

## 2. 汇总结果

| 套件 | MySQL 8.0.46 | TiDB v8.5.8 |
| --- | --- | --- |
| `mysql_identity` / `tidb_identity`（各 32 项） | 30 PASS / 2 前置不满足 | 28 PASS / 2 前置不满足 / 2 挂起 |
| `migration_v4`（空库 → v3 → v4，16 表无 FK） | PASS | PASS |
| `metrics_real_db`（3 项：分布对账、字节级分组、慢查询不耗尽业务池） | 3 PASS | 3 PASS |

**合计**：72 项真库用例，66 PASS，4 项因环境权限前置不满足未执行成功，2 项为待定位真实挂起。

## 3. 本轮修复（已提交 `13d41b4`）

真库首跑暴露统一根因：identity 夹具停在 schema v3，而生产入口（`bootstrap_admin`、`AdmissionStore::compare_and_set`）的完整性守卫期望编译期 `SUPPORTED_SCHEMA_VERSION = 4`，因此必然出现 `Config("identity data meta.guard")`；同时 v3 夹具路径误用了 v4 启动门禁 `check_identity_schema`，产生 `SchemaNotReady { found: Some(3), required: 4 }`。

修复内容（仅测试代码，未改 `src/`）：

1. v3 语义的夹具校验统一改用 `check_v3_ready`（32 处调用点 + 2 处 import）；
2. 新增 `upgrade_fixture_to_v4` / `assert_v4_fixture_healthy` 与 `fresh_v4`，把 admission、bootstrap、CAS 类用例的夹具经 `run_schema_v4_migration` 升级到当前受支持的 v4（16 表、0 CHECK/FK）；
3. 未删除或弱化任何原子性、事件计数、审计、幂等、拒绝原因断言；`guard` 版本断言按链路等价从 `(1,3)` 更新为 `(1,4)`。

**修复前后**：MySQL 12 → 30 PASS；TiDB 11 → 28 PASS。

## 4. 未通过项与根因

### 4.1 锁观测用例（两引擎各 2 项）——环境权限前置不满足

用例：`admission_deactivation_holds_guard_cas_waits_then_denied_on_fresh_v3_*`、`admission_cas_commits_before_deactivation_then_new_cas_denied_on_fresh_v3_*`。

前置探测由 `task4_participant_pool` 的 `after_connect` 强制校验，实测：

| 引擎 | 前置 | 实测 |
| --- | --- | --- |
| MySQL | `@@performance_schema = 1` | 通过 |
| MySQL | `performance_schema.threads` / `data_locks` / `data_lock_waits` 可 SELECT | **拒绝（1142）** |
| TiDB | `tidb_txn_mode = pessimistic`（session 与 global） | 通过 |
| TiDB | `tidb_config.enable-global-kill = true` | 通过 |
| TiDB | `information_schema.DATA_LOCK_WAITS` 可读 | **拒绝（1227，需 PROCESS 权限）** |

结论：这是**数据库账号权限前置未满足**，不是代码缺陷，也不是可绕过的测试口径。需要 DBA 授予只读观测权限（MySQL `SELECT ON performance_schema.*`；TiDB `PROCESS`）后才能执行。本轮不伪造通过。

MySQL 侧表现为 120s 级挂起（sqlx 池对 `after_connect` 失败反复重建连接），TiDB 侧表现为快速失败（`task4_fixture_operation_failed`），两者根因同一前置。

### 4.2 TiDB 两个 legacy 升级挂起——已定位并加有界保护，仍超时未闭环

用例：`partial_v1_column_is_resumed_only_by_explicit_command`、`tidb_legacy_v2_to_v3_positive_single_case`。

**定位过程（临时埋点，已移除）**：

1. 直接执行 `migrate-identity-test --mode upgrade`（partial-v1 夹具之后）**90s 未返回**；
2. 同时 TiDB 侧 `information_schema.DDL_JOBS` 显示相关 `create table` / `modify column` 全部 `synced`，库内 11 张表已建成；
3. 在 `upgrade_legacy_with_probe` 与 `preflight_legacy_with_probe` / `validate_schema_shape_with_policy` 逐阶段插桩后确认：挂起发生在**某次 `information_schema` 元数据查询**（`SELECT ... FROM information_schema.columns WHERE table_schema=DATABASE() AND table_name=?` 一类），且挂起点随运行漂移（alter 3 / alter 5 / `device_groups` 列读取等），属时间相关而非固定位置；
4. 用 `pymysql` 独立连接手动执行同样的 6 条 `ALTER TABLE ... MODIFY COLUMN`，每条后立即查询 `information_schema.columns`：全部 0.02s 返回。**说明 TiDB 元数据本身可用，问题出在迁移进程经 sqlx 连接池连续 DDL 后的查询路径**。

**已落地的保护（提交 `be339e8`）**：`preflight_with_retry` 以 3s 单次超时、120 次上限、500ms 退避做探测式重试；超限返回明确 `preflight timeout` 错误，把「无限挂起」变成有界失败，绝不把超时当成功。

**当前状态**：MySQL 同路径实测无回归（1.5s 内通过）；TiDB 在 280s 内仍未完成（重试能让进度继续推进，但每次元数据查询阻塞可达数十秒）。该实例的元数据查询不稳定属环境侧，**本项仍未闭环，如实记为失败，不计入通过**。

## 5. 未纳入本轮的用例

- `acceptance_runtime::two_process_real_db_smoke`：恒 `#[ignore]`，函数体为 `unimplemented!`（双进程生产接线尚未就绪），按设计必须显式失败而非 fake 通过，本轮未执行，状态不变；
- 其余 50 余个 `#[ignore]`（MySQL/TiDB 对称、指标、迁移）已在两引擎逐项跑完，未发现新增失败。

## 6. 复现命令（脱敏）

```bash
# 环境变量在进程内传入，绝不回显具体值
export CONTROLLER_TEST_DATABASE_URL='<authorized dev db url>'
export CONTROLLER_TEST_ALLOW_DESTRUCTIVE=1
export CONTROLLER_TEST_EXPECTED_DATABASE='<authorized dev db name>'
export CONTROLLER_TEST_BACKUP_REF='<operator backup reference>'
export CONTROLLER_TEST_MIGRATION_ACK='isolated-exclusive-backed-up-disposable'

cargo test -p rsetup-controller --test mysql_identity -- --ignored --exact <case> --test-threads=1
cargo test -p rsetup-controller --test migration_v4 -- --ignored --test-threads=1
cargo test -p rsetup-controller --test metrics_real_db -- --ignored --test-threads=1
```

## 7. 门禁

- `make fmt-check`：通过；
- `cargo clippy -p rsetup-controller --all-targets -- -D warnings`：通过；
- `cargo test -p rsetup-controller --lib`：926 passed / 0 failed / 3 ignored。
