# Controller Task4 observer：独立监护者与 cgroup 生命周期设计（未获批准，已由单体方向取代）

**历史草案，不得实施：**用户在书面审阅关卡明确要求“不再外挂，使用单体程序架构”，并选择严格单进程 Rust 程序。本文件及其 G/W/cgroup、300+5 秒条款均未获设计批准，保留仅供风险追溯；后续须另写并获批单体规格，不能引用本文件为实现、启动或 live 的许可。

日期：2026-10-03。状态：**条件性规格草案，尚待用户审阅批准；仅设计，不授权实现、cgroup 探测、真实子进程、凭据读取、数据库连接、reset 或 live 测试。** 它讨论隔离 observer 启动安全，不改变生产 API、原 writer 身份/权限或已提交的前端基础层。

关联：[隔离 observer 正式规格](<2026-10-03-controller-task4-isolated-observer-design.md>)、[原实施计划](<../plans/2026-10-03-controller-task4-isolated-observer.md>)、本地去敏分析 [worker](<../../../.superpowers/sdd/2026-10-03-controller-task4-isolated-observer/supervision-worker-analysis.md>)、[后置 fd](<../../../.superpowers/sdd/2026-10-03-controller-task4-isolated-observer/late-fd-handoff-analysis.md>)、[对抗审查](<../../../.superpowers/sdd/2026-10-03-controller-task4-isolated-observer/supervision-design-adversarial-review.md>)。本规格获批前，现有 O5B1 的 `Popen` 登记真空 **Critical 保持阻断**；不能把本草案当已部署的监护证明。

## 1. 目标、威胁与不能承诺的性质

目标不是“多跑一个 worker”，而是：**本次新增 observer 的 W/C/D 首次取得或继承 writer/O 私有配置 fd 之前，独立于原 Python runner 和 worker 存活的监护者 G 已预先持有经证明不可由受控进程伪造/迁出的 job containment；未确认安全终止的 job 永不使 runner 继续 reset、迁移或下一引擎。** 保护范围是新增 observer preflight 与四个 ignored case 的本地进程/描述符生命周期；原 P/旧 writer probe 在既有授权门禁下先读 writer 配置，**不在 G 的此项监护保证内**，也不得把该旧 fd 继续转交给新 observer 子进程。若未来目标扩为保护 P/旧 probe 的全部读密窗口，须另行重排其启动/入组及授权，本规格不暗示已经实现。G 不提供数据库权限、操作者授权、受信可执行文件或实际锁边证明。

威胁包括父 runner P 或 worker W 被 SIGKILL/崩溃；W 启动 probe C 后、`Popen` 对象赋值前遭任意中断；C/迁移后代 D 持有继承 fd；重复 `KeyboardInterrupt`/`SystemExit`；无 JSON、timeout、输出过量、kill/wait/cgroup 查询失败；PGID 复用、`setsid` 脱组、同 UID 对可写 artifact inode 的原位改写。**单纯 `killpg`、PDEATHSIG、直接 child 的 `wait`、fake `REAP`、成功 JSON、fd pin 或 cgroup 空组中的任一个均不能独自证明整个性质。** 已发往数据库的请求不能因杀掉本地进程而撤销。`SIGKILL` 对不可中断内核态任务不能保证在固定时间内结束；失败一律是 StopUnknown，而非 Passed。

本设计条件性地保护事先封闭在 job cgroup 内的后代。无法证明未经许可的同 UID 外部接收者没有取得复制 fd、特权进程不能将任务迁出、可信二进制及动态加载依赖不可改写时，不能宣称绝对“任何时刻无持密者”。上述边界作为外部阻断，不能用离线 fake 测试抹去。

## 2. 角色、信任边界与前置外部能力

