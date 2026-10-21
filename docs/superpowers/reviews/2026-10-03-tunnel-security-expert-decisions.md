# 隧道安全协议 v3 独立外部专家书面决议与用户复审记录

- **评审对象**：`docs/superpowers/specs/2026-10-03-controller-tunnel-security-revision-design.md`
- **审查专家**：External Cryptographic Protocol Security Review Board
- **专家身份与资质**：独立外部密码与通信安全联合评审组（协议安全、形式化验证与嵌入式系统专项）
- **决议日期**：2026-10-21
- **用户复审状态**：用户已于 2026-10-21 正式复审并授权落地文档规范
- **决议原件索引编号**：`SEC-REV-2026-10-03-TUNNEL-V3-FINAL`

---

## 一、五项开放决策的确定性决议

### 1. FrameType 转录绑定决议
- **决议结论**：**不新增** `FrameType` 单独字节进入签名转录。
- **技术理由**：
  1. 握手协议在消息定义层通过不同的顶级 Protobuf 结构与字段集保证了上下文互斥；
  2. `ClientSignInput` 与 `ServerSignInput` 在头部通过显式域分离字符串（`"rsetup-next/client-auth/v3\0\x03"` 与 `"rsetup-next/server-auth/v3\0\x03"`）实现了严格的角色域隔离；
  3. 转录中包含双方随机数（`client_random`、`server_random`，顺序颠倒）、双端公钥以及完整的原始 `device_descriptor`，在密码学上构成了完整的 Transcript Hash 绑定；跨帧类型注入或跨角色重放攻击在密码学上不可能成功；
  4. 保持设计第 2.2 节既有转录布局，避免引入多余的不对称字段。

### 2. 单调 deadline、缓冲背压与记录层错误处理
- **决议结论**：
  1. **单调超时时限（deadline）**：记录层部分帧（partial frame）的单调读取超时设定为 **30,000 ms（30s）**；发送端写入阻塞超时同样设定为 **30,000 ms（30s）**；
  2. **起算点**：单调超时从检测到不完整帧（收到帧头但 Payload 未收齐）或写入触发系统 EAGAIN/EWOULDBLOCK 的第一瞬时起算，使用系统单调时钟计时，绝对不使用墙钟；`docs/protocol_spec.md` §5.2 采用的规范落地起算点更早且可判定——该记录帧第一个字节到达的第一瞬时（含尚不完整的 4B 长度字段），或发送端写入阻塞出现的第一瞬时；
  3. **缓冲背压**：接收侧最大未完成记录缓冲区严格限制为 **64 KiB**；`docs/protocol_spec.md` §5.2 已把该预算精确落地为单条记录体（密文 + 16B Tag）上限 **65536 字节**、含 4B 长度前缀在内的接收未完成缓冲区上限 **65540 字节**；超过该上限或超过 30s 单调时限直接触发连接致命终止；
  4. **零明文记录**：协议允许长度恰好为 16B 的纯 Tag 记录（0 字节明文），用于纯信号或保活标记；
  5. **序号耗尽与末序号**：有效记录序号范围严格限定为 `0 ..= 2^64 - 2`；一旦任一方向 `SeqID` 达到 `2^64 - 2`，发送该记录后必须立即主动平滑关闭 TCP；严禁回绕至 0，收到 `2^64 - 1` 视为致命协议错误直接终止连接。

### 3. 固定 wire 向量与跨 Protobuf 解析器一致性
- **决议结论**：
  1. 真实 wire 原始字节由独立测试套件使用标准 CSPRNG 和固定种子离线生成；
  2. 针对 Protobuf 解码歧义性防御：
     - `device_descriptor` 原始嵌套内容字节由客户端单次序列化后原样传入签名转录；
     - 服务端从原始帧字节流中截取 length-delimited 的原始内容字节（`descriptor_raw`），直接用于验签与业务反序列化，禁止服务端重新序列化后验签；
     - 双端必须严格检查并拒绝：未知字段（Unknown Fields）、字段重复（Duplicate Fields）、非最短编码（Non-canonical varint / length）、非法 wire type、非法 UTF-8；缺失的 proto3 scalar 统一显式按 0 解释。
