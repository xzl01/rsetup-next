# 03 · 板端业务协议规格

- `controller-v1 / draft-1`，待审阅；[00总目录](2026-09-23-controller-v1-00-index.md)。
- 本文只定义 `TunnelPacket.payload` 内的业务消息，不改[传输协议](../../protocol_spec.md)的握手、签名、加密或 `tunnel.ping`。
- 任务执行及证据解释见[04](2026-09-23-controller-v1-04-task-lifecycle.md)，时间换算见[05](2026-09-23-controller-v1-05-runtime-operations.md)。

## 1. 信封与版本

B-01：首版业务请求/响应与device.task.result事件均在OpenControl，小载荷串行编码但允许独立请求并发。响应保持同action、trace_id、原流和当前连接代际；请求由中控发起，事件由板端发出。OpenData即使没有业务消息仍建立并参与传输协议规定的应用探测。

每个顶层业务消息schema_version必须等于1，缺失/0不作默认兼容；未知版本不执行变更，返回UNSUPPORTED_VERSION。普通请求5s期限是接收响应期限，不是重启完成期限。

正常业务status_code=0；非零响应payload为空，error_message仅作安全诊断、不作程序判断，不包含凭据。未知普通action返回UNSUPPORTED_ACTION，不因一个不支持的业务动作吊销身份。对于reboot.execute，非零错误仅允许在确认没有接收执行、没有进入OS调用边界时返回；已接受后的失败必须以成功信封携带TaskRecord的failed/unknown及证据，不能用含糊INTERNAL_ERROR掩盖可能已执行的状态。

| status_code | 稳定语义 |
| --- | --- |
| 1001 | UNSUPPORTED_VERSION |
| 1002 | UNSUPPORTED_ACTION |
| 1003 | INVALID_ARGUMENT |
| 1004 | CAPABILITY_UNAVAILABLE |
| 1005 | DEVICE_BUSY |
| 1006 | TASK_NOT_FOUND |
| 1007 | TASK_CONFLICT |
| 1008 | RESOURCE_EXHAUSTED |
| 1009 | INTERNAL_ERROR |
| 1010 | EXECUTION_TOKEN_INVALID |

传输未知/过期响应的静默丢弃、协议错误及流恢复仍按原协议。业务错误不能冒充连接保活成功。

### action目录

| action | type/方向 | payload |
| --- | --- | --- |
| device.capabilities.get | REQUEST中控→板端/RESPONSE | Query / Capabilities |
| device.status.get | REQUEST中控→板端/RESPONSE | Query / DeviceStatus |
| device.clock.get | REQUEST中控→板端/RESPONSE | Query / ClockSample |
| device.reboot.prepare | REQUEST中控→板端/RESPONSE | RebootPrepare / RebootTicket |
| device.reboot.execute | REQUEST中控→板端/RESPONSE | RebootExecute / TaskRecord |
| device.task.get | REQUEST中控→板端/RESPONSE | TaskQuery / TaskRecord |
| device.task.result | EVENT板端→中控 | TaskRecord，禁止回包 |

prepare是本稿新增的短期执行票据建议，仍是重启同一子任务的内部步骤，不是新增设备变更能力。保活 `tunnel.ping` 继续使用原始16B nonce，**不能套用以下Protobuf**。

## 2. 载荷定义

以下是规格内schema，不创建或生成源文件。使用proto3 optional区分缺测与0。UUID用标准字符串；boot_id是Linux启动UUID；clock_epoch是板端检测到墙钟不连续后生成的新UUID；agent_epoch是每次agent实例启动生成的新UUID（agent重启生成新值，连接重连不变）；journal_epoch是日志库初始化时生成的新UUID，不随普通agent重启改变。unix_ms有符号，允许1970等原始异常值但必须附质量；monotonic_ns不与另一主机直接相减。

