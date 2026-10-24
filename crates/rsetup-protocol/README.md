# rsetup-protocol

板端 ↔ 中控的**设备协议字节契约**：纯函数、无 IO、无网络、无全局状态、无数据库访问。
本 crate 只定义与校验字节/结构，不实现监听、连接管理、密钥落盘或准入持久化；它的任何测试通过
**都不构成**握手完成、互通成立或验收 PASS。

## 模块职责

| 模块 | 职责 | 明确不做 |
| --- | --- | --- |
| [`src/frame.rs`](<src/frame.rs>) | 握手帧 8B 帧头编解码与有界长度（`protocol_spec.md` §4.1） | 解析 payload 内容；判定消息顺序是否合法 |
| [`src/sig.rs`](<src/sig.rs>) | §4.3 两种签名输入的 raw wire bytes 拼接与长度/状态字节守卫 | 解析或重序列化 `device_descriptor` |
| [`src/ed25519.rs`](<src/ed25519.rs>) | ring 0.17.14 Ed25519 签/验薄封装——本 crate 的 **Ed25519 签名入口**（§4.3 握手身份签名），不是唯一的真实密码学入口 | 密钥生成、加载与文件权限；X25519/HKDF/AES-GCM（见 `crypto` 模块） |
| [`src/crypto.rs`](<src/crypto.rs>) | §4.4/§5 传输加密核心：X25519 ECDH + HKDF-SHA256 四条 `info` 派生 + AES-256-GCM 记录层（`RecordLayer`/`RecordLayerPair` 序号、AAD、fatal 语义） | 网络 socket、握手消息编解码、`seal`/`open` 的串行化（由调用方保证）；密钥落盘与轮换 |
| [`src/handshake_msg.rs`](<src/handshake_msg.rs>) | §4.2 七种握手消息（`ClientHello`/`ServerChallenge`/`ClientAuthRequest`/`DeviceDescriptor`/`ServerAuthResponse`/`ServerPending`/`PendingPong`）的 proto3 canonical wire 编解码与严格校验，`device_descriptor` 原字节保留 | 解析帧头（见 `frame`）、构造签名输入（见 `sig`）、状态机迁移（见 `handshake`） |
| [`src/handshake.rs`](<src/handshake.rs>) | 服务端握手帧序列状态机与 PENDING 在途 probe 约束 | payload 级 pong token / `status_nonce` 核验；记录层加解密（见 `crypto`） |
| [`src/admission.rs`](<src/admission.rs>) | 准入与连接限额分类纯模型 | 持久化准入决定、修改快照 |
| [`src/wire.rs`](<src/wire.rs>) | `TunnelPacket` 信封结构校验 + 03 §2 业务 DTO + codec 边界（`Codec`、`ApprovedCodecWitness`、`CodecError`） | 真实网络读写；含 action/metadata 的整包 wire 长度终审 |
| [`src/codec.rs`](<src/codec.rs>) | `BusinessCodec`：03 §2 九个业务消息的 proto3 canonical wire 编解码（手写实现，不引入 protobuf/prost 依赖，不用 `unsafe`） | 信封编解码、网络、DTO 之外的业务判定 |

密码学入口因此有两个：`ed25519`（Ed25519 签名/验签）与 `crypto`（X25519 密钥协商、HKDF-SHA256
密钥派生、AES-256-GCM 记录层），两者都只到「纯字节进、纯字节出」为止；握手完成、互通成立仍属
第 ③/④ 层证据，本 crate 不得宣称。

## 业务 codec 边界

业务消息（`Query`/`Capabilities`/`DeviceStatus`/`ClockSample`/`RebootPrepare`/`RebootTicket`/
`RebootExecute`/`TaskQuery`/`TaskRecord`）的编解码只有一条受控入口 `encode_business` /
`decode_business`：

- `trait Codec` 是 **sealed trait**，crate 外部无法实现；
- 编解码都要求 `ApprovedCodecWitness`，该类型的内部字段私有且**没有 crate 外部可见的构造路径**，
  因此第三方不能靠实现一个 `is_approved()` 之类的方法接入自制 codec；
- `NoCodec` 保留为 fail-closed 参照实现：编解码一律 `CodecError::CodecUnapproved`，
  「无 codec 时原始字节必须被拒绝」的负例契约不变；`witness: None` 同样在**触碰 `bytes` 之前**被拒。
  `CodecUnapproved` 只描述这一件事（本端没有获批见证），板端据此映射为 1009 `INTERNAL_ERROR`，
  见下文分类表。

### 所有者授权背景（必读）

