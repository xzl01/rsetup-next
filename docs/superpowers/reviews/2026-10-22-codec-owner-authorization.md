# 生产业务 codec 的所有者授权记录（非外部专家决议）

- **记录日期**：2026-10-22
- **记录性质**：**项目所有者（用户）授权**记录，用于留档「谁在什么范围内授权跳过未完成门禁」这一事实。
- **不是**：外部专家书面决议、独立评审报告、固定向量套件、互操作验收记录或任何验收 PASS。
- **授权主体**：项目所有者（本项目用户本人）。
- **被授权执行方**：本项目开发代理（在同一轮改动中实现并启用业务 codec）。
- **涉及对象**：`TunnelPacket.payload` 内的业务 Protobuf codec（[03 §2 载荷定义](../specs/2026-09-23-controller-v1-03-device-protocol.md)）
  的实现与其生产 `witness` 启用，代码位置在
  [`crates/rsetup-protocol/src/codec.rs`](<../../../crates/rsetup-protocol/src/codec.rs>)（编解码实现）与
  [`crates/rsetup-protocol/src/wire.rs`](<../../../crates/rsetup-protocol/src/wire.rs>)（`Codec` 边界、见证类型与前置守卫）。

## 一、授权原话与范围

- **授权原话（逐字，中文）**：

  > 直接实现并启用生产 codec

- **授权日期**：2026-10-22（用户在会话内作出该选择的当日）。
- **授权范围（仅此两项）**：
  1. **实现**业务 Protobuf codec：为 03 §2 的九个业务消息（`Query`、`Capabilities`、`DeviceStatus`、
     `ClockSample`、`RebootPrepare`、`RebootTicket`、`RebootExecute`、`TaskQuery`、`TaskRecord`）
     提供 DTO ↔ 字节的编解码实现。本轮落地形态为
     [`codec::BusinessCodec`](<../../../crates/rsetup-protocol/src/codec.rs>)：按 03 §2 字段号手写的
     **proto3 canonical** 编解码，**不引入 protobuf/prost 等 Protobuf 运行时依赖**，不使用 `unsafe`。
  2. **启用**生产 `witness`：[`wire::approved_codec_witness()`](<../../../crates/rsetup-protocol/src/wire.rs>)
     产出有效的 `ApprovedCodecWitness`，使 [`encode_business` / `decode_business`](<../../../crates/rsetup-protocol/src/wire.rs>)
     的见证守卫在默认生产入口上可放行 `BusinessCodec`；`NoCodec` 与 `witness: None` 两条
     fail-closed 路径语义不变。
- **范围的边界（授权本身不覆盖）**：本授权只解决「谁批准现在把它做出来并启用」，不解决「这个字节格式是否已被证明安全且跨实现一致」。
  具体实现的符号与行为一律以 `src/codec.rs` 与 `src/wire.rs` 的实际代码为准；本记录不描述、不担保实现细节。

## 二、本授权明确不等于（以下证据仍然缺失）

下表各项是[验收账本](../../testing/controller-v1-acceptance.md)判定规则与
[`docs/protocol_spec.md` §8](../../protocol_spec.md) 要求的门禁证据。**它们一条都没有因为本授权而存在**，
也不得由本授权推定：

