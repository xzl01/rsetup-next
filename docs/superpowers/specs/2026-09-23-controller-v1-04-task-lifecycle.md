# 04 · 主子任务状态机与恢复规格

- `controller-v1 / draft-1`，待审阅；[00总目录](2026-09-23-controller-v1-00-index.md)。
- HTTP/存储见[02](2026-09-23-controller-v1-02-data-api.md)，板端消息见[03](2026-09-23-controller-v1-03-device-protocol.md)，时钟/参数见[05](2026-09-23-controller-v1-05-runtime-operations.md)。

## 1. 不变量

T-01：主任务只组织、调度、汇总；每个去重设备目标恰好一个子任务，包括检查失败目标。单台同样一主一子。提交后目标固定，组成员变化不自动增删目标；最新授权仍在执行前检查。

T-02：不同设备受限并发，子任务内部阶段串行。一个设备不能同时被两个尚未释放的重启子任务占用；后来的子任务设备忙终止，不排队等待占用释放。

T-03：中控与板端均不因超时/断线/崩溃自动再次执行同一重启。持久task_id不同于trace_id；HTTP重试不创建新任务，数据库重试不能重新发送设备指令。不承诺跨网络与OS的exactly-once。

## 2. 预览、确认与提交

1. 预览展开显式device_ids和本人可见组成员，去重排序。显式不可见ID整体404；空集400；最多1024个目标。
2. 返回固定目标和当前检查结果，token绑定用户、reboot、目标清单、进程代际，建议60s有效。预览不是权限或在线保证。
3. 用户确认风险后提交preview_token和幂等键。第一次请求重查token、可见性和当前权限；任何已完全不可见目标使请求整体404且不建任务。
4. 仍可见但无reboot、离线、能力不支持的目标也创建子任务，直接failed并给对应reason；不静默减少数量。普通用户响应仍按01过滤。
5. 短事务锁定authz_epoch保护行并创建主任务/全部子任务，对其余合格目标按DeviceId排序逐项申请DB设备锁。已被占用者failed/DEVICE_BUSY，其他queued；审计和幂等记录同事务。
6. 事务提交成功后才返回202，再由调度器执行；任一数据库失败整体不留半个批次，不外发设备指令。

设备锁在提交时取得，所以“后来的同设备任务”即使先占槽者尚未下发，也不能排队等待其完成。进程内mutex仅优化，DB唯一键是恢复依据。

幂等唯一范围actor+Idempotency-Key。相同键与相同preview请求重放返回原task_id，过期preview不影响已经提交的幂等重放；不同内容409。需保存preview_token_hash和规范化目标哈希，不能依赖短期preview仍存在。

## 3. 子任务状态

| state | 含义 | 批次终态 | 设备锁 |
| --- | --- | --- | --- |
| queued | 已检查并占用，等待全局并发槽 | 否 | 持有 |
| held | 中控恢复后原有效期不可信，等待受控核验 | 否 | 持有 |
| dispatching | 已持久化发送意图，可能已发送execute | 否 | 持有 |
| accepted | 收到板端持久接收记录 | 否 | 持有 |
| verifying | 等重连/查询结果，不能据此重发 | 否 | 持有 |
| succeeded | 取得定义好的重启证据 | 是 | 释放 |
| failed | 明确未执行或明确失败 | 是 | 释放/未取得 |
| unknown | 是否执行/完成不确定，观察窗口已结束 | 是 | 保留，待核实/管理员释放 |
| cancelled | 未下发前取消 | 是 | 释放 |
| expired | 未下发且有效期已过 | 是 | 释放 |

reason_code包括 `PERMISSION_DENIED,DEVICE_OFFLINE,CAPABILITY_UNAVAILABLE,DEVICE_BUSY,QUEUE_EXPIRED,TIME_UNCERTAIN,EXECUTION_REJECTED,OS_ERROR,RESULT_TIMEOUT,JOURNAL_LOST,PROTOCOL_MISMATCH,USER_CANCELLED`。reason不是state，也不是翻译文字。

