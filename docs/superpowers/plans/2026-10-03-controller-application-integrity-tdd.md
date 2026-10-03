# Controller 应用层完整性 TDD 实施计划

> **致执行代理：** 使用 superpowers:subagent-driven-development 或 superpowers:executing-plans，逐项测试、审查和提交，使用 `- [ ]` 跟踪。本轮修订规格与计划，不是生产 ALTER 许可。

**Goal:** identity schema v3 不声明或依赖外键/CHECK，通过应用校验、引用行锁和事务保证受控写入一致性。

**Architecture:** 保留 PK/UNIQUE/NOT NULL/类型/长度；共享纯校验与只读存量扫描，写入在保护行/引用行锁内校验并 CAS。空库采用完整 v3 基线，旧库显式升级；普通启动只读。

**Tech Stack:** Rust2024/MSRV1.85，现有 SQLx0.8.6、Tokio、serde_json、UUID；不新增依赖。

**Spec:** `docs/superpowers/specs/2026-10-03-controller-application-integrity-design.md`，并阅读01身份授权、02数据/API规格。

## Global Constraints

- 0001/0002迁移逐字保留。identity目标版本3；未来Tasks为0004/版本4。实际部署占用编号时停止默认路线，另审。
- 不CREATE/DROP DATABASE、不启用TiDB全局CHECK、不用触发器替代。目标、独占停写、真实备份和秘密保护遵循AGENTS，只用已授权dev库。
- 保留全部PK/UNIQUE/NOT NULL/JSON/长度、username ascii_bin、业务BIGINT UNSIGNED/u64。元数据字符串CAST AS CHAR+alias，小数字统计字段CAST AS SIGNED+alias，不改变业务类型。
- 库名1..64 ASCII字母数字、下划线/连字符，与actual精确匹配，无test_要求；URL不同不证明隔离。
- DDL不能整体事务回滚，每步重读。普通启动不DDL/写版本，提交前不输出秘密/外发网络，不重试外部副作用。
- RED必须可编译并因目标行为缺失断言失败；连接失败、零用例和ignored不是RED/PASS。源码字符串测试不替代真实库。
- 已知dev版本MySQL8.0.46/TiDB8.5.8，不将8.0证据推广至8.4。生产IdentityRepository/管理HTTP/角色组grant写入尚不存在，本计划不虚构这些功能完成。

## 文件结构与依赖

- 新增 `crates/rsetup-controller/src/integrity.rs`：值域/组合、事务保护行/actor校验、只读存量扫描。
- 新增 `crates/rsetup-controller/migrations/0003_identity_application_integrity.sql`：完整十一表v3基线，与v2业务类型/索引相同，无CHECK/FK。
- 修改 `src/{db.rs,lib.rs,bootstrap.rs,error.rs}`、`src/bin/migrate-identity-test.rs`：版本化结构、受控迁移、现有写入口接线。
- 修改 `tests/{common/mod.rs,mysql_identity.rs,tidb_identity.rs}`：v3夹具、迁移/污染/并发/回滚。
- 修改原01/03计划与02数据规格：后续仓储责任和编号。

Task1→2→3→4→6顺序执行；Task5仅文档，可与Task4并行。共享db.rs的实现串行。命令从隔离worktree执行，CARGO_HOME用本地`.cargo-home`、target用共享缓存，缓存不提交。

## Task 1: 纯应用校验

**Files:** 新增src/integrity.rs，修改src/lib.rs及db.rs解码接线，模块内测试。

**Interfaces:** 新增并导出以下函数，模块导入crate::ControllerError；非法输入返回InvalidArgument，后续数据扫描将其映射为固定规则错误。

```rust
pub fn validate_singleton_ids(ids: &[i8]) -> Result<(), ControllerError>;
pub fn validate_device_fields(state: &str, decision: &str) -> Result<(), ControllerError>;
pub fn validate_grant_fields(source: &str, has_role: bool,
 permissions: Option<&serde_json::Value>, scope: &str,
 has_group: bool, has_device: bool) -> Result<(), ControllerError>;
```

- [ ] **Step1 RED：** 写以下测试，先用宽松可编译基线观察非法输入误接受，不以缺函数编译失败充数。

