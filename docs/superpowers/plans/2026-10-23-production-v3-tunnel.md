# 生产 v3 加密隧道接线实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. 每个子代理只执行一次任务，首行必须为「请使用中文思考，不允许再派发子代理」。

**Goal:** 让板端主动连接中控，完成双向 Ed25519 认证、X25519/HKDF/AES-GCM、同 socket h2c 的 Control/Data gRPC 双流，并接入已有准入与 BoardClient；以真实两进程负例作为验收，而不是只以库内测试宣称生产完成。

**Architecture:** 既有 rsetup-protocol 仅保存纯协议/密码学。新增 rsetup-tunnel 提供身份文件、Tokio 握手、方向安全的加密 I/O、信封与 h2 会话。controller 持有 SQL 准入与连接管理；board-agent 主动连接、分发已有 BusinessCodec/BoardAgent，不另造 JSON 生产协议。

**Tech Stack:** Rust stable / edition 2024、ring、tokio、h2、http、bytes；前端仍由构建生成，不提交 dist。

**Spec:** `docs/protocol_spec.md`（§3–8），以及既有 controller 与 board-agent 契约。

## Global Constraints

- 只支持 amd64 / arm64；Rust 工具链为 stable，不重引入 1.85/MSRV。
- 未经用户明确允许不得 push。子代理不得 commit/push，不得访问工作区外文件进行修改。
- 用户已批准完整生产 v3 实现；不替换为 TLS 临时协议，不降级为 v2，不用原始 JSON 假冒业务通道。
- 设备是 TCP/h2 客户端，中控是 TCP/h2 服务端；先 OpenControl，再 OpenData。
- 私钥文件 0600，身份目录 0700；禁止隐式 TOFU，板端必须配置可信中控公钥列表。
- 帧 Payload ≤16384；记录体 16..65536，未完成记录缓冲≤65540；记录 partial read/write deadline 30s。
- 非 PENDING 握手步10s；PENDING probe周期5s、pong期限10s、板端静默15s；没有审批总期限。
- 信封≤512KiB，gRPC消息≤1MiB；trace_id非零、进程内单调；连接内pending≤1024且为双流probe预留。
- 记录认证失败、截断、deadline、部分写出即双向终止；不重放已发送变更，不在旧连接重同步。
- 不把主机真实重启实现扩为本任务隐含授权；原有能力不支持仍须如实返回。
- 无已部署实例：不继续扩展账户/配置升级迁移功能；之前审查的 conffile 必然变成 .dpkg-bak 的结论经实测未证实，不作为发布事实。
- 所有审查必须以源码与实测为依据；不虚构专家批准、备份、跨库或网络证据。

## 状态与裁决

- 已有：v3帧、签名转录、加密记录层、严格握手消息codec、初版适配层。修复尚未最终集成复审。
- Ruling: 在当前用户指定 dev-aghost 工作区继续，不另建 worktree搬移全部未提交成果；各writer按文件所有权隔离、使用不同CARGO_TARGET_DIR且禁止cargo clean。
- Ruling: writer优先使用codex-proxy/gemini-3.8-flash-high high。此前该路由无输出失败不可等同永久不可用；新任务重试，若失败记录原因并告知用户，不静默换模型。
- Ruling: 新增 rsetup-tunnel 隔离 IO，避免把 pure protocol crate 变成带生产网络和数据库依赖的大模块。

## 文件与接口地图

- `crates/rsetup-tunnel/src/secure_io.rs`：`SecureIo<S>`，实现 Tokio AsyncRead/AsyncWrite。构造 `SecureIo::new(io, RecordLayerPair)`。只能通过发送方向seal/接收方向open，任一失败导致共享关闭；认证前不得交付字节；空明文记录不误作EOF。
- `crates/rsetup-tunnel/src/identity.rs`：`IdentityKey::load_or_generate(path)`、`from_pkcs8(bytes)`、`public_key()`、`sign(message)`；Debug与错误去敏。
- `crates/rsetup-tunnel/src/handshake.rs`：下面列出的稳定接口。
- `crates/rsetup-tunnel/src/envelope.rs`：`StreamKind::{Control,Data}`、`PacketType`、`TunnelPacket::{encode,decode,validate}`。
- `crates/rsetup-tunnel/src/grpc.rs`：`encode_message(&TunnelPacket)`与有界`GrpcDecoder::push`。
- 下一波 `crates/rsetup-tunnel/src/session.rs`：h2双流、pending表、probe、流代际、连接PING、流重开/关闭。
- controller `src/transport/`：SQL准入及真实BoardClient；board-agent `src/transport.rs`：持久身份/信任、主动连接、journal与动作分发。

