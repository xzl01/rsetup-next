# Controller Task4 隔离 observer 正式规格

日期：2026-10-03。状态：`task4_observer_design_approval = '批准方案及限定离线实现'`。本次批准只限 offline；本文落地方案及限定离线实现的契约，不是新凭据读取、真实连接、reset 或账号管理许可。本次文档任务不实现代码、不运行 cargo。

关联：[原完整性规格](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/docs/superpowers/specs/2026-10-03-controller-application-integrity-design.md>)、[原 Task4 计划](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/docs/superpowers/plans/2026-10-03-controller-application-integrity-tdd.md>)、[详细实施计划](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/docs/superpowers/plans/2026-10-03-controller-task4-isolated-observer.md>)、[历史提案](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/.superpowers/sdd/2026-10-03-controller-application-integrity-tdd/task-4-isolated-observer-proposal.md>)。本文将历史提案的待批准方向转为已批准的离线设计，不重写历史提案或把其真实前提视为已满足。

## 1. 不变边界与批准范围

- 每个原目标保留原 writer 身份、原授权/备份/migration gate、生产 `AdmissionStore::compare_and_set` 与所有生产 API。A/B 是同一原业务身份的两条物理参与连接；O 是每目标各自独立只读诊断身份的一条连接。不是三个 endpoint，也不是强制新建两个 writer 账号。fixture/migration pool 不计作参与者，不宣称整个进程只有三连接。
- 原 writer parser、migration parser、其旧 callers 及旧模式语义逐字保持。尤其原 writer 对 `WITH GRANT OPTION` 的既有处理不借机整理；observer 的严格拒绝规则不回灌 writer。
- MySQL 三张诊断表 SELECT 可见其他 schema/用户线程、锁及语句；TiDB PROCESS 可跨 schema/用户/节点观察诊断信息且 DATA_LOCK_WAITS 有集群采集开销。用户已知悉这些风险；本工具只取必要列不等于 DB 行列隔离。
- 账号、权限、来源限制、有效期与撤权由操作者另准备。代理不创建账号、不执行 GRANT/REVOKE，不提供假授权记录。文件存在、独立用户名、配置 pin、离线 fake 返回 passed 均不是权限/执行授权。
- 新身份配置读取、真实只读 preflight、真实 reset/迁移/并发执行分别等待用户明确授权。以前的 writer/dev 授权不自动延伸为本轮 live 许可。不得读取任何真实 secret、自动发现 O、自动补供 O 身份或自动 repin。
- Rust2024/MSRV1.85，SQLx0.8.6；不新增依赖。仅测试支持模块和本地 ignored runner/probe 接线；无生产公开 API，无生产 pause seam，无泛重构。
- 所有 ignored 工具保持本地；禁止 `git add -f`。实现者不得 stage/commit；父协调代理只在审查后提交 tracked 模块/文档，ignored 工具变动通过本地去敏报告留档。

## 2. 已读实现事实及职责拆分

[workspace manifest](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/Cargo.toml>) 和 [controller manifest](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/crates/rsetup-controller/Cargo.toml>) 已有 SQLx、Tokio、serde_json、sha2；[本地 probe manifest](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/.superpowers/dev-db-probe/Cargo.toml>) 同样已有这些依赖，另有 libc。共享模块只用交集依赖和 std，不为 derive 增加 serde、不为文件检查给 controller 增加 libc。

[probe](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/.superpowers/dev-db-probe/src/main.rs>) 的 writer parser 位于 204–278，现有 `load` 只构造 host/port/user/password/database，`lock_connect_options` 已关闭 SQLx 初始化 SET。[runner](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/.superpowers/dev-db-probe/run_dev_tests.py>) 的 `probe()` 强制 writer `direct_database_all`，不能接收 O 结果；`execute()` 当前 engine 内捕获错误后可继续另一引擎，本次 observer 批次须显式改为首失败停两库。[common](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/crates/rsetup-controller/tests/common/mod.rs>) 已有约1855行，A/B callback 还读取锁视图，wait helper 还依赖 `DATABASE()`；只增 O 而不移走这些查询会继续失败。

限定新增职责：

