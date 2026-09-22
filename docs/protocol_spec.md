# 板端与中控端自定义安全通信协议设计规范 (HTTP/2 + gRPC 方案)

本文档定义了中控端（Controller / Server）与嵌入式 Linux 单板计算机（SBC / Client）之间的自定义安全通信协议架构与规范。本版本聚焦**传输协议**部分（前置握手、准入状态语义、传输加密、HTTP/2 与 gRPC 流/信封路由、弱网与异常容错），范围边界见 §2。

---

## 1. 总体架构设计

本协议采用分层设计思想：在物理 TCP 之上构建**自定义安全加密通道（Secure Tunnel）**，在加密信道内直接运行**标准 HTTP/2 明文协议（h2c）**，并通过 **gRPC 双向流（Bidirectional Streaming）** 承载全对等业务通信。

```text
+-------------------------------------------------------------------------------+
| 4. 业务应用层 (Application Layer)                                              |
|    - 业务子消息以序列化二进制放入 TunnelPacket.payload                            |
|      (内容格式由业务协议另行定义，不在本文档范围)                                  |
+-------------------------------------------------------------------------------+
| 3. RPC 与对等通信总线层 (gRPC Bidi-Streaming Layer)                            |
|    - 持久双向流: OpenControl (控制流) 与 OpenData (数据流)                        |
|    - 信元信封 (Envelope Pattern) 实现 Req/Resp 配对 (同步/异步) 与单向通知         |
|    - 两条 HTTP/2 流共享连接流控与调度；跨流通常相互隔离，但不承诺绝对无阻塞   |
|    - 两条流各自承载应用级 tunnel.ping 请求/响应探测（含业务时仍运行）              |
+-------------------------------------------------------------------------------+
| 2. 标准传输层 (HTTP/2 - h2c)                                                   |
|    - 运行于加密通道内部，无需 TLS                                              |
|    - 原生流多路复用 (Stream Multiplexing)                                      |
|    - 流量控制窗口 (建议调大至 >=1MiB)                                          |
|    - 原生 PING/ACK 帧快速心跳与死链检测                                       |
+-------------------------------------------------------------------------------+
| 1. 自定义安全通道层 (Secure Tunnel / Crypto Layer)                            |
|    - 前置安全握手 (Handshake): 挑战-响应 (防重放) + Ed25519 身份认证           |
|    - 准入控制 (Admission): PENDING / APPROVED / REVOKED 状态机                 |
|    - 会话密钥协商: 临时 X25519 ECDH + HKDF-SHA256 密钥派生                      |
|    - 传输加密: AES-256-GCM 双向独立对称密钥 (C2S / S2C) + 严格单调递增 Nonce   |
+-------------------------------------------------------------------------------+
| 0. 物理传输层 (Physical Transport)                                             |
|    - 原生 TCP 长连接 (针对内网/Wi-Fi 高丢包网络优化)                             |
+-------------------------------------------------------------------------------+
```

与单流设计不同，本方案的总线层承载**两条相互独立的 gRPC 双向流**：

* **OpenControl（控制流）**：承载请求/响应与系统信令（`TYPE_SYSTEM`）；
* **OpenData（数据流）**：承载大载荷（文件分块）与高频遥测。

* **双向业务流**：`OpenControl` 与 `OpenData` 都是双向流；每条流均启用应用级 `tunnel.ping` 请求/响应探测。首版 `OpenData` 无业务消息时不发送空占位业务包，但仍必须建流并运行探测，不另增第三条心跳流。

两条流共享同一 HTTP/2 连接、连接级流控和调度资源。数据流流控通常不影响控制流，但协议不承诺绝对无阻塞；同一 HTTP/2 流内部消息仍为 FIFO，本协议不提供跨流排序或重排保证。

---

## 2. 范围与全局约定

### 2.1 文档范围

本文档只规划**传输协议**部分，包括：

* 前置握手：二进制帧格式、握手消息、签名输入字节布局、超时与消息序列约束；
* 准入状态语义：PENDING / APPROVED / REVOKED 及其状态迁移；
* 传输加密（记录层）：密钥派生、AES-256-GCM 记录帧、Nonce 与 AAD 规则；
* HTTP/2 与 gRPC 流/信封路由：双流服务定义、路由规则、信封约束、流级故障处理、保活；
* 弱网与异常容错：退避重连、单连接排他、Kick/Revoke 信令。

以下内容**不属于本文档范围**：

* 服务端准入状态持久化的具体表结构、审批 API/UI、多设备会话管理属于中控应用设计（边界声明见 §4.6）；
* 业务消息内容定义：业务子消息以序列化二进制放入 `TunnelPacket.payload`，其内容格式由业务协议另行定义，不在本文档范围。对大载荷传输，本文档仅给出“建议分块 32KiB”的传输层建议；分块序号、校验和、续传等均为业务语义，不在本文档定义；
* 本文档不引用或绑定任何业务动作目录；普通业务消息的 `action` 字段只定义格式约束。`tunnel.kick` 与 `tunnel.revoke` 是 `TYPE_SYSTEM` 信令，`tunnel.ping` 是双流应用探测，必须使用 `TYPE_REQUEST`/`TYPE_RESPONSE`，不属于 `TYPE_SYSTEM`。