```rust
#[test]
fn application_shapes_reject_invalid_combinations() {
 use serde_json::json;
 assert!(validate_singleton_ids(&[1]).is_ok());
 for ids in [&[][..], &[2][..], &[1,2][..]] {
  assert!(validate_singleton_ids(ids).is_err());
 }
 for (state,decision) in [("PENDING","none"),("PENDING","denied"),
   ("APPROVED","approved"),("REVOKED","revoked")] {
  assert!(validate_device_fields(state,decision).is_ok());
 }
 assert!(validate_device_fields("DENIED","denied").is_err());
 assert!(validate_device_fields("APPROVED","none").is_err());
 let p=json!(["device.read"]);
 assert!(validate_grant_fields("role",true,None,"all",false,false).is_ok());
 assert!(validate_grant_fields("direct",false,Some(&p),"device",false,true).is_ok());
 assert!(validate_grant_fields("direct",true,Some(&p),"all",false,false).is_err());
 assert!(validate_grant_fields("role",true,None,"all",true,false).is_err());
 for bad in [json!(null),json!([]),json!({}),json!([1]),json!(["unknown"]),
   json!(["device.read","device.read"])] {
  assert!(validate_grant_fields("direct",false,Some(&bad),"all",false,false).is_err());
 }
}
```

- [ ] **Step2 运行：** `cargo test --locked -p rsetup-controller integrity::tests::application_shapes_reject_invalid_combinations`，记录目标断言RED。
- [ ] **Step3 GREEN：** singleton仅[1]，device四个合法tuple，source与scope穷尽match；direct为非空字符串数组、四项权限目录内且不重复，SQL None不同于JSON Null。`decode_snapshot`复用规则，拒绝各枚举合法但组合非法的行；状态转换不放宽。
- [ ] **Step4 验证：** 补全source/scope各NULL互斥、空值/大小写/未知枚举/重复权限反例，controller普通单测全绿。
- [ ] **Step5 提交：** 显式暂存本Task文件，提交`feat(controller): validate application integrity shapes`。

## Task 2: v3结构与只读数据扫描

**Files:** 新增v3基线SQL，修改src/{db.rs,integrity.rs,lib.rs}及模块内测试。

**Interfaces:** 消费Task1；新增`pub async fn check_identity_data(db: &DbPool) -> Result<(), ControllerError>`；保留check_identity_schema签名，目标常量`IDENTITY_SCHEMA_VERSION: i32 = 3`，结构通过后调用数据扫描。新增私有`async fn require_no_identity_foreign_keys(db: &DbPool) -> Result<(), ControllerError>`，由`validate_schema_shape`的LegacyForExplicitUpgrade/ApplicationV3两种策略共同调用；仅查询元数据，不修改库。

- [ ] **Step1 RED：** 现有版本测试改为v2返回SchemaNotReady{found:Some(2),required:3}、v3进入结构/数据校验，当前只认2的实现应失败。基线测试先用旧目标契约，断言无CHECK/FK失败，不能以缺文件作为RED。对可编译的纯TDD metadata probe 增加 `foreign_keys: Vec<(table_name,constraint_name)>` 和 `foreign_keys_error`/不可用分支：分别模拟一条额外FK、元数据读失败及可靠空集，断言前两者在v3形状检查与Legacy迁移预检均拒绝且不能进入DDL、空集可继续；旧只检查CHECK的实现应在目标行为断言上RED，不能把连接失败/元数据查询失败当空集或当作RED证据。
- [ ] **Step2 GREEN：** 新SQL显式列出十一表，v2列/索引保持但无五个CHECK，不加FK、不INSERT元数据。DDL解析接受选定静态契约；策略枚举LegacyForExplicitUpgrade/ApplicationV3：前者的形状策略仅允许原五个已知具名CHECK的子集，但旧库迁移还须在任何DDL前按Task3验证每个现存表达式与0001声明安全等价；后者CHECK为空，未知对象仍拒绝。不得把“名称属于子集”当作可DROP的充分条件。`require_no_identity_foreign_keys`在两种策略中对固定十一表执行独立只读元数据查询（表名是源码固定集合，`DATABASE()`绑定当前schema，不能仅从现有CHECK分支推断FK）：

```sql
SELECT CAST(table_name AS CHAR) AS table_name, CAST(constraint_name AS CHAR) AS constraint_name
FROM information_schema.table_constraints
WHERE table_schema = DATABASE()
  AND table_name IN ('schema_meta','users','sessions','roles','role_permissions',
    'grants','device_groups','group_members','devices','admission_decisions','audit_events')
  AND constraint_type = 'FOREIGN KEY';
```