| 模块（拟新增） | 职责与依赖 |
|---|---|
| `tests/support/observer/mod.rs` | test-only 类型/内部可见性汇总；不由 src/lib 导出 |
| `tests/support/observer/grants.rs` | 独立封闭 grammar、CURRENT_USER/roles 验证 |
| `tests/support/observer/config.rs` | 单次 bytes 双 pin、strict JSON、无默认库 options；接收受控打开的 fd，不自行搜索秘密路径 |
| `tests/support/observer/session.rs` | dedicated O、身份/角色/实际扫描、deadline 与固定错误 |
| `tests/support/observer/queries.rs` | 固定 SQL 与 exact schema/ID/start_ts 关系 |
| `tests/observer_contract.rs` | 无凭据无网络的契约/async fake 测试入口 |
| `tests/common/task4_concurrency.rs` | 仅移出原 Task4 并发 helpers，接 O；保留原 fixture/生产 CAS |
| `.superpowers/dev-db-probe/src/observer.rs` | 本地新 `observer-capabilities` 模式薄适配；path 引入共享模块 |
| `.superpowers/dev-db-probe/observer_runner.py` | 私有读取、授权绑定、fd/cleanenv、O 结果协议与 case 分类 |
| `.superpowers/dev-db-probe/test_observer_runner.py` | fake 进程/配置/拓扑与 reset 顺序测试 |

共享通过 `#[path]` 编译进 integration test 和 ignored probe，不建 crate、不改 manifest/lockfile、不增生产 public API。各模块约束为职责必要拆分；不把新增500行继续压入 common/main。原其他测试 helpers 不搬迁。编译单元可有窄的 `pub(crate)`，不等于库对外接口。

## 3. 权限、身份、角色闭合接受条件

O grammar 与 writer grammar 是两套实现、两套测试。只接受下列完整集合，行序可换：

- MySQL：`SELECT` on exact `performance_schema.data_lock_waits`、`performance_schema.data_locks`、`performance_schema.threads` 各一次；可选一次 `USAGE ON *.*`。不得少表、多表、重复、schema wildcard、column grant 或 mixed privileges。
- TiDB：exact `PROCESS ON *.*` 一次，可选一次 USAGE；不加 `mysql.tidb` SELECT、不加业务权限、全局 SELECT 或 SUPER。
- 使用被审查的 SHOW GRANTS 序列化格式：大写固定关键字、单空格、反引号 exact 表名，account 两段可为一致单引号或反引号；仅接受无转义的非空 ASCII account atom（字母数字、`_ . % - :`），两段 quote 完整。未知 escaping、quote 组合或实际账号超出该子集即 shape_rejected，另审而非自动接受。`CURRENT_USER()` 必须等于已授权的精确 user@host；user/host 分开保存在私有授权输入以免分隔歧义，且不得与 A/B 实际 CURRENT_USER 相同。
- identity 门禁的 writer 必须来自实际连接成功查询 `CURRENT_USER()` 并非 NULL 解码的结果，查询/NULL/解码失败不得 fallback；O3 caller 必须核对 A/B 为同一实际 writer 或对两者分别验证 O（此为 caller 验收义务，不表示 O3 已实现）。
- NULL、空集、未知行、注释、截断、尾空白、第二语句、REVOKE、roles/proxy/dynamic grants、WITH GRANT OPTION/ADMIN OPTION 一律拒绝。不以 substring 包含 SELECT/PROCESS 决定。
- O 固定读 `CURRENT_ROLE()`，只接受对应版本确定的无角色值 `NONE`；MySQL 再读 `@@GLOBAL.mandatory_roles` 必须是空字符串。未知/NULL/查询失败一律拒绝；TiDB 不臆造 mandatory_roles sysvar，操作者绑定当前窗口确认没有外部/强制权限注入。grants 出现授予但未激活角色也拒绝。
- SHOW GRANTS、身份、角色、真实扫描是四个独立门禁；文本通过不是能力通过。错误分别固定分类 `grants_query_error`、`decode_error`、`shape_rejected`、`identity_mismatch`、`roles_unverified`。不输出 raw grant/账号/SQLx error。

## 4. 私有配置、双 pin 与有效传输策略

O 私有 JSON 唯一字段为 `username`、`password`，均非空字符串；duplicate keys、未知字段、host/port/URL/database/TLS/engine 等覆盖一律拒绝。不把授权确认或 topology 凭据塞进 O secrets。原 writer raw bytes 与 O raw bytes 各自 SHA-256 全长小写 pin；pin 来自本次明确授权，不能读取后自行认定授权。两次消费都针对同一份读到的 bytes 校验再解析；替换/轮换立即拒绝。