### 2.2 全局约定

1. 所有多字节整数字段（Magic、Payload Length、Record Length、SeqID 等）均为**大端（Big-Endian）**。
2. 所有随机值（`client_random`、`server_random`、临时密钥等）必须由 **CSPRNG** 生成。

---

## 3. 身份凭据与初始配置

### 3.1 身份凭据自生成机制 (Self-Generating Keypairs)
* **板端 (Client)**：
  * 程序首次启动时检查本地配置目录/文件系统中的 Ed25519 长期私钥文件（如 `identity.key`）。
  * 若文件不存在，自动生成新的 Ed25519 密钥对并安全落盘；若已存在则直接加载复用。
* **中控端 (Server)**：
  * 程序首次启动时执行相同的自生成流程，生成并持久化中控端的 Ed25519 长期身份密钥对。

### 3.2 初始配置约定
* **中控端配置**：监听的 IP 和端口（其余应用层配置属于中控应用设计，不在本文档范围）。
* **板端静态配置**：
  * 在程序安装部署后，通过配置文件（如 `config.yaml`）人工填入，配置内容为：
    * 中控端连接目标：`Server_IP:Server_Port`；
    * **可信中控公钥列表**：一个或多个 Hex 公钥（取自中控端公钥导出值）。
* **中控密钥轮换**：轮换步骤：先向板端可信公钥列表追加新公钥 → 中控切换 → 移除旧公钥。

### 3.3 安全与威胁模型
* 板端/中控端 Ed25519 长期私钥文件权限必须为 `0600`。
* 威胁模型声明：板端 root 被视为可信（root 被攻破即身份被攻破）。

---

## 4. 安全握手与准入控制规范

在 TCP 物理连接建立后、HTTP/2 启动之前，双方在裸 TCP 上运行轻量级的前置握手协议（采用固定二进制帧封装明文 Protobuf 握手信令）。

**前置握手签名要求（规范性）**：前置握手数据必须由双方签名——

* **板端**须使用其**本地 Ed25519 长期私钥**对认证请求数据签名（签名输入布局见 §4.3 第 1 条）；
* **中控端**须使用其 **Ed25519 长期私钥**对认证响应数据签名，板端必须以**可信中控公钥列表**验签（签名输入布局见 §4.3 第 2 条）；
* 任一方验签失败，对端必须立即中止握手并断开 TCP。

### 4.1 前置握手二进制帧格式
```text
  0                   1                   2                   3
  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 |          Magic (0x5345, 2B)   |   Ver (0x02)  |  FrameType(1B)|
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 |                        Payload Length (4B)                    |
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 |                        Payload (Protobuf) ...                 |
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```
* **Magic**：固定 `0x53,0x45`（ASCII "SE"）。
* **Ver**：`0x02`。本版增加审批阶段握手级 ping-pong；与 `0x01` 不兼容，双方不得自动降级，收到旧版本立即拒绝。最外层 Magic 保持不变；本次设计只定义变更，未实现代码。
* **FrameType**：
  * `0x01`: `CLIENT_HELLO`
  * `0x02`: `SERVER_CHALLENGE`
  * `0x03`: `CLIENT_AUTH_REQUEST`
  * `0x04`: `SERVER_AUTH_RESPONSE`
  * `0x05`: `SERVER_PENDING`
  * `0x06`: `PENDING_PONG`
* **Payload Length**（4B，大端）= 其后 Payload 的字节数。
* **单帧 Payload 上限 16KiB**，接收方超限立即断开。
* **严格消息序列**：
  * 服务端先接收 `CLIENT_HELLO`，回复 `SERVER_CHALLENGE`，再接收 `CLIENT_AUTH_REQUEST`；认证与准入检查后可直接回复 `SERVER_AUTH_RESPONSE`，也可先进入 PENDING。
  * 客户端在发送认证请求后接收最终响应或首个 `SERVER_PENDING`；挂起期间收到 `SERVER_PENDING` 后回送 `PENDING_PONG`。服务端在挂起期间只接收匹配的 `PENDING_PONG`，客户端只接收新的 `SERVER_PENDING` 或最终响应，不允许反向使用帧类型。
  * 每连接仅一个在途 probe；非法帧类型、乱序、错误长度、token/nonce/序号不匹配或 Magic/Ver 不匹配均立即断开。转入加密通道后不得再收发握手帧。
