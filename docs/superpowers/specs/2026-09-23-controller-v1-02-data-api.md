# 02 · 数据模型与管理 API 规格

- `controller-v1 / draft-1`，待审阅；[总目录](2026-09-23-controller-v1-00-index.md)。
- 鉴权见[01](2026-09-23-controller-v1-01-identity-access.md)，任务语义见[04](2026-09-23-controller-v1-04-task-lifecycle.md)。不是SQL迁移或HTTP实现。

## 1. 公共类型

| 类型 | 规范 |
| --- | --- |
| DeviceId | Ed25519公钥32B的小写hex64；数据库BINARY(32)，非SN/IP |
| Id | UUIDv4，JSON/业务Protobuf为小写标准字符串，数据库BINARY(16) |
| Revision/Counter | 无符号64位，JSON十进制字符串，避免JS精度损失 |
| Instant | UTC RFC3339；数据库DATETIME(6)附质量，不作为运行TTL唯一时钟 |
| Name | UTF-8纯文本1–128字符，最多512字节 |
| TimeEvidence | 05定义的时间证据对象，未知字段null |
| Page | `{items:[],next_cursor:string|null}`，默认50/max200 |

请求未知字段或未知枚举400；响应允许扩展字段，客户端忽略未知字段。翻译/显示名不作业务身份。分页cursor绑定用户、筛选和排序，篡改400；排序有ID作为稳定决胜键，不仅依赖可回跳时间。

### 响应投影

- Device最低投影为device_id/display_name/effective_permissions；具device.read时增加descriptor、admission_state、review_decision、connection_state（online/offline）、control_health/data_health（starting/healthy/degraded/offline，判定规则见05 §7）、capabilities、revision。
- status响应为snapshot（null或03的DeviceStatus语义投影）、received_time（TimeEvidence）、freshness（unknown/fresh/stale/error）与age_ms。Protobuf64位字段映射十进制字符串，原始板端毫秒值不强转成中控参考时间；无样本age_ms=null。error保留最后好样本时同时返回last_error，不把其age清零。
- MainTask含id、operation、state、outcome、revision、created_time、view_scope与按state分组的counts。SubTask含id、device_id、state、reason_code、revision及授权可见的result_evidence；不暴露execution_token或token摘要。
- Capabilities和TaskRecord的HTTP投影保留03语义，64位版本映射Counter；创建/取消响应不能通过这些投影绕过01的可见性。


## 2. 数据库约束

本稿建议分别验证MySQL8.4 LTS与TiDB8.5 LTS，尚未验证，不宣称支持所有兼容版本。使用utf8mb4、显式唯一约束、revision CAS、短事务；不依赖自增全序或两库完全一致的锁/DDL行为。

下述为逻辑模型非DDL。小结构/TimeEvidence可用JSON，但关键唯一性/授权不依赖JSON查询。关联完整性必须由事务验证，FK仅额外保护。数据库重试不包含网络指令、密码输出或事件发送。

### 2.1 身份与授权

| 表 | 字段/约束 | 索引 |
| --- | --- | --- |
| schema_meta | 单例schema_version,instance_id,initialized,authz_epoch,admin_guard_revision | 初始化及管理员保护行 |
| users | id,username,display_name,password_hash,active,is_admin,must_change_password,revision,created_time | username唯一 |
| sessions | token_hash BINARY32,user_id,process_epoch,created_time,revoked；截止内存 | token_hash主键,user_id |
| roles | id,name,builtin,archived,revision | name唯一 |
| role_permissions | role_id,permission，仅01权限目录 | 联合唯一 |
| device_groups | id,name,archived,revision | name唯一 |
| group_members | group_id,device_id | 联合唯一，device反向索引 |
| grants | id,user_id,source_kind,role_id?,permissions?,scope_kind,scope_group_id?,scope_device_id?,revision | user_id；source/scope字段互斥 |

角色/组删除采用归档并同事务撤销关联，递增epoch；用户只停用保留任务引用。内置角色不可归档，可复制自定义。

### 2.2 设备与准入

| 表 | 字段/约束 | 索引 |
| --- | --- | --- |
| devices | public_key主键,display_name,descriptor_json,admission_state,review_decision,revision,first_seen,last_seen证据,archived | 状态/decision |
| admission_decisions | id,device_id,actor_id,decision,previous_revision,new_revision,reason,TimeEvidence | device_id/id，只追加 |

设备换公钥产生新身份，不按SN自动合并或迁移授权；归档不删除吊销记录。描述是最后已知档案，不能作最新遥测或在线事实。

传输准入仍PENDING/APPROVED/REVOKED；本稿另列应用审核决定none/approved/denied/revoked：

- 新身份PENDING+none，超配额不创建记录。
- 批准APPROVED+approved，不自动分组。
- 拒绝PENDING+denied，当前及后续握手APPROVAL_DENIED，板端停止重连，记录保留。
- reopen恢复PENDING+none，提示板端人工恢复重连。
- 吊销REVOKED+revoked；重新授权APPROVED+approved，仍须板端人工重置。

应用decision不是第四种传输状态。决定/revision/审计同事务，最终接纳重查最新代际及状态；资源不足不写denied或revoked。

