# Controller V1 四专题 Task 4 收敛设计

日期：2026-10-07。状态：架构与安全门禁经对话逐节确认，用户已确认本书面规格方向；实现仍须依照实施计划和各操作门禁，**本文件不构成数据库迁移或密码学实施许可**。

## 1. 目标与边界

完成四份当前 Controller V1 remaining 计划的 Task 4：Identity 管理 API、Runtime 任务调度与恢复、Transport 板端持久任务日志、Web 任务交互。按实际依赖补足 Identity Task 2/3、Runtime Task 2/3、Transport Task 3、Runtime Task 6 中仅为任务界面真实可用所需的 HTTP 端点，以及管理路由生产挂载。完整互通还需 Transport 后续任务中的票据、OS 边界、双流绑定及获批安全层；这些仅按实际依赖增加，密码学实现仍等待外部专家书面决议与用户复审，不擅自定义转录格式。不将无关 SSE、轮询扩展、root CLI、其余 Transport 扩展或生产发布能力冒充为本目标的一部分；Identity Task 4 承诺的 `GET /api/v1/system/status` 仍须实现，未实施完整 Runtime Task 5/6 的其他 `/system/time`、SSE 与快照增强另列验收缺口，不能伪称完整 Runtime Task 6。既有 Web `task-presentation.ts` 和独立 `Idempotency-Key` 请求头切片、Runtime SNTP 结构解析切片作为已实现前置；后两者尚处独立 worktree 分支，须在目标集成分支再次验证。

用户选择**完整验收后才本地合并 `dev-aghost`**；未要求再次推送。若线协议安全审批、隔离双库验证或必要生产接线未就绪，允许继续可独立审查的代码切片，但四个 Task 4 与最终合并仍标未完成，不以 mock GREEN 代替完整验收。

权威约束：`docs/superpowers/specs/2026-09-23-controller-v1-03-device-protocol.md`、`docs/superpowers/specs/2026-09-23-controller-v1-04-task-lifecycle.md`、四份 `docs/superpowers/plans/2026-10-07-controller-v1-remaining-{identity,runtime,transport,web}-tdd.md`。示例伪代码与上层规范冲突时按安全规范裁决，记录偏差并经审查；不直接照搬缺失类型或不完整桩。

## 2. 组件与依赖顺序

| 阶段 | 交付与消费者 | 验证边界 |
| --- | --- | --- |
| A：并行离线基础 | Identity 仓储接口/严格最后管理员判定；Runtime 完整 NTP 请求关联与 ≤30s 启动门禁、任务模型/原因码；Transport 板端 crate、类型化动作白名单；Web 纯组件与幂等交互 | 实际行为 RED→GREEN；不得声称数据库、wire 或浏览器已验收 |
| B：真实数据与持久化 | Identity SQLx 读写、权限服务及设备服务；Runtime v4 五表/版本化完整性与原子预览提交；Transport journal 两阶段 fsync 与持久 epoch | 本地故障注入，备份门禁后的隔离双引擎事务测试 |
| C：任务执行与 API | Runtime 共享类型化 BoardClient、调度与跨进程恢复；Identity 共享可信认证边界及所有承诺的真实管理网关；最小必需任务 HTTP 路由 | 受控 fake BoardClient、Axum oneshot、真库恢复；生产网关不能返回常量空结果 |
| D：联通和交付 | Web 预览、提交、详情、取消、time-review、unknown 释放锁及路由；必要生产挂载；Transport 的票据/固定无参 OS 边界、双流绑定和**获批后**的 codec/认证加密层 | 前后端同源和授权投影验证、线协议/安全决议验收，然后整分支审查与本地合并 |

Identity Task 2 的仓储可先按现有 v3 编写并用类型/查询测试，但不能将尚未连通的 SQLx 仓储说成真实事务已验证；Runtime v4 迁移在只读 v3 结构与数据检查、真实备份、隔离、独占和停写门禁之后执行。升级后所有登录、改密、准入及任务路径均须共用版本化只读结构/数据 guard；旧版、未知版本或污染数据不得走普通启动。各 worktree 使用私有 `CARGO_TARGET_DIR`；前后端不同专题先独立审查，再按上述依赖顺序合并到集成分支。

## 3. 授权、事务与 HTTP 边界