- **P（原批次 runner）**只提出当前 engine/scope 的一次任务，保留原目标双别名、独占、真实备份、迁移 ACK、原 writer preflight 门禁；不在注册安全 job 前打开/传递 O 配置 fd。P 只在收到 G 的终止与协议双重认可后决定是否 reset，且 observer 每例重新授权/预检。P 无权以本地 JSON 或进程退出码自行认定 G 已清空 job。
- **G（独立监护者）**由可信外部服务管理器拥有独立生命周期和受保护 cgroup v2 子树的管理权限，不是 P 的线程、`atexit` 回调或会随 P 同死的普通子代理。G 为每个精确绑定创建不可复用 opaque JobHandle，保持已打开的 job cgroup 目录句柄与内部状态，由 G 自己作为 W 的父进程启动阻塞 W、确认其入组，再发单次放行。G 必须先证明 W/C/D 及任何同 UID 组外进程无 cgroup 控制文件写/迁组权限、无可达组外 `SCM_RIGHTS` 或 `/proc/<pid>/fd` 复制通道、无特权逃逸能力，且可信可执行文件与加载链未被组内进程改写；只有布尔“不得迁出”不是强制机制。任一隔离前提不明时 G 不放行。G 可以在启动 W 前成为 Linux child subreaper；它只 wait 自己的直接 W，W 活着时必须由 W wait 其直接 C，C 活着时由 C wait 其直接 D。若 W/C 异常先死，G 仅能 wait 实际被内核重收养且可证属于本 job 的后代；否则保留 StopUnknown，不能凭 `populated=0` 自称已经 wait。
- **W（一次性 worker）**只在 G 放行后使用既有严格 private opener 读取本次两份文件、各自双 SHA-256 pin 与独立受信 artifact 身份；只执行本次 O probe 或 O6 case。它不能生成 OperatorRecord、延长 scope/window、给 P 发不经 G 确认的 passed，也不持有继续下一例的权限。
- **C（固定 probe 或 ignored test binary）**只能由当前 W 在 job 内启动；继承 cgroup 成员资格，不允许因 `start_new_session` 脱离 cgroup。其原始 fd 接管仍由已审 test-only `unsafe` 边界承担调用方独占/无并发 close 前提；经严格 `OwnedFd` loader 读双 pin 后关闭，artifact fd 在任何 migration child 之前关闭。C 只能在已有授权 envelope、实际 writer 物理 `CURRENT_USER()` 与其他门禁满足时建立 dedicated O 连接。**原 O4A runtime 当前固定拒绝，这里是未来设计，不是已接线事实。**
- **D（C 派生的子进程）**继续受相同 job cgroup 约束。O6 启动任何迁移/测试/诊断子进程前必须关闭 writer/O/artifact/control fd，并在子进程 builder 上删除所有 `RSETUP_OBSERVER_*` fd/pin 环境项、令 stdin 为 null；Cargo/list/旧 reset 不获得 O fd。

G 所需能力及受信记录来源当前**未核实**：受保护且可管理的 cgroup v2 subtree、`cgroup.kill` 与 `cgroup.events populated` 可用性、可审计的“W 先入组再放行”机制、G 的独立服务管理器及死亡恢复策略、可信外部操作者记录和当前窗口拓扑/传输绑定、受信构建指纹与不可被未受信同 UID/组原位改写的 executable+加载依赖。强制隔离证据必须逐项覆盖 G/W/C/组外接收者实际 UID/GID/capabilities、受保护 cgroup 控制文件与迁组权限、可达 Unix IPC 和 `/proc` fd 复制/ptrace 能力、加载链；同 UID 可通信或可迁组时，`populated=0` 不能证明没有组外持密者。G 单独死亡时，独立上级服务管理器只有在**G 死亡前已预持 job handle**时才能接管 kill/空组核验；该 handle 不授予 `wait(W/C/D)` 的亲子关系。若 manager 无法通过自身先行设置的 subreaper 身份和实际内核收养关系 wait，或无法取得另一可信 reaper 的可核对退出凭据，则**只能签 `StopUnknown`，即便 cgroup 已空，也不得签 `StoppedVerified` 或允许 P 继续 reset/下一引擎**。若目标要求 manager 接管后仍能签发成功，必须另行证明其真实父/祖先拓扑、subreaper 生效时序与每个后代的 wait 账本；不以 G 旧日志补造证明。**G 与其 manager 同时失效时 cgroup 不会自动 kill 进程**，除非另有独立故障域或经验证的内核级 fail-stop，本设计无法保证持续监护，真实部署保持阻断。当前 worktree/target 的组可写权限不满足 artifact 部署门禁；不得 chmod 既有目录或自动创建/授权真实账户来冒充满足。若任何一个能力不可证明，**只允许离线设计与合成实现，不启动真实 observer**。

## 3. job 状态机与确定性顺序