### 2.3 任务与审计

| 表 | 字段/约束 | 索引 |
| --- | --- | --- |
| task_previews | token_hash,actor_id,process_epoch,固定device_ids,operation,created_time | token_hash唯一；token至少128bit随机，库只存摘要，60s单调有效，重启失效 |
| main_tasks | id,actor_id,operation,state,outcome,idempotency_key,request_hash,preview_token_hash,固定目标证据,created_time,revision | actor+key唯一 |
| sub_tasks | id,main_task_id,device_id,state,reason_code,revision,created_time,queue_deadline_evidence,dispatch_intent,pre_boot_id,result_evidence,lock_released | main+device唯一；state/device索引 |
| device_operation_locks | device_id主键,sub_task_id唯一,acquired_time,generation | DB唯一插入，不能只靠内存mutex |
| audit_events | id,actor_kind/user_id,event_type,target_kind/id,params_redacted,outcome,TimeEvidence,process_epoch,event_seq | process_epoch+seq唯一，目标/actor/id |
| recovery_checks | id,task_id?,device_id?,actor_id,check_type,evidence,decision,TimeEvidence | 时间核验、unknown释放、受控恢复 |

未决/held/持锁记录不按普通终态期限删。结果更新CAS，板端结果版本存evidence，详细状态唯一来源为04。

必审事件（最小集）：登录成功/失败、登出；改密（自助/强制/管理员重置）、reset-admin；会话撤销（停用、重置、重启）；用户/角色/授权/组变更（含最后管理员保护命中）；准入approve/reject/reopen/revoke/reauthorize（资源拒绝与人工拒绝分别记录）；任务创建/取消/时间核验/unknown释放；设备锁获取与释放；受控恢复模式进入/退出；NTP回退/恢复/过期/跳钟。event_type为稳定语言中立枚举，参数细节入params_redacted，不记录密码、私钥与凭据。

## 3. HTTP公共契约

基础 `/api/v1`，根路径探针除外。成功 `{data:...,request_id:"UUID"}`；错误 `{error:{code,message_key,params:{}},request_id}`。User/Role/Grant/Group返回非敏感字段与revision，禁止密码哈希、token_hash和私钥。

写对象携带expected_revision，冲突409；GET无变更副作用，不因读设备详情触发重启或授权变更。不可见和不存在均404。安全要求继承01。

| HTTP | code |
| --- | --- |
| 400 | INVALID_ARGUMENT |
| 401 | AUTH_REQUIRED / INVALID_CREDENTIALS |
| 403 | PERMISSION_DENIED / PASSWORD_CHANGE_REQUIRED / CSRF_INVALID |
| 404 | NOT_FOUND |
| 409 | REVISION_CONFLICT / IDEMPOTENCY_CONFLICT / LAST_ADMIN / INVALID_STATE |
| 429 | RATE_LIMITED，附Retry-After |
| 503 | NOT_READY / STORAGE_UNAVAILABLE / RESOURCE_EXHAUSTED |

超过显式声明的输入上限（如批量大小）为400；429仅用于限速器，503 RESOURCE_EXHAUSTED表示服务端资源池耗尽并失败关闭，亦附Retry-After。管理API单个JSON body ≤1MiB，超限400；登录以外限速：按会话写与按端点读请求亦有限额（429），阈值见05 §6建议默认。批量管理200且逐项ok/error，不隐藏部分失败；输入整体非法400。任务创建202仅表示持久接收。所有表中的请求是JSON，`?`可选；查询字段位于query，路径ID按公共类型。

## 4. 账号与授权API

除auth接口外本节均管理员操作。

| 方法/路径 | 请求 | data响应/约束 |
| --- | --- | --- |
| POST /auth/login | username,password | user,csrf_token；设置cookie；401/429 |
| GET /auth/me | 无 | user,csrf_token,authz_epoch；未改密可用 |
| POST /auth/password | current_password,new_password | changed:true；清cookie，未改密可用 |
| POST /auth/logout | 空对象 | logged_out:true；清cookie，未改密可用 |
| GET /users | cursor?,limit? | Page<User> |
| POST /users | username,display_name,is_admin | user,temporary_password，一次展示 |
| PATCH /users/{id} | expected_revision,active?,is_admin?,display_name? | User，最后管理员保护 |
| POST /users/{id}/reset-password | expected_revision | user,temporary_password，撤销会话 |
| GET /roles | cursor?,limit? | Page<Role> |
| POST /roles | name,permissions[] | Role |
| PATCH /roles/{id} | expected_revision,name?,permissions?,archived? | Role；内置归档409 |
| GET /grants | user_id?,cursor?,limit? | Page<Grant> |
| POST /grants | user_id,source,scope | Grant |
| DELETE /grants/{id} | expected_revision | deleted:true，仍需JSON/CSRF |
| GET /users/{id}/effective-permissions | device_id?,cursor?,limit? | Page<设备有效权限和来源> |

