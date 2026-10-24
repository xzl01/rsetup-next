# Tunnel Task 3 修复报告：TunnelPacket 信封与 gRPC 编帧

- 日期：2026-10-23
- 依据：`.superpowers/sdd/2026-10-23-production-v3-tunnel/task-3-fix-brief.md`、`docs/superpowers/plans/2026-10-23-production-v3-tunnel.md` Task 3、`docs/protocol_spec.md` §6.3–§6.7
- 独占文件：`crates/rsetup-tunnel/src/envelope.rs`、`crates/rsetup-tunnel/src/grpc.rs`、`crates/rsetup-tunnel/tests/envelope.rs`、`crates/rsetup-tunnel/tests/grpc.rs`、本文件，以及新增的同组件定向回归 `crates/rsetup-tunnel/tests/grpc_memory_budget.rs`
- 构建目录：`CARGO_TARGET_DIR=target/tunnel-task-3-fix`（独立目录，未清缓存、未 commit/push、未 `cargo clean`）

> 本报告取代同路径旧报告。旧报告宣称"内部缓冲严格有界/全部语义校验/错误不泄露敏感信息"，与独立复现不符，其结论作废。

## 0. 结论（以及明确没有做的事）

Task 3 的六个缺陷类别已在**公共入口本身**修复并由测试固定：公共 `encode`/`decode`、`grpc::encode_message`、`GrpcDecoder` 任何一条路径都不再依赖调用方记得调用 `validate`；解码在任何字符串构造之前按预算拒绝；编码先做 checked 长度计算再分配；gRPC 先验 5 字节头再按长度增量填充，单次调用输出条数真实有界。

**没有做、因此不得当作已完成**：

- 未做 h2 / h2c 端到端与任意真实 gRPC 实现（grpc-go、grpc-rs）互操作验证；`encode_message`/`GrpcDecoder` 只覆盖本仓自实现的 wire 往返与负例矩阵。
- 未涉及 `lib.rs`、manifest、`secure_io`、`h2_keepalive`、`handshake`、`identity` 等其它 writer 的文件。
- `next_packet`/`next_packets` 已被 session 使用的条件尚未成立：尚未有 session 组件把它接入真实 read 循环（背压、未决表 1024 预算、`validate(stream)` 的调用位置由后续 Task 负责）。
- 未测跨包/跨流语义（RESPONSE 必须回到同流同 action 同 trace_id、未决表匹配、ping 代际），这些属 session 层，`envelope` 只能做单包判定。

## 1. 缺陷与修复对照

