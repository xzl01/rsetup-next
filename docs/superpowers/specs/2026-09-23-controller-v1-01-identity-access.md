# 01 · 身份与授权规格

- `controller-v1 / draft-1`，待审阅；规范地位见[00总目录](2026-09-23-controller-v1-00-index.md)。
- HTTP字段与数据模型见[02](2026-09-23-controller-v1-02-data-api.md)，跨重启与运行期计时边界见[05](2026-09-23-controller-v1-05-runtime-operations.md)。

## 1. 初始化与管理员

A-01：初始化使用数据库持久标记、唯一约束和短事务，只创建一次admin。不得以“用户表为空”或正常重启/迁移作为重置密码理由；DDL不假定与账号事务整体回滚。

随机初始密码至少128bit熵，事务提交后仅在首次初始化启动日志输出一次。数据库只存Argon2id哈希，建议参数 `m=65536 KiB,t=3,p=1`，独立随机盐。密码日志属于敏感数据，不进入普通审计或必要备份集。

G0 用户确认仅限定开发验证的恢复路径：`reset-admin` 首版是真正的本机进程 CLI，不暴露为 Web；只允许操作引导账号 username=`admin`，且该账号仍 `active && is_admin`，inactive 或非 admin 必须拒绝，不自动重新启用、不重新初始化数据库。仅接受 OS 实际 `real UID` 与 `effective UID` 均为 root，不能信任 `USER`、`SUDO_USER` 或任何环境变量声称的权限；`服务账号执行延后`，待可信 UID 来源另行安全审阅，首版不允许服务账号代替 root。要求 controlling TTY 且 `stdin/stdout/stderr` 各自均为交互 TTY；任一重定向或无 TTY 均在修改 DB 前拒绝。新临时密码只直接写 `/dev/tty` 一次，不写普通 stdout/stderr、普通日志或审计。新 Argon2id 哈希、`must_change_password`、账号 `revision`、全部 sessions revoke 与脱敏审计在同一事务提交；仅在确认 commit 成功后输出密码。TTY 写失败不得重复初始化或重显旧密码，须由 root 受控再次 reset；commit 返回错误时提交结果未知，不得输出该次秘密或声称已回滚，应受控核实 DB 后再决定是否再次 reset。该 CLI 实现后仍需独立安全审查及本机 PTY + 隔离真实 DB 验证；G0 不是生产批准。

管理员为 `active && is_admin && !must_change_password` 的用户，具有全局及全部设备权限。设备角色不能授予is_admin。防止删除最后管理员时，按 `active && is_admin` 统计保留身份，不因临时改密关卡把该身份当作不存在；最后一个这样的账号不能停用/删除/降级，相关事务锁定共同保护行串行判断。

## 2. 密码与账号生命周期

A-02：初始及管理员重置的临时密码设置must_change_password，仅准访问auth/me、auth/password、auth/logout；不准SSE、设备或管理功能。密码建议12–128个Unicode字符、最多512 UTF-8字节，不截断、不等于旧密码；改密撤销全部会话并重新登录。

管理员创建用户、重置密码的临时密码仅在成功响应展示一次，不记日志/审计；唯一明文日志例外是首次数据库初始化。账号删除采用停用，历史任务发起人引用不删除。停用/重置立即撤销会话，未下发任务执行前重新鉴权；已下发不保证撤回。

用户名采用ASCII小写3–64字符，允许字母、数字、点、下划线和连字符，首字符字母/数字；显示名是独立UTF-8文本，不能作为登录或鉴权身份。

## 3. 会话与HTTP安全

A-03：拟议opaque cookie session，至少256bit随机token，只存SHA-256摘要；单调计时器绑定当前进程代际。建议空闲30min、绝对12h，中控重启所有会话失效，不能按不可靠跨重启墙钟延长会话。

Cookie：`HttpOnly; SameSite=Strict; Path=/`。HTTP直连不设置Secure；外部HTTPS可显式配置Secure及可信代理，不盲信转发头。cookie/token不放URL或日志，不使用浏览器localStorage长期bearer凭据。

非安全方法默认要求 JSON 与精确允许列表中的 Host 和 Origin；已有有效 session 的非登录写请求还须绑定该 session 的 `X-CSRF-Token`。初次 `POST /auth/login` 无先验 session/CSRF token，但同样必须 JSON、精确允许 Host 与允许的 Origin；无 Origin 的初次 CLI 登录拒绝，CLI 如需登录必须显式发送允许的 Origin（Origin 不是身份认证）。登录成功生成 CSRF token。缺失/不匹配 Origin 的浏览器写请求拒绝，不启用任意来源加凭据 CORS。无 Origin 的非浏览器客户端仅在**已有有效 session**、绑定的 `X-CSRF-Token`、允许 Host 及应有的 JSON 四者成立时接受非登录操作；此例外不适用于初次登录，也不放宽浏览器写请求的 Origin 检查。