资源生命周期状态为 `RejectedBeforeRegister | Registered → WorkerBlocked → Admitted → Running → Stopping → StoppedVerified`，若已登记 job 的停止证据不完整，则转入吸收态 `StopUnknown`；`StopUnknown` 不得转回运行态，仅外部人工核查后凭**新授权、新 job**可重启。协议状态独立为 `NotReceived | Accepted | Rejected`，业务结果独立为 `NotRun | Passed | Failed`。协议 `Accepted` 按 scope 分别判定：O4 preflight 必须是恰一个固定 JSON、`writes_executed:false`、四个 capability 为 true 且不得出现 `direct_database_all`；O6 ignored case 则按原 runner 对恰一个指定测试的 summary、计数及 `--ignored --exact` 参数做严格验收，迁移/fixture 写入仍受原备份/授权门禁，**不得**套用 O4 的“无写入”字段。资源 `StoppedVerified` 不取代以上业务核验。`StoppedVerified` **只**表示 G 核查该 job cgroup 的任务为空、已由真正父进程或经证明的内核收养者 wait/reap 应回收的直接/后代子进程，并且经强制隔离证明不存在组外复制 fd；它不表示 JSON 正确或数据库查询成功。无固定 JSON 可使资源已安全停止但协议 `Rejected`，合法 JSON 也不能把 `StopUnknown` 升级为成功。继续下一受授权动作须同时具备 `StoppedVerified`、固定协议 `Accepted`、业务 `Passed`、当前有效的原外部授权与**同一精确 job 的一次性最终票据**；其它拒绝一律停两引擎，未执行项标 blocked/not_run，不能清理、重试或删除失败 fixture。

G 在 `Register` 时冻结并通过可信通道核验完整不可混淆 binding：engine、两目标别名、当前 writer/O 双 pin、expected O account、外部 OperatorRecord 的受信来源及 run/window、scope、artifact 构建指纹/不可改写部署身份、P 会话、传输与拓扑引用和 deadline。opaque JobHandle 仅定位该 binding，不提供授权；`Admit`、W 每次 opening 两 fd/连接前、G 签发最终票据以及 P 消费票据时都须以可信时钟核对同一 binding 与时窗。错 job/错引擎/错目标/过期/重复消费均拒绝，已登记而停止不明则 `StopUnknown`，不能拿另一个空组证明通过本 job。

