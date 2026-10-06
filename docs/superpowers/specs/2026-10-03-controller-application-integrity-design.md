# Controller 应用层完整性设计（无外键 / 无 CHECK）

日期：2026-10-03。用户已批准本设计的原则并要求修订规格与实施计划；本文具体迁移和接口细节待书面审阅，不是生产数据库操作许可。

关联：[身份授权](2026-09-23-controller-v1-01-identity-access.md)、[数据/API](2026-09-23-controller-v1-02-data-api.md)、[实施计划](../plans/2026-10-03-controller-application-integrity-tdd.md)。本修订优先于旧计划中“FK 额外保护”“数据库可行时增加 CHECK”和 identity v2 必须存在五个 CHECK 的要求；不改变其他安全关卡。

## 1. 决定与范围

- 新目标 schema 不声明、不依赖 `FOREIGN KEY` 或 `CHECK`。不启用 TiDB `tidb_enable_check_constraint`，不修改全局服务器参数，不通过触发器绕回数据库业务校验。
- 保留主键、唯一索引、`NOT NULL`、JSON/二进制/数值类型、字段长度、用户名 `ascii_bin` 和业务 revision/counter `BIGINT UNSIGNED`。数据库仍负责基础存储限制与并发唯一性，不取消这些保障。
- 应用负责值域、组合、引用和生命周期验证；提交前验证与写入必须处于同一数据库事务，必要时锁定相同保护行和引用对象，并检查 revision/CAS。
- 事务本身不提供业务正确性。必须有显式校验、锁顺序、受影响行数检查和回滚测试。提交前不输出秘密、不发送网络事件；未知提交结果不能直接重试外部副作用。
- 保证覆盖 controller 受控写入入口。直接 SQL、另一个未遵守约定的程序或管理员绕过应用写入不在保证范围；启动检查与读取时校验只能发现污染并失败关闭，不能阻止外部绕过。
- 本补丁只实现现有写入入口的保护、统一校验器和新 schema 迁移。尚未存在的生产账户/角色/grant/组成员/任务写入由各自原计划实现，必须消费本契约；不得把校验器或 fake repository 当这些功能已完成。

## 2. 当前事实（不是新设计的验收）

当前 `0001_identity_devices.sql` 有五个 CHECK，没有外键；`0002_identity_contract.sql` 修订了十个业务计数列与用户名。当前 controller 生产写入包含显式测试迁移、bootstrap 与 AdmissionStore CAS；auth/service 仍含 fake repository，管理和任务完整写入面未就绪。

本轮实测目标为 MySQL 8.0.46 与 TiDB 8.5.8，前者不能替代 MySQL 8.4 LTS 验收。旧套件首例失败，未形成双库全通过结论。已观察到 TiDB 的全局 CHECK 开关关闭，旧表没有 CHECK；这不是允许放弃应用约束的理由。

未提交的元数据字符串 CAST、小写别名与统计数字类型兼容修复仍有独立用途；旧 CHECK 转义规范化补丁不作为新目标 schema 的必需路径。实施前将当前差异冻结并审查，不能混淆暂停前试作与验收后的代码。

## 3. 值域与组合规则

### 3.1 五个原 CHECK 的应用责任

| 原约束 | 应用不变量 | 主要边界 |
| --- | --- | --- |
| `chk_schema_singleton` | schema_meta 恰好一行，singleton=1；schema_version 为受支持版本 | 启动只读检查、显式迁移、bootstrap 保护行获取 |
| `chk_devices_state` | admission_state 仅 PENDING / APPROVED / REVOKED | 数据解码、设备写入、准入 CAS |
| `chk_devices_decision` | review_decision 仅 none / approved / denied / revoked | 数据解码、设备写入、准入 CAS |
| `chk_grants_source` | role 时 role_id 有值且 permissions 为 SQL NULL；direct 时 role_id 为 SQL NULL、permissions 为有效非空权限数组 | grant DTO、迁移数据预检、授权读取、后续 grant repository |
| `chk_grants_scope` | all 时两个 scope ID 均 NULL；group 仅 group_id 有值；device 仅 device_id 有值 | 同上 |