* **初始超时与审批保活**：
  * 非 PENDING 的初始握手步骤均保留 **10s** 单调时钟等待期限；服务端验签及准入检查必须在此期限内作出直接响应或发送首个 `SERVER_PENDING`，过载不能无限等待。
  * **取消双方审批总等待期限**：已接纳且健康的挂起连接一直保留，直到人工决定、对端关闭、连接替换、服务停止或保活/协议失败；不因等待审批时间长而拒绝。此规则不取消资源配额，超限新连接按 §4.6 的可重试资源拒绝处理。
  * 首个 `SERVER_PENDING` 立即发送，兼作进入 PENDING 的通知和第一轮 ping。`status_nonce` 为本次挂起会话生成的随机 16B 值，生命周期内不变；`pending_token` 为每轮新生成的随机 16B 挑战；`probe_seq` 从 1 开始递增，不允许回绕。三者均绑定当前 TCP 连接及握手上下文，不作为授权凭据。
  * 每轮从 probe 提交到有界发送队列时开始计 **10s** 响应期限，队列阻塞不暂停计时。客户端立即原样回显三字段。名义发送周期 **5s**；上一轮未结清不叠加 probe，下一轮发送时刻为“不早于上轮发送后 5s 且已收到匹配 pong”。超时关闭连接并进入退避重连，不更改持久准入决定。
  * 客户端进入 PENDING 后，用 **15s** 接收静默期限检测中控失联：仅新的合法 `SERVER_PENDING` 或最终 `SERVER_AUTH_RESPONSE` 能结束/刷新该等待。发送 pong 本身不刷新期限；这不是审批总期限。客户端核对 `status_nonce` 不变、`probe_seq` 严格递增和字段长度后才回送 pong。
  * 审批决定已持久化后停止发新 probe，最多等当前 probe 剩余的 10s 期限结清，再发送最终认证响应；若 pong 未到则关闭连接，已保存决定保留到下一次握手。真正接纳前，在当前连接代际的短临界区复查准入状态，撤销优先；吊销或取消连接可以立即终止，不必等待 probe。
  * 明文 ping-pong 仅作连通性提示，没有认证中控或批准设备的效力；主动中间人仍可能伪造未签名的挂起通知。客户端只有验证最终签名后才能进入加密业务阶段，保活不能提升信任或改变授权。


### 4.2 握手消息 Protobuf 定义
```protobuf
syntax = "proto3";

message ClientHello { bytes client_random = 1; }                    // 32B
message ServerChallenge { bytes server_random = 1; }                // 32B
message DeviceDescriptor {
  string device_sn = 1;        // 硬件序列号 / MAC
  string device_model = 2;     // 硬件型号
  string fw_version = 3;       // 固件版本
  string extra_info = 4;       // 附加说明
}
message ClientAuthRequest {
  bytes client_ed25519_pub = 1;      // 32B
  bytes client_x25519_eph_pub = 2;   // 32B
  DeviceDescriptor device_descriptor = 3;
  bytes signature = 4;               // 64B Ed25519
}
message ServerPending {
  uint32 wait_hint_ms = 1;       // 固定 5000；只是名义周期提示，不能变更本版超时规则
  bytes pending_token = 2;       // 16B，每轮重新随机生成的挑战
  bytes status_nonce = 3;        // 16B，本次挂起会话不变的随机标识
  uint64 probe_seq = 4;          // 从 1 开始单调递增，不回绕
}
message PendingPong {
  bytes pending_token = 1;       // 必须逐字匹配
  bytes status_nonce = 2;        // 必须逐字匹配
  uint64 probe_seq = 3;          // 必须逐字匹配
}
message ServerAuthResponse {
  uint32 status = 1;                 // 0 = APPROVED, 1 = REJECTED
  uint32 reason_code = 2;            // 仅 REJECTED 有效：0=UNSPECIFIED, 1=SIGNATURE_INVALID, 2=REVOKED, 3=APPROVAL_DENIED, 4=HANDSHAKE_TIMEOUT（仅握手步超时，不是审批总期限）, 5=SERVER_ERROR
  bytes server_x25519_eph_pub = 3;   // 32B；REJECTED 时为全零占位（固定布局）
  bytes signature = 4;               // 64B Ed25519
}
```

### 4.3 签名输入字节布局
签名输入按以下顺序直接拼接，**无长度前缀**、无填充。

1. **客户端签名输入**（使用板端 Ed25519 长期私钥签名）：
   ```text
   client_random (32B) || server_random (32B) || client_x25519_eph_pub (32B) || device_descriptor_wire_bytes
   ```
   * 其中 `device_descriptor_wire_bytes` = 帧内 `DeviceDescriptor` 的 Protobuf 序列化字节原样。客户端只序列化一次：帧内携带字节与签名输入字节必须完全一致，服务端不得重新序列化（须按帧内字节原样验签）。