| reason_code | 典型状态 | 触发 | 来源 |
| --- | --- | --- | --- |
| PERMISSION_DENIED | failed | 提交或取得槽位时最新权限复查缺reboot | 中控 |
| DEVICE_OFFLINE | failed | 取得槽位时目标不在线或控制流不健康 | 中控 |
| CAPABILITY_UNAVAILABLE | failed | prepare失败或能力/持久日志不满足 | 中控 |
| DEVICE_BUSY | failed | 提交时设备锁被占用 | 中控 |
| QUEUE_EXPIRED | expired | queued超过排队有效期，或held核验证明原期限已过期 | 中控 |
| TIME_UNCERTAIN | held/failed | 恢复时剩余寿命不可信进入held；核验无法证明期限且管理员终止进入failed | 中控 |
| EXECUTION_REJECTED | failed | 板端执行前明确拒绝（能力、票据、日志满等，对应RPC错误） | 中控依板端拒绝填写 |
| OS_ERROR | failed | 板端TaskRecord报告OS调用明确错误 | 板端报告 |
| RESULT_TIMEOUT | unknown | 观察窗口到期仍无确定证据 | 中控 |
| JOURNAL_LOST | unknown | 板端TaskRecord报告日志丢失或不可读 | 板端报告 |
| PROTOCOL_MISMATCH | failed/unknown | 核实证据版本、字段或协议不匹配 | 中控 |
| USER_CANCELLED | cancelled | 本人或管理员取消 | 中控 |

reason_code在转入held或终态时写入，queued/dispatching/accepted/verifying等执行中状态不置reason；板端可产生的代码见03 §5。

### 合法转移

- queued → dispatching：取得并发槽、prepare完成、执行前检查通过，发送意图已提交。
- queued → failed/cancelled/expired；queued → held只用于恢复时剩余寿命不可信。
- held → queued：管理员完成时间核验且原截止仍有效，重新检查全部前置条件；held → expired/cancelled/failed。
- dispatching → accepted/verifying/failed/unknown；只有证明execute未被接收/执行或明确拒绝才failed；首次获得的TaskRecord已是终态时仍先经accepted再到verifying转终态，不允许dispatching直接转succeeded。
- accepted → verifying/succeeded/failed/unknown。
- verifying → succeeded/failed/unknown。
- unknown → succeeded/failed：只由后续有效证据收敛，不能回queued/dispatching；手工释放锁不改变unknown。
- succeeded/failed/cancelled/expired不可反向变为执行中。矛盾的迟到结果只记一致性告警，不能静默覆盖。

每次改变state、reason、证据或锁状态都更新revision并记录必要审计；事件版本与DB revision是不同计数。

## 4. 主任务状态与汇总

| state | 计算规则 |
| --- | --- |
| completed | 所有子任务属于批次终态，包括unknown |
| running | 存在dispatching/accepted/verifying，或批次已开始且仍有可运行queued |
| held | 无正在执行或可运行queued，至少一个held |
| queued | 尚未开始且存在queued |

“批次已开始”指至少一个子任务已离开queued（进入dispatching或直接进入终态）；“尚未开始”指全部子任务仍为queued。

completed的outcome：全succeeded为success；全cancelled为cancelled；有unknown则unknown；无unknown且有成功和非成功则partial；其余为failed。非completed时outcome为null。计数按各state返回；普通用户须按可见子集重算并标明subset。

unknown后续证据可修正completed的outcome与计数并递增revision，但不重新下发任务、不转回running。主任务完成不代表所有设备锁都已释放。

## 5. 子任务内部串行流程

### 5.1 取得执行机会

建议全局重启并发16，排队有效期60min，在线提交但等待槽位是允许的；不能为提交时离线目标排队等上线。核实阶段也占槽，unknown释放全局槽但仍保设备锁。

取得槽位后检查账号active/最新reboot权限、准入APPROVED、当前控制流健康、能力/持久日志、设备锁归属和原期限。任何检查失败且尚未dispatch，failed并释放锁。

查询当前boot_id，再调用03的prepare取得当前连接短期票据。prepare失败可结束为failed，因为尚未execute；不得把prepare当作重启已接收。

### 5.2 写意图再发送

T-04：在短事务中重查权限epoch与锁归属，将子任务改为dispatching，保存controller/session/stream代际、pre_boot_id、票据摘要、请求内容摘要和观察截止；事务提交后才发送execute。

网络发送不在事务内。提交dispatching后崩溃即使实际上未发送，也按“可能已发送”恢复核实，不能自动发送来填补这个窗口。服务端只允许当前进程该子任务一个发送协程，票据明文不落审计。

入队后5s没有RPC响应是通信超时，不是执行失败；转verifying。发送错误只有能证明请求从未进入可发送队列且没有其他执行路径时可failed，否则保留不确定性。

### 5.3 接收与核实

有效TaskRecord.accepted表示板端去重日志已落盘；保存accepted，随后verifying。即使execute响应丢失，task.get或事件可补齐；不重发execute。

