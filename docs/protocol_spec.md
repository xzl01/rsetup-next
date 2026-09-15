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
|    - 双独立 HTTP/2 流: 数据流流控阻塞不阻塞控制流 (跨流无队头阻塞; 同流内 FIFO)     |
+-------------------------------------------------------------------------------+
| 2. 标准传输层 (HTTP/2 - h2c)                                                   |
|    - 运行于加密通道内部，无需 TLS                                              |
|    - 原生流多路复用 (Stream Multiplexing)                                      |
|    - 流量控制窗口 (建议调大至 >=1MiB)                                          |
|    - 原生 PING/PONG 帧快速心跳与死链检测                                       |
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

两条流是**独立的 HTTP/2 流**，数据流的流控阻塞不会阻塞控制流——这是“紧急控制不被大传输阻塞”（跨流无队头阻塞）的依据；同一 HTTP/2 流内部消息仍为 FIFO 顺序，本协议不提供跨流排序或重排保证。

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
* 本文档不引用或绑定任何业务动作目录；`action` 字段只定义格式约束，`tunnel.` 前缀保留给协议级系统信令。

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
 |          Magic (0x5345, 2B)   |   Ver (0x01)  |  FrameType(1B)|
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 |                        Payload Length (4B)                    |
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 |                        Payload (Protobuf) ...                 |
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```
* **Magic**：固定 `0x53,0x45`（ASCII "SE"）。
* **Ver**：`0x01`。
* **FrameType**：
  * `0x01`: `CLIENT_HELLO`
  * `0x02`: `SERVER_CHALLENGE`
  * `0x03`: `CLIENT_AUTH_REQUEST`
  * `0x04`: `SERVER_AUTH_RESPONSE`
  * `0x05`: `SERVER_PENDING`（审批挂起期间服务端周期通知，建议每 5s 一帧，须显著小于客户端 10s 帧等待超时）
* **Payload Length**（4B，大端）= 其后 Payload 的字节数。
* **单帧 Payload 上限 16KiB**，接收方超限立即断开。
* **严格消息序列**：
  * 服务端必须“先收到 `CLIENT_HELLO`、后收到 `CLIENT_AUTH_REQUEST`”；
  * 客户端必须“先收到 `SERVER_CHALLENGE`，然后 0..n 帧 `SERVER_PENDING`，最后收到 `SERVER_AUTH_RESPONSE`”。
  * 任何非法帧类型、乱序、Magic/Ver 不匹配均**立即断开 TCP**。
* **超时**：
  * 任一方等待对端握手帧 **10s**，超时断开；
  * 客户端整体握手等待上限 **360s**（自 TCP 连接建立起计，不重置）；PENDING 阶段内只要 **10s** 内收到任意一帧 `SERVER_PENDING` 或 `SERVER_AUTH_RESPONSE` 即视为等待有效（该 10s 帧等待窗口须对服务端 5s 的 PENDING 周期留有裕量）；超时断开；
  * 服务端 PENDING 挂起上限 **300s**，超时自动拒绝（`reason=HANDSHAKE_TIMEOUT`）。
* **PENDING 期间同一公钥的新连接直接替换（踢掉）旧的挂起连接。**

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
message ServerPending { uint32 wait_hint_ms = 1; }
message ServerAuthResponse {
  uint32 status = 1;                 // 0 = APPROVED, 1 = REJECTED
  uint32 reason_code = 2;            // 仅 REJECTED 有效：0=UNSPECIFIED, 1=SIGNATURE_INVALID, 2=REVOKED, 3=APPROVAL_DENIED, 4=HANDSHAKE_TIMEOUT, 5=SERVER_ERROR
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
          |              [PENDING 分支: 重复 0..n 帧]              |
          | 7a. SERVER_PENDING (wait_hint_ms, 建议每 5s 一帧)       |
          |<------------------------------------------------------| (客户端等待计时每帧重置为 10s)
          |                                                       |
          |              [审批结果 / 300s 挂起超时自动拒绝]          |
          | 7b. SERVER_AUTH_RESPONSE                              |
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

* **PENDING 挂起/超时**：服务端挂起期间周期发送 `SERVER_PENDING`（建议每 5s 一帧，显著小于客户端 10s 帧等待超时），挂起上限 300s，超时自动拒绝（`reason_code=4`，HANDSHAKE_TIMEOUT）；PENDING 期间同一公钥的新连接直接替换（踢掉）旧的挂起连接。
* **REJECTED 分支**：`status=1` 时 `server_x25519_eph_pub` 为全零占位；客户端验签后按 §4.6 的客户端行为处理。
* 任何非法帧类型、乱序、Magic/Ver 不匹配，双方均立即断开 TCP。

### 4.6 准入状态机

中控端按板端公钥维护设备准入状态：

* **PENDING**（待审批）：首次连接的新设备。中控端在内存中挂起该握手，周期下发 `SERVER_PENDING` 并触发人工审批；挂起上限 300s，超时自动拒绝（reason=HANDSHAKE_TIMEOUT）。
* **APPROVED**（已批准）：管理员已批准的设备。后续建连自动放行。
* **REVOKED**（已吊销/黑名单）：管理员手动吊销的设备。握手阶段直接回绝（reason=REVOKED）并掐断 TCP。

补充规则：

1. **加锁串行化**：服务端必须对同一公钥的“验签 → 查库 → 踢旧连接 → 接纳新连接”过程加锁串行化（防止两个新连接互相踢除）。
2. **客户端收到 REJECTED 后的行为**：`reason=REVOKED` 或 `APPROVAL_DENIED` → 终止自动重连、本地置为未授权状态；其他原因 → 按退避策略继续重连（见 §7）。

数据持久化边界：**服务端须以板端公钥为主键持久化准入状态（PENDING/APPROVED/REVOKED 及最后活跃时间等）；具体表结构、审批 API/UI、多设备会话管理属于中控应用设计，不在本文档范围。**

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

* `TYPE_SYSTEM` **仅允许在控制流上发送**；
* 其余类型可在任一流发送；`RESPONSE` **必须回到与其 `REQUEST` 相同的流**；
* 大载荷（文件分块）与高频遥测应**优先走数据流**；
* 控制流与数据流是独立 HTTP/2 流，数据流流控阻塞不会阻塞控制流——这是“紧急控制不被大传输阻塞”的依据；同一 HTTP/2 流内部仍为 FIFO，本协议不提供跨流排序/重排保证。

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
  string action = 3;             // 路由标识 (格式约束见下; tunnel. 前缀保留给协议级系统信令)

  int32 status_code = 4;         // 仅在 RESPONSE 中使用 (0=成功)
  string error_message = 5;      // 仅在 RESPONSE 异常时使用

  bytes payload = 6;             // 业务子消息序列化二进制 (内容格式由业务协议另行定义)
  map<string, string> metadata = 7; // 扩展元数据头 (可选)
}
```