2. **服务端签名输入**（使用中控端 Ed25519 长期私钥签名；覆盖板端全部认证数据，使批准/拒绝结果与具体板端密码学绑定）：
   ```text
   server_random (32B) || client_random (32B) || client_ed25519_pub (32B) || client_x25519_eph_pub (32B) || device_descriptor_wire_bytes || server_x25519_eph_pub (32B) || status (1B)
   ```
   * `client_ed25519_pub`、`client_x25519_eph_pub`、`device_descriptor_wire_bytes` 均取 `CLIENT_AUTH_REQUEST` 帧内携带值原样（其中 `device_descriptor_wire_bytes` 与第 1 条板端签名输入所用字节一致）；
   * 板端用自身发送的字段与收到的 `SERVER_AUTH_RESPONSE` 字段重建期望签名输入后验签：中控端若未针对本板端认证数据签名，验签必然失败；
   * `REJECTED` 时 `server_x25519_eph_pub` 为全零（与消息体内全零占位一致）；`status` 为 1 字节（0 = APPROVED, 1 = REJECTED）。
3. **重放防护**：`server_random` 为每会话新鲜值并绑定进双方签名，因此跨会话重放握手帧必然验签失败。

### 4.4 X25519 约束
遵循 RFC 7748：拒绝全零/低阶公钥；共享密钥为全零时终止会话。

### 4.5 握手与准入流程时序

```text
    板端 (Client)                                          中控端 (Server)
          |                                                       |
          | 1. TCP Connect                                        |
          |------------------------------------------------------>|
          |                                                       |
          | 2. CLIENT_HELLO (client_random, 32B)                  |
          |------------------------------------------------------>|
          |                                                       |
          | 3. SERVER_CHALLENGE (server_random, 32B)              |
          |<------------------------------------------------------|
          |                                                       |
          | [本地准备] 生成临时 X25519 密钥对 (c_eph_pri, c_eph_pub)  |
          |    按 §4.3 布局以板端 Ed25519 私钥签名                 |
          | 4. CLIENT_AUTH_REQUEST                                |
          |       - client_ed25519_pub (32B)                      |
          |       - client_x25519_eph_pub (32B)                   |
          |       - device_descriptor (SN, Model, FW, Extra)      |
          |       - signature (64B)                               |
          |------------------------------------------------------>|
          |                                                       | 5. 校验板端签名
          |                                                       |    失败 -> REJECTED (reason_code=1)
          |                                                       | 6. 按公钥查库 (同公钥过程加锁串行化):
          |                                                       |    REVOKED  -> REJECTED (reason_code=2)
          |                                                       |    APPROVED -> 批准
          |                                                       |    未录入   -> PENDING, 挂起连接, 触发审批
          |                                                       |
          |              [PENDING 分支：握手级 ping-pong]              |
          | 7a. SERVER_PENDING (token, status_nonce, probe_seq)       |
          |<------------------------------------------------------|
          | 7b. PENDING_PONG (板端→中控，回显 token/status_nonce/probe_seq) |
          |------------------------------------------------------>|
          |                                                       |
          |              [审批结果]          |
          | 7c. SERVER_AUTH_RESPONSE                              |
          |       - status = 0 (APPROVED) / 1 (REJECTED)          |
          |       - reason_code (仅 REJECTED 有效)                 |
          |       - server_x25519_eph_pub (REJECTED 时全零占位)    |
          |       - signature (64B)                               |
          |<------------------------------------------------------|
          |                                                       |
          | 8. 校验中控端签名 (匹配可信中控公钥列表)                 |
          |    APPROVED: 计算 X25519 共享密钥                      |
          |               HKDF 派生会话密钥                        |
          |    REJECTED: 见 §4.6 客户端行为                        |
          |                                                       |
          +=======================================================+
          | 握手成功: TCP Socket 接入 AES-256-GCM 双工加密包装器    |
          | 同一 socket 启动 h2c: 先开控制流 OpenControl            |
          | 再开数据流 OpenData                                    |
          +=======================================================+
```

分支说明：

* **PENDING 挂起**：审批没有总期限；按 §4.1 的名义 5s 周期、10s pong 期限和客户端 15s 静默期限检测连接，不叠加总等待时限。超时仅关闭当前连接，保留最新持久准入决定，重连必须从新握手开始。
* **审批与 probe 竞争**：按 §4.1 停止新 probe 并结清在途；最终接纳前复查当前代际和准入决定，延迟 pong 不得覆盖吊销。
* **REJECTED 分支**：`status=1` 时 `server_x25519_eph_pub` 为全零占位；客户端验签后按 §4.6 的客户端行为处理。
* 任何非法帧类型、乱序、Magic/Ver 不匹配，双方均立即断开 TCP；握手阶段不在 HTTP/2 流中混入握手帧。

### 4.6 准入状态机

中控端按板端公钥维护设备准入状态，记录去重持久化，连接断开不删除记录：

* **PENDING**（待审批）：板端签名验证通过、身份配额允许且尚未作出决定的新身份。健康挂起不设审批总期限；记录关联当前连接代际，probe token/nonce 仅在会话内有效，不跨重连复用。
* **APPROVED**（已批准）：管理员已明确批准的公钥，后续建连自动放行；接纳前在短临界区核对最新状态和连接代际。
* **REVOKED**（已吊销/黑名单）：管理员吊销的设备，握手阶段回绝；恢复必须“管理员重新授权 + 板端人工重置本地未授权状态”，见 §7 第 4 条。不能因普通重连或旧批准恢复。