### Task 1: 安全记录方向与异步 I/O（可与 Task 2/3 并行）

**Files:** crypto.rs及其测试、handshake_adapter.rs及其测试、rsetup-tunnel/src/secure_io.rs、rsetup-tunnel/tests/secure_io.rs。

**Interfaces:** `SecureIo<S>::new(S, RecordLayerPair)` where S: AsyncRead + AsyncWrite + Unpin；`SecureIo` 本身实现 AsyncRead/AsyncWrite/Unpin，无外部后台任务；record split必须类型区分Tx仅seal、Rx仅open，不暴露两用原语。

- [ ] 先补回归：接收方向不能seal、发送方向不能open；单侧失败使对向失败；Reassembler发生fatal后不可复用；超限prefix到达即拒绝。
- [ ] 对 `SecureIo` 编写先失败的duplex测试：分片、多记录、大于单记录明文、空记录后合法数据、坏tag零明文、EOF截断、对向熔断。
- [ ] 用tokio paused time检验partial header/body固定30s，不因后续碎片刷新；没有首字节时不启动record partial deadline；partial write/写阻塞也有30s。
- [ ] AsyncWrite内部持有单条待写ciphertext，正确遵守 poll_write Pending 重入、poll_flush 与 shutdown；任何不确定写出不重新seal重发。
- [ ] 记录末有效seq发完后flush并关闭连接；不能等再一次write才关。
- [ ] 重构通过后：`cargo test -p rsetup-tunnel --test secure_io`，协议crate测试和clippy；禁止cargo clean。

### Task 2: 身份文件与真实签名握手（与Task1/3独立）

**Files:** rsetup-tunnel/src/identity.rs、handshake.rs及对应tests；不改secure_io/envelope/grpc。

**Interfaces:**
```rust
pub type AdmissionFuture<'a> = Pin<Box<dyn Future<Output=Result<AdmissionDecision, HandshakeError>> + Send + 'a>>;
pub enum AdmissionDecision { Pending, Approved, Rejected(u32) }
pub struct VerifiedPeer { pub public_key:[u8;32], pub descriptor:DeviceDescriptor }
pub trait AdmissionPolicy: Send + Sync { fn evaluate<'a>(&'a self, peer:&'a VerifiedPeer) -> AdmissionFuture<'a>; }
pub struct ClientHandshakeConfig { pub identity:Arc<IdentityKey>, pub trusted_servers:Vec<[u8;32]>, pub descriptor:DeviceDescriptor }
pub struct ServerHandshakeConfig { pub identity:Arc<IdentityKey> }
pub struct HandshakeResult<S> { pub io:S, pub session_keys:SessionKeys, pub peer_public_key:[u8;32], pub descriptor:Option<DeviceDescriptor> }
pub async fn client_handshake<S>(io:S, config:&ClientHandshakeConfig) -> Result<HandshakeResult<S>,HandshakeError>;
pub async fn server_handshake<S>(io:S, config:&ServerHandshakeConfig, policy:&dyn AdmissionPolicy) -> Result<HandshakeResult<S>,HandshakeError>;
```

- [ ] 实测RED：duplex双端签名握手产出可互解的SessionKeys；错误信任/篡改客户端签名不调用policy。
- [ ] 身份生成落盘0600，不覆盖已有有效身份；拒绝符号链接、错误属主/权限、坏格式；并发首次启动不产生两个身份；不回显私钥/路径内容。
- [ ] 确保解码的descriptor_raw直接进入签名转录；先验签再准入；CSPRNG random/eph每次新建。
- [ ] pending首probe立即、每5s、单在途10s、不设总期限；客户端15s静默；决定已持久化不发新probe、结清当前再final、最终前复查policy。
- [ ] 测试签名拒绝原因绑定、旧版本、pending超出旧总期限仍存活、nonce/seq失配、决定与pong竞争、未签名拒绝不得成为永久拒绝。
- [ ] 定向tests/clippy/fmt，记录未覆盖项，不改网络批准门默认值掩盖缺失。

### Task 3: TunnelPacket 与 gRPC 编帧（与Task1/2独立）

**Files:** rsetup-tunnel/src/envelope.rs、grpc.rs及对应tests。