| 门禁证据 | 当前状态 | 规范出处 |
| --- | --- | --- |
| 外部专家书面决议（业务 codec 路线与字节格式） | **缺失**；本轮为所有者授权，不是外部决议 | 验收账本「判定与填写规则」实质性门禁条款 |
| 获批 codec 库与版本（依赖选型、许可、来源审批） | **缺失**；本轮路线是**手写 proto3 codec，不引入 Protobuf 运行时依赖**，「继续手写」还是「改用经过 fuzz 的成熟库」这一选型决定尚未审定 | [`2026-10-05 协议核心设计`](<../specs/2026-10-05-controller-device-protocol-core-design.md>)（自写 codec 与新增依赖都需独立审定） |
| 固定跨实现向量（含边界值与期望错误分类） | **缺失**；无独立跨实现向量原件；仓库内仅有本仓 `tests/codec.rs` 的手算 canonical 向量，不构成跨实现/独立证据 | [`protocol_spec.md` §8 第 1 条](../../protocol_spec.md) |
| 真实两进程负向互操作（真中控 × 真板端独立进程） | **缺失**；库内 fake、同进程桩、单 crate 测试均不构成该证据 | [`protocol_spec.md` §8 第 4 条](../../protocol_spec.md) |
| 跨 Protobuf 库解析器一致性矩阵 | **缺失**；未在不同 Protobuf 运行时/版本上逐项比对接受/拒绝结论 | [`protocol_spec.md` §8 第 1 条](../../protocol_spec.md)、[`2026-10-03 专家决议`](<2026-10-03-tunnel-security-expert-decisions.md>) 第 3 项 |
| 握手/记录层解析器 fuzz | **缺失** | [`protocol_spec.md` §8 第 2 条](../../protocol_spec.md) |

补充说明：[`2026-10-03 隧道安全专家决议`](<2026-10-03-tunnel-security-expert-decisions.md>) 处理的是**握手与记录层**
（v3 握手、双域签名转录、AES-GCM 记录层、deadline/序号/版本回退）的开放决策，**没有**对
03 §2 的业务 payload 字节格式作出决议，也没有批准任何业务 codec 库选型。引用它来替代业务 codec 的
专家决议是错误引用。

## 三、风险登记（所有者授权已知晓并承担）

1. **双端解析差异**：未经跨实现一致性比对的 codec，可能在中控端与板端对同一批字节得出不同的接受/拒绝结论
   （未知字段、重复字段、非最短 varint、错误 wire type、非法 UTF-8、非法长度、未消费尾随字节等歧义点）。
   一侧解码成功、另一侧失败的组合会表现为随机断流或语义错乱，且不易定位。
2. **坏包被接受**：本轮是**自研手写解析器**，既无固定向量也无 fuzz 覆盖，畸形输入可能被静默接受并进入
   业务判定（票据、任务记录、重启准备等安全关键路径），把解析器差异变成授权或执行层面的错误判定。
3. **不可复现字节**：canonical 编码的确定性（同一 DTO 恒得同一字节、`encode(decode(b)) == b`）目前只有
   本仓自己的测试支撑，没有第三方固定向量固化；一旦实现细节回归，不同构建产物对同一 DTO 编出不同字节
   将无法在离线环境被发现，互操作故障也难以复现与证明已修复。
4. **不兼容需版本化迁移**：生产启用后，一旦未来获批的 codec 与本轮实现字节不兼容，将出现「存量板端
   与新版中控无法互通」，需要按版本化迁移（含双读窗口或强制升级窗口）处理；本轮未定义迁移方案，
   也不提供任何自动降级通道（不得借版本协商降级）。
5. **不得当作验收证据**：本记录**不是** PASS 依据，不改变验收账本任何行的 `status`。
   `T4-09` 保持 `BLOCKED`；`AT-09`、`AT-11`、`AT-12` 等保持「未验证」；HTTP/2（h2）双流互通与真实
   wire 端到端一律不得据此宣称通过。低层证据不得冒充高层证据。
6. **实现者即授权受益者的自证风险**：本轮实现、本轮测试与本轮启用由同一开发流执行，缺少独立性。
   独立性只能由后续的外部复审与第三方生成/复核的固定向量补足。

## 四、后续补证清单（每项单独登记，不打包推定）

以下每一项补齐后须各自留下仓库内可持久复核的证据原件，并按验收账本行合同另记
`executed_at`/`layer`/`command_or_steps`/`evidence`；一条完成不推定其它条完成。