补充规则：

1. **有界资源**：未认证连接、挂起连接、新身份记录分别限额，并限制全局及来源的握手速率。按公钥去重不防御攻击者生成大量不同公钥，因此即使验签通过，新身份入库也必须受容量约束。已接纳且健康的挂起连接不为容纳新连接而按等待时长驱逐；已接纳记录保留。
2. **资源不足不是拒绝审批**：已具备完整认证上下文时返回签名 `REJECTED(reason=SERVER_ERROR)`，客户端退避重试；尚未具备该上下文时直接拒接/关闭 TCP，并按连接失败退避。不得用 `APPROVAL_DENIED` 或 `REVOKED` 表示过载，也不写入吊销决定；中控日志明确记录资源限额原因。
3. **锁与连接代际**：验签、查库和连接登记的同公钥状态转换串行化，但等待人工审批、等待 pong 和网络写入不长时间持锁。新连接替换旧挂起连接后，旧连接的异步回调无权更改新连接状态；批准接纳与吊销需要按最新代际重新检查。
4. **拒绝后的板端行为**：`REVOKED` 或 `APPROVAL_DENIED` 停止自动重连并置未授权；其他拒绝原因按 §7 退避重试。`APPROVAL_DENIED` 的持久记录及管理员重新开放申请流程仍需应用层细化，不与资源拒绝混用。
5. **审批界面边界**：应用设计建议提供按明确公钥清单的批量审批，不能仅依赖 IP 或序列号自动信任。公钥预录入为后续可选能力，不是本协议的隐式准入路径。

数据持久化边界：以板端公钥为主键保存准入状态、最后活跃时间及状态代际等；具体表结构、审批 API/UI 和多设备管理见[中控应用设计 §5](superpowers/specs/2026-09-22-controller-design.md)。

---

## 5. 传输加密规范 (AES-256-GCM)

安全握手完成后，底层 TCP 转化为基于 AES-256-GCM 的加密记录流。

### 5.1 密钥派生规范
1. **ECDH 共享密钥**：
   $$\text{SharedSecret} = \text{X25519}(\text{eph\_private}, \text{remote\_eph\_public})$$
2. **HKDF-SHA256 派生**：
   * $\text{Salt} = \text{client\_random} \parallel \text{server\_random}$（64B）
   * $\text{PRK} = \text{HKDF-Extract}(\text{Salt}, \text{SharedSecret})$
   * 以 HKDF-Expand 派生四次（`info` 字符串逐字使用，不得更改）：

   | `info`（逐字） | 输出 |
   | --- | --- |
   | `rsetup-next/tunnel/c2s-key` | 32B C2S AES-256 密钥 |
   | `rsetup-next/tunnel/s2c-key` | 32B S2C AES-256 密钥 |
   | `rsetup-next/tunnel/c2s-iv` | 12B C2S IV-Base |
   | `rsetup-next/tunnel/s2c-iv` | 12B S2C IV-Base |

   $$\text{KDF}(\text{info}, L) = \text{HKDF-Expand}(\text{PRK}, \text{info}, L)$$

### 5.2 加密记录帧与 Nonce 生成
加密通道上的数据以记录帧传输：
```text
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 |       Record Length (4B, BE)  |   AES-256-GCM 密文 + 16B Tag  |
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```
* **Record Length**（4B，大端）= 其后密文字节数（含 16B GCM Tag）；**单记录上限 64KiB（密文+Tag）**。
* **Nonce 生成**：
  * 发送方独立维护单调自增计数器 `TxSeq`（uint64，初值 0，每发一记录 +1）；
  * Nonce 前 4 字节 = IV_Base 前 4 字节；
  * Nonce 后 8 字节 = IV_Base 后 8 字节 XOR `BE64(TxSeq)`（计数器 XOR 进最后 8 字节，TLS 1.3 风格）：
    $$\text{Nonce}[0:4] = \text{IV\_Base}[0:4]$$
    $$\text{Nonce}[4:12] = \text{IV\_Base}[4:12] \oplus \text{BE64}(\text{TxSeq})$$
* **AAD 保护**：
  * $\text{AAD} = \text{Record Length (4B)} \parallel \text{BE64}(\text{SeqID})$（共 12B），防止长度字段与计数器位置被篡改。其中 `SeqID` 为**当前记录所对应序号的 64 位值**（即参与该记录 Nonce 构造的同一计数值）：发送方填入其 `TxSeq`，接收方用当前期望的 `RcvSeq` 重建 AAD 参与校验。