**Interfaces:**
```rust
pub enum StreamKind { Control, Data }
pub enum PacketType { Request, Response, Event, System }
pub struct TunnelPacket { pub trace_id:u64, pub packet_type:PacketType, pub action:String, pub status_code:i32, pub error_message:String, pub payload:Vec<u8>, pub metadata:BTreeMap<String,String> }
impl TunnelPacket { pub fn encode(&self)->Result<Vec<u8>,EnvelopeError>; pub fn decode(bytes:&[u8])->Result<Self,EnvelopeError>; pub fn validate(&self, stream:StreamKind)->Result<(),EnvelopeError>; }
pub fn encode_message(packet:&TunnelPacket)->Result<Vec<u8>,GrpcError>;
pub struct InputCursor<'a>; // new(&[u8]), consumed(), remaining(), unconsumed()
pub struct PushOutcome<'a>; // packets(), consumed(), rest(), budget_exhausted()
pub struct GrpcDecoder; // new(), next_packet(&mut InputCursor)->Result<Option<TunnelPacket>>, next_packets(cursor,max<=64), push(&[u8])->Result<PushOutcome>, finish()
```

生产session逐个调用next_packet并背压处理，不能丢弃InputCursor未消费尾部；push仅有界便捷包装，不再声称一次完整消费任意chunk且返回无界Vec。

- [ ] 先写schema逐字fixture和RED：§6.4字段1..7、map嵌套、int32负数编码、512KiB边界。
- [ ] action小写/数字点分隔≤64B；metadata≤16/key64/value256；Request/Event/System不带response错误字段；SYSTEM仅control kick/revoke；ping请求响应nonce16、metadata空、响应status0/error空。
- [ ] gRPC 5B header（压缩标志0，BE32长度）、拒绝压缩、>1MiB前缀、累计垃圾与截断；每信封≤512KiB；不能一次分配无界peer长度。
- [ ] 分片/多消息/尾随/非法type/UTF8/重复metadata key 等精确断言；fatal解码器不得复用。
- [ ] 定向tests/clippy/fmt通过，必要跨实现fixture记为实测，不把它当负例矩阵已完成。

### Task 4a: 逐窗口唯一 HTTP/2 PING 适配器

**Files:** `rsetup-tunnel/src/h2_keepalive.rs`、`tests/h2_keepalive.rs`。
**Interface:** `H2KeepaliveIo<S>::new(io:S, role:H2Role)`，`H2Role::{Client,Server}`；实现 AsyncRead/AsyncWrite，不开后台task。

- h2 0.4.20 的公开 Ping::opaque/send_ping 忽略用户payload且内部复用固定token，不能直接满足每窗口唯一token/迟到ACK隔离；不得在会话中另调用h2::PingPong调度。
- 适配器位于SecureIo之上、h2之下。透明转发HTTP/2前导和全部普通帧；独立注入长度8/type6/stream0的PING，CSPRNG唯一token、15s窗口、至多一个逻辑在途，连续3次未匹配ACK关闭底层连接。
- 只匹配当前窗口token的ACK能刷新健康，迟到/重复/陌生ACK不算成功。对端PING请求仍交h2应答，绝不双重ACK。
- 输出只能在HTTP/2帧边界插入PING，禁止插入DATA/HEADERS中间；prefix与payload碎片/批量均透明，client出站/server入站24B前导不得误认帧头。
- 定时窗口从预定调度开始，写阻塞不暂停；不积压旧窗口PING；所有额外暂存缓冲有固定上限，帧payload优先流式转发，不囤积peer指定巨大帧。
- 先写duplex/paused-time RED测试：普通字节逐字不变、PING在线程/帧边界、token跨窗口不同、正常业务不代ACK、迟到ACK无效、3窗口失败、当前ACK清失败、shutdown不补历史探测。
- 不把该组件单测等同h2会话/真实双进程验收。

### Task 4: h2 会话与有界 RPC 路由

**经源码/RFC复核的接口裁决（覆盖预审中的错误建议）：**