管理和任务 HTTP 使用现有单值 Host、严格 Cookie、权威会话认证、Origin/CSRF、限速、JSON Content-Type 与 1 MiB 上限的统一边界；写端点缺失有效会话不得触达业务仓储。管理事务按 `schema_meta` guard → 用户按 UUID 字节序 → roles → groups → devices 的锁序执行；在 guard 内重新核验 actor 的有效管理员/所需权限，revision CAS、`authz_epoch` checked CAS 自增与 11 字段脱敏审计在同一事务内提交。审计 `time_evidence` 非空、`process_epoch` 与 `event_seq` 合法且唯一；审计失败连同业务修改回滚。用户降权/停用不能绕过最后管理员防线。

生产管理路由只从真实 SQLx 仓储装配，读取最新角色、授权、分组及设备记录；数据库错误返回 503，不降级为空权限或不存在。设备详情需 `device.read`，列表最小投影不能越权用于详情；认证成功后的不可见/不存在按相同 404 投影。管理路由按既有 26 条契约逐个接入真实读写及权限；未实现的路由不注册、不返回恒定 200。`LastAdmin` 明确映射 409 `LAST_ADMIN`。只在确认生产装配和必要挂载后称 Identity Task 4 就绪。

最小任务 HTTP 集合（不定义 `GET /task-previews`）：`POST /api/v1/task-previews`、`GET /api/v1/tasks`、`POST /api/v1/tasks`、`GET /api/v1/tasks/{id}`、`GET /api/v1/tasks/{id}/children`、`POST /api/v1/tasks/{id}/cancel`、`POST /api/v1/tasks/{id}/time-review`、`POST /api/v1/subtasks/{id}/release-lock`。逐端点重验当前身份与权限。普通用户只看本人且当前有 `device.task.read` 的目标（规格中 `task.read` 简写统一指 `device.task.read`，严禁新增独立权限点），隐藏目标不泄露计数；管理员全局读取与跨用户取消必须在事务内重新核验。取消只对 queued/held 且持有必要权限的目标 CAS 生效，释放锁需匹配 device、owner_sub_task、generation；dispatching 及之后不得伪称取消成功。time-review 对主任务全部 held 子任务原子生效，仅恢复**原**期限或过期，绝不延长；无 held 返回 400。unknown 释放要求管理员、当前 revision、独立二次风险确认、非空 reason 与 `acknowledge_unknown:true`，保留旧任务为 unknown 并审计。

## 4. 任务持久执行、证据与失败收敛

预览固定设备清单并校验 1..=1024；首次提交事务原子校验预览 token、规范小写 UUIDv4 `Idempotency-Key`、当前账号与逐设备权限。**相同 actor、相同 key、相同 preview 请求的重放须先命中已持久幂等记录返回原任务，即使预览已消费或过期；不同内容返回 409，不能再次消费预览或扩大目标。**首次提交才消费预览，设备唯一操作锁竞争失败的目标记 `failed/DEVICE_BUSY`，不放入等待队列；事务内禁止网络。调度在有限槽位下重验准入、权限、控制流、能力、boot、期限与锁，prepare 返回票据保留 `boot_id`/`valid_for_ms`。prepare 后重新核验 `authz_epoch` 与锁，短事务提交 `dispatching` 意图及执行证据摘要；仅事务提交成功后，在事务外由当前进程至多一次发送 execute。发送结果超时/断线不能直接断定未执行。

板端任务按**已认证** controller 公钥与 sub_task_id 联合键去重；raw Protobuf 解码未获批时，类型化领域核心可以测试，但原始字节入口必须失败关闭，不以自定义编码器或固定业务 bytes 伪造互通。journal 首次初始化生成并可靠持久化 UUID `journal_epoch`，普通 agent 重启沿用，丢失/损坏重建才更换并上报 `JOURNAL_LOST`；`pre_boot_id` 在 `accepted` 即落盘。每次状态提交执行同目录临时文件写入→文件 `sync_all`→原子 rename→父目录 `sync_all`；rename 前确定失败与提交结果未知分开分类。仅可靠 `accepted` 可以答已接收；`attempted` 未可靠持久化前 OS 调用次数必须为零；提交不明断流待查，不返回伪未执行或虚假终态。测试使用 fake OS，宿主机真实 reboot 次数始终为零。重复同 ID 同请求返回持久记录，变更载荷同 ID 冲突；日志满拒绝新任务，不淘汰旧去重记录。