`(table_name,constraint_name)`必须都非NULL且属于固定表，结果必须空；元数据不可用、权限不足、列缺失、解码失败一律固定错误fail-closed，不能用`unwrap_or_default`或查询失败时当无FK。与原形状探测用同一连接身份确认当前DATABASE()与目标一致、十一表可见；单个空结果不能单独证明元数据权限完整，权限不能证明时拒绝，并在获授权隔离dev环境用真实FK验证可见性。必要时增加同schema、同固定十一表的交叉核验，核对去重后的 `(table_name,constraint_name)` 集合；两次查询任一失败或集合不一致都拒绝（复合FK在key_column_usage有多行，去重后再比较；不把PRIMARY/UNIQUE当FK）：

```sql
SELECT CAST(table_name AS CHAR) AS table_name, CAST(constraint_name AS CHAR) AS constraint_name
FROM information_schema.key_column_usage
WHERE table_schema = DATABASE()
  AND table_name IN ('schema_meta','users','sessions','roles','role_permissions',
    'grants','device_groups','group_members','devices','admission_decisions','audit_events')
  AND referenced_table_name IS NOT NULL;
```

- [ ] **Step3 数据扫描：** 固定查询结果调用Task1；只报告固定规则代码，不打印行值或连接信息：

```sql
SELECT singleton FROM schema_meta ORDER BY singleton;
SELECT admission_state,review_decision FROM devices;
SELECT source_kind,role_id IS NOT NULL AS has_role,
 CAST(permissions AS CHAR) AS permissions_json,scope_kind,
 scope_group_id IS NOT NULL AS has_group,scope_device_id IS NOT NULL AS has_device FROM grants;
SELECT 1 FROM sessions s LEFT JOIN users u ON u.id=s.user_id WHERE u.id IS NULL LIMIT 1;
SELECT 1 FROM role_permissions p LEFT JOIN roles r ON r.id=p.role_id WHERE r.id IS NULL LIMIT 1;
SELECT 1 FROM group_members m LEFT JOIN device_groups g ON g.id=m.group_id
 LEFT JOIN devices d ON d.public_key=m.device_id WHERE g.id IS NULL OR d.public_key IS NULL LIMIT 1;
SELECT 1 FROM grants x LEFT JOIN users u ON u.id=x.user_id
 LEFT JOIN roles r ON r.id=x.role_id LEFT JOIN device_groups g ON g.id=x.scope_group_id
 LEFT JOIN devices d ON d.public_key=x.scope_device_id
 WHERE u.id IS NULL OR (x.role_id IS NOT NULL AND r.id IS NULL)
 OR (x.scope_group_id IS NOT NULL AND g.id IS NULL)
 OR (x.scope_device_id IS NOT NULL AND d.public_key IS NULL) LIMIT 1;
SELECT 1 FROM admission_decisions a LEFT JOIN devices d ON d.public_key=a.device_id
 LEFT JOIN users u ON u.id=a.actor_id
 WHERE d.public_key IS NULL OR (a.actor_id IS NOT NULL AND u.id IS NULL) LIMIT 1;
SELECT 1 FROM audit_events a LEFT JOIN users u ON u.id=a.actor_user_id
 WHERE a.actor_user_id IS NOT NULL AND u.id IS NULL LIMIT 1;
```

同一步扫描users用户名（复用valid_username）、所有布尔列0/1、role_permissions权限目录。内部`validate_identity_rows(db,require_meta:bool)`在空库建表未插单例时false、启动true；旧库扫描只检查单例和数据，版本由外层迁移状态机处理，不能递归调用只认3的startup。inactive/归档父记录仍存在，不把合法历史错报孤儿。

- [ ] **Step4 验证：** fake probe断言版本错0次shape/data调用，v3检查错则bootstrap/监听计数0；真库污染/无写证据在Task6，不由fake替代。
- [ ] **Step5 提交：** 单测与fmt全绿，显式暂存本Task文件，提交`feat(controller): define check-free identity schema v3`。

## Task 3: 显式迁移与非事务恢复

**Files:** 修改src/db.rs、src/bin/migrate-identity-test.rs、tests/common/mod.rs和双库wrapper。