- `GOAWAY.last_stream_id` 描述发送方处理过的**对端发起流**（RFC9113 §6.8），不是发送方本端创建的最大流。本协议禁用server push、server不创建流，故client发GOAWAY时last=0；server用h2官方graceful处理已接收的client奇数流。禁止手工取client本端最大奇数ID填GOAWAY。
- h2客户端确无公开graceful接口；IO交给h2后需预先取出的有界共享控制句柄（例如`H2WireControl`，单槽幂等GOAWAY请求+peerGOAWAY watch通知+AtomicWaker），不能假设具体泛型就能访问已移交IO。生产session入口直接接收`SecureIo<S>`并在内部建立H2KeepaliveIo与控制句柄，再把IO交h2。
- 控制句柄在已有HEADERS/CONTINUATION块结束后的安全边界注入client NO_ERROR GOAWAY，并通过poll_flush确认已写出；不得阻止h2输出已接流的DATA/trailers。入站GOAWAY观察只读其有界8B字段并逐字转交h2，以watch通知session；不能只等未来send_request报错（双流长开期间可能永不再调用）。
- `SendStream::stream_id().as_u32()`是公开API；不需要猜测或用自增计数代替真实h2流ID。
- 初次Control→Data；同类新流为旧流顶替（RST旧、旧代际失效后接新），server最大并发可为4容纳顶替，不硬拒重开；未知path返回grpc-status12。
- ready仅表示**本端**两流open且本端各首probe收到有效pong，不能声称观测到对端probe结果。首probe立即，不用initial jitter延后；后续连接间可相位错开但已启动窗口期限不放大。
- gRPC headers/trailers属于长生命HTTP2流，不是每个逻辑RPC；逻辑错误码走TunnelPacket，grpc-status在流EOS。GrpcDecoder生产用InputCursor+next_packet逐包，先处理/释放再继续，不忽略rest。
- probe独立小容量/优先队列，业务剩余pending<=2即Busy；每端每连接总<=1024，handler执行组有界且退出abort；事件有界丢弃计数，不堵reader；正常业务不能替代probe。
- 优雅关闭停止新业务、流重开和周期应用probe，发GOAWAY后drain至少5s，不无限等待心跳；h2所有衍生引用/任务退出并释放底层。连接级PING只用wrapper一套，h2内部graceful SHUTDOWN PING/ACK必须透传。

**Files:** session/{mod,driver,pending,probe,streams,route}.rs及tests；如需wire-control只在Task4a修复复审通过后扩展h2_keepalive.rs与测试。
**Consumes:** SecureIo任意AsyncReadWrite、HandshakeResult、TunnelPacket/GrpcDecoder。
**Produces:** 两条同连接h2流、cloneable有界SessionHandle、request(stream,action,payload)、同流回包、events/system信令。

- [ ] 红测试：board客户端按序开OpenControl/OpenData，服务端拒绝路径/方法/Content-Type错误、第三流/重复同种流；响应gRPC status合法。
- [ ] 1024 pending预算、两probe预留；全局单调trace；未知响应丢弃；timeout结果未知不重放；流代际防旧回调。
- [ ] 每流15s probe窗口，首次健康确认、3窗口失败重开单流、3次重开失败升整连接；连接PING/ACK独立3窗口，CSPRNG token，不以业务帧代替。
- [ ] h2流控1MiB、bounded发送队列、panic/错误结清所有pending、GOAWAY drain≥5s。
- [ ] 真socket测试后再接应用。

### Task 5: controller 生产准入与 BoardClient

**Files:** controller/src/transport/{mod,admission,manager,board_client}.rs、main.rs、Cargo.toml、tests。

- [ ] 经签名VerifiedPeer创建/查找设备、按公钥串行、容量/来源/全局速率限制、代际复查；通知sink唤醒pending/撤销，数据库不可用fail closed。
- [ ] SessionHandle实现六方法typed BoardClient；ticket/subtask/错误码映射；DeviceLiveProbe与RecoveryModeProbe依真实状态，SnapshotStore生产写入及poll循环。
- [ ] 新连接抢占旧连接kick+500ms、revoke终止并持久身份状态；HTTP认证与任务intent-before-send保持。
- [ ] 装配替换Unwired，不使用常量“在线/ready”蒙混；非配置显式禁用时准确NOT_READY。

### Task 6: board-agent 生产客户端及交付

**Files:** board-agent/src/transport.rs、bin入口、Cargo.toml、unit、debian示例/README。

- [ ] --connect/--config生产模式、静态信任列表、身份自生成/公钥导出、root unit、StateDirectory；不再默认watch假装生产。
- [ ] 1/2/4..60s+[0,1s)退避；签名revoke/deny才持久停止重连，人工reset才恢复；其他错误可重试且全新eph/random。
- [ ] handler复用BoardAgent与真实journal；业务codec、trace/action/同流响应，能力不支持不得假装执行；无自动重发变更。

### Task 7: 故障/互操作与容量验收

**Files:** scripts/tunnel验收、protocol fuzz targets、tests和去敏报告。

- [ ] 真实controller/board两个独立进程：批准/待审批/撤销/只读RPC，篡改、旧版本、坏签名、重放/乱序、15/65537、截断、partial写、30s deadline、fresh-key重连。
- [ ] 空Data仍有probe，业务流量不跳probe、流重开不误杀另一流、旧代际回包无效。
- [ ] ~1024设备与未认证/来源配额；独立protobuf实现正反矩阵；fuzz只声明实际运行次数/时间。
- [ ] 两个新只读reviewer验证代码与证据，按真实发现修复；全workspace测试/clippy/fmt、前端/CI合同、三deb检查。
- [ ] 在用户明确同意前不push；不能运行的外部验收如实列出，不宣称生产完成。