- 2026-10-22 项目所有者明确授权「直接实现并启用生产 codec」，接受跳过当时尚未完成的外部专家书面决议、
  获批库选型、固定向量与真实两进程互操作门禁。授权主体、原话、范围、风险与**仍缺证据**逐条登记在
  [`docs/superpowers/reviews/2026-10-22-codec-owner-authorization.md`](<../../docs/superpowers/reviews/2026-10-22-codec-owner-authorization.md>)。
- 默认生产入口：`wire::approved_codec_witness()` 产出见证 + `codec::BusinessCodec`。
- **启用 ≠ 获批**：`approved_codec_witness()` 的文档注释本身就写明它是**所有者授权**，
  不是外部专家决议，也不构成对 03 规格「待审阅」状态的批准。本 crate 不因为存在可用实现而声称 codec
  已通过外部评审。
- 选型现状：本轮路线是**手写 proto3 codec**（不引入 Protobuf 运行时依赖，规避供应链授权）。
  「继续手写」还是「改用经过 fuzz 的成熟库」这一**选型决定仍未审定**，属未闭环证据。

### 严格失败规则

`encode_business` / `decode_business` 两侧守卫 + `BusinessCodec` 自身规则，全部 fail-closed
且错误**不回显 payload 内容**：

1. **编码侧**：`message.validate()` 先通过（`BusinessCodec::encode` 内再兜一次），DTO 不合法不落字节；
   产出字节后再过 512 KiB 上限。
2. **见证侧**：无有效见证 → `CodecUnapproved`（`NoCodec` 与 `witness: None` 两条路径语义不变）。
3. **canonical 编码**：字段号升序、重复字段按 `Vec` 顺序连续写出、proto3 隐式缺省不写、
   `optional` 仅 `Some(..)` 才写（`Some(0)`/`Some("")` 必须写）、varint 最短编码、
   `optional double` 用 fixed64 小端 ⇒ 同一 DTO 恒得同一字节串，合规 canonical 字节满足 `encode(decode(b)) == b`。
4. **解码侧拒绝**：未知字段号（`UnknownField`，首版不做 unknown-field 保留）、singular/optional 字段重复
   （`DuplicateField`）、错误 wire type、截断的 varint/长度/长度前缀、长度超过剩余字节、非最短 varint、
   varint 溢出 u64（以上 `Malformed`）、`string` 非合法 UTF-8（`InvalidUtf8`）、
   `execution_token` 长度非 32B、消息（含嵌套子消息）字节数越过 512 KiB 整包上限（`LengthLimit`）、
   `metrics` 超过 128 项（`TooManyMetrics`：独立的**资源数量**约束，与整包长度无关，
   129 条完整合法的 metric 远不到 512 KiB，不得与 `LengthLimit` 混成一个错误）、
   顶层 `schema_version` 缺失（`MissingField`，03 §1「缺失/0 不作默认兼容」）。
   **未消费的尾随字节不会被静默接受**：它会继续按 tag 解析，落进 `UnknownField` 或
   `Malformed("truncated")`——本 crate **没有**「尾随字节」专用变体，上层映射也不得臆造一个。
5. **形状与二次校验**：解码结果 `kind()` 与调用方声明的 `expected` 不一致 →
   `Malformed("payload kind mismatch")`（含义是「字节被按错误类型解释」，与尾部多余字节无关）；
   结构解出但 DTO 自校验不过 → 版本不受支持走可协商的 `UnsupportedSchemaVersion(版本号)`，
   其余契约违规走去敏的 `InvalidDto` 固定标签。`encode_business` 侧的前置 DTO 校验走
   **同一套**映射，两条路径的分类必须一致。
6. **信封侧**：`validate_packet` 只做不会误拒合法包的确定越界检测（action 词表与 `tunnel.` 保留前缀、
   `trace_id != 0`、非响应包不得带 `status_code`/`error_message`、`payload < 512 KiB`、metadata 条目上限）。
   含 action/metadata 的 **`TunnelPacket` 整包 wire 长度** ≤ 512 KiB 的终审仍须在真实 wire 边界执行，
   本 crate 不宣称已做（`codec.rs` 只保证**单条业务消息** ≤ 512 KiB）。

任何一侧对畸形输入放宽上述任一条，都属于回归。

### `CodecError` 变体清单与上层错误分类

`CodecError` 共 **10 个变体**，语义互斥、不得互相冒充。上层（`rsetup-board-agent::dispatch::codec_error_status`）
按 03 §1 的状态码表把它映射为业务 `status_code`，本 crate 不产出 `status_code`，但分类语义必须与之对齐：