| # | 缺陷（复现结论） | 修复 | 固定它的测试 |
|---|---|---|---|
| 1 | `encode`/`decode`/`encode_message`/`push` 均不做语义检查，trace_id=0、非法 action、metadata 越界、非法 System 可通过 | `validate_fields()` 成为公共字段校验并被 `encode`（前）与 `decode`（后）直接调用；`validate(stream)` = `validate_fields` + Control/Data 路由 | `test_encode_rejects_illegal_semantics_at_public_entry`、`test_decode_rejects_illegal_semantics_at_public_entry`、`test_validate_fields_and_stream_split`、`test_encode_message_rejects_illegal_semantics`、`test_grpc_rejects_illegal_semantic_envelope` |
| 2 | ping 分支提前 `Ok` 绕过通用 status/error 规则（ping Request 带 `status_code` 直接通过） | 规则顺序固定为 trace → action → metadata 预算 → **通用错误字段规则** → tunnel./System 归属 → ping 专用规则；ping 分支不再有"跳过通用规则"的早退 | `test_ping_rules_do_not_bypass_generic_error_field_rules` |
| 3 | 解码不查预算即构造字符串/条目，20,000 项 metadata 可解出 | 变长字段一律"先读长度 → 比预算 → 才 `take`/`to_string`"：action ≤64、key ≤64、value ≤256、单 entry ≤325、第 17 项在 `read_length`/解析/插入之前拒绝、累计 ≤5120B | `test_decode_rejects_oversized_metadata_key_value_and_entry`、`test_action_and_string_length_budgets_at_decode`、`test_decode_rejects_20000_metadata_entries_before_construction`、`metadata_heavy_decode_fails_in_small_constant_budget` |
| 4 | `encode` 先复制超大 payload 再查 512KiB（8MiB 输入触发 8MiB 二次分配） | 新增 `serialized_len()`：全 `checked_add` 预算/溢出判定，超限即 `MessageTooLarge`，之后才 `Vec::with_capacity(total)` + 复制；复制上限即 512KiB | `test_encode_rejects_oversized_payload_without_copying_it`、`encode_8mib_payload_does_not_allocate_it` |
| 5 | `push_internal` 先 `extend_from_slice(chunk)` 再判 flag/长度（8MiB chunk 先分配）；批量输出 10,000 包无预算 | 解码器改为"5 字节头 → flag/长度校验 → 按已确认长度增量填充"的游标机器：单次只补当前消息所需字节，未完成缓冲上限 `5 + 512KiB`；每次调用输出 ≤ `MAX_PACKETS_PER_PUSH`(64)，剩余输入以 `rest`/游标形式交回，合法粘包不污染连接 | `push_8mib_chunk_does_not_allocate_it`、`sticky_10000_packets_stay_within_batch_and_memory_budget`、`partial_large_frame_buffer_stays_under_512kib_plus_5`、`test_grpc_push_batch_budget_is_real_and_lossless`、`test_grpc_partial_buffer_stays_bounded_for_incomplete_message`、`test_grpc_partial_buffer_with_valid_large_envelope` |
| 6 | int32 解码 `v as i64 as i32` 截断：2^32→0、2^32+1→1、5B `0xffffffff`→-1、9B `0x7fffffffffffffff`→-1 | 只接受 `0..=i32::MAX`，或 `v as i64` 为负且正好是某 i32 的 64 位符号扩展；其余 `Int32OutOfRange`。负数编码仍为 10 字节 sign-extended varint | `test_int32_rejects_truncated_and_out_of_range_values`、`test_int32_legal_range_boundaries`、`test_negative_status_code_varint_10_bytes` |
| 7 | `DuplicateMetadataKey(String)` 的 Display 及 gRPC 包装回显对端 key 与控制字符 | 该变体改为 `DuplicateMetadataKey(usize)`（出现序号，必 <16）；`EnvelopeError` 全部变体只携带 `&'static str` 或数字，不保存对端字节 | `test_errors_never_echo_peer_content`、`test_grpc_error_details_never_leak_peer_content` |

## 2. RED 证据（修复前，原模块实测）

取证方式：临时在同一 crate 内新增探针二进制 `tests/task3_red_probe.rs`，其中用 `#[global_allocator]` 包装记录 `max_request`/总量，**在被测调用之前**构造好超大 fixture（不计入度量），单线程执行。探针已在取证后删除，等价断言落在最终三个测试文件里。

命令：

```bash
CARGO_TARGET_DIR=target/tunnel-task-3-fix cargo test -p rsetup-tunnel --test task3_red_probe -- --test-threads=1
```

结果（9/9 失败，逐条对应上表）：

```text
RED1a: encode() 接受了 trace_id=0 的包
RED1b: decode() 接受了非法 System 包
RED1c: encode_message 接受了非法包
RED2: 20000 项 metadata 竟然解码成功
RED3: encode 超限前分配了 8388632 字节（应先在预算内失败）
RED4: push 在校验前分配了 8388613 字节
RED5: 单次 push 输出 10000 个包，无批量预算
RED6: 2^32 -> 0 被解码为 status_code=0
RED7: 错误文案回显对端原文: "duplicate metadata key: EVIL\nKEY\u{1}secret"
test result: FAILED. 0 passed; 9 failed; 0 ignored; 0 measured; 0 filtered out

# 单独一条：ping 绕过顺序
RED8: ping 请求带 status_code 逃过校验: ()
test result: FAILED. 0 passed; 1 failed; ...
```

要点：RED3/RED4 直接测得 8,388,632 与 8,388,613 字节的分配请求（≈8MiB），即"先复制后检查"；RED5 单次 `push` 返回 10,000 个包，无任何输出预算；RED8 说明旧 `validate` 对 ping Request 返回的是 `Ok(())`。

## 3. 实现要点

### 3.1 `envelope.rs`