```protobuf
syntax = "proto3";
package rsetup.controller.business.v1;

message Query {
  uint32 schema_version = 1;
}

message Capabilities {
  uint32 schema_version = 1;
  string boot_id = 2;
  string agent_version = 3;
  repeated string supported_actions = 4;
  bool durable_task_journal = 5;
  uint32 max_status_bytes = 6;
  string journal_epoch = 7;
}

message Metric {
  string key = 1;
  optional double value = 2;
  string unit = 3;
  string quality = 4;
}

message DeviceStatus {
  uint32 schema_version = 1;
  string boot_id = 2;
  string agent_epoch = 3;
  uint64 sample_seq = 4;
  int64 sampled_unix_ms = 5;
  uint64 sampled_monotonic_ns = 6;
  string clock_epoch = 7;
  repeated Metric metrics = 8;
  string clock_quality = 9;
}

message ClockSample {
  uint32 schema_version = 1;
  string boot_id = 2;
  string clock_epoch = 3;
  int64 receive_unix_ms = 4;
  int64 send_unix_ms = 5;
  uint64 receive_monotonic_ns = 6;
  uint64 send_monotonic_ns = 7;
  string quality = 8;
}

message RebootPrepare {
  uint32 schema_version = 1;
  string sub_task_id = 2;
  string expected_boot_id = 3;
}

message RebootTicket {
  uint32 schema_version = 1;
  string sub_task_id = 2;
  string boot_id = 3;
  bytes execution_token = 4;
  uint32 valid_for_ms = 5;
}

message RebootExecute {
  uint32 schema_version = 1;
  string sub_task_id = 2;
  string expected_boot_id = 3;
  bytes execution_token = 4;
}

message TaskQuery {
  uint32 schema_version = 1;
  string sub_task_id = 2;
}

message TaskRecord {
  uint32 schema_version = 1;
  string sub_task_id = 2;
  string journal_epoch = 3;
  uint64 record_version = 4;
  string state = 5;
  string pre_boot_id = 6;
  optional string observed_boot_id = 7;
  optional string reason_code = 8;
  optional int64 updated_unix_ms = 9;
  string evidence_kind = 10;
}
```

### 字段约束

- UUID格式合法，schema_version=1；必需字符串非空。sample_seq、record_version、票据valid_for_ms必须大于0；时间戳0、指标0或durable_task_journal=false本身不是非法值，必须按对应语义解释。
- DeviceStatus的sample_seq从1单调增长，作用域 `(boot_id,agent_epoch)`；旧连接、旧agent_epoch或更低/相同序号不能覆盖新快照。
- metrics最多128项，key≤64B、unit≤16B，不重复；value只允许有限数，quality为ok/unavailable/error。非ok不提供value，避免错误显示为0。
- 首版常见key为 `cpu.usage_pct`（unit=`percent`、0..100）、`memory.used_bytes` / `memory.total_bytes`（unit=`bytes`、非负整数）、`temperature.cpu_celsius`（unit=`celsius`）。CPU百分比按全CPU归一化到0..100，首次缺少采样差分时为unavailable；缺失硬件不强造值。单位和key由本表定义，不能依赖本地化文字。
- bytes计数以double表示时必须在精确整数范围内；超过2^53−1不能伪装精确，质量标error。后续新增精确64位指标须提升业务schema或定义新类型。
- DeviceStatus建议编码≤64KiB；所有消息仍受传输整包≤512KiB和metadata限制。超限返回RESOURCE_EXHAUSTED，不隐式切成未定义分块。
- ClockSample.quality与DeviceStatus.clock_quality为observed/clock_unstable；observed只表示本机读到且采样期间未检测跳钟，不宣称板端有NTP。查询/采样期间跳钟或启动变化返回clock_unstable，不能计算偏差。
- record_version从1递增，只在当前journal_epoch内比较；updated_unix_ms仅诊断，不能按它覆盖任务状态。

## 3. 能力与设备身份

B-02：中控从已认证连接公钥得到目标设备，不接受payload指定任意另一台设备。板端把任务去重命名空间绑定可信中控公钥和sub_task_id，不能让不同控制器的任务标识互相覆盖。

只有supported_actions包含prepare/execute/task.get，且durable_task_journal=true时才开放重启。缺能力显示不可执行，不回退为任意shell。板端重启/agent重启后重新读取能力，不无限沿用旧快照。