1. **固定向量套件**：离线生成的业务消息字节向量，至少覆盖
   - 每个 payload 的合法边界与最小/最大字段（`DeviceStatus` 64 KiB 建议预算、`MAX_METRICS` 等重复字段上限）；
   - **坏 varint**：非最短的字段 key、长度前缀与整数值编码；
   - **未知字段**、**已知字段重复**、**错误 wire type**、**非法 UTF-8**、**非法长度**；
   - **未消费尾随字节**（含恰好 0 字节与 ≥1 字节两类）：必须被拒绝；本轮实现里它继续按 tag 解析，
     落进 `UnknownField` 或 `Malformed("truncated")`，向量须固化**期望的错误分类**而不是只写「拒绝」；
   - `schema_version` 缺失/为 0/未知值，以及九种 payload 的形状不匹配；
   - 每例写明期望结论（明确成功或明确错误分类）。
2. **真实两进程负向互操作**：真实中控与真实板端各为独立进程，注入上述畸形与歧义字节，
   以及超长信封（512 KiB 整包边界内外）、版本不匹配、票据过期/重用等，逐项记录双端一致行为与连接处置。
   库内 fake、同进程桩不计数。
3. **解析器一致性矩阵**：同一批字节在**至少两种彼此独立的 Protobuf 运行时实现（不同库或不同 major 版本）**
   上分别解码，逐例记录接受/拒绝结果，任一分歧即判套件不合格。
4. **codec 路线与选型的审定记录**：写明「继续手写实现」或「改用经过 fuzz 的成熟 Protobuf 库」的决定、
   理由、许可与来源审阅结论；若引入依赖，须与 `Cargo.lock` 实际解析结果及钉住版本核对一致。
5. **解析器 fuzz**：对业务 codec 的读取路径（以及 §8 第 2 条要求的握手/记录层解析器）执行畸形帧、
   超长长度、截断与非法 tag 的模糊测试，留下可复现语料与崩溃/拒绝分类记录。
6. **外部专家复审**：由与实现者不同的独立评审者对业务 payload 格式、歧义字段规则与上述各项产物的
   方法论出具书面决议，并留原件索引编号。

## 五、可审计性

- 本记录只描述授权事实与缺口，不含凭据、主机名、真实地址、连接串或备份引用具体值。
- 代码内留痕：[`wire::approved_codec_witness()`](<../../../crates/rsetup-protocol/src/wire.rs>) 的文档注释
  写明它是**所有者授权**、不是外部专家决议、也不构成对 03 规格「待审阅」状态的批准，与本记录口径一致；
  后续任何把它当作外部评审结论引用的说法都属于误引。
- 核验方式：本文件与同轮的 known-gaps 变更（[`docs/testing/controller-v1-known-gaps.md`](../../testing/controller-v1-known-gaps.md)
  的 i 节）互为引用；codec 的实际行为以仓库内代码与测试为准，不以任何口头说法为准。
- 本轮改动按项目约定保持未提交状态，不作为已入库已验收的凭据。

## 相关文档

- [`docs/protocol_spec.md` §8 一致性测试与验收](../../protocol_spec.md) — 向量/互操作/一致性矩阵的原始门禁条款
- [`2026-09-23-controller-v1-03-device-protocol.md`](../specs/2026-09-23-controller-v1-03-device-protocol.md) — 业务 payload 的规范 schema
- [`2026-10-03-tunnel-security-expert-decisions.md`](<2026-10-03-tunnel-security-expert-decisions.md>) — 握手/记录层的外部决议（不覆盖业务 codec）
- [`crates/rsetup-protocol/README.md`](<../../../crates/rsetup-protocol/README.md>) — codec 边界的使用方式与边界声明
- [`docs/testing/controller-v1-known-gaps.md`](../../testing/controller-v1-known-gaps.md) — 当前已知缺口登记
- [`docs/testing/controller-v1-acceptance.md`](../../testing/controller-v1-acceptance.md) — 唯一状态登记处（本记录不改动它）
