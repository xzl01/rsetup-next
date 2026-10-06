# Controller 设备协议核心首切片设计（2026-10-05）

## 目的与非目标

经用户同意，先为真实设备接入建立可审查的纯函数核心，不接网络、不解析 Protobuf、不写数据库。新建独立 Rust 1.85 库 `crates/rsetup-protocol`，只实现握手帧外壳、协议 §4.3 原始字节签名输入和基于已锁定并离线缓存的 `ring` 0.17.14 的真正 Ed25519 签名/验签。此切片本身**不构成握手、连接或 enrollment**，不得由中控 `main` 接线；不能出现公开/手动设备登记入口。

与其他方案比较：直接开始 TCP/板端实现会在无完整握手状态机时形成绕过验签的入库风险；现在自写通用 Proto3 编解码器会把未经评审的高风险解析引入安全关键路径；联网添加 `prost` 也需额外供应链授权。故本切片不涉及 codec，后续需独立审定固定字段的 codec 路线，再依次实现 X25519/HKDF、AES-GCM、独立 TCP listener、板端 agent、验签后受配额保护的内部 PENDING 入库，以及真实跨进程 TCP + 授权开发库验收。

## 固定字节契约

协议唯一依据为 `docs/protocol_spec.md` §4.1–4.3。本切片的 8B 帧头依序为 Magic `53 45`、版本 `02`、帧类型 `01..06`（CLIENT_HELLO、SERVER_CHALLENGE、CLIENT_AUTH_REQUEST、SERVER_AUTH_RESPONSE、SERVER_PENDING、PENDING_PONG）、4B 大端 PayloadLength。Payload 为不透明原始字节，长度 `0..=16384`，总帧不超过 16392B；先校验声明长度再分配，禁自动接受 v0.01 或未知类型。不完整 TCP 片段是可续收的 `Incomplete`，不是可忽略的畸形包。帧解析只核验外壳，不表示消息顺序已合法；真正序列检查留到未来握手状态机。

客户端 Ed25519 签名输入为 `client_random[32] || server_random[32] || client_x25519_eph_pub[32] || device_descriptor_wire_bytes`，总长 `96+D`；服务端输入为 `server_random[32] || client_random[32] || client_ed25519_pub[32] || client_x25519_eph_pub[32] || device_descriptor_wire_bytes || server_x25519_eph_pub[32] || status[1]`，总长 `161+D`，status 只允许 0/1。签名输入**不加长度前缀、不填充、不重序列化 descriptor**；descriptor 就是未来帧内嵌套消息的原始 wire bytes。`ClientAuthRequest` 固定字段开销 134B，嵌套 tag+2B varint，故由 16KiB 单帧界限派生 `D<=16247`；本切片只作上界防御，不定义 descriptor 字段语义。`reason_code` 当前协议不在签名输入内，不能私自改变签名格式。服务端 REJECTED 的临时 X25519 公钥为全零 32B 占位；此纯函数并不负责协商密钥。

Ed25519 验证先要求公钥 32B、签名 64B，再用 `ring::signature::UnparsedPublicKey::new(&ED25519, key).verify(message, signature)`；错误固定、去敏，不回显 key、消息或签名。签名薄封装只接收真实 `ring::signature::Ed25519KeyPair`，不得以固定返回值/字节相等冒充真实验签。测试固定输入经真实签名生成向量，并用被篡改的 random、eph 公钥、descriptor raw bytes、status、签名与错误公钥分别验证拒绝；测试签名成功不是联网握手证据。

## 接口与隔离

文件职责：`src/frame.rs` 只做帧编码/解析与有界长度；`src/sig.rs` 只拼两种签名输入并守长度/状态；`src/ed25519.rs` 只封装 ring 验签/签名；`src/lib.rs` 仅对外导出三者。工作区根 `Cargo.toml` 仅新增成员与 `ring` 现有版本直依赖，`Cargo.lock` 只允许**离线解析后检查**，不得先假定 `--locked` 下可直接通过，不能联网/升级其它版本或依赖漂移。新 crate 不含 `tokio`、listener、磁盘密钥加载、数据库访问、Protobuf codec 或身份授权接口。

## 测试和未验门禁

严格 TDD：先放置能编译的函数/类型桩，用行为断言观察 RED，再最小实现 GREEN。覆盖帧 0/16KiB/16KiB+1、分片不完整、Magic/Ver/FrameType/长度畸形、BE 字节序与无越界分配；覆盖签名顺序、`D=0/16247/16248`、status 0/1/非法、descriptor 一字节篡改、不同随机数的真实 Ed25519 正负向验签。测试向量由真实 ring 实现产生，不能捏造预期常量；能取得独立跨实现向量时须额外钉死。Cargo 首次仅允许 `--offline` 解析新工作区成员与已有 ring，检查 lock diff 无依赖版本漂移后，后续测试全部 `--offline --locked`；如不可离线解析即停止且保持不完整状态，不放宽来源。fmt/clippy 均过后独立审查规格与代码。

Protobuf 编解码、设备握手状态机/10s 超时、审批 PENDING ping-pong、server 信任根、X25519/HKDF/AES-GCM、资源配额、签名后入库、板端密钥持久化/0600、跨进程真实 socket 和 MySQL/TiDB 原子性**全部仍待后续切片**。真实设备 enrollment 的最终验收需两个独立进程经真正 TCP 握手与 Ed25519 签名，合法连接使获授权隔离 dev DB 落 PENDING，非法签名 0 写入；尚无此证据。