- **公共字段校验拆分**：`validate_fields()`（与流无关）与 `validate(stream)`（= 前者 + `TYPE_SYSTEM` 仅控制流）。公共规则内部顺序固定，通用错误字段规则先于 ping；`decode` 末尾调用 `validate_fields`，所以公共解码入口不会产出语义非法的信元；流归属由调用方按流上下文再判（`decode` 无法知道 stream，这一点在测试 `test_validate_fields_and_stream_split` 中明确固定，不用 stream 参数掩盖公共规则）。
- **编码**：`encode` = `validate_fields` → `serialized_len()` → `Vec::with_capacity(total)` → 复制。`serialized_len()` 用 `checked_add` 逐字段累加并对 512KiB 设限，`metadata_entry_len()` 与实写长度同源；`debug_assert_eq!(buf.len(), total)` 防漂移。
- **解码预算前置**：`ProtoReader` 拆成 `read_length()` + `take(len)` + `read_string_of(len)`，调用点必须先按预算判定（action/key/value/entry），因此构造 `String` 的分配上限即该预算；entry 计数与累计字节在 `insert` 之前检查。
- **int32**：见上表；`read_varint` 保留原有的非最短/超 10 字节/溢出拒绝。
- **错误去敏**：新增 `Int32OutOfRange`、`ActionTooLarge(n,max)`、`MetadataTooManyEntries(max)`、`MetadataKeyTooLarge(n,max)`、`MetadataValueTooLarge(n,max)`、`MetadataTotalTooLarge(max)`、`FieldTooLarge(n,max)`、`SizeOverflow`；`DuplicateMetadataKey` 只带序号。语义类规则仍用 `Validation(&'static str)`。
- 语义规则本身（trace≠0、action `[a-z0-9]` 点分 ≤64B、metadata ≤16/64/256、非 Response 禁错误字段、Response status=0 禁 error_message、kick/revoke 仅 System、ping 16B nonce + 空 metadata + 响应 status 0）与 §6.4 一致，未弱化；`Response` 的 `status != 0`（含带或不带 error_message）是合法错误态。

### 3.2 `grpc.rs`

- **增量解码机器**：`fill_and_take` 先补满 5 字节头（≤5 字节）→ 校验压缩标志 → 读 BE32 长度 → 依次判 `>1MiB`(`MessageTooLarge`)、`>512KiB`(`EnvelopeTooLarge`) → 再按该长度从游标增量补数据 → 切帧解码。内部缓冲的**逻辑长度** ≤ `5 + 512KiB`（`buffered_len()` 可观测，测试逐次断言）；这不是实际分配容量上限。独立复审以 8KiB 分片输入最大帧时测得逻辑长度 524,292B、`BytesMut` 实际容量/常驻约 1MiB。
- **消费计数/游标**：`InputCursor<'a>` 记录"已消费/剩余"，解码器只按需取用；一次到达的 chunk 由调用方持有，超大输入不会被整体复制。
- **批量输出预算**：`MAX_PACKETS_PER_PUSH = 64`。`next_packet` 一次一条；`next_packets(cursor, max)` 夹紧到 64；`push(chunk)` 为便捷包装，返回 `PushOutcome { packets, consumed, rest }`。
- **不污染连接**：预算耗尽属正常粘包，返回 `rest` 而非错误，也不 `poisoned`；只有协议级致命错误（压缩标志、长度越限、信封解码失败、`finish` 时有残留）才进入 `DecoderPoisoned` 粘滞并清空缓冲。

## 4. 公共 API 变更（含破坏性）与限制

新增：

```rust
// envelope
pub enum EnvelopeError { /* 新增 8 个变体，DuplicateMetadataKey(String) -> (usize) */ }
pub const MAX_METADATA_TOTAL_BYTES: usize;   // 16*(64+256) = 5120
pub const MAX_METADATA_ENTRY_BYTES: usize;   // (1+1+64)+(1+2+256) = 325
impl TunnelPacket {
    pub fn validate_fields(&self) -> Result<(), EnvelopeError>;
    pub fn serialized_len(&self) -> Result<usize, EnvelopeError>;
}

// grpc
pub const GRPC_HEADER_SIZE: usize;           // 5
pub const MAX_PACKETS_PER_PUSH: usize;       // 64
pub struct InputCursor<'a>;                  // new / consumed / remaining / unconsumed
pub struct PushOutcome<'a>;                  // packets / into_packets / consumed / rest / budget_exhausted
impl GrpcDecoder {
    pub fn next_packet(&mut self, cursor: &mut InputCursor<'_>) -> Result<Option<TunnelPacket>, GrpcError>;
    pub fn next_packets(&mut self, cursor: &mut InputCursor<'_>, max_packets: usize) -> Result<Vec<TunnelPacket>, GrpcError>;
    pub fn buffered_len(&self) -> usize;
    pub fn is_poisoned(&self) -> bool;
}
```