**Interfaces:** 保留TestMigrationConfig多确认。TestMigrationMode保留Upgrade/FixtureV1/FixturePartialV1，新增FixtureV2，仅dev夹具。私有DbEngine::{Mysql,Tidb}经SELECT VERSION识别，未知拒绝；新增drop_known_legacy_checks(db:&DbPool,engine:DbEngine)，普通main不调用。

- [ ] **Step1 RED：** fresh升级目标断言3（旧实现2失败），v2升级后无CHECK断言（旧实现保留CHECK失败）；另以旧版本合法数据和同名但表达式改为非法新表达式的CHECK构造迁移反例：预检必须在首次ALTER/任何DDL前拒绝，断言0DDL、0版本写且原版本/完整数据快照不变，旧仅比名称的实现应失败。补已知CHECK任意子集的安全等价MySQL/TiDB元数据格式允许通过、无CHECK但数据合法允许通过；`PENDING`→`PEND ING`等字面量改变必须拒绝。必须清洁已授权fixture上的业务断言；真库fixture无法模拟无信息元数据时以probe/纯比较器反例覆盖，不能伪装成实测。
- [ ] **Step2 预检：** 在任何DDL前先版本/闭合形状（含复用Task2的`require_no_identity_foreign_keys(db)`，v1/v2与v3同样要求固定十一表FK为空），再枚举每个实际存在的具名CHECK，取得可判定的表达式并与对应0001原声明安全比较，同时完成引用、用户名、负计数等全部数据规则。允许经可靠枚举确实不存在的已知CHECK（包括全无CHECK），但元数据缺失/NULL/不可读/无法安全比较者拒绝，绝不将其解释为缺失。未知FK/CHECK、同名篡改CHECK拒绝；不生成任何`DROP FOREIGN KEY`，只有原始语义确证且实际存在的五个具名CHECK可供后续白名单DROP。失败必须0DDL/0版本写/0密码变更，原版本和数据不变。
- [ ] **Step3 GREEN：** 实现固定状态机，每个DDL后重读，不将完整CREATE文件直接套到旧库：

```text
None + 0 tables -> create v3 baseline -> validate shape/rows(require_meta=false)
 -> insert singleton1/version3/initialized=false/fresh instance_id -> readonly full validation
v1 -> validate legacy/mixed shape+rows -> replay remaining fixed 0002 ALTERs
 -> drop present known CHECKs -> validate v3 shape/rows -> CAS version1 to3
v2 -> validate v2 shape+rows -> drop present known CHECKs
 -> validate v3 shape/rows -> CAS version2 to3
v3 -> readonly full validation only
otherwise -> refuse
```

白名单：(schema_meta,chk_schema_singleton)、(devices,chk_devices_state)、(devices,chk_devices_decision)、(grants,chk_grants_source)、(grants,chk_grants_scope)。仅对预检确认其表达式安全等价于0001原声明的现存具名CHECK执行DROP。可复用/收紧当前db.rs的`normalize_check`/`check_clause_matches`，只规范化经证实无语义差异的外部格式（括号、引用、大小写、特定`_utf8mb4`引介符和已证实的完整转义读回）；SQL字面量字节敏感，不得因无条件消空格将`PENDING`和`PEND ING`视作相等。比较器必须fail-closed拒绝未知格式、NULL/缺失表达式，不可用名称匹配取代语义匹配；v3目标结构校验仍不要求CHECK文本归一化。MySQL固定ALTER TABLE ... DROP CHECK ...；TiDB按真实版本验证同语法，若该版本仅支持DROP CONSTRAINT则使用识别引擎的固定分支。标识符全部源码白名单，不来自请求。不启用全局开关。

- [ ] **Step4 恢复：** 删1/2项后中断保留旧版本，重跑跳过已删项；沿用0002列混合恢复；无版本非空库拒绝自动接管。版本CAS恰好1行。旧initialized/instance_id/hash/revision逐项不变。fixture-v1/v2只建旧布局，TiDB无CHECK为显式旧fixture变体，仍全量数据预检。
- [ ] **Step5 提交：** 普通测试及受控迁移单例通过，显式暂存本Task文件，提交`feat(controller): migrate identity to application integrity`。

## Task 4: 现有写入口的事务保护

**Files:** 修改src/{integrity.rs,db.rs,bootstrap.rs,error.rs}与三测试文件。

**Interfaces:** 新增内部函数：