信封字段约束：

* 序列化后整包 **≤512KiB**；gRPC 收发最大消息尺寸均配置为 **1MiB**；
* `action`：小写字母/数字/点分隔（如 `tunnel.kick`），长度 **≤64B**；`tunnel.` 前缀保留给协议级系统信令（`tunnel.kick`、`tunnel.revoke`），业务 action 不得使用；
* `metadata`：**≤16 项**，key **≤64B**，value **≤256B**；
* `status_code`：0=成功，非 0 错误码由业务协议定义；
* `trace_id`：uint64，各方进程内单调自增（**初值 1，双向独立分配**）；未决请求表容量 **≤1024**；默认请求超时 **5s**（可配置）；收到未知/已超时/已完成的 `trace_id` 的 `RESPONSE` 一律**静默丢弃**；
* `payload`：业务子消息以序列化二进制放入，其内容格式由业务协议另行定义，不在本文档范围。

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
* **连接级致命错误**（GOAWAY 非优雅、HTTP/2 连接错误、死链判定）→ 关闭 TCP → 按退避策略（§7）重新握手 → 重开双流；
* **单条流 RST_STREAM** → 该流关闭并**立即重开该流**，另一条流不受影响；连续 **3 次**重开失败则拆除整条连接（转入连接级重握手流程）；
* **优雅关闭**：关闭方先发 `GOAWAY`（drain ≥5s）再关 TCP；kick/revoke 不受此限制。

### 6.7 保活 (Keepalive)
* 启用 HTTP/2 协议级 `PING` 帧，**间隔 15s**；连续 **3 次**无回应（**45s**）判定死链并强制断开；
* gRPC keepalive 配置须与此一致，**不得叠加第二套心跳**。

---

## 7. 弱网（Wi-Fi）与异常容错规范

1. **大包与小包并发调度**：
   * 大文件/固件传输时，传输层**建议分块 32KiB** 连续发送（分块序号、校验和、续传等业务语义由业务协议另行定义，不在本文档范围）；
   * 大载荷走数据流、紧急控制包（如急停、状态查询）走控制流：两条流是独立 HTTP/2 流，数据流流控阻塞不会阻塞控制流，紧急控制包不会被大传输阻断（跨流无队头阻塞）；同一 HTTP/2 流内部仍为 FIFO。
2. **死链快速检测 (Keepalive)**：
   * HTTP/2 `PING` 间隔 15s，连续 3 次无回应（45s）判定连接假死，强制断开 TCP（配置见 §6.7，gRPC keepalive 须与此一致，不得叠加第二套心跳）。
3. **退避重连与连接排他**：
   * **重连退避**：板端断线后执行指数退避重连（1s, 2s, 4s, …，上限 60s），每次在退避值上附加 **[0,1s) 均匀随机抖动**，避免服务端网络抖动时的重连风暴。
   * **单连接排他（抢占式）**：中控检测到同公钥新连接完成准入后，先在旧连接控制流发送 `TYPE_SYSTEM` "tunnel.kick"（若旧流仍可用），**延迟 500ms 后关闭旧 TCP**，保障同一公钥仅存一条活跃连接。
4. **踢出与吊销信令**：
   * **临时踢出 (Kick)**：中控下发 `type = TYPE_SYSTEM`，`action = "tunnel.kick"`，随后关闭 TCP。板端按退避重连。
   * **永久吊销 (Revoke)**：中控数据库标记 `REVOKED`，下发 `type = TYPE_SYSTEM`，`action = "tunnel.revoke"` 并强制切断。板端终止所有自动重连循环，置本地为未授权状态；恢复需管理员重新授权 + 板端人工重置。

---

## 8. 一致性测试与验收

1. 握手各消息须提供跨实现 test vectors（含验签、HKDF 派生、Nonce 构造的固定输入输出）；
2. 握手解析器与记录层解析器必须通过 fuzz 测试（畸形帧、超长长度、乱序、重放记录）；
3. 双端实现必须通过双流路由规则与流故障处理的互操作测试。