## 4. 执行票据与过期

B-03：prepare不重启、不创建已接受执行记录，只为 `(controller_key,sub_task_id,expected_boot_id,current_connection)` 生成随机32B token，板端单调期限建议30s；返回valid_for_ms。每连接最多一个活跃重启票据，新的prepare不能延长已经发出的execute有效期。

票据只在当前连接/启动/agent实例有效，重连或agent重启失效。prepare先校验expected_boot_id与当前boot一致，不一致返回1003，不发放票据。execute入站时检查token、单调有效期、boot_id和当前设备操作占用：token无效/过期或boot_id不一致返回1010，占用冲突返回1005，不执行。不能仅依赖中控墙钟截止来拒绝长时间滞留网络中的请求。

已存在同sub_task_id的持久记录时，execute先走去重查询：相同操作与pre_boot返回现有TaskRecord，不再次执行，不要求旧票据仍有效；不一致返回TASK_CONFLICT。不存在记录则必须有有效票据。收到execute后消费票据，重发不能获得第二次执行机会。

票据只限制新执行请求的入站有效期，不能证明重启一定发生。中控也需在发送前检查自己任务期限，不因有票据忽略撤权或任务取消。

## 5. 板端日志与任务状态

TaskRecord.state取 `accepted/attempted/succeeded/failed/unknown`：

- accepted：请求与去重记录已可靠落盘，尚未跨入OS调用边界。
- attempted：调用意图及pre_boot已落盘，即将或可能已经调用OS重启；不等于成功。
- succeeded：满足04定义的启动变化证据，evidence_kind=boot_transition_observed。
- failed：有明确的不执行/OS错误证据，reason_code稳定；不能把响应丢失记成failed。
- unknown：agent恢复、日志缺失或证据不足，不能自动重试。

TaskRecord.reason_code与04 §3的reason_code共用同一稳定词表；板端只产生OS_ERROR（OS调用明确错误）与JOURNAL_LOST（日志丢失或不可读），其余代码为中控侧原因，板端不产生。

B-04：accepted落盘后才能回执行接收响应；attempted在调用OS前落盘。写日志失败必须拒绝，不调用OS。agent恢复不能仅因发现accepted/attempted就自动调用重启；只核实或记unknown。

journal_epoch在日志库重新初始化/丢失重建时更换，不随普通agent重启改变。建议容量4096条；首版不自动淘汰去重记录，满时拒绝新任务RESOURCE_EXHAUSTED。管理员维护/清理需停变更并更换epoch，旧任务仅可核实、不能重发。保留策略可后续优化，不牺牲去重换容量。

## 6. 事件与结果合并

B-05：完成事件先落盘后发送，使用TYPE_EVENT无需回包。中控仅接收当前认证身份的任务记录；检查任务设备、sub_task_id、journal_epoch、record_version和状态转移。事件可以丢失、重复或迟到，不能作为唯一证据。

查询响应与事件进入同一状态合并逻辑：较低版本丢弃，同版本不同内容为一致性错误，标记核实异常而不是覆盖；新journal_epoch意味着原历史不可直接比较，中控保留unknown或受控核实。

TASK_NOT_FOUND只证明当前日志找不到，不能证明从未执行，不能授权重发。只有有效查询/事件证据才能按04收敛unknown，保活pong不能完成任务。

## 7. 专项验收

- WIRE-01：未知版本/action、缺字段、非法UUID、超限载荷与非有限指标不触发设备变更。
- WIRE-02：请求/响应同流配对、旧代际拒绝，tunnel.ping仍原始16B，不误包Protobuf。
- WIRE-03：票据过期、换连接、换boot、重复execute、不同payload同ID均不会第二次重启。
- WIRE-04：accepted/attempted落盘和OS调用之间各处崩溃，恢复不自动重试；满日志拒绝新变更。
- WIRE-05：事件重复/乱序/丢失和journal_epoch变化，以查询恢复且不假称exactly-once。