```rust
pub(crate) async fn lock_integrity_guard(
 tx:&mut sqlx::Transaction<'_,sqlx::MySql>) -> Result<(),ControllerError>;
pub(crate) async fn require_active_admin(
 tx:&mut sqlx::Transaction<'_,sqlx::MySql>,actor:[u8;16]) -> Result<(),ControllerError>;
```

guard执行SELECT singleton,schema_version FROM schema_meta ORDER BY singleton FOR UPDATE，恰一行singleton1/version3。actor固定users主键FOR UPDATE，缺失NotFound，active/is_admin/!must_change_password不满足返回新增ControllerError::PermissionDenied（固定消息，不含账号）。

- [ ] **Step1 RED：** 新共享用例使用已有common的unique_public_key/counts；当前错误接受不存在actor应触发断言：

```rust
pub async fn admission_missing_actor_is_atomic(db:&DbPool) {
 use rsetup_controller::{AdmissionState,AdmissionStore,ReviewDecision};
 let key=unique_public_key();
 sqlx::query("INSERT INTO devices (public_key,display_name,admission_state,review_decision,revision,archived) VALUES (?,'actor-test','PENDING','none',1,FALSE)")
  .bind(key.as_slice()).execute(&db.0).await.unwrap();
 let before=db.load(key).await.unwrap(); let prior=counts(db,&key).await;
 let missing=*uuid::Uuid::new_v4().as_bytes();
 let r=db.compare_and_set(key,1,AdmissionState::Pending,ReviewDecision::Approved,Some(missing),None).await;
 assert!(r.is_err()); assert_eq!(db.load(key).await.unwrap(),before);
 assert_eq!(counts(db,&key).await,prior);
}
```

双库wrapper先fresh/upgrade再调用该函数，不以迁移失败充actor校验RED。

- [ ] **Step2 GREEN：** CAS顺序guard→可选actor user→device→原CAS/history/audit；bootstrap先guard再原初始化事务。actor=None保留内部可信系统语义，不允许HTTP省略actor取得该权限。
- [ ] **Step3 负向：** 缺actor/inactive/非admin/must_change分别断言设备/历史/审计无变更，保留审计碰撞回滚。直接SQL污染状态组合后load/CAS/startup拒绝，不自修复。
- [ ] **Step4 并发：** 两连接+barrier：停用fixture先持guard/users锁→CAS等待→提交后CAS拒绝；反序CAS先提交→停用后新CAS拒绝。fixture遵守锁序，不用sleep猜先后。bootstrap并发只一次输出、提交后sink失败不重显仍回归。
- [ ] **Step5 提交：** 普通及两库单例全绿，显式暂存本Task文件，提交`fix(controller): validate references inside mutation transactions`。

## Task 5: 后续消费者契约（不新增不存在的写入口）

**Files:** 原01/03计划与02数据规格。**Consumes:** Task1规则、Task4锁序、设计引用矩阵。

- [ ] **Step1** 搜索FK/CHECK、“可行时加约束”、0003_tasks/v2→v3，区分历史说明和当前要求。
- [ ] **Step2** 01 Task2B建session在guard/users锁内重验active/revision，停用/改密同事务撤销；Task3角色/grant复用校验及引用锁、epoch/audit；Task4组设备归档清理成员/grants但保留历史父行。统一guard→users→roles→groups→devices→依赖行，各同类型按ID排序。
- [ ] **Step3** Tasks全部路径改0004、identity3→runtime4；任务提交/撤权共享guard、设备锁唯一性保留；runtime无FK/CHECK，原协议和HTTP安全关卡不降低。
- [ ] **Step4** grep版本/path一致、git diff --check，显式暂存文档，提交`docs(controller): align downstream application integrity contracts`。

## Task 6: 双库验收矩阵

**Files:** 三测试文件；结果产生后新增`docs/testing/controller-application-integrity-results.md`。本地secret、派生配置和临时runner不强制入库。