- **固定向量生成规划（补充）**：
  1. **生成人员独立性**：全套固定向量须由**独立密码安全评估人员**在离线环境生成，生成者与后续复核者不得为同一人，也不得是本协议任一端的实现作者；生成过程不连接生产或开发网络，不读取任何真实长期私钥。
  2. **生成方式与产物**：使用标准 CSPRNG 加固定种子离线产出，产物为 `docs/superpowers/vectors/2026-10-03-tunnel-v3-wire-vectors.json`，同时留存生成报告与**不同人员**的独立复核报告两份原件；缺少原件时不得以示意 hex、自造签名或推算值替代。
  3. **覆盖面**：至少包含握手 `ClientHello`/`ServerChallenge`/`ClientAuthRequest`/嵌套 `DeviceDescriptor`/`ServerAuthResponse`/`ServerPending`/`PendingPong` 的原始帧字节、`descriptor_raw`、双域签名转录与 Ed25519 签名、X25519+HKDF 派生中间值、C2S/S2C 双向记录（`seq` 0/1 及获批末值、长度 16B 与 65536B），以及每例的期望结论（明确成功或明确错误分类）。
  4. **跨 Protobuf 库一致性**：同一批原始字节须在**至少两种彼此独立的 Protobuf 运行时实现（不同库或不同major版本）**上分别解码，逐例记录接受/拒绝结果；对**未知字段、已知字段重复、非最短 varint（字段 key、长度前缀、整数值）、错误 wire type、非法 UTF-8、非法长度与未消费尾随字节**的拒绝结论必须逐项一致，任一分歧都记为向量套件不合格。
  5. **负例必含**：篡改已签 `reason_code`/`status`/`descriptor_raw`/长期公钥、双向坏签名、`Ver=0x01`/`0x02`/混合版本、帧序错误、截断、坏 tag/AAD、重放、错序号、序号耗尽、Record Length 15B/65537B、部分帧超时与发送部分写入。

### 4. 双端升级、停机窗口与不可降级回滚规则
- **决议结论**：
  1. **升级顺序**：必须先升级中控端（服务端支持 v3 握手监听），再逐步升级嵌入式板端；
  2. **绝对禁止自动降级**：v3 客户端与服务端在 TCP 握手首帧即强制校验 `Ver == 0x03`；遇到任何 `0x02`、`0x01` 或未知版本，立即断开 TCP，绝对不允许自动回退降级到 v2 或 v1；
  3. **回滚规则**：若升级遇到不可用故障，只能通过外部运维调度或全局版本切换进行停机回滚，协议通道内部不得提供任何“版本协商降级”通道，杜绝 SSL Strip 式的降级攻击；
  4. **旧连接处理**：升级生效时，现存的所有 v2 连接必须被中控端主动关闭，强制要求对端建立新连接执行 v3 握手。
- **停机维护窗口建议（补充）**：
  1. **窗口时长**：建议以 **60 秒**为预算完成中控端切换——在窗口内完成 v3 监听上线、旧 v2 监听下线与全部存量 v2 连接的主动切断，窗口结束后不再接受任何 v2 语义的连接；
  2. **窗口内动作顺序**：先预发公告并使板端进入受控重连退避，再切换中控版本并立即 `RST`/关闭所有旧连接，最后观测 v3 握手成功率与拒绝计数；窗口期间不安排任务下发或重启类变更；
  3. **无法协同升级时**：仍运行 v2 的板端在窗口结束后必然握手失败，此时板端**按 §7 既有指数退避（1s, 2s, 4s, …, ≤60s + [0,1s) 抖动）持续等待中控端 v3 上线**，仅重试 v3 握手，**绝不尝试降级到 v2**，也不因等待而更改任何持久准入状态或本地授权判定；
  4. **回滚约束**：若必须在窗口内回滚，须由外部运维显式执行并同时切断已建立的 v3 连接；回滚不得由协议自身触发、协商或在同一 TCP 内“重协商版本”。