1. **先授权门禁，不打开 fd。** P 在指定两目标上核查原 alias/独占/真实备份/ACK/原 writer preflight；从操作者**另行提供**的当前窗口记录核对 engine、目标、双 pin、O 身份、scope、传输与 TiDB 拓扑/路由。记录/字符串/双 pin/envelope 本身不自签授权。缺失时 P 与 G 均 fail-closed，不构造 live 许可。
2. **G 预登记并启动阻塞 W。** G 创建唯一 job cgroup、持有稳定内核句柄，作为 W 真正父进程用不可由 W 自放行的控制闸启动无密 W；G 自身尚未记录 W PID 的窗口中，W 不打开 writer/O/artifact、不 spawn C 或碰数据库。P 收到登记票据之前死亡时 G 不放行；G 经内核身份/pidfd、可信成员查询及完整 frozen binding 核对 W 已在 job cgroup、P 会话/窗口有效，才发一次性 `Admit`。一次 READY/PID 自报或配置 bool 不能替代入组/授权；W 无 cgroup 控制写权和组外 fd 通道的隔离证据缺失则 `Admit` 永不发送。
3. **W 在已监护边界内开 fd 与运行 C。** W 在每次打开配置及发起数据库连接前以同一 frozen binding 重新核查 scope/window/目标/双 pin/独立受信 artifact，再使用 no-follow 私有 opener；子进程 `pass_fds` 只含本次已验 writer/O/artifact，授权 envelope 仅专用 stdin，argv 不含凭据/路径/URL。W 在 C 已 spawn、`Popen` 未赋值时可失去局部 PID，但 C 必须继承不可迁出的 job cgroup；G 已预注册该 containment，W 消失或通道 EOF 时 G 按 job handle 独立杀全组。W/C 不拥有迁组、向组外发送/复制 fd 或绕过 G 报告成功的能力。C 在任何迁移子进程前关闭三 fd 并清理 env/stdin；如果父子关系或后代账本无法核对，`StopUnknown`。
4. **按 scope 隔离期限，预留收束时间。** O4 preflight 使用单调时钟从 job 登记前记 `t0`：connect 最多 10 秒、单查询最多 3 秒、preflight 共用 30 秒绝对期限；G 的此类 job 最迟在 `t_stop≤t0+40s` 发起 `Stopping`，为 kill/drain/wait/cgroup 查询预留至多 5 秒，`t_end≤t0+45s` 仍未有 `StoppedVerified` 就变 `StopUnknown`，不得追加新一轮等待。输出异常/过量、W/P 控制通道 EOF、无固定 JSON 或结果不一致可以更早停止。O6 ignored **test executable 单次调用**不是 45 秒 job：原 runner 对这条调用的父侧上限为 300 秒，其 test binary 内部可能执行迁移/fixture/并发；并发实际边的子预算为 20 秒、轮询100ms仅作限流。原 writer reset-tables 是前置的**另一次**最长 90 秒 probe 调用，Cargo build/list 也有各自原门禁与预算，均不因本规格自动入 G job；若要保护它们必须另行重排并授权。P 从单次 O6 case child 启动前冻结单调 `t0_case`：原 case 二进制的运行上限维持 `t_run≤t0_case+300s`；G 在失败时可更早启动 `Stopping`，超时则最迟于 `t0_case+300s` 启动 kill/drain/wait/空组核验，另用至多 5 秒收束，监护总截止 `t_end≤t0_case+305s`。这是用户仅批准的**书面期限选择**，以后调整原 runner 的300秒命令上限/新增5秒监护兜底须另经实施与安全复审；超时或证据未完在截止时为 `StopUnknown`、不执行 reset/另一引擎。O4 预检的 30 秒与 O6 并发的 20 秒不得相加或替代原 case 300 秒。任何持 fd 的 W/C/D 都受同一当前 job 的 G 监护；若 job scope 切换，先要求上一个 `StoppedVerified` 再注册新 job。
5. **最终收束而后继续。** W 活着时必须 wait C、C 必须 wait 其直接 D；G wait 自己的直接 W，并只对已被内核收养且属于当前 job 的后代执行 wait，记录每个可信收束证据来源。G 核查 cgroup `populated=0` 和入组/不外送 fd 的强制隔离证明，不能凭 W 的文本 ACK 或空组二选一签发。资源收束经确认后签发与 frozen binding 一致、仅可消费一次的 `StoppedVerified` 票据；固定协议与业务结论**单独**评估，三者同时为正且当前授权未过期时 P 才可进入被批准的 reset/下一例。每一例 reset **之前**重新做 O preflight，case 本身需独立 G job 直到它的进程终止。任何协议、身份、权限或收束错误均停两引擎，已初始化 fixture 保留；已安全停止但协议被拒不能误标 `StopUnknown`。

## 4. 失效窗口与必须诚实保留的 StopUnknown

| 失效 | G 能做什么 | 不可宣称什么 |
|---|---|---|
| P 在 W 返回 PID 之前崩溃 | G 已登记 job；W 仍阻塞且未持秘密；控制通道 EOF 令 G 关闭闸、kill job，确认空组后回收 | 不能仅凭 P 进程退出认为 W 已被 wait 或不存在 |
| W 在打开 fd 的 Python 登记窗口中断/被杀 | G 监护 W 所在 job，kill 全组并等待空组；失败为 StopUnknown | Python `finally`、fd 已写入局部变量或 W exit 均不单独证明 fd 回收 |
| C 在 `Popen` 返回/赋值前已继承三 fd | G 在 fork 前已持 job containment；W 死亡/控制 EOF 时 G `cgroup.kill` 并核查空组 | 不能依赖 W 局部 `Popen` 对象、仅 kill W PGID 或 C 的固定 JSON |
| P 或 W 连续 KI/SE、SIGKILL，G 仍活 | G 独立感知租约/控制通道失效，禁止下一动作并杀 job；未确认空组则 StopUnknown | 不能把多次 Python `_fatal_interrupt`/`os._exit` 当成全路径监护 |
| C `setsid` 或 D 从 C 脱组 | PGID 可变，可信 cgroup 成员资格不因 session 改变；G 依据 job 而非 PGID kill | 不能假设 cgroup 外已被复制 fd 的进程也受控 |
| kill/wait/cgroup 查询失败或 D-state 久留 | 固定 StopUnknown、保留 fixture、人工核查、新授权才重试 | 不报告 StoppedVerified/Passed，不触发 reset/另一引擎 |
| G 单独退出、独立 service manager 仍活 | manager 已先持 job handle 才能接管 kill/空组核验；否则 `StopUnknown`，P 停止 | 不能靠被监护者自己递归保证 G 存活或清理 |
| G 与其 manager 同时失效 | cgroup 本身不会自动 kill；无另一个独立故障域/内核 fail-stop 的证明时**不满足部署前提** | 不能称 P 也失效时本地 child 已停，fixture/DB 外部状态仍未知 |