| `CodecError` 变体 | 语义 | 板端 `status_code` |
| --- | --- | --- |
| `UnsupportedSchemaVersion(u32)` | **可协商的版本错误**：对端声明了本部署不支持的 `schema_version` | 1001 `UNSUPPORTED_VERSION` |
| `LengthLimit` | 消息（含嵌套子消息）字节数越过 512 KiB 整包上限 | 1008 `RESOURCE_EXHAUSTED` |
| `TooManyMetrics` | 重复字段 `metrics` 条目数超契约上限（schema 集合规模超限，与整包长度无关） | 1003 `INVALID_ARGUMENT` |
| `CodecUnapproved(&'static str)` | **只**表示「本次没有获批 codec 见证」（`witness: None` / `NoCodec`）：本端部署·门禁故障，**不是**对端版本问题 | 1009 `INTERNAL_ERROR` |
| `Malformed` / `UnknownField` / `DuplicateField` / `MissingField` / `InvalidUtf8` / `InvalidDto` | 字节已抵达获批 codec 边界，是这条请求自身的编码/契约缺陷 | 1003 `INVALID_ARGUMENT` |

- 1001 与 1009 的区别不可混淆：前者告诉中控「换个 schema 版本重试可能成功」，后者表示
  「本端没有获批 codec，重试多少次都一样」。`CodecUnapproved` **不得**再映射成 1001，
  它的 fail-closed 语义（在触碰 `bytes` 与任何 I/O 之前拒绝）保持不变。
- 1001 与 1003 的区别也不可混淆：`UnsupportedSchemaVersion` 只带版本号、不带 payload，
  降级成参数错误会让中控丢失版本协商的触发点。
- 上层映射对 `CodecError` **穷尽匹配**（无 `_` 兜底）：本 crate 新增变体会让板端编译失败，
  强制显式归类。端到端见证见 `crates/rsetup-board-agent/tests/wire_codec.rs`。

## 尚未闭合的评审证据（本 crate 不得宣称）

以下证据**目前都不存在**，本 README 与 crate 内测试都不构成替代；逐项登记见上述授权记录第四节：

- 业务 payload 字节格式的**外部专家书面决议**（现有
  [`docs/superpowers/reviews/2026-10-03-tunnel-security-expert-decisions.md`](<../../docs/superpowers/reviews/2026-10-03-tunnel-security-expert-decisions.md>)
  只覆盖握手与记录层，**不覆盖**业务 codec）；
- **codec 路线与依赖选型**的审定结论（手写 vs 经过 fuzz 的成熟 Protobuf 库，及许可/来源审阅）；
- **与第三方 Protobuf 实现的跨库固定测试向量**（含合法/越界边界、坏 varint、未知字段、重复字段、
  错误 wire type、非法 UTF-8、非法长度、未消费尾随字节，及每例期望错误分类）；
- **真实两进程负向互操作**（[`docs/protocol_spec.md` §8 第 4 条](<../../docs/protocol_spec.md>)）：
  库内 fake、同进程桩与单 crate 测试均不算该层证据；
- **跨 Protobuf 库解析器一致性矩阵**与 codec 解析器 fuzz。

因此本 crate 的 codec 能力不得被引用为 `T4-09` 的 PASS 依据，也不得据此宣称 HTTP/2（h2）双流互通、
真实 wire 端到端或 `AT-09`/`AT-11`/`AT-12` 等验收项通过；状态一律以
[`docs/testing/controller-v1-acceptance.md`](<../../docs/testing/controller-v1-acceptance.md>) 账本为准。

## 测试

```bash
cargo test -p rsetup-protocol
```

测试位于 [`tests/`](<tests/>)，覆盖帧边界与序列、签名输入、Ed25519、§4.2 握手消息编解码、
§5 记录层（派生向量、序号/AAD/tag 篡改与 fatal 语义）、准入分类、wire 信封合同、业务 DTO
以及 codec 的 fail-closed 与严格解码边界。这些属验收分层里的**第 ① / ② 层**（纯逻辑与进程内），
不得冒充第 ③ 层真库或第 ④ 层「外部决议 + 用户复审 + 获批 codec 后的真实 wire」证据。

## 相关文档

- [`docs/protocol_spec.md`](<../../docs/protocol_spec.md>) — 握手、签名、记录层与 §8 一致性测试门禁
- [`docs/superpowers/specs/2026-09-23-controller-v1-03-device-protocol.md`](<../../docs/superpowers/specs/2026-09-23-controller-v1-03-device-protocol.md>) — 业务 payload 的规范 schema（本 crate 的 DTO 与字段号来源）
- [`docs/superpowers/reviews/2026-10-22-codec-owner-authorization.md`](<../../docs/superpowers/reviews/2026-10-22-codec-owner-authorization.md>) — 业务 codec 的所有者授权记录
- [`docs/testing/controller-v1-known-gaps.md`](<../../docs/testing/controller-v1-known-gaps.md>) — 当前已知缺口登记