建议登录按账号和来源分别限速，15min内5次失败后该窗口429，不永久锁死管理员；不存在账号使用同样失败形态与时延（执行dummy Argon2id校验，避免时延侧信道泄露账号存在性）。来源默认连接地址，仅显式可信代理可覆盖。

HTTP不能保护密码或会话免于链路窃听/篡改，上述措施不替代HTTPS，必须保留可信网络部署警告。

## 4. 设备权限点

| 权限点 | 允许内容 |
| --- | --- |
| device.read | 设备档案、能力、准入/连接状态和本设备组名，不泄露其他设备成员 |
| device.status.read | 最新监控快照和板端时间质量 |
| device.reboot | 为该设备提交重启，不隐含其他权限 |
| device.task.read | 查看该设备上本人发起的任务及必要执行证据 |

A-04：任一有效设备权限允许最低识别列表：device_id、display_name、effective_permissions。其余字段逐项鉴权；有reboot但无status/read可选设备执行，但不能因此读取完整状态。中控内部采集boot_id用于任务核实，不自动向无读取权限用户公开。

建议内置viewer为read/status.read/task.read，operator另含reboot；均非全局管理员。未知权限点拒绝，不能通过任意字符串动态创建新执行能力。

## 5. 动态授权

每条grant包含用户、权限来源和资源范围：

- source仅一种：role引用或direct权限点集合。
- scope仅一种：all、group或device。
- 所有匹配用户及目标设备的权限集合做并集，无deny，无普通用户转授权。

A-05：角色和范围必须成对，不能合并全部角色与全部范围后交叉扩展。角色变化、组成员变化动态生效；移出一组不抵消其他来源，单设备删除不抵消组授权。all覆盖后来接入的设备。

角色/授权/组成员/账号变更和审计同事务递增全局authz_epoch。缓存必须随epoch失效；敏感写请求短事务重查账号和权限，任务提交与授权修改通过epoch保护行序列化。执行前再次检查。

数据库事务不能原子覆盖网络：dispatch意图提交后的撤权只能尽力阻止未发送请求，不能承诺撤回已发重启。必须记录意图、撤权和发送结果的顺序证据。

## 6. 读取、任务与订阅可见性

A-06：不可见与不存在统一404；列表仅当前可见设备，不泄漏过滤前总数。admin可见全局；普通用户只看自己发起且至少有一个当前device.task.read目标的主任务。

子任务和汇总仅按当前可见设备重算，标记 `view_scope=authorized_subset`，不返回隐藏数量或原始目标总数。完全不可见主任务404。创建及幂等重放仅返回task_id和接收确认，不能借创建响应绕过读取权限。

取消仅作用于本人主任务中当前仍具device.reboot的queued/held子任务；管理员可操作全部。结果详情仍须task.read，不在取消响应泄漏无读取权限的执行数据。

SSE发送前检查session/epoch；撤权关闭旧订阅，排队的旧授权事件不得发送。停用立即断开订阅。新的鉴权上下文不能复用旧设备列表缓存。

## 7. 特殊管理操作

只有管理员可审批、拒绝、重新开放审批、吊销、重新授权、改组、改角色/授权、核验held时间和释放unknown占用。unknown释放须独立风险确认，不能因此修改结果为成功或重放旧任务。

批量管理使用明确设备公钥清单和各自revision，不能以SN/IP自动信任。吊销恢复需管理员重新授权加板端人工重置；UI必须明确中控批准不代表设备立即恢复连接。

## 8. 专项验收

- AUTH-01：初始化只一次，密码只首次日志输出；临时密码访问非白名单全部拒绝。
- AUTH-02：停用/改密/重启使会话失效，CSRF/Origin/Host及登录限速有正反用例。
- ACL-01：角色跨范围不串权，移组/删授权不误撤其他来源；reboot不隐含状态读取。
- ACL-02：组变更与任务提交并发只有一致epoch结果，执行前重验，已发任务不假称可撤回。
- ACL-03：普通用户任务汇总与SSE不包含隐藏设备/计数，撤权后排队消息不再发出。
- ADMIN-01：并发降级/停用不能删除最后管理员，设备角色不能获得管理员身份。

均为待执行验收，非本次已验证实现。