- [ ] **Step1** 既有23例更新v3夹具/版本断言；保留bootstrap/CAS回滚/u64高半区/溢出/十列负值。测试元数据column_type也CAST AS CHAR。`identity_unsigned_high_half_round_trip` 中 direct grant 的 permissions 从空对象`{}`改为合法非空数组`["device.read"]`，相关用户/角色/组/设备父记录先建立；不要把旧fixture自身不符合新规则误报成数值溢出缺陷。负值仍在第一次DDL前拒绝，不能因移除CHECK删测试。
- [ ] **Step2** 新矩阵：fresh无FK/CHECK；v1/v2有/无/部分CHECK升级；重复/只读重启；未知FK/CHECK/索引拒绝；中断恢复；singleton/device组合/grant互斥/权限/JSON错误及全部关系孤儿首次DDL前拒绝；actor失效/并发停用；基础unique/NOT NULL仍由DB拒绝。独立未知FK真库反例只在已确认授权、隔离、可丢弃的指定dev库之fixture专用空库中运行，先满足目标精确匹配、独占停写/锁、真实备份和多确认门槛；runner先reset-tables并构造合法v2固定十一表，随后只做表级`ALTER TABLE sessions ADD CONSTRAINT fk_fixture_sessions_user FOREIGN KEY (user_id) REFERENCES users(id)`（现有两表、固定标识符；不DROP DATABASE），记录FK注入后版本、完整固定表排序行快照及metadata。调用迁移须在首次迁移DDL前拒绝，断言迁移0DDL、0版本写、原版本/完整行快照和FK等metadata不变；另逐例reset-tables建立合法v3夹具、注入同样FK，断言只读启动拒绝且完整快照不变。fixture建FK属于测试准备DDL，不计入被测迁移0DDL；不得在普通/生产库注入。FK注入负例仅在测试进程正常、断言全部通过后，由受控`finally`阶段清除表级fixture，或由runner在下一例开始前按门槛执行表级`reset-tables`；不由应用迁移删除未知FK。断言失败、panic、timeout、中断或清理失败时不再尝试破坏性DROP，保留脱敏诊断与实际库状态并停止该引擎，标记隔离库待人工核验/授权表级reset后才能继续；不承诺进程崩溃后无残留，不自动`DROP DATABASE`或修改集群全局开关。TiDB语法与视图兼容性未实测时按引擎停下并标未验证。
- [ ] **Step3** common中新增固定夹具契约：

```rust
enum IntegrityViolation {
 Singleton,DevicePair,GrantSource,GrantScope,GrantPermissions,
 SessionUser,RolePermissionParent,GrantUser,GrantRole,GrantGroup,GrantDevice,
 GroupMemberGroup,GroupMemberDevice,AdmissionDevice,AdmissionActor,AuditActor,
}
async fn inject_violation(db:&DbPool,case:&IntegrityViolation);
async fn assert_upgrade_preserves_invalid_fixture(db:&DbPool,case:&IntegrityViolation);
```

每variant用固定SQL+随机fixture ID、依设计§3构造唯一坏条件并建立其余合法父行，不接自由SQL。前后对比version、metadata和固定表完整排序行快照（仅进程内，不打印秘密），不只count。双库wrapper均ignored，命名需安全runner能识别。

- [ ] **Step4** runner逐引擎逐例在确认前例正常完成且清理/重置状态可信后执行表级`reset-tables`，再运行`--ignored --exact CASE --nocapture --test-threads=1`；用Cargo JSON artifact路径。必须returncode0、唯一汇总1passed/0failed/0ignored/0measured/filtered=total-1。FK注入用例正常且断言通过时按Step2受控清理；任一断言失败、panic、timeout、中断或清理失败时，保留脱敏白名单诊断和实际fixture/库状态、停止该引擎，不在失败后自动reset或继续下一例。下一轮必须先人工核验隔离库实际状态，确认获授权的表级reset已完成且恢复目标精确匹配、独占停写/锁、真实备份和多确认门槛后才能继续；建表成功不等于用例通过。
- [ ] **Step5** 回归：fmt全workspace、cargo test --workspace --locked、clippy --workspace --all-targets --locked -- -D warnings、cargo +1.85.0 test -p rsetup-controller --locked、aarch64全workspace check、node --test ui/*.test.mjs。普通回归不替代ignored真库。
- [ ] **Step6** 报告精确版本/每case结果/非零总数、无FK/CHECK证明、启动无写/保留旧数据/并发回滚证据。8.4缺环境标未验证，不宣称controller整项目或生产批准。扫描tracked/index/diff无秘密后仅提交测试及去敏报告，提交`test(controller): verify application integrity on dev engines`。

## 完成标准

六任务都有独立证据与审查；新schema无FK/CHECK，校验器已接当前写入口；历史文件不变、升级可恢复、普通启动不写；两引擎适用用例全部执行且无失败。任一引擎失败/未跑不标阶段完成；本专题不替代原01后续生产repository/API、03运行期和协议专家门。