source为 `{kind:"role",role_id}` 或 `{kind:"direct",permissions:[]}`；scope为 `{kind:"all"}`、`{kind:"group",group_id}`、`{kind:"device",device_id}`，严格互斥。空permissions拒绝，不提供普通用户自助授权。

## 5. 组、设备与审批API

| 方法/路径 | 请求 | 响应/权限 |
| --- | --- | --- |
| GET /groups | cursor?,limit? | Page<Group>，管理员 |
| POST /groups | name | Group，管理员 |
| PATCH /groups/{id} | expected_revision,name?,archived? | Group，管理员 |
| PUT /groups/{id}/members | expected_revision,device_ids[] | Group,member_count；管理员，替换显式去重集合 |
| GET /devices | cursor?,limit?,group_id?,search? | Page<授权投影Device>，只可见范围 |
| GET /devices/{id} | 无 | 档案/能力/连接；device.read |
| GET /devices/{id}/status | 无 | snapshot或null,freshness,time_quality；device.status.read，只缓存 |
| PATCH /devices/{id} | expected_revision,display_name? | Device；管理员，仅中控档案 |
| GET /approvals | cursor?,limit?,decision? | Page<公钥/指纹/描述/连接/decision/revision>；管理员 |
| POST /approvals/approve | targets:[{device_id,expected_revision}],confirm:true | 逐项结果，管理员，max1024 |
| POST /approvals/reject | targets,reason,confirm:true | 逐项结果，人工拒绝，max1024 |
| POST /approvals/{device_id}/reopen | expected_revision,confirm:true | decision,requires_device_reset:true；管理员 |
| POST /devices/{id}/revoke | expected_revision,reason,confirm:true | state,requires_device_reset:true；管理员 |
| POST /devices/{id}/reauthorize | expected_revision,confirm:true | state,requires_device_reset:true；管理员，仅REVOKED |

批量必须明确清单，不能确认后纳入新搜索结果。逐项revision检查优先，同决定可幂等成功，但不覆盖较新吊销。吊销先落库再切断，不能因网络通知失败撤销DB决定。

## 6. 任务API

| 方法/路径 | 请求 | 响应/权限 |
| --- | --- | --- |
| POST /task-previews | operation:"reboot",device_ids?,group_ids? | preview_token,expires_in_ms,targets:[{device_id,eligible,reason}]；至少最低可见 |
| POST /tasks | preview_token,confirm:true；Idempotency-Key头 | task_id,accepted:true；202，逐设备检查 |
| GET /tasks | cursor?,limit?,state? | Page<授权投影MainTask>；01可见性 |
| GET /tasks/{id} | 无 | MainTask及可见汇总 |
| GET /tasks/{id}/children | cursor?,limit? | Page<可见SubTask> |
| POST /tasks/{id}/cancel | expected_revision,confirm:true | 已授权目标的取消请求接收确认及可读取结果 |
| POST /tasks/{id}/time-review | expected_revision,decision:"resume_original_deadline"/"expire",evidence | 当前视图；管理员，不延长期限，作用于主任务全部held子任务、原子，无held则400 |
| POST /subtasks/{id}/release-lock | expected_revision,acknowledge_unknown:true,reason | released:true,state:"unknown"；管理员风险确认 |

预览绑定用户/操作/去重排序目标，建议60s、重启失效。显式不可见device_id使整体404；组只展开本人可见成员，不返回隐藏数量。输入至少一个目标、去重后最多1024。预览不能保证提交时有权/在线/支持。

Idempotency-Key为UUID，actor+key唯一；存固定清单+operation规范化哈希及preview_token_hash。相同key相同token/请求返回原ID，不同409。成功重放允许preview过期，使用main_tasks存下的token哈希核对，不依赖preview仍存在。第一次请求须有效preview。

若提交时有目标已完全不可见，整体404不创建；仍可见但无reboot/离线/不支持/设备忙则创建失败子任务，不静默丢目标。设备锁及任务执行见04。

## 7. 系统与事件入口

| 方法/路径 | 请求 | 响应/权限 |
| --- | --- | --- |
| GET /audit | actor_id?,device_id?,cursor?,limit? | Page<AuditEvent>，管理员 |
| GET /system/time | 无 | TimeEvidence,NTP状态/下次重试；管理员，响应不含凭据 |
| GET /system/status | 无 | ready,版本,资源计数,恢复模式；管理员 |
| GET /events | 无 | SSE，已改密会话，握手校验Host/Origin允许列表，逐事件过滤，见06 |
| GET /healthz（根） | 无 | 最小存活JSON，不表示DB/NTP正常 |
| GET /readyz（根） | 无 | 最小readyJSON，未ready503 |

普通业务响应必须提供结构化状态与错误，不以“HTTP200+中文错误字符串”代替机器错误码。

## 8. 专项验收

DB-01：两数据库唯一性/CAS/管理员保护/关联一致；回滚不外发指令。

API-01：未知字段、分页、不可见404、错误码、CSRF、初次改密白名单有正反用例。

API-02：并发提交、重复key、preview到期、HTTP响应丢失不重复任务、不泄露目标。

ADM-01：资源不足不会变成永久人工拒绝；reopen/reauthorize明确人工重置；批量不纳入确认后新增设备。