Linux `PR_SET_PDEATHSIG`、pidfd 与 subreaper 可作为 W/C 直接父死的冗余与子孙回收补强，但不是 G 的替身：PDEATHSIG 不保证 fork 后代、不证明父仍存活时的失控；pidfd 只标识直接进程；subreaper 本身不会杀进程。父进程死亡与已经发送的只读诊断请求可能竞速，远端动作不可回滚，必须将持久性及授权事实单独核查。`cgroup.kill` 不能证明内核态立即退出；`populated=0` 不能证明曾逃出/组外的 fd 接收者不存在。只有在隔离/不可逃逸、G 生命周期和这些外部边界得到证据时，才允许把 G 判定用于下一步。

## 5. 数据/输出最小化与旧路径冻结

保留已有封闭 grants grammar：MySQL 原 writer 不加诊断视图权限，O 只可在独立账号真实授予三张 `performance_schema` 表精确 SELECT；TiDB O 的 PROCESS 跨用户/集群暴露属于外部操作者已知风险。MySQL 实例 UUID 与 A/B/O 实际连接绑定、TiDB 当前窗口可信拓扑及 `enable-global-kill`/pessimistic/事务 start_ts 映射仍必须真实核对，不能用同 host、fake true 或睡眠替代等待边。O 无默认库，不允许 SET/USE/BEGIN/DML/DDL，SQLx 初始 SET 对 O 关闭；所有错误仅固定 stage/class，不输出原始 SQLx/grants/账号/文件内容。旧 writer parser、probe 四模式、原 reset/备份门禁与失败 fixture 保留不变。`DROP DATABASE`、新 GRANT、真实凭据读/连接/reset 均不因本规格获得授权。

G 的私有证据仅留 opaque job/cgroup inode、冻结的完整去敏授权 binding 摘要、每个直接/收养后代的可信 wait 来源、空组/未逃逸与时间戳；最终票据由 G 经受信控制通道与本次 P 会话绑定且仅消费一次。公开结果不得包含 pid/fd 数字、原始路径、密码/URL、诊断 KEY_INFO/SQL、拓扑敏感详情或原始 stderr。日志写失败不能升级业务结果；没有 `StoppedVerified` 或 ticket 错 job/过期时不能 reset。协议被拒但资源停止已确认可明确报告 `StoppedVerified + Rejected + Failed`，资源清空不可确认才报告 `StopUnknown`，绝不凭日志缺失继续。

## 6. 验收证据、计划拆分与批准关卡

**本规格的离线设计验收**是覆盖上述每个边界的职责与 fail-closed 顺序、原规格冲突清单（§4 出生即传三 fd；§7 进程回收/每例 reset 前顺序；O4/O5/O6 参数和 env）和明确外部阻断，**不**以 fake 测试宣布 cgroup 权限或 live 可运行。若获本规格批准，后续实施计划将至少拆成：G/job 状态机与权限接口（先 fake）、阻塞 W 入组/放行（先 fake）、W 内既有 O5A/private opener 与固定协议适配、C/O6 关闭继承 fd/无后代逃逸审查、P 原 runner 的 StoppedVerified-gated reset/首失败停双库、独立安全复审及只在另获许可时的无秘密 Linux cgroup 子进程实验。每项新的实现者都不能用 fake 完成代替 OS 证据。

任何真实 cgroup 创建/管理、外部监护服务安装、构建产物改属主/迁移到可信目录、无秘密真实 fork/exec/kill 实验、新 operator 凭据/授权记录/只读 preflight、reset/迁移/并发及 O7 live 都分别需要**新的明确授权**。当前没有证明环境提供 G 所需的 cgroup delegation/独立生命周期/不可逃逸机制；未满足时继续保持 O5B1 Critical、O4B/O5B2/O6 真实启动 No-go、Task4 未完成。用户批准书面规格并不自动批准实施或任何真实动作。