JSON 字面量 `null` 不等于 SQL NULL；空数组、非数组、非字符串元素、重复权限和未知权限均拒绝，不悄悄规范化。权限目录仍为 `device.read`、`device.status.read`、`device.reboot`、`device.task.read`。

准入状态组合也必须合法：`PENDING+none`、`PENDING+denied`、`APPROVED+approved`、`REVOKED+revoked`。状态变更继续使用现有状态机，不能只检查两个枚举各自有效。旧库中发现非法组合时迁移拒绝，不替用户猜测或修复。

用户名规则、布尔值仅 0/1、计数 checked_add、UUID/public_key 长度、秘密处理继续遵循原规格。归档/inactive 的历史父记录合法存在，但不能被用于创建新的有效关联。

### 3.2 引用与生命周期

| 关联 | 新写入检查 | 停用/归档行为 |
| --- | --- | --- |
| sessions → users | 用户存在、active，revision/hash 与已验证身份相符；同事务建立会话 | 停用/改密/重置同事务撤销会话；用户不物理删除 |
| role_permissions → roles | 角色存在且未归档，权限合法 | 角色归档时同事务撤销 role grants，清理 role_permissions；保留角色记录 |
| grants → users / roles / group / device | 用户存在且 active，选中的角色/组/设备存在且未归档；source/scope 互斥 | 角色/组归档删除其 grants；用户停用后 grants 可保留但无效，禁止新授权；设备归档撤销直接 device grants |
| group_members → groups / devices | 两端存在且未归档；输入显式去重并保持固定集合 | 组/设备归档同事务清理对应成员关系 |
| admission_decisions → devices / actor | 设备是本事务锁住的行；非空 actor 必须存在且为有效管理员 | 保留历史；设备、用户归档/停用不物理删除历史父行 |
| audit_events → actor / target | 非空 actor 必须对应已有用户；多态 target 不强制通用实时关联（例如失败登录目标可不存在） | 审计追加且脱敏，不随业务对象归档级联删除 |

读取历史记录不得因父对象停用而误判成孤儿；授权计算必须同时检查其当前有效性。迁移预检校验引用存在性，不把“有效关联创建条件”错误应用于合法历史快照。将来 tasks/runtime 的 actor/device/main/subtask/lock 关联依本规则扩展，在其计划中独立验证。

## 4. 事务与并发协议

### 4.1 固定顺序

受控关系写入与授权敏感写入采用共同顺序：

1. `schema_meta(singleton=1) FOR UPDATE`；核验单例与目标版本。该行同时串行化 authz_epoch/最后管理员保护。首版优先正确性，不承诺高吞吐。
2. users，按 UUID 字节序；再 roles、device_groups（各按 UUID）；再 devices（按公钥字节序）。
3. 对应 grants/group_members/sessions 等依赖行；更新必要 revision、epoch 与审计。
4. 检查条件更新影响行数，提交；之后才触发通知或一次性秘密输出。

不支持数据库写锁时拒绝或通过受控数据库错误返回，不退回内存 mutex。死锁/写冲突只允许有界重跑整个数据库事务并重新校验，不复用旧鉴权判断；网络、输出密码或发送事件不进入重试闭包。

### 4.2 当前入口必须完成的改动

- bootstrap 已锁 initialized 所在行，补完整单例/版本检查，保持并发只初始化一次与提交后一次输出，不因用户表为空重置。
- AdmissionStore CAS 在锁设备前获取共同保护行。actor_id 非空时锁定用户并验证 `active && is_admin && !must_change_password`，同事务内才更新设备和追加历史/审计；缺 actor 拒绝且无任何持久变更。
- actor_id=None 是现有可信系统/传输调用边界，不开放成可由 HTTP 省略字段获得系统权限的路径；更改/限制该内部语义需与隧道计划一起审阅，不因本修订偷偷改变协议。
- 当前没有 grant 管理入口，不新增无鉴权的临时写接口。新增纯校验器先被迁移预检/存量校验消费，原计划 Task 3 的实际写入必须在仓储层复用。

## 5. 存量数据与启动