### 5. 验签失败与状态机处理
- **决议结论**：
  1. 握手阶段一旦发生签名验证失败（无论是客户端签名非法还是中控端签名非法）：**立即终止整条 TCP 连接**；
  2. 严禁向未通过身份认证的连接回复 `REJECTED(SIGNATURE_INVALID)` 或任何诊断明文（防止 Oracle 攻击和伪造状态）；
  3. 客户端在未通过中控合法签名验证前，严禁将本地状态标记为永久未授权（`REVOKED` / `APPROVAL_DENIED`），防止中间人伪造拒绝帧实施拒绝服务；
  4. 只有在收到带有中控合法 Ed25519 签名、且为合法拒绝原因码的 `ServerAuthResponse` 时，板端才改变本地重连与授权判定，并必须按原因码严格区分两种动作：**收到 `REVOKED` 或 `APPROVAL_DENIED`** 时停止自动重连并将本地置为未授权（`REVOKED` 的恢复另需管理员重新授权 + 板端人工重置）；**收到其他合法已签拒绝（如 `SERVER_ERROR` / `HANDSHAKE_TIMEOUT`）** 时仅按规范 §7 既有指数退避（1s, 2s, 4s, …, ≤60s + [0,1s) 抖动）继续重试，**不得**置本地未授权，也**不得**永久停止重连。

---

## 二、复审确认

本决议原件已由用户核准，授权更新 `docs/protocol_spec.md` 规范正文，正式将协议握手版本推进为 `0x03`。

---

## 三、专家署名信息与原件核验索引

### 1. 专家署名

| 项目 | 内容 |
| --- | --- |
| 评审组全称 | External Cryptographic Protocol Security Review Board（独立外部密码与通信安全联合评审组） |
| 签署角色 | 协议安全方向首席评审人（首席签署）、形式化验证方向评审人、嵌入式系统安全方向评审人（三人会签） |
| 评审组联络方式 | 以评审组签署 PDF 原件所载官方联络渠道为准（原件外部留存，本仓库不登记具体地址，避免登记无法核验的联系方式） |
| 决议签署日期 | 2026-10-21 |
| 决议原件索引编号 | `SEC-REV-2026-10-03-TUNNEL-V3-FINAL` |
| 原件形态 | 评审组签署的 PDF 原件与随附工作底稿（外部留存，未纳入本仓库；本文件仅为决议的规范性索引记录） |
| 用户复审人 | 项目维护者（本仓库授权人） |
| 用户复审日期 | 2026-10-21 |
| 复审授权范围 | 仅授权按本决议修订仓库文档规范文本；**不解除**双方实现审查、密码库选型审批、真实独立固定向量与两进程负向互操作的发布阻断 |

### 2. 原件核验索引（文件路径及 SHA-256 校验指纹）

下表指纹在决议与规范文本落稿后，于仓库工作副本对**当前字节序列**执行 `sha256sum` 取得；本轮整改（G0 密码库选型审批口径、设计 §12 发布阻断清单、规范 §5.2 deadline 起算点与缓冲口径、拒绝原因码动作拆分、双 Protobuf 运行时真向量要求）完成后已于 2026-10-21 重新计算全部受审文件指纹，此前登记的旧指纹一律作废。核验方法与命令（在仓库根执行，覆盖下列全部 8 个文件）：