破坏性变更（调用方必须适配）：

1. `GrpcDecoder::push` 的返回类型由 `Result<Vec<TunnelPacket>, GrpcError>` 改为 `Result<PushOutcome<'_>, GrpcError>`。**`rest` 非空表示预算用尽，调用方必须把它作为后续输入重新喂回（拼在下一个 chunk 之前），否则会丢消息。** 需要零拷贝续喂语义时优先用 `InputCursor` + `next_packet`/`next_packets`。
2. `EnvelopeError::DuplicateMetadataKey` 携带序号而非 key 原文；同流内的其它错误变体签名也有新增。

限制与后续接入要求：

- 单次调用输出 64 包是常量预算，**不是单包级常驻内存保证**。64 个最大信封可接近 32MiB 字段数据，另有容器与帧缓冲开销；独立复审探针测得峰值 34,085,357B（约 32.51MiB，不计调用方输入）。生产会话使用 `next_packet` 逐包处理/释放，再继续消费，并实施连接/全局内存背压与每端每连接 1024 未决预算；本层不代替。
- `error_message`/`payload` 无独立规范上限，仅受整包 512KiB 约束（§6.4 未对其定义长度上限）；metadata 与 action 使用常量小预算。
- `push(&[u8])` 形态无法在"完整消费任意 chunk"的同时限制输出（内存与调用方 Vec 二选一），故按 brief 裁定改为带预算的便捷包装，生产路径使用增量接口。
- 未实现 gRPC 压缩（flag ≠ 0 一律 fatal），未支持 1MiB 以上消息。

## 5. 测试与验证

### 5.1 测试矩阵（51 例）

- `tests/envelope.rs`（26 例）：schema 逐字 fixture 与往返、预算上限往返、负数 10 字节 varint（-1 与 `i32::MIN`）、512KiB 精确边界（524,288 放行 / 524,289 编码与解码双拒）、8MiB payload 拒绝、int32 合法与越界边界（2^31、2^32、2^32+1、u32::MAX、i64::MAX、`0xFFFFFFFF00000000`、`0xFFFFFFFF7FFFFFFF`）、公共入口语义拒绝（encode/decode/encode_message/push）、`validate_fields` 与 `validate(stream)` 拆分、ping 顺序、Response 错误态合法、metadata 预算（17 项/越界 key/value/entry/撒谎长度）、错误去敏（含 `Debug` 形态）、action 规则、tunnel 前缀、wire 严格性（截断/非最短/超 10 字节/溢出 varint、重复字段、entry 内重复与未知字段、未知字段 8、非法 wire type、非法 UTF-8、非法与缺省 packet type、撒谎长度）。
- `tests/grpc.rs`（19 例）：5 字节头布局与往返、压缩标志、>1MiB、>512KiB、8MiB chunk 拒收且失败后缓冲为 0、恰好 512KiB 信封放行/+1 拒绝、逐字节分片、`next_packet` 增量与 `consumed` 记账、批量 3 包、10,000 包粘包预算与无损续喂、游标预算夹紧、未完成帧缓冲有界、512KiB 大信封 7KiB 分片、空输入、`finish` 截断粘滞、畸形信封粘滞、非法语义信封、错误链不回显。
- `tests/grpc_memory_budget.rs`（6 例，新增的定向 allocator 回归）：见 5.2。

原有强断言全部保留：负数 10 字节 varint、512KiB 精确边界、逐字节分片、wire 严格性各例。仅按要求把"用非法信元充当 wire fixture"的构造改为**合法信元 + 单一 wire 缺陷**（例如截断 varint 用例改为"合法头 + 字段 6 长度只写一半"，非最短 varint 改为字段 4，UTF-8 负例改用字段 5，重复字段改用重复的字段 3），断言的错误类型因此唯一可辨，没有放宽任何校验。