- 新目标 identity schema_version=3。普通启动只执行 SELECT 元数据/数据检查；空库、v1/v2 或未知版本返回结构化 SchemaNotReady，不 CREATE/ALTER/DROP、不 bootstrap、不监听业务接口。
- v3 启动仍严格核对身份表、列、PK/唯一/普通索引、NULL/类型/字符集、版本单例；目标 CHECK/FK 集合为空。出现未知额外对象拒绝，不静默删除或忽略。固定十一表（`schema_meta`、`users`、`sessions`、`roles`、`role_permissions`、`grants`、`device_groups`、`group_members`、`devices`、`admission_decisions`、`audit_events`）的只读结构检查必须单独枚举 `information_schema.table_constraints` 中 `table_schema=DATABASE()`、`table_name IN (固定十一表)`、`constraint_type='FOREIGN KEY'` 的 `(table_name,constraint_name)`，断言结果空集；不能以 CHECK 数量、PK/UNIQUE 查询或基线 SQL 没声明 FK 代替。元数据查询报错/权限不足、字段缺失或无法可靠枚举时 fail-closed，不把失败当空集；必要时与 `information_schema.key_column_usage` 中同 schema/表且 `REFERENCED_TABLE_NAME IS NOT NULL` 的 `(table_name,constraint_name)` 集合交叉核验，不一致同样拒绝。
- v3 首次启动执行只读数据完整性扫描：原五项规则、用户名/布尔值、grant 权限数据形状、关系孤儿检查。查询只报告规则代码/固定表列，不打印密码、token、真实连接值或用户输入。
- 读取路径仍校验触及的数据，存量扫描不是长期事务保护。非法记录导致 NOT_READY/STORAGE_UNAVAILABLE 类失败关闭；不把非法值转成默认枚举、空权限成功或自动修复。

## 6. 迁移与编号

### 6.1 不重写历史

`0001_identity_devices.sql` 和 `0002_identity_contract.sql` 保持逐字不变。新文件 `0003_identity_application_integrity.sql` 是完整 v3 基线（十一表结构与 v2 业务类型一致，但无 CHECK/FK）；用于空库建表和 v3 形状契约。v1/v2 升级由受控 runner 根据元数据执行固定差量，不将完整 CREATE 文件盲目执行到旧库。

未来尚未实施的 Tasks 迁移顺延为 `0004_tasks.sql`，前置 identity v3、完成后 schema_version=4；普通启动仍使用按版本闭合验证。若实际部署或备份已经占用 0003/0004，停止此默认编号路线，另审；不能仅凭版本号猜是哪份迁移。

### 6.2 显式迁移路径

- 空库：确认目标身份与表数0；执行新 v3 基线，无 CHECK/FK；验证结构后插入 singleton=1/schema_version=3/initialized=false/新 instance_id；普通启动不做这些操作。
- v1：在任何 DDL 前只读验证允许的旧/已升级混合列、基础索引、数据不变量，以及每个现存具名 CHECK 的名称与表达式语义；同时复用 §5 的固定十一表 FK 空集检查，未知 FK 或 FK 元数据不可可靠枚举即拒绝，绝不删除未知 FK。先全部预检，随后复用0002固定十一条 ALTER 的按列恢复逻辑；只移除经预检确认为原始定义的已知具名 CHECK，完整 v3 校验后 CAS 写版本3。
- v2：同样在任何 DDL 前完成数据/元数据、现存 CHECK 语义及同一 FK 空集预检；不重复列 ALTER，只移除经确认的已知具名 CHECK，再最终校验并 CAS 2→3。
- 旧 CHECK 仅接受 `0001_identity_devices.sql` 中原五个表名+约束名+表达式的任意子集。按每条现存约束取得可判定的表达式元数据，并与对应 0001 声明做安全可证明等价比较；只容许经验证的 MySQL/TiDB 元数据重写（例如外层括号、标识符引用、大小写、特定 `_utf8mb4` 引介符及已证实的转义读回格式），SQL 字符串字面量须保留字节敏感，`PENDING` 不等于 `PEND ING`。不能无条件删除空白或改写字面量；同名但表达式被篡改者视为不兼容，首次 DDL 前拒绝，原版本与数据不变。
- 必须区分“可可靠枚举、确认约束确实不存在”和“元数据缺失/NULL/不可读/无法安全比较”；后者一律在 DDL 前 fail-closed，不将未知或无法证明原定义的对象列入 DROP 白名单。旧引擎忽略 CHECK 而真正不存在的已知约束可缺失，不单独视为数据正确：仍须通过逐项应用数据预检。未知 CHECK/FK、不相符列/索引和已占用版本拒绝；不删除未知对象。
- MySQL 使用固定模板 `ALTER TABLE <known_table> DROP CHECK <known_constraint>`；TiDB 使用经真实引擎验证的固定模板（优先显式 `DROP CHECK`，若对应版本只支持 `DROP CONSTRAINT`，按识别的引擎分支）；仅对元数据已确证为 0001 原定义且实际存在的五个具名约束使用源码白名单标识符，不从请求构造标识符。不设置 TiDB 全局开关。
- 每次 DDL 后重读元数据；DDL 不可整体事务回滚。旧版本升级中断后保留旧版本，允许已知 CHECK 子集及旧/新列混合态继续；错误不伪称恢复成功。空库建表中断形成无版本非空库则拒绝自动接管，保留证据，由已授权操作者恢复空库后重试。
- 迁移预检不变更用户密码、不插入 admin、不重置 initialized、instance_id 或业务 revision。真实备份、独占停写、准确目标及现有多确认门禁全部保留；生产 ALTER 仍未批准。