* **接收端规则**：
  * 接收方维护 `RcvSeq`（初值 0），用当前 `RcvSeq` 重建期望 Nonce 进行 GCM 校验；
  * 仅当校验通过后 `RcvSeq += 1`；校验失败的记录丢弃且计数器不自增；
  * 由此本会话内重放旧记录必然因 Nonce 错位导致 Tag 校验失败。

---

## 6. HTTP/2 与 gRPC 双流规范

### 6.1 绑定与启动
* 加解密记录流就绪后，**同一 TCP socket 上启动 h2c**：板端为 h2c 客户端，中控端为 h2c 服务端。
* 建议启用 `TCP_NODELAY`；建议将 HTTP/2 流级/连接级流控窗口调大至 **≥1MiB**（大文件传输场景）。

### 6.2 服务定义
```protobuf
service TunnelService {
  rpc OpenControl(stream TunnelPacket) returns (stream TunnelPacket);  // 控制流：请求/响应、系统信令
  rpc OpenData(stream TunnelPacket) returns (stream TunnelPacket);     // 数据流：大载荷、高频遥测
}
```
板端建连后立即**先开控制流（OpenControl）、再开数据流（OpenData）**。

### 6.3 流路由规则
两种流共用同一 `TunnelPacket` 信封结构（见 §6.4），路由规则如下：

* `TYPE_SYSTEM` 仅允许在控制流上发送，且只用于 `tunnel.kick` / `tunnel.revoke`；`tunnel.ping` 不得使用 `TYPE_SYSTEM`；
* `TYPE_REQUEST`/`TYPE_RESPONSE` 可在任一流发送；`RESPONSE` 必须回到与其 `REQUEST` 相同的流，并保持同 action、同 trace_id；
* `TYPE_EVENT` 可在任一流发送，不回包；高频事件优先走数据流；
* 大载荷（文件分块）与高频遥测应优先走数据流；
* 控制流与数据流共享同一 HTTP/2 连接、连接级流控和调度资源；数据流流控通常不影响控制流，但不能承诺绝对无阻塞；同一 HTTP/2 流内部仍为 FIFO，本协议不提供跨流排序/重排保证。

### 6.4 通用信元信封 (Envelope Pattern)
流内传输的统一 Protobuf 消息结构：

```protobuf
syntax = "proto3";
package custom_tunnel;

enum PacketType {
  TYPE_UNKNOWN = 0;
  TYPE_REQUEST = 1;     // 双向请求 (需要对应响应)
  TYPE_RESPONSE = 2;    // 对请求的响应
  TYPE_EVENT = 3;       // 单向通知 / 遥测上报 (无需响应)
  TYPE_SYSTEM = 4;      // 协议级系统信令 (tunnel.kick / tunnel.revoke)
}

message TunnelPacket {
  uint64 trace_id = 1;           // 消息唯一跟踪/关联 ID
  PacketType type = 2;           // 信元类型
  string action = 3;             // 路由标识 (tunnel. 保留给协议动作，含系统信令和探测)

  int32 status_code = 4;         // 仅在 RESPONSE 中使用 (0=成功)
  string error_message = 5;      // 仅在 RESPONSE 异常时使用

  bytes payload = 6;             // 普通业务二进制；tunnel.ping 例外为原始 16B nonce
  map<string, string> metadata = 7; // 扩展元数据头 (可选)
}
```

信封字段约束：

* 序列化后整包 **≤512KiB**；gRPC 收发最大消息尺寸均配置为 **1MiB**；
* `action`：小写字母/数字/点分隔（如 `tunnel.kick`），长度 **≤64B**；`tunnel.kick`、`tunnel.revoke` 仅作 `TYPE_SYSTEM` 信令，`tunnel.ping` 仅作双流应用 `TYPE_REQUEST`/`TYPE_RESPONSE` 探测；其他业务 action 不得使用 `tunnel.` 前缀；
* `metadata`：**≤16 项**，key **≤64B**，value **≤256B**；
* `status_code`：0=成功，非 0 错误码由业务协议定义；
* `trace_id`：uint64，各方进程内单调自增（**初值 1，双向独立分配**）；未决请求表容量按**每端、每设备连接会话**计 **≤1024**（含该连接两条流的本端 probe；不按容量预分配）；默认普通请求超时 **5s**（可配置，`tunnel.ping` 专用 15s 见 §6.7.2）；收到未知/已超时/已完成的 `trace_id` 的 `RESPONSE` 一律**静默丢弃**；
* `payload`：普通业务子消息的二进制格式由业务协议另行定义；协议探测 `tunnel.ping` 的请求及响应载荷例外，由 §6.7.2 定义为原始 16B nonce。

### 6.5 三种交互模式的实现机制