结果观察窗口建议5min，从dispatching单调开始。重连期间等待，在线时以全局查询预算定期task.get，建议5s一次，不额外为每任务叠加网络保活。窗口到期未知则unknown，保留设备锁并释放并发槽。

TASK_NOT_FOUND、journal_epoch变化、agent崩溃恢复后的unknown不证明从未执行。已有终态证据按版本规则合并；低版本或旧会话不能覆盖。

## 6. 重启成功与失败证据

T-05：本稿的成功定义为“对应持久调用意图之后，观察到目标设备进入不同启动代际”，不是密码学证明该次OS调用是唯一重启原因。

succeeded必须同时满足：

1. 同一认证设备、同一sub_task_id、同一journal_epoch的持久记录。
2. 板端有可靠attempted记录，pre_boot_id与中控保存的发送前boot_id一致。
3. 当前认证连接查询到的boot_id与pre_boot_id不同，且与TaskRecord.observed_boot_id一致。
4. 板端记录evidence_kind=boot_transition_observed，版本合法，无已确认矛盾失败记录。

另一次人工/故障重启可能产生相同观察，因此UI应展示证据类别，不写“确定由本指令导致”。只有boot_id变化、TCP断开或Pong恢复都不足以成功。没有可靠日志或pre_boot不一致为unknown/一致性异常。

明确能力拒绝、无效票据、日志满/写入失败等执行前拒绝可failed。OS调用返回明确错误并可靠落盘也可failed。崩溃窗口无法确认则unknown，不为追求确定终态伪造失败。

## 7. 中控与板端恢复

### 中控正常进程重启

- 先恢复数据库锁/未决任务，再开启新变更调度；登录会话失效。
- queued期限可信且仍有效：重查权限/在线/能力后继续；不可信held，已过期expired。
- dispatching/accepted/verifying：只查询和核实，不重发execute。若不能重建可靠剩余观察窗口，可直接unknown，保留锁和证据。
- unknown：低频查询已有证据，不自动重启；锁保留直到核实或管理员风险释放。
- 终态不重新执行；释放锁用 `device_id + owner_sub_task_id + generation` 条件，旧回调不能释放新任务锁。

### 板端崩溃与重启

板端先落accepted、再落attempted意图、再调用OS。agent恢复发现accepted但没有attempted，不能自动补调用；记unknown或明确未执行证据。attempted但boot未变也不自动重试。boot变化按§6核实并记录，日志丢失更换journal_epoch。

### 旧备份恢复

不同于普通进程重启。进入05受控恢复模式，不能把备份中的queued当作现实中未发送；查板端日志、补授权/吊销历史后才分类。无法核实则unknown/held，不自动重放。

## 8. 取消、时间核验与unknown释放

取消只有queued/held可原子改cancelled并释放自己持有的锁；与dispatching竞争用CAS，只有一个胜出。dispatching以后仅拒绝取消或报告不可撤回，不伪装已取消。

held核验须管理员证据：比较创建时间质量/参考截止和当前来源，能可信证明剩余寿命才恢复原期限；不能修改创建时间或延长TTL。能可信证明原期限已过期则置expired（QUEUE_EXPIRED）；无法证明时管理员可取消（USER_CANCELLED）或显式终止为failed（TIME_UNCERTAIN），不得凭当前墙钟随意判断。核验按主任务粒度发起：对该主任务全部held子任务原子生效，主任务处于任何状态只要存在held子任务即可调用，无held子任务则400。

T-06：unknown风险释放只由管理员显式确认，记录原因、操作者、原证据及锁generation；仍保持unknown，不重发旧ID。后续新任务是新的明确人工授权，UI警告旧操作可能已执行。迟到结果只更新旧任务证据，不能删除后续任务的锁。

## 9. 专项验收

- TASK-01：重复HTTP/并发幂等、固定清单和一目标一子任务；显式不可见失败不泄漏。
- TASK-02：同设备两个主任务竞争仅一方获锁，后者DEVICE_BUSY；其它设备继续。
- TASK-03：撤权/停用、取消、TTL与dispatching并发，不出现双发送或伪取消。
- TASK-04：每个持久化/网络/OS边界故障注入，不自动补发，结果未知保留。
- TASK-05：日志丢失、boot不一致、仅掉线/保活恢复都不判成功；正确证据收敛。
- TASK-06：unknown后管理员释放再新任务，旧响应不能释放新锁；主任务汇总随证据修正但不重开执行。
- TASK-07：重启恢复的时间不可信进入held，旧备份不按queued盲目重放。

这些是设计验收，不是已经实现exactly-once的声明。