Linux 本地 runner 用 Python stdlib `os.open` 的 directory fd/`dir_fd`、`O_NOFOLLOW|O_NONBLOCK` 和 `fstat` 校验文件与私有目录的 owner=当前 euid、目录0700/文件0600或更严、regular file、路径组件无 symlink；祖先不可信可写组件拒绝。目录 fd 锚定，读取时不靠先 stat 后任意重新打开路径。大小有界（每份至多64KiB），读取后比较 inode/size/mtime，任何漂移拒绝。仅授权后才打开真实文件。

新 observer 子进程用显式 `pass_fds` 传已验证的 writer/O 描述符引用及双 pin。**经用户追加批准的可执行文件固定：**受控父进程还须独立确认当次 Cargo 构建的 probe artifact 路径、SHA-256、owner/mode/inode 及不可由未受信主体更改的部署位置；父进程在 `Popen` 前以 no-follow 已核验的 executable fd 固定 inode，第三份 `pass_fds` 只用于 `executable=/proc/self/fd/<artifact_fd>` 执行该 inode，argv 仍为 `[binary,engine,observer-capabilities]`。这消除路径重命名/替换窗口，**不能**阻止同 UID 对可写 inode 的原位改写；当前 worktree/target 组可写，不得放宽校验或 chmod 既有目录来称 live 安全。artifact fd 的数字只在本次受控子进程的专用 `RSETUP_OBSERVER_ARTIFACT_FD` 环境字段传递，供 Rust 在凭据/授权核验后立即接管并关闭；它不含秘密或路径，也不能从继承环境自行作为授权来源。artifact fd 与两份 O 配置 fd 必须在新子进程任何 migration child 启动前关闭；旧模式仍无新增 pass_fds。本次授权绑定的必要metadata（expected account、run/窗口、双pin、传输/拓扑核验引用、操作范围）另由runner经专用stdin传入，不含密码/URL、不接受配置fallback。没有操作者有效记录时runner拒绝生成该envelope；envelope只是当前父进程转交的上下文，不是自签授权或数据库权限证明。**经用户追加批准的 O2 所有权修订：**共享 Rust loader 的入口接收两份已拥有的 `OwnedFd`，不接收裸 `i32`，从入口开始由 RAII 保证包括 `/proc` 校验失败在内的每条返回路径关闭两份已取得的 fd。只读 regular、不同且有效的描述符必须在受控子进程的**唯一**原始整数转换边界验证；该边界由后续受控 probe 与 ignored test 子进程入口接线/单独审查，转换前验证失败必须立即终止该子进程、由父进程监督回收，禁止继续 spawn/migration，不能把此种内核进程回收冒称为 loader 返回时关闭。转换必须具有明确的独占移交、无并发 close/reuse 安全前提，不把 `/proc` 证明当作所有权证明；已转换为 `OwnedFd` 后不再用第二次 raw 接管。loader 从 fd seek 到起始、限长读同一 bytes、验证 regular metadata 和双 pin；缺 fd/非法编号/相同号由受控 raw 边界 fail-closed，不从 stdin/路径/继承 URL fallback。配置读取在 wrapper 的任何 migration child 启动前完成并关闭 inherited fd，migration/reset/cargo/list 子进程既不带引用、pin，也不带打开的 O fd。共享纯 bytes 逻辑不依赖 libc；唯一 `unsafe FromRawFd` 只在受控子进程原始接管边界，单独审查。普通 run_command 保持默认 close_fds；新 observer 适配器才可使用 pass_fds。本修订是离线实现接口更正，不授权凭据读取、真实连接或 reset。

O options 从已 pin 的原连接字段按**现有构造语义**在内存建立，替换身份且不调用 `.database(...)`。不能 clone 后 `.database("")`：SQLx0.8.6 没有公开 unset API，空字符串仍是 Some。新对象 `get_database()==None`，实际 `DATABASE() IS NULL`；无 socket/环境 DSN/第三 endpoint。只接受当前已支持的原配置形状，发现额外传输字段不能忽略后猜测。

现链路没有显式 TLS fields，SQLx 默认 `Preferred` 可回退明文，**不是 exactTLS 已具备**。保留 A/B 原连接 options，不悄悄加强或放宽 writer。O 在当前支持的原配置上保持同样 SSL mode/host/default certificate 语义，只关闭其初始化 SET（`pipes_as_concat(false).no_engine_substitution(false).timezone(None).set_names(false)`）及 statement logging。新 credentials/live 授权必须明确当前有效传输策略及部署可信边界；要求验证证书/ServerName等而旧形状无法表达/核验时为 external blocker，另审字段/支持，不能本次补造 TLS、宣称 options 相等就是实际链路安全证明。SSL_CERT_FILE/SSL_CERT_DIR 等既有 SAFE_ENV 项不能任意漂移；窗口授权必须绑定有效环境，O 不额外读取新证书秘密。