#### 1. 双向对等同步调用 (Sync RPC)
* 无论是板端调中控，还是中控调板端：
  1. 调用端分配进程内单调自增 `trace_id`（初值 1，双向独立），创建本地等待上下文（Promise / Future / Event），设定调用超时（默认 5s，可配置）；
  2. 发送 `type = TYPE_REQUEST` 的 `TunnelPacket`（记录所在流，供响应回流）；
  3. 接收端分发并处理，生成回包并封装为 `type = TYPE_RESPONSE`，填充相同的 `trace_id`，并回到与 REQUEST 相同的流发送；
  4. 调用端接收层根据 `trace_id` 匹配未决请求表（容量 ≤1024），恢复调用上下文并返回结果；未知/已超时/已完成的 `trace_id` 一律静默丢弃。

#### 2. 双向对等异步调用 (Async Callback)
* 机制与同步完全相同，区别在于调用端不挂起线程，而是将回调函数句柄与 `trace_id` 关联保存在回调表中；收到响应包后触发异步执行。

#### 3. 单向通知 / 遥测上报 (One-way Notification)
* 发送端直接发送 `type = TYPE_EVENT` 包（高频遥测建议走数据流）；接收端消费即可，禁止回包。

### 6.6 流级与连接级故障处理

* 连接级致命错误（非优雅 GOAWAY、HTTP/2 错误或连接死链）→ 关闭 TCP、失败结清该代际全部未决请求、按 §7 退避重连并重新握手。多个故障结果合并处理，不重复清理或启动多条重连循环。
* 单流 `RST_STREAM` 或 §6.7 的应用探测失败 → 仅关闭该流并结清该流未决请求，另一流不自动判坏。板端作为 gRPC 发起方负责重开原类型的流；中控通过 `RST_STREAM` 触发，不能反向调用 RPC 创建替代流。
* 初次开流/重开建立的等待期限为 15s；建立后立即发首个应用 probe，首次健康确认期限为该 probe 的 15s。打开失败或未通过首次健康确认算一次重开失败，连续 3 次才升级为连接级重建。各端只有收到本端首次 probe 的有效 pong 后才重置该流重开失败计数，不能仅因 RPC 打开成功重置；任一端认为失败均可终止该流。
* 重开后分配新流代际，旧流响应、定时器和排队消息不能作用于新流。未完成的业务操作不能因重开流而自动重放，是否核实结果由业务协议决定。
* 优雅关闭先发 GOAWAY，drain 至少 5s 后关 TCP；kick/revoke 不受此限制。进入排空后不再启动新周期探测，不为等心跳无限延长排空。

### 6.7 保活 (Keepalive)

本版保留连接级保活，同时要求每条双向 gRPC 流通过应用级探测。二者检查不同边界，允许且要求共存；gRPC 与 HTTP/2 不得重复启动两套底层 PING。

#### 6.7.1 HTTP/2 连接级 PING/ACK

* 使用 HTTP/2 `PING` 帧及带 ACK 标志的回应，而非单独的 PONG 帧。每端至多一套连接保活调度器，gRPC keepalive 映射此机制。
* 采用连续 15s 探测窗口，每窗口发一个唯一 opaque token（HTTP/2 固定 8B），至窗口结束等待匹配 ACK；最多一个在途。正常业务帧不能代替该 ACK。
* 连续 3 个窗口无有效 ACK（从首个失败窗口开始约 45s）判定连接死链；匹配 ACK 清除连续失败数。迟到 ACK 不计入新的窗口；发送阻塞也计入窗口，不停止计时。
* 连接级成功只证明 HTTP/2 通路活跃，不证明应用 handler 或设备操作完成。约 45s 从首次未获回应的探测计算，不是任意物理故障发生后的硬实时上界。

#### 6.7.2 OpenControl / OpenData 应用级 Ping/Pong

* 两端分别在 `OpenControl` 和 `OpenData` 主动探测；即使有业务流量也不跳过。首版 `OpenData` 没有业务消息时仍建流并探测，不创建第三条心跳流。
* 请求固定 `action="tunnel.ping"`、`type=TYPE_REQUEST`；`payload` 是**恰好 16B 的原始随机 nonce**，不额外包一层 Protobuf，metadata 为空。使用正常分配的非零 `trace_id`，在未决表记录当前连接、流代际和该 nonce。
* pong 为同一流上的 `TYPE_RESPONSE`：同 action、同 `trace_id`、`status_code=0`、空 `error_message`/metadata、原样回显 16B nonce。不得用 `TYPE_SYSTEM` 或 `TYPE_EVENT` 代替。
* 请求接收方经应用消息解码和分发后验证类型、action、字段约束及当前流代际，再生成 pong；不能由 socket 层或 HTTP/2 ACK 代答。响应接收方另外检查未决项、nonce、连接/流代际及截止时刻。流代际来自本地流上下文，不信任对端自报；双向 trace_id 可以相同，按请求/响应方向区分。
* 非法探测请求按流级协议错误终止该流；未知/过期/已完成的响应静默丢弃。已知 probe 的字段或 nonce 不匹配不能认定健康，也不能刷新期限；探测消息不能触发设备操作。
* 开流后立即发送首个 probe；健康运行后使用连续 15s 窗口，在窗口开始提交 probe，期限为窗口结束。探测专用超时是 **15s**，不是普通请求的 5s。窗口边界先结束上一轮再创建下一轮，迟到响应不能匹配新轮。首次健康确认与运行期连续失败的不同规则见 §6.6。
* 每方向/每流只一个在途，正常运行连续 3 个窗口失败判该流不健康；有效 pong 清零该流连续失败数。全部计时从预定调度/入队开始，排队或写阻塞不暂停，不积压补发历史窗口。
* probe 与普通请求共用每端、每连接最多 1024 项的未决预算，必须为该连接每个已建立流的本端 probe 预留名额；中控另设跨设备全局在途与内存预算，不按 1024×设备数预分配。业务入队限流而不是让保活无限等待。接收端 probe 处理和响应队列也需有界。不能宣称预留队列就能越过已写入的 FIFO 或避免 TCP 丢包阻塞。
* 成功只证明对应解码/分发和往返链路可用，不证明业务 worker、采集结果或重启成功；任务仍需自己的执行期限与结果证据。