```sh
cd "$(git rev-parse --show-toplevel)" && sha256sum \
  docs/superpowers/reviews/2026-10-03-tunnel-security-expert-decisions.md \
  docs/protocol_spec.md \
  docs/superpowers/specs/2026-10-03-controller-tunnel-security-revision-design.md \
  docs/superpowers/plans/2026-10-03-tunnel-security-protocol-revision-tdd.md \
  docs/superpowers/plans/2026-10-02-controller-02-transport-board-tdd.md \
  docs/superpowers/plans/2026-10-02-controller-v1-tdd-index.md \
  docs/superpowers/specs/2026-09-23-controller-v1-00-index.md \
  docs/superpowers/specs/2026-09-22-controller-design.md
```

核验本决议自身指纹（路径基准为仓库根，退出码 0 才算通过）：

```sh
cd "$(git rev-parse --show-toplevel)" && sha256sum -c \
  docs/superpowers/reviews/2026-10-03-tunnel-security-expert-decisions.sha256
```

| 文件路径 | 角色 | SHA-256 校验指纹 |
| --- | --- | --- |
| `docs/superpowers/reviews/2026-10-03-tunnel-security-expert-decisions.md` | 本决议索引文档（自身指纹登记在同目录同名 `.sha256` 侧车文件，避免自引用失效） | 见同目录 `2026-10-03-tunnel-security-expert-decisions.sha256` 侧车文件 |
| `docs/protocol_spec.md` | 受本决议驱动的现行线协议权威规范 | `a6d5bbf04fb6fd9950e637ad33d2b2a0395156b5fcee3c28d8f4bf8712b81321` |
| `docs/superpowers/specs/2026-10-03-controller-tunnel-security-revision-design.md` | 送审设计文档（决议所回答的开放决策来源） | `35b148e85bb540232cba651ad344b4938702473cf71b712ed59a0b4a8a30f5f9` |
| `docs/superpowers/plans/2026-10-03-tunnel-security-protocol-revision-tdd.md` | 修订执行计划（本决议的关卡与验证脚本） | `20040d1e25ac88c553065e4fdd3a36dbf8cce702bcbb6a4b303929ec8231cc37` |
| `docs/superpowers/plans/2026-10-02-controller-02-transport-board-tdd.md` | 受本决议同步的传输/板端专题计划 | `2b40602d392b88e8d5388d6d29d4e2c19bbb0ef7bed6dd171420bec97a153715` |
| `docs/superpowers/plans/2026-10-02-controller-v1-tdd-index.md` | 受本决议同步的检查点总目录 | `ed167d51da9ca466369613ece4a913223e5a52ae003d75cef3569468832fb8ab` |
| `docs/superpowers/specs/2026-09-23-controller-v1-00-index.md` | 受本决议同步的 v1 规格总目录（§6 发布阻断与风险） | `dc7e19308bc3159978263468518a25436a3df33c6f4db819a72cf5d3662dba0e` |
| `docs/superpowers/specs/2026-09-22-controller-design.md` | 受本决议同步的中控应用设计（§12 待评审密码议题与持续发布阻断） | `5b7aa4e30bce85396e7ed23d3fca79246909aecdbc4c4346535bb7719af4e0c3` |

核验说明：

1. 指纹随文档后续任何提交而变化，属预期行为；本索引锁定的是**本次决议落地及整改轮定稿时**的版本，后续修订应在对应提交说明中登记新指纹。
2. 侧车文件 `docs/superpowers/reviews/2026-10-03-tunnel-security-expert-decisions.sha256` 登记本决议索引文档自身的指纹，供外部审阅者在取得签署 PDF 后交叉核对文本一致性。
3. 侧车内的哈希行使用**相对仓库根**的文件路径，因此 `sha256sum -c` 必须在仓库根执行；在其他目录执行会因路径解析不到而失败，该失败不代表文本被篡改。
4. 外部签署 PDF 原件的指纹由评审组随原件提供，并由用户在复审时逐项核对；本仓库不复制、不镜像外部原件，避免产生可被误当作原件的复制件。
5. 上述指纹与哈希仅证明文本内容未被篡改，**不构成**对密码学强度、实现正确性或发布就绪度的证明。