## 7. 验收与完成边界

### Task4 observer 补充（2026-10-03，仅 offline 批准）

用户已选择 `task4_observer_design_approval='批准方案及限定离线实现'`；后续 Task4 并发采用**两条原 writer 业务参与连接 A/B + 一条独立 observer 连接 O**，不把三连接冒称原两连接自观察验收。参见[正式 observer 规格](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/docs/superpowers/specs/2026-10-03-controller-task4-isolated-observer-design.md>)及[详细实施计划](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/docs/superpowers/plans/2026-10-03-controller-task4-isolated-observer.md>)。保留原历史决定、其他任务与生产API、writer权限/parser/callers及全部门禁；只替代Task4测试侧的A自观察约束。MySQL三表SELECT/TiDB PROCESS跨会话元数据风险已知悉；账号权限由操作者另准备，新凭据读取、真实DB、GRANT、reset均另批。有效传输策略/当前窗口TiDB拓扑不能确认即fail-closed，不补造TLS或同集群sysvar。无O凭据可完成offline-ready，但Task4真库验收仍未完成；live任一例失败停两库。

### 原专题验收要求（保留）

- 纯测试：完整五规则正反矩阵、JSON null/SQL NULL、非法状态组合、未知权限与revision溢出；源文件扫描只能辅助防回归，不替代真实行为证据。
- 两引擎独立实测：新空库→v3、v1→v3、v2→v3、旧 CHECK 确实缺失/已知子集的等价元数据读回、非事务中断重试、非法数据在首次 DDL 前无损拒绝、重复升级、只读启动拒旧版本；同名 CHECK 被篡改为非法新表达式在首次 DDL 前拒绝，证明 0 DDL、原版本与数据不变。无 CHECK 且数据合法的旧库允许升级；无法在真实引擎制造的元数据缺失/不可比较反例以 probe/纯比较器验证，明确标记证据类型而不冒称实测。MySQL与TiDB分别记录精确版本、实际用例数与失败数。
- 明确证明数据库没有 CHECK/FK，但受控应用写入仍拒绝非法值/悬空actor；审计写失败回滚设备/历史/epoch；bootstrap 并发只一个账号与一次输出。
- 已授权事务路径之间用 barrier 控制并发测试，不使用 sleep 假定锁先后。未来账户/授权/组成员入口必须覆盖停用↔建会话、归档↔新增关联、撤权↔任务提交的两种串行顺序。
- 不把客户端任意SQL写入视为应用拒绝证据。测试可用直接SQL注入污染，再断言读取/启动/迁移拒绝；这不是对绕过应用SQL的防护承诺。
- 本专题不等于原 controller 01 完成：生产认证仓储、HTTP安全、reset-admin、动态授权管理、设备服务及隧道专家门仍由其原任务继续验收。