#### 6.7.3 统一计时与调度边界

握手保活、连接 PING、逐流 probe 和重试计时均使用本机单调时钟，不受 NTP 或墙钟变化影响。连接之间错开探测相位但不放大已启动窗口的期限；连接故障优先于子流恢复，每个流只有一个恢复流程。审批阶段按 §4.1 使用握手帧，不在业务流中发送审批帧。

---

## 7. 弱网（Wi-Fi）与异常容错规范

1. **大包与小包并发调度**：
   * 大文件/固件传输时，传输层**建议分块 32KiB** 连续发送（分块序号、校验和、续传等业务语义由业务协议另行定义，不在本文档范围）；
   * 大载荷走数据流、紧急控制包（如急停、状态查询）走控制流：两条流共享 HTTP/2 连接、流控和调度；控制流通常可独立推进，但共享资源下不保证紧急包绝不阻塞；同一 HTTP/2 流内部仍为 FIFO。
2. **死链快速检测 (Keepalive)**：
   * HTTP/2 `PING`/`ACK` 连接级保活和双流应用 `tunnel.ping` 探测分别按 §6.7.1 与 §6.7.2 执行；运行期连续失败与重开流的首次健康确认规则按 §6.6 处理，不把连接健康当成业务成功。
3. **退避重连与连接排他**：
   * **重连退避**：板端断线后执行指数退避重连（1s, 2s, 4s, …，上限 60s），每次在退避值上附加 **[0,1s) 均匀随机抖动**，避免服务端网络抖动时的重连风暴。
   * **单连接排他（抢占式）**：中控检测到同公钥新连接完成准入后，先在旧连接控制流发送 `TYPE_SYSTEM` "tunnel.kick"（若旧流仍可用），**延迟 500ms 后关闭旧 TCP**，保障同一公钥仅存一条活跃连接。
4. **踢出与吊销信令**：
   * **临时踢出 (Kick)**：中控下发 `type = TYPE_SYSTEM`，`action = "tunnel.kick"`，随后关闭 TCP。板端按退避重连。
   * **永久吊销 (Revoke)**：中控数据库标记 `REVOKED`，下发 `type = TYPE_SYSTEM`，`action = "tunnel.revoke"` 并强制切断。板端终止所有自动重连循环，置本地为未授权状态；恢复需管理员重新授权 + 板端人工重置。

---

## 8. 一致性测试与验收

1. 握手各消息须提供跨实现 test vectors（含验签、HKDF 派生、Nonce 构造的固定输入输出）；Ver `0x01` 必须拒绝且不得自动降级，Magic 保持不变。
2. 握手解析器与记录层解析器必须通过 fuzz 测试（畸形帧、超长长度、乱序、重放记录）；覆盖 `SERVER_PENDING`/`PENDING_PONG` 的 token、status_nonce、probe_seq 匹配、单连接单在途和 10s 步超时，验证超时断线后 PENDING 记录保留。
3. 双端实现必须通过双流路由规则与流故障处理的互操作测试：两条流均发送/响应 `tunnel.ping`，类型为 `TYPE_REQUEST`/`TYPE_RESPONSE`，同 action、trace_id、流和 16B nonce；验证有业务与空 OpenData 都探测，3 个 15s 窗口失败只重开流，升级条件和连接级 PING/ACK 分层一致。
4. 容量测试覆盖约 1024 台设备、未认证/挂起按来源与全局配额、审批等待锁外、按公钥去重持久化和批量明确选钥；验证超限为明确资源拒绝，不因未知随机身份无限增库。
5. 覆盖审批等待超过原有总期限仍健康保留、审批决定与在途 pong 竞争、超限拒接可退避重连、客户端静默检测、旧流响应与探测 nonce 不匹配、发送队列阻塞和墙钟回跳；协议计时不因 NTP 恢复改变。中控 NTP、备份和界面验收另见[中控应用设计 §13](superpowers/specs/2026-09-22-controller-design.md)。