## 5. 同目标的两层证据

**MySQL：** preflight 及实际 A/B/O 物理会话重新验证 exact VERSION `8.0.46`、有效 ID、非空 `@@GLOBAL.server_uuid`；UUID相同且与本次目标核验绑定，三 ID 两两不同。A/B DATABASE 精确 expected_schema，O 为 NULL；threads 对 A/B 映射各唯一。保持 A/B 单连接池禁 idle/lifetime 淘汰及物理替换，O dedicated connection 不重连。UUID 相同仍依赖可信部署、TLS/拓扑，不防恶意克隆 UUID。

**TiDB：** exact VERSION `8.0.11-TiDB-v8.5.8`；操作者在 reset 前通过其已有控制面/权限确认原 endpoint 对 A/B/O 所有可能后端均属同一获准开发集群、无跨集群/账号路由，并将私有核验记录绑定本次 run id、目标 pin、O pin、身份、有效起止窗口。缺失、过期、换目标/身份、无法保证会话稳定均 prerequisite_missing。备份回执不能替代拓扑证明；同 host/port 不作独立证明。不读取新 PD/HTTP endpoint、不授 `mysql.tidb` SELECT、不发明 `@@tidb_cluster_id`。

实际 A/B/O 各读取已存在固定表达式 `JSON_UNQUOTE(JSON_EXTRACT(@@GLOBAL.tidb_config,'$."enable-global-kill"'))` 必须为 true；只取这个布尔值，不取整份配置。A/B 真实 session 的 `@@SESSION.tidb_txn_mode` 必须 pessimistic；不替用户 SET。CLUSTER_TIDB_TRX 用 A/B SESSION_ID 唯一匹配 ID（事务 start_ts），holder 必须存在，waiter 暂缺可在预算内继续；重复/歧义立即失败。正向边 TRX_ID 与 CURRENT_HOLDING_TRX_ID 必须对应当次 B/A start_ts，必要 INSTANCE 只在内存核对，不输出。此为“操作者拓扑核验 + 实际 ID/start_ts 关系”，不是纯 SQL 自动证明 cluster identity。

## 6. O 查询与并发顺序

O 仅执行固定 SELECT/SHOW GRANTS，不 BEGIN/USE/SET/FOR UPDATE/DDL/DML/任意输入 SQL。O 不包装成 DbPool 传给 AdmissionStore。preflight 每张依赖表真实非恒假有限扫描，例如 `SELECT CAST(1 AS SIGNED) AS readable FROM performance_schema.data_locks LIMIT 1`，其余两表及 TiDB 两视图同样各自实际查询；不使用 WHERE false/LIMIT0，也不以空 join 替代各表执行权限核验。空表成功不证明行解码或 wait edge。

MySQL wait 查询保留 w→wt/ht threads→requesting data_locks 的 ENGINE/LOCK_ID join，过滤 ENGINE=INNODB、B waiter/A holder、WAITING、schema_meta。用 `CAST(dl.OBJECT_SCHEMA AS BINARY)=CAST(? AS BINARY)` 绑定 exact schema，不用 O 的 DATABASE()；表名同样 exact 二进制比较。只取 bounded count/boolean。额外映射检查拒绝歧义。

TiDB 固定 join `w.TRX_ID=wt.ID` 与 `w.CURRENT_HOLDING_TRX_ID=ht.ID`，session IDs=B/A；服务端用 `CASE WHEN JSON_VALID(KEY_INFO) THEN KEY_INFO ELSE NULL END` 提取 db_name/table_name，二进制精确匹配绑定 schema/固定 schema_meta，非法/缺字段不匹配。不 fetch 完整 KEY_INFO、KEY、LOCK_DATA、PROCESSLIST_INFO、SQL_DIGEST/TEXT、ALL_SQL_DIGESTS 或用户数据。start_ts/ID/INSTANCE仅内存，输出固定 stage/class 与布尔值。

Case A：A guard→users 停用未提交 → barrier 放 B 原 CAS → O 观察 B→A 正向 edge，CAS未提前完成 → A commit → B 精确 PermissionDenied；设备 AdmissionSnapshot/history/audit/meta business values 不变、actor inactive/revision8 已提交。singleton=1 归属由严格 v3 fixture 和固定 SQL 支撑，不声称读取 lock key 独立证明。