恢复扫描中已存 `dispatching/accepted/verifying` 只做只读 task.get，不自动重发 execute；不可信 queued 期限进入 held，expired 则安全终止。`unknown` 释放全局执行槽但始终持设备锁，只有管理员显式风险释放或合格证据才能改变后续处理。成功必须由同一认证设备、sub_task_id、journal_epoch、可靠 attempted、匹配的 pre_boot_id、合法递增 record_version、当前认证 boot 与记录 observed_boot_id 相同且不同于 pre_boot_id、`boot_transition_observed` 及无矛盾已确认 failed 共同证明；boot 变化本身、断线、Pong、TASK_NOT_FOUND 或跨 epoch 结果均不够。旧版本丢弃、同版本不同内容报一致性异常，迟到有效证据可修正完成任务的 outcome/计数但不重新执行、不得删除后续任务新 generation 的锁。

现有 `rsetup-protocol::wire` 尚无计划示例引用的 Capabilities/DeviceStatus/ClockSample/RebootTicket/TaskRecord。先审定唯一共享、保留全部证据的类型化 DTO，再定义 BoardClient；DTO 不等于获批 Protobuf codec 或受信网络通道，不用 `Result<()>`/`Result<String>` 丢失证据。

## 5. 时间与 Web 交互

SNTP 结构解析仅校验 48B 头、LI、版本、模式、stratum 与发送时间戳，绝不产生 `NtpValid`；完整客户端校验所请求的源、originate 时间戳与本次 nonce、服务端接收/发送时间戳及 RTT/偏移质量。启动预算 ≤30s，超时用 `SystemFallback` 继续启动并在后台有界退避重试；跳钟改变 `clock_epoch` 并记录受控审计。测试用可控时钟和 fake/本机受控应答，不将任意外部 NTP 或仅结构合法报文升级为可信样本。

前端 `post()` 只通过受限 RequestOptions 发送规范 UUIDv4 `Idempotency-Key` header，不放入 JSON/body 或持久存储；一次确认生成 CSPRNG key，请求结果不明时重试复用同 key，preview token 或清单改变重置确认与 key，busy 防双击。任务显示 10 种状态和稳定原因码，不把已接收显示成成功；authorized_subset 仅显示服务端当前授权子集与证据类型。管理员 unknown 释放独立于批量重启确认，弹框须 role=dialog、aria-modal、Esc、焦点捕获与恢复、非空 reason/勾选风险，取消零 POST；成功后 GET 重取旧任务，不自动再提交新任务。held 核验按服务端原子结果重取，不在 UI 延期或本地构造审计事实。真正浏览器键盘与布局验收和 jsdom 组件测试分别记录。

## 6. 数据库备份及各级验收

用户要求由执行方自行备份。真实库步骤必须串行、逐引擎进行：确认当前目标属于已授权的两个专用隔离开发库，凭据仅在进程内读取，先只读核对 `SELECT DATABASE()`、schema v3 形状/数据完整性、引擎版本、实际权限、独占与停写条件；连接别名不同不是隔离证明。制作该目标的**真实备份**，使用私有目录/文件权限，保留去敏目标匹配回执并校验备份文件 SHA256 和可读取性；若需恢复演练，只能在另行核准的隔离目标执行，不擅自 CREATE/DROP DATABASE。无法确认备份真实可恢复时迁移保持阻断。不得将任何凭据或备份内容写入 git 跟踪文件、聊天或普通日志，不伪造 backup_ref/ACK；不执行 `DROP DATABASE`，不向其他目标扩权。确认现有显式迁移门禁后才可执行 v4，不能由普通启动隐式迁移。用户只要求跳过**需要数据库 root 权限的锁验证**，不跳过应用账号下可安全进行的真实事务与设备锁验收。

验收账本分层记录：①可编译行为 RED→GREEN 的纯逻辑/组件；②进程内 HTTP 权限矩阵、文件持久化故障注入、模拟网络/时钟；③双引擎真实 SQLx 读写、并发/CAS、跨进程崩溃恢复，缺备份或隔离时 ignored 测试保持未运行，绝不用 `return` 冒充通过；④经独立专家书面决议、协议规范修订所需的用户再次复审及批准 codec 后验证准确 512KiB Protobuf 序列化信封、真实认证/双流网络互通与受控浏览器端到端。外部决议、获批 wire codec、真实电源故障、生产发布均不得由纯内存/常规 reopen 测试推断。真实硬件 reboot 与数据库 root 锁验证不属于自动化执行。

每一增量独立代码审查规格符合性与质量；最终做整分支复审、Rust 全 workspace fmt/test/clippy、Web 全套测试/typecheck/build、凭据与提交范围检查、真库及线协议证据核对。**全部门禁满足后才本地合并；未满足时明确报告剩余阻断与已验证切片，不推送远端。**