### 5.2 内存预算回归（独立二进制 + unsafe 说明）

`tests/grpc_memory_budget.rs` 用 `#[global_allocator]` 包装 `System` 记录分配请求；**这是本任务唯一的 `unsafe`，且只在测试代码里**（`GlobalAlloc` 是 unsafe trait，实现只做计数后原样委托，不改变分配语义）；生产代码 `envelope.rs`/`grpc.rs` 不含 `unsafe`。因 `#[global_allocator]` 二进制级生效且并发分配互相污染，该文件用 `MEASURE_LOCK` 串行化度量段，并要求以 `--test-threads=1` 运行；断言只针对"是否出现 ≥1MiB 分配"和分配总量，不用对并发敏感的小数精确值。`baseline_allocations_are_actually_observed` 先自证计数器有效（能观测到 2MiB 分配），避免预算断言"什么都没测到"而虚过。

实测（通过）：8MiB payload 的 `encode` 以常量级分配失败、无 payload 等大的二次复制（独立复审测得 action 校验临时分配 64B，故不应称“零分配”）；8MiB chunk 的 `push` 失败且累计分配 < 64KiB；10,000 包粘包全部交付、单次 ≤64 包、累计分配 < 4MiB、无 ≥1MiB 分配；200,007 字节 / 20,000 项 metadata 在小常量预算内失败（最大请求与累计均 < 64KiB）；头声明 512KiB 且尾随 8MiB 的 chunk，最大分配 < 4MiB（仅当前帧所需的有界分配）。这些 allocator 测试记录请求大小与累计分配，未直接测量 live/high-water；上文实际容量与 64 大包峰值来自独立复审的一次性探针，尚未持久化为专项回归。

### 5.3 命令与输出（修复后）

```bash
CARGO_TARGET_DIR=target/tunnel-task-3-fix cargo test -p rsetup-tunnel --test envelope --test grpc
# running 26 tests ... test result: ok. 26 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
# running 19 tests ... test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s

CARGO_TARGET_DIR=target/tunnel-task-3-fix cargo test -p rsetup-tunnel --test grpc_memory_budget -- --test-threads=1
# running 6 tests ... test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
# （默认并发模式同样 6 passed，仅作参考；--test-threads=1 为要求模式）

CARGO_TARGET_DIR=target/tunnel-task-3-fix cargo clippy -p rsetup-tunnel --lib \
  --test envelope --test grpc --test grpc_memory_budget -- -D warnings
# Checking rsetup-tunnel v0.5.0 ... Finished `dev` profile ... （零警告）

cargo fmt --package rsetup-tunnel -- --check
# 无差异，exit=0
```

写入格式的只有本任务归属的 5 个文件：由于同包内 `src/secure_io.rs`、`src/h2_keepalive.rs` 等正被其它 writer 编辑，未执行会写入整包的 `cargo fmt --package rsetup-tunnel`，改用 `rustfmt --edition 2024 <归属文件>`，随后以只读的 `cargo fmt --package rsetup-tunnel -- --check` 复核为零差异。

### 5.4 环境与其他 writer 的影响

- 未运行其它组件的测试，也未对它们做任何写操作；`cargo clippy --lib` 会编译同 crate 的 `secure_io`/`h2_keepalive`/`handshake`/`identity`，本次执行时它们可编译（`Finished dev profile`），若在其它 writer 提交过程中出现编译中断，与本任务的改动无关。
- 未 commit、未 push、未 `cargo clean`、未改动 `Cargo.toml`/`lib.rs`/manifest 及其它组件文件。

## 6. 交接检查项（给使用增量接口的 session Task）

1. 用 `InputCursor` + `next_packet`/`next_packets` 驱动读循环；只有确认输入很小的场景才用 `push`，且必须把 `PushOutcome::rest` 续喂。
2. 每收到一个 `TunnelPacket` 仍需调用 `validate(stream)` 做 Control/Data 路由判定（`decode` 只保证公共字段规则）。
3. `GrpcError` 任一 fatal 变体即视为该流协议错误，解码器已 poisoned 不可复用（§6.6 单流 RST 处理）。
4. 错误文案可直接上报：不含对端 metadata/action 原文。