Case B：B 原 CAS先 commit并验证 approved/revision2/一条history+audit → barrier 放 A停用并 commit → 新 CAS拒绝且持久状态不变。也要求 O前置验证，不新增反向wait断言。

只将四个原 fresh-v3 wrappers 原位替换为 observer 明示名称，详见计划；不并存旧自观察 wrapper进入默认发现。fresh-v3 仍通过原 upgrade 命令建立严格v3，不将这些四例冒称额外 v1/v2升级并发矩阵已测；原其他升级矩阵保留。

## 7. reset 前顺序、deadline、错误回收

`显式授权 → 原 target/双引擎alias/独占/真实备份门禁 → 本次 O私有配置与双pin/传输/拓扑绑定 → 原writer preflight不变 → 新observer preflight → 每例reset → fresh-v3 upgrade/严格校验 → 实际A/B/O重新验证 → 并发 → 持久性断言`。

每个 observer case 的 reset 前重新 preflight，不能复用前例结果/票据。独立 O capability JSON 仅新分支消费，不传给原 probe()。preflight前失败 reset_attempted=false；实际会话漂移在fixture后失败则保留已初始化fixture，不自动reset清理。runner独占锁不等于其他操作者停写确认。

预算固定：connect最多10秒、单query最多3秒、preflight总30秒（共同绝对deadline，阶段不能累加突破）、父进程45秒killpg/drain兜底；实际并发含barrier/channel/采样总20秒，采样间隔100ms仅限流非证据，每query同受总deadline截断。不得自动retry/重连，不把超时归为权限不足。错误/超时/CAS提前结束均取消并回收所有future，关闭O与参与池、A按事务路径rollback；不留后台CAS进入下一例。关闭/rollback异常分类记录后停止，不宣称清理成功。

observer批次任一例失败（含未知状态、中断、配置/权限/拓扑/超时）立即停止两引擎，后续未执行标 blocked/not_run，不生成虚假的第二引擎passed。若父 Python runner 在私有 fd 获取与登记的异步窗口遇 `KeyboardInterrupt`/`SystemExit`，经用户追加批准采取进程级 fail-stop：尽力关闭已登记 fd/杀死并回收已启动子进程，随后**立即终止父 runner 进程**，让内核回收尚未登记的 fd，不把中断折叠成可由上层捕获后继续 reset/下个引擎的普通错误。此路径不保证能写出结果 JSON，fixture 状态标未知、留待人工核对及重新授权，绝不自动 retry/清理；也不把 OS 退出回收冒称为函数在所有错误返回路径自行关闭。不reset失败fixture，不DROP DATABASE。一轮重启需要重新授权/人工核验实际状态。

## 8. 验收及 external blockers

**offline-ready**：两套grammar行为回归及writer/callers零diff；closed grants/身份/roles；双pin/private-fd/未知字段/无库endpoint；secretcleanenv与fd隔离；reset前fake短路及首失败停两库；deadline/取消；两引擎四wrapper真实调用图、生产CAS不复制；SQL exact schema/ID关系；当前工具链及1.85离线编译/测试通过，tracked/ignored界线审查完成。无需O凭据即可达成。fake只能证明门禁/接线，不是grant或真实边。

**Task4未完成**：offline-ready不等于Task4完成。以下是 fail-closed external blockers，不是让实现者自补的TODO：

1. 操作者尚未提供独立最小权限身份以及逐配置/当次内容pin的读取授权。
2. 有效传输策略/可信部署无法在现有options边界确认，或需要新TLS字段/新支持。
3. TiDB当前窗口拓扑/无跨集群路由确认缺失或失效；MySQL实例绑定无法核对。
4. 实际CURRENT_USER/role/grants序列化不在闭合集合，权限扫描/版本/全局ID/pessimistic/RPC/解码失败。
5. 未获真实只读preflight或reset/迁移/四例并发授权、原独占/备份门禁未满足。
6. 获准后任何真实正向边未观察到、持久性断言失败或其中一引擎未执行。

live只能在新授权明确列明目标/窗口/操作范围后进入；授权不包含GRANT。先真实只读验证，再单例reset/执行，首失败停两库；精确版本、非零例数、实际执行/未执行和失败原因分开记录。MySQL8.0不替代8.4，Task4不代表controller整体完成。
