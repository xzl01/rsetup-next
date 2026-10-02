# 独立中控 02：传输与板端 TDD 实施计划

> 致执行代理：使用 `superpowers:subagent-driven-development`（推荐）或 `superpowers:executing-plans` 逐项勾选。本文仅计划，未实施。

**Goal:** 新增 `rsetup-protocol`、`rsetup-board-agent`，实现双方共用协议类型/握手/加密记录层、gRPC双流/probe、准入及板端只读、持久日志、prepare/execute OS安全边界。

**Architecture:** protocol独立于core，输出wire/frame/crypto/record/admission/tunnel接口；board-agent依赖protocol/core，限于只读和固定reboot。Controller管理HTTP/auth/调度/Web与DB adapter由其他专题负责。

**Tech Stack:** Rust2024/MSRV1.85。prost/tonic、密码库、板端journal引擎、cargo-fuzz均为条件候选，未经版本/板端验证；cargo-fuzz随密码库/记录层审批确认。

**Spec:** `docs/superpowers/specs/2026-09-22-controller-design.md`、`docs/superpowers/specs/2026-09-23-controller-v1-00-index.md`、`docs/superpowers/specs/2026-09-23-controller-v1-02-data-api.md`、`docs/superpowers/specs/2026-09-23-controller-v1-03-device-protocol.md`、`docs/superpowers/specs/2026-09-23-controller-v1-04-task-lifecycle.md`、`docs/superpowers/specs/2026-09-23-controller-v1-05-runtime-operations.md`、`docs/protocol_spec.md`、`crates/rsetup-core/src/lib.rs`、`Cargo.toml`。

## Global Constraints
- **安全生产阻断：** reason_code未签名覆盖、验签失败描述冲突、AEAD tag失效后终止未定。不可擅选密码布局；协议修订审查、双方实现审查与双端测试完成前不得交付生产/宣称安全，不可忽略安全测试。
- v1不得自动降级v2；握手payload≤16KiB、record密文+tag≤64KiB、信封≤512KiB，gRPC1MiB是待验证建议。
- 两流双端均主动`tunnel.ping`：TYPE_REQUEST/TYPE_RESPONSE、raw16B nonce，同action/trace/流/代际。业务存在及空闲OpenData均probe。HTTP2 PING/ACK、应用probe、硬件结果不同。
- 健康PENDING无人工审批总TTL；握手5s名义、pong10s、客户端静默15s与流15s窗口分离，均单调时钟。资源有界、超限不误记REVOKED/APPROVAL_DENIED；等待审批/I/O锁外、旧代际回调无效。
- 远端白名单仅capabilities/status/clock/task.get、reboot.prepare/execute、task.result事件；绝不暴露现有单板HTTP危险动作、任意shell/命令、配置写入、关机/升级/分发。
- 准入持久DB由01中控专题注入：本计划 `AdmissionStore::{load,compare_and_set}` 必须按公钥返回包含 `admission_state`、`review_decision`、`revision` 的完整 `AdmissionSnapshot`；CAS校验公钥、预期revision与预期状态/决定（见02规格§2.2），冲突返回冲突，DB adapter由中控crate提供。PENDING+denied持久拒绝必须签名回APPROVAL_DENIED，不能再次挂起或误报REVOKED。板端accepted/attempted可靠落盘早于OS，恢复不自动重试。票据30s、日志4096均条件参数。**每个**列在“另测/补测”的行为都先写单独失败测试并确认目标断言因行为缺失而失败，随后最小实现、跑绿、重构并回归；不能在绿灯后直接补测试。测试工具/模块缺失本身不算有效RED，先建立最小可加载边界再确认断言失败。
- 中控侧仅 `device.reboot.execute` 的网络发送必须在03持久 `dispatching` 发送意图事务提交后开始（04规格T-04）；只读查询、能力查询与 `device.reboot.prepare` 可以在此前进行，不能提前发送execute。**所有DB事务中均禁止任何网络调用**（含准入kick/revoke通知、prepare、只读及execute）；01的准入决定/审计/revision提交后，才可异步通知02传输层。中控侧BoardClient适配由03实现（消费本计划下方接口清单的wire类型与已认证Control流），本计划不重复定义。
- 新增依赖须同任务提交manifest与`Cargo.lock`，并通过MSRV 1.85与CI多target检查。Task 2内存路径可与01并行；Task 3/4因安全修订BLOCKED时，Task 5–8不依赖3/4，可并行推进，但最终互操作和生产交付仍须过安全门。

## 文件清单、接口
新增 `crates/rsetup-protocol/Cargo.toml`、条件性`build.rs`、`proto/{handshake,tunnel}.proto`、`src/{lib,wire,frame,crypto,record,admission,tunnel,probe}.rs`、`tests/{wire_contract,frame_sequences,admission_state,crypto_vectors,handshake_interop,record_layer,tunnel_probe}.rs`、`fuzz/Cargo.toml`、`fuzz/fuzz_targets/{frame_decoder,record_decoder}.rs`；wire类型采用prost从proto生成，cargo-fuzz命令在`crates/rsetup-protocol`目录运行。新增 `crates/rsetup-board-agent/Cargo.toml`、`src/{lib,dispatch,read_only,journal,ticket,reboot,tunnel_binding}.rs`、`tests/{read_only,wire_03_tickets,wire_04_journal,wire_05_events,reboot_boundary,interop_dual_stream}.rs`；`tunnel_binding.rs`绑定板端双流与dispatch，`interop_dual_stream.rs`跨crate组装protocol tunnel与board-agent dispatch为Control+Data双流，覆盖握手→开流→业务往返→probe→票据执行。根Cargo加两个workspace members及经批准的依赖/lock更新。01 crate 已就绪后，Task 9 另修改 `crates/rsetup-controller/{Cargo.toml,src/main.rs,src/lib.rs,src/devices/mod.rs}` 并新增 `src/devices/admission_events.rs`、`tests/admission_tunnel.rs`，仅组装提交后准入通知 adapter 与02传输入口，不在 protocol 内反向依赖 controller。Task 10 在可实施条件满足后另新增板端 `src/main.rs`、条件性 `src/config.rs`、`tests/process_loopback_smoke.rs`（测试文件可归入现有测试目录，具体模块/配置载体待审），并扩展 controller `src/main.rs`、条件性配置接线与同进程独立设备监听；不预设配置格式/端口/新命令名。

输出 `rsetup_protocol::wire::{Query,Capabilities,DeviceStatus,ClockSample,RebootPrepare,RebootTicket,RebootExecute,TaskQuery,TaskRecord}`按03字段；ping不套业务protobuf。`AdmissionSnapshot { admission_state: Pending|Approved|Revoked, review_decision: None|Approved|Denied|Revoked, revision }` 随公钥持久保存；`AdmissionStore::load(public_key)` 返回完整快照/不存在，`compare_and_set(public_key, expected_revision, expected_state_and_decision, next_snapshot)` 原子检查公钥、revision和状态/决定，返回新快照或冲突（实际接口以01共同契约统一，不能退化为只返回三态）。DB adapter与01审批HTTP由中控crate负责，02的准入判定/连接代际和通知消费由protocol tunnel负责。`ReadOnlySource::snapshot()`返回core只读快照；`TaskJournal::{get,list,insert_accepted,mark_attempted,finish}`返回持久TaskRecord，`finish`只有可靠提交后才能产生终态证据；`RebootOs::reboot()`无参数，不能接收command/path/argv。

## Task 1：共享wire（WIRE-01）
**Files:** protocol manifest、条件性`build.rs`、`proto/tunnel.proto`（先定义TunnelPacket消息，Task 5扩展双流service）、`src/{lib,wire}.rs`、`tests/wire_contract.rs`、根Cargo成员。
**Interfaces:** `validate_business_version(u32)`、`validate_packet(&TunnelPacket)`；action≤64B、metadata≤16项/key≤64B/value≤256B、整包≤512KiB、schema_version=1。Task 1 的wire预检负责约束 `tunnel.ping` 请求/响应字段：raw16B nonce、请求metadata为空、响应`status_code=0`且`error_message`/metadata为空；但预检不认定pong健康，也不删除未决项，Task 5 的probe接收方仍须针对当前未决probe独立匹配这些字段及trace/nonce/连接流代际/截止时刻。非法ping请求按流级协议错误终止该流；已知probe响应字段错误归不健康、不刷新期限（未知/过期/已完成响应静默丢弃），不得仅因全局wire预检拒包就漏计当前窗口/首probe失败。
- [ ] RED准备：先新增最小可编译的proto/生成与 `wire` 类型、`validate_business_version`/`validate_packet` 签名及错误枚举；stub对不支持的业务版本或15B ping暂错误放行。执行编译确认测试可运行，API/模块缺失或编译失败不计RED。
- [ ] 红测试（基于可编译stub）：
```rust
#[test] fn bad_business_version() {
    assert_eq!(validate_business_version(2), Err(WireError::UnsupportedVersion(2)));
}
#[test] fn bad_ping_nonce() {
    assert_eq!(validate_packet(&TunnelPacket::request(1, "tunnel.ping", vec![0; 15])),
               Err(WireError::InvalidPingNonceLength(15)));
}
```
- [ ] 失败：分别运行 `cargo test -p rsetup-protocol --test wire_contract bad_business_version -- --exact` 与 `cargo test -p rsetup-protocol --test wire_contract bad_ping_nonce -- --exact`，预期两个测试均实际运行，且各自断言因版本/nonce行为错误而失败，不能把API缺失称RED。
- [ ] 实现：`pub fn validate_business_version(v:u32)->Result<(),WireError>{(v==1).then_some(()).ok_or(WireError::UnsupportedVersion(v))}`；ping只准请求/响应raw16B；先为响应非零`status_code`、非空`error_message`、非空metadata各写独立wire校验RED→GREEN（即便nonce/trace合法也拒绝），补缺字段/UUID/非有限metric/超限测试（信封整包≤512KiB在wire/业务层测试，记录密文+tag≤64KiB归Task 4 record测试）。
- [ ] 通过：`cargo test -p rsetup-protocol`。
- [ ] 重构/提交：`cargo fmt --all -- --check && cargo test -p rsetup-protocol`；`git add Cargo.toml Cargo.lock crates/rsetup-protocol && git commit -m "feat(protocol): add wire contracts"`。

## Task 2：握手与准入
**Files:** `proto/handshake.proto`、`src/{frame,admission}.rs`、`tests/{frame_sequences,admission_state}.rs`、`fuzz/{Cargo.toml,fuzz_targets/frame_decoder.rs}`。
**Interfaces:** `FrameDecoder::push`、`AdmissionSnapshot`/`AdmissionStore::{load,compare_and_set}`、`AdmissionDecision`；SE/v2/BE长度/16KiB/严格序列；PENDING token/status_nonce/probe_seq严格匹配。新身份 PENDING+none，人工拒绝 PENDING+denied，吊销 REVOKED+revoked；按公钥和expected_revision/旧状态及决定CAS，不把denied合并到Revoked，也不把已拒绝视为新身份。
- [ ] 红测试：
```rust
#[test] fn rejects_old_version_and_resource_misclassification() {
    assert_eq!(FrameDecoder::default().push(&v1_hello()), Err(ProtocolError::UnsupportedVersion(1)));
    assert_eq!(classify_new_identity(IdentitySlot::CapacityExhausted), AdmissionDecision::RetryableServerError); // ADM-01：资源耗尽不记denied/revoked
}
#[test] fn denied_identity_stays_denied_across_reconnect() {
    let mut f = AdmissionFixture::pending_none();
    let denied = f.cas_review_denied(f.revision()).unwrap();
    assert_eq!(f.reconnect_decision(), AdmissionDecision::Reject(ReasonCode::ApprovalDenied));
    assert_eq!(f.snapshot(), denied); // PENDING+denied，不能再挂起、不能伪装REVOKED
    assert!(f.cas_review_approved(denied.revision - 1).is_conflict());
}
```
`AdmissionFixture` 是未来 `admission_state` 测试契约：同一公钥内存持久store，可设置 PENDING+none、提交 CAS、断线重新调用握手判定并查看最新revision/决定；不是现成API。上述测试先使断言因缺少拒绝判定/CAS代际检查而失败，不能用缺类型/编译失败替代RED。
- [ ] 失败：`cargo test -p rsetup-protocol --test frame_sequences && cargo test -p rsetup-protocol --test admission_state`，分别确认行为断言失败。
- [ ] 实现：
```rust
pub fn classify_new_identity(slot: IdentitySlot) -> AdmissionDecision {
    match slot {
        IdentitySlot::Available => AdmissionDecision::Pending,
        IdentitySlot::CapacityExhausted => AdmissionDecision::RetryableServerError,
    }
}
```
先验header再分配，单在途、monotonic clock、审批/I/O锁外。签名认证后按公钥读取**完整**快照：不存在时受配额限制创建 PENDING+none，PENDING+none继续挂起，PENDING+denied立即签名拒绝APPROVAL_DENIED，APPROVED+approved可接纳，REVOKED+revoked签名拒绝REVOKED；01的reopen/reauthorize经预期revision及旧状态CAS后才可能改变判定。最终接纳前同公钥串行化核查最新DB快照和连接代际，撤销/拒绝优先；CAS冲突重读权威状态而非覆盖。
- [ ] 通过：同两命令及`cargo fuzz run frame_decoder`（cargo-fuzz获批后）；每项新增断言先单独RED：测乱序/magic/token/nonce/seq、ADM-01长期健康pending无人工总TTL、超时不改持久准入、资源超限不记denied/revoked且重连不重复记录、旧代际；测PENDING+denied重连明确APPROVAL_DENIED且停止自动重连、PENDING+none继续挂起、REVOKED+revoked明确REVOKED、reopen/reauthorize的旧revision/旧状态CAS冲突与最终接纳前拒绝/吊销竞争；另分别测试约1024台**已验签 PENDING**长期健康（05 §6 建议已验签 PENDING 2048 上限），以及未认证连接 128 上限和来源/全局握手配额；二者是不同资源池，不以1024未认证连接作为验收目标；再测审批决定与在途pong竞争、墙钟回跳/NTP恢复不改变单调时钟协议计时。
- [ ] 重构/提交：fmt+protocol全测，提交`feat(protocol): validate handshake and admission`。

## Task 3：认证与密钥协商（安全阻断）
**Files:** `src/crypto.rs`、`tests/{crypto_vectors,handshake_interop}.rs`、获批后依赖。
**Interfaces:** `IdentitySigner`、`SignatureVerifier`、`EphemeralKeyAgreement`、`HandshakeTranscript`；库和reason_code布局未定。
- [ ] 红测试（仅使用修订审查提供的向量；下方测试体与向量随修订统一，当前暂定，若修订选择不同修法须同步调整测试体）：
```rust
#[test] fn changing_rejection_reason_invalidates_signature() {
    let v = approved_protocol_vectors::signed_rejection();
    assert!(verify_server_response(&v.key, &v.response, &v.transcript).is_ok());
    let mut altered = v.response.clone(); altered.reason_code ^= 1;
    assert_eq!(verify_server_response(&v.key, &altered, &v.transcript), Err(HandshakeError::SignatureInvalid));
}
```
向量不得自造。
- [ ] 失败：`cargo test -p rsetup-protocol --test crypto_vectors`；签名布局已存在于`docs/protocol_spec.md`，修订是否覆盖reason_code未批，BLOCKED。
- [ ] 实现仅批准后：
```rust
let input=transcript.approved_server_response_input(response)?;
verifier.verify(key,&input,&response.signature).map_err(|_|HandshakeError::SignatureInvalid)
```
两端按修订统一验签失败动作，握手前不启动h2c。
- [ ] 通过：`cargo test -p rsetup-protocol --test crypto_vectors --test handshake_interop`；双端向量、重放、可信key、非法X25519点。
- [ ] 重构/提交：记录修订/库review/双端证据，fmt+全测后提交`feat(protocol): implement reviewed handshake crypto`；未过门不生产。

## Task 4：AEAD record（失效策略阻断）
**Files:** `src/record.rs`、`tests/record_layer.rs`、`fuzz/fuzz_targets/record_decoder.rs`、获批provider。
**Interfaces:** `RecordEncoder::encode`、`RecordDecoder::decode`、方向独立序号，Nonce/AAD按修订，tag失效终止未定。
- [ ] 红测试：`let r=tx.encode(b"frame")?; assert_eq!(rx.decode(&r)?,b"frame"); assert!(rx.decode(&tamper_length(r)).is_err());`，另测tag/重放；超限负例仅测record密文+tag>64KiB，信封整包>512KiB归Task 1 wire/业务层。
- [ ] 失败：`cargo test -p rsetup-protocol --test record_layer`；终止策略未批保持BLOCKED。
- [ ] 实现：
```rust
let frame = parse_bounded_record(bytes, MAX_CIPHERTEXT_PLUS_TAG)?;
let plaintext = cipher.open(expected_nonce, &frame.aad, frame.ciphertext)
    .map_err(|_| RecordError::AuthenticationFailed)?;
rx_seq = rx_seq.checked_add(1).ok_or(RecordError::SequenceExhausted)?;
```
仅认证成功推进序号；认证失败连接处置待修订。
- [ ] 通过：`cargo test -p rsetup-protocol --test record_layer && cargo fuzz run record_decoder -- -max_len=65540`，畸形/超限/乱序无panic越界。
- [ ] 重构/提交：双端tag失败review后fmt+全测，提交`feat(protocol): add authenticated record layer`。

## Task 5：双流与probe
**Files:** `proto/tunnel.proto`、条件性`build.rs`、`src/{tunnel,probe}.rs`、`tests/tunnel_probe.rs`。
**Interfaces:** `OpenControl(stream TunnelPacket) returns (stream TunnelPacket)`及同型OpenData；Control先开，connection/stream epoch和trace/nonce；probe每流单在途，业务请求可独立并发（受每连接≤1024未决预算约束，预留probe）。开流/重开各15s及首个probe15s：打开失败或首pong无效/超时各算一次**重开失败**，连续3次升级连接级重建；仅本端本流首次probe有效pong清该计数，开流成功本身不清。健康运行后的15s窗口连续3失败只重开坏流（不同于首probe三次升级）。连接级HTTP/2 PING/ACK由本任务实现：每端一套调度器、唯一8B token、15s窗口至多一在途，连续3窗口无匹配ACK判连接死链；与流级probe独立计时。`tunnel.kick`/`tunnel.revoke`仅Control流TYPE_SYSTEM；同公钥新连接完成准入替换旧连接时旧Control可用则先kick、500ms后关旧TCP，新连接代际不受旧定时器影响；kick按1/2/4…≤60s+[0,1s)抖动重连，revoke停止全部自动重连并置未授权。
- [ ] 红测试：
```rust
#[tokio::test(start_paused=true)] async fn data_failure_keeps_control(){
 let mut p=Peer::new(); p.open(Control).await; p.open(Data).await;
 let q=p.probe(Control).await; assert_eq!(q.action,"tunnel.ping"); assert_eq!(q.payload.len(),16);
 p.pong(Control,q).await; p.fail_windows(Data,3).await;
 assert_eq!(p.reopens(Control),0); assert_eq!(p.reopens(Data),1);
}
#[tokio::test(start_paused=true)] async fn first_probe_failures_escalate() {
    let mut p = Peer::new();
    p.open(Control).await; p.pong_first_valid(Control).await;
    for attempt in 1..=3 {
        p.open(Data).await; // RPC打开成功，但首probe故意超时/错nonce
        p.fail_first_pong(Data).await;
        assert_eq!(p.connection_rebuilds(), if attempt == 3 { 1 } else { 0 });
    }
    p.open(Control).await; p.pong_first_valid(Control).await; // 整连接重建后仍先Control
    p.open(Data).await; p.pong_first_valid(Data).await;
    assert_eq!(p.reopen_failures(Data), 0); // 仅有效首pong清计数
}
```
`Peer` 是将来 `tunnel_probe` 的虚拟时钟/流代际测试fixture：能注入开流成功/失败、首probe有效/错误/超时、运行期窗口和连接重建次数；测试不假定已有该类型，先建立可编译fixture/stub再测行为RED。
- [ ] 失败：`cargo test -p rsetup-protocol --test tunnel_probe`；分别RED补错nonce/流/epoch、未知响应静默丢弃、排队堵塞仍计时；**另各自独立RED**：同流、同连接/流代际、匹配trace与16B nonce且未超时的pong，仅将`status_code`设非零、仅将`error_message`设非空、仅将metadata设非空，三例均不得标健康、清pending或清首probe重开失败数/运行期窗口失败数、延长原deadline；原期限到后照常计失败并执行原定升级/重开。首开/重开15s未建立及首probe15s失败连续3次升级连接、前两次只重开、单次有效首pong清计数而RPC打开不清；运行期3窗口只坏流。补连接级PING/ACK红测试：15s每窗口唯一8B token/至多一在途、迟到/错ACK无效、连续3窗口无响应判连接死链、ACK不代答业务pong，与两流probe独立计时（反向亦不代答连接ACK）。另单独RED测TYPE_SYSTEM仅Control允许kick/revoke，`tunnel.ping`不能走SYSTEM；同公钥新准入连接抢占时旧Control可用先kick、恰500ms后旧TCP关闭且新连接存活，旧回调不得关闭新代际；kick/临时断线按指数退避+抖动，revoke停止重连并置未授权。
- [ ] 实现：
```rust
if packet.kind != PacketType::Response || packet.action != "tunnel.ping"
    || packet.trace_id != pending.trace_id || packet.payload != pending.nonce
    || packet.status_code != 0 || !packet.error_message.is_empty()
    || !packet.metadata.is_empty()
    || local_connection_epoch != pending.connection_epoch
    || local_stream_epoch != pending.stream_epoch || now > pending.deadline {
    return ProbeResult::IgnoreUnhealthy; // 不清pending/失败计数，不延长deadline；原窗口按时计失败
}
pending_by_stream.remove(&stream); ProbeResult::Healthy
```
未知响应静默丢弃；已知未决probe的字段错误即便被Task 1的通用预检拦截，也须向probe计时状态保留“不健康直到原截止时刻”的结果，不可当作有效pong或重置失败计数；仅有效pong删除pending并清相应计数。连接级PING/ACK与流级probe分调度器和单调时钟窗口，匹配ACK只清连接连续失败数，3窗口无效ACK触发连接重建并结清该代际未决请求。按连接/流代际隔离首probe失败计数与运行期窗口；前者连续3次失败（包括开流未完成）升级连接、后者连续3窗口只重开对应坏流。旧连接Control可用时kick后用单调计时500ms再关闭旧TCP，旧代际kick定时器不得作用新连接；revoke断开并终止重连，网络断线/kick才走有界指数退避。
- [ ] 准入通知集成RED（`tunnel_probe`增测试，先构造可编译 `AdmissionNoticeFixture`）：01中控审批/吊销事务提交后只发送 `{device_id,revision}` 给02的 `on_admission_committed`，02**不能相信通知附带的状态**，按device_id重读 `AdmissionStore::load` 权威 `admission_state/review_decision/revision`，比较通知revision、读到的revision与活动连接代际；乱序/旧revision通知不能踢新连接或覆盖新决定，尚未观察到对应提交则触发补偿重读而非盲处理。REVOKED+revoked、PENDING+denied 若命中当前活动连接须切断；REVOKED可用Control先发TYPE_SYSTEM revoke并立即断开，denied关闭当前连接且后续握手APPROVAL_DENIED，不把它伪装REVOKED；APPROVED+approved不能从旧通知重建已经撤销的连接。注入通知投递失败、DB重读失败及通知/旧连接回调竞争，断言01的已提交决定不会回滚、02失败关闭**该设备**活动连接并安排补偿核对，其他设备与新代际连接不受旧通知误关；DB事务内零网络。
- [ ] 通知集成GREEN：实现02传输层 `on_admission_committed(device_id,revision)`/按device_id定向fail-closed与持久DB核对入口，调用方01仅在DB事务提交后尽力通知并在投递失败时触发该设备连接关闭/补偿核对；补偿以权威DB决定收敛，绝不通过未提交内存状态恢复准入。`AdmissionNoticeFixture` 是未来测试契约：可分别提交DB快照、延迟/乱序/丢弃通知、失败注入load、观察特定device_id+connection_epoch的关闭/重连和补偿队列；不引入新的DB或网络层实现。
- [ ] 通过：`cargo test -p rsetup-protocol --test tunnel_probe && cargo test -p rsetup-protocol`；OpenData无业务/有业务均probe，首probe与健康期故障分别验证、连接级15s窗口/连续3窗口无ACK判死链、HTTP/2 ACK不代答业务；kick/revoke/退避、同公钥排他和提交后通知的CAS重读/失败补偿用独立RED→GREEN用例逐项通过。
- [ ] 重构/提交：fmt+全测，提交`feat(protocol): add dual grpc streams and probes`。

## Task 6：只读板端白名单（WIRE-01/02）
**Files:** board-agent manifest、`src/{lib,dispatch,read_only}.rs`、`tests/read_only.rs`、根Cargo成员；Task 6 临时在 `lib.rs` 定义可注入的只读 journal 查询接口/测试 stub，Task 7 再创建 `src/journal.rs` 的持久实现，不提前开启重启执行。
**Interfaces:** `BoardAgent::handle(action,payload,peer)`、`ReadOnlySource::snapshot()`、临时 `TaskJournalReader::query(TaskQuery)->Result<TaskRecord,BusinessStatus>` 注入接口；capabilities/status/clock只读core，task.get先通过可编译查询 stub 校验白名单且不伪造持久结果，Task 7 将此接口接到真实 journal 并补查询/恢复测试，Task8前重启拒绝。
- [ ] 红测试：
```rust
#[test] fn remote_mutation_is_not_in_allowlist() {
    let mut agent = FakeAgent::default();
    let peer = FakePeer::default();
    assert_eq!(agent.handle("device.fan.apply", &[], &peer), Err(BusinessStatus::UnsupportedAction));
    assert_eq!(agent.mutation_calls(), 0);
}
```
另测畸形status错误且写调用仍为0；`device.task.result`事件固定走OpenControl（v1 B-01）；`durable_task_journal=false`时拒绝票据执行，OS spy=0（Task 8再测执行路径）。
- [ ] 失败：`cargo test -p rsetup-board-agent --test read_only`。
- [ ] 实现：
```rust
match action {
 "device.capabilities.get"=>self.capabilities().await,
 "device.status.get"=>self.read_only.status().await,
 "device.clock.get"=>self.read_only.clock().await,
 "device.task.get"=>self.journal.query(decode(payload)?).await,
 _=>Err(BusinessStatus::UnsupportedAction),
}
```
- [ ] 通过：`cargo test -p rsetup-board-agent --test read_only && cargo test -p rsetup-protocol --test wire_contract`；非法schema/UUID/metrics无副作用，旧agent_epoch/sample_seq不覆盖；v1 `device.task.result`固定走OpenControl，能力缺`durable_task_journal`拒绝票据执行（执行路径在Task 8回归）。
- [ ] 重构/提交：fmt+board全测，提交`feat(board-agent): add read-only handlers`。

## Task 7：板端journal（WIRE-04/05）
**Files:** `src/journal.rs`、`tests/{wire_04_journal,wire_05_events}.rs`、`lib.rs`窄出口。
**Interfaces:** `TaskJournal::{get,insert_accepted,mark_attempted,finish,list,recover}`；键(controller key,sub_task_id)，journal_epoch普通重启不变、日志丢失/重建更换，record_version从1递增并持久化；满日志拒绝且不淘汰旧记录，库待板端验证。`recover(current_boot_id)`只读OS启动ID并核实持久记录，不调用`RebootOs`：同一认证设备的可靠attempted及pre_boot_id存在、当前boot与pre_boot不同且无已确认矛盾失败时，原子生成并可靠落盘新版`succeeded`记录，设置`observed_boot_id=current_boot_id`与`evidence_kind=boot_transition_observed`；不能仅凭boot变化、断线或pong成功。same boot attempted、只有accepted（即使boot变）以及日志丢失/不可读均不能判成功/明确失败，保留或可靠写入`unknown`和原证据、**绝不自动重试**；日志丢失不具备可写可靠性时不能假称持久unknown，改换journal_epoch并向中控报告不确定/JOURNAL_LOST，由中控保留unknown核实。可靠`failed/OS_ERROR`等终态与随后冲突boot证据只能报告一致性异常，不能覆盖、提升succeeded或静默合并。
- [ ] 红测试：先建立可编译的持久fixture/stub，再分别运行单独目标测试，断言因恢复/版本/证据行为缺失而RED，而非因缺模块失败：
```rust
#[test] fn attempted_then_new_boot_persists_success_evidence() {
    let j = CrashableJournal::new();
    j.insert_accepted(task_key(), payload_hash()).unwrap();
    let attempted = j.mark_attempted(task_key(), pre_boot()).unwrap();
    let os = CountingRebootOs::default();
    let recovered = recover_agent(j.reopen(), new_boot(), &os).unwrap();
    assert_eq!(recovered.get(task_key()).unwrap().state, "succeeded");
    assert_eq!(recovered.get(task_key()).unwrap().record_version, attempted.record_version + 1);
    assert_eq!(recovered.get(task_key()).unwrap().observed_boot_id, Some(new_boot()));
    assert_eq!(recovered.get(task_key()).unwrap().evidence_kind, "boot_transition_observed");
    assert_eq!(recovered.reopen().get(task_key()).unwrap().state, "succeeded");
    assert_eq!(os.calls(), 0);
}
```
`CrashableJournal/recover_agent`是未来fixture契约：持久快照reopen/丢失重建、指定当前boot_id、每个事务前后崩溃/提交失败注入、观测record_version/持久event；`CountingRebootOs`仅计数，不是新增产品API。分别RED→GREEN测same boot attempted仍unknown/不重启、只有accepted跨boot仍unknown/不重启、日志丢失变epoch且中控unknown/不重发、可靠failed/OS_ERROR后异步boot变化仅告警不覆盖、重复恢复不增version、同ID同payload幂等与异payload冲突；事件必须先可靠落盘再发送。
- [ ] 失败：`cargo test -p rsetup-board-agent --test wire_04_journal && cargo test -p rsetup-board-agent --test wire_05_events`，记录各项实际失败断言。
- [ ] 实现：先可靠提交accepted，再在OS边界前可靠提交attempted；`recover`对可靠attempted且boot确实转换执行带version CAS 的持久 `finish(succeeded, observed_boot_id, boot_transition_observed)`，不得覆盖任何已确定终态。提交前后崩溃重开核实持久version和结果；同boot/仅accepted无充分证据时保留可查询旧记录或**可靠**记录unknown，日志丢失/finish失败则对外保留不确定性并禁止重试，不凭内存unknown伪装持久状态。`device.task.get`查询最新可用持久记录；`device.task.result`仅为落盘后在Control发出的TYPE_EVENT，不要求回包。
- [ ] 通过：同两命令；满库RESOURCE_EXHAUSTED、epoch变化、accepted/attempted/finish每个崩溃点、version随证据增加且reopen可见、event重复/乱序/丢失及查询补偿、低version丢弃、同version异内容一致性错误、TASK_NOT_FOUND不授权重试。事件与query沿同一合并规则核对认证身份/任务/epoch/version。
- [ ] 重构/提交：真实持久日志reopen，fmt+board全测，提交`feat(board-agent): persist idempotent task journal`。

## Task 8：短票据与固定OS reboot（WIRE-03）
**Files:** `src/{ticket,reboot}.rs`、`tests/{wire_03_tickets,reboot_boundary}.rs`、`src/dispatch.rs`。
**Interfaces:** prepare绑定controller key/sub_task/expected boot/connection epoch/agent epoch，建议32B CSPRNG token、每连接一张、30s单调TTL；换连接/boot/agent失效。execute先日志去重再验票/占用；accepted/attempted可靠落盘后才能固定无参OS reboot。execute在**确认accepted未提交、请求未接受且未进入OS边界**时（无效票据、资源满、确认未提交的accepted写入失败等）可用非零RPC status且payload为空；accepted提交结果不明须不发业务结果/断流待查询，不得以非零假拒绝。已可靠落盘accepted之后的attempted写入错误不得冒充“从未接收”，无论提交结果如何均不调用OS/自动补执行：若证明attempted未提交，可用成功信封回复最后可靠持久的accepted（仅接收证据），若attempted提交结果不明则不发业务结果/断流待查询，不能无条件回复accepted。一旦attempted可靠落盘并可能进入OS边界，不能将`os.reboot()`的`Err`经`?`直接映射成非零RPC。OS明确错误且`finish(failed, OS_ERROR)`**可靠落盘后**返回`status_code=0`与持久TaskRecord failed；OS未明确返回、崩溃或结果不足时仅保留已可靠记录及不确定性，unknown只有可靠落盘才可发送，不重试。OS错误后的`finish`提交失败不能伪造可靠failed/unknown结果：不发送业务结果/断流留给task.get核实，不发错误RPC暗示未执行；以后query/恢复仍按不确定核实，绝不自动二次调用OS。
- [ ] 红测试：
```rust
#[test] fn expired_ticket_never_reboots() {
    let f = RebootFixture::new();
    let token = f.prepare().unwrap();
    f.advance(Duration::from_secs(31));
    assert_eq!(f.execute(token), Err(BusinessStatus::ExecutionTokenInvalid));
    assert_eq!(f.os.calls(), 0);
}
#[test] fn os_error_is_a_durable_task_result_not_rpc_failure() {
    let mut f = RebootFixture::with_os_error();
    let token = f.prepare().unwrap();
    let reply = f.execute(token).unwrap();
    assert_eq!(reply.status_code, 0);
    assert_eq!(reply.task_record.state, "failed");
    assert_eq!(reply.task_record.reason_code, Some("OS_ERROR".into()));
    assert_eq!(f.reopen().task_get().unwrap(), reply.task_record);
    assert_eq!(f.os.calls(), 1);
}
```
`RebootFixture`是未来测试契约：可注入固定无参OS明确Err/不返回、journal各次提交失败和进程崩溃，观察RPC信封、OS调用次数、持久reopen与task.get；例子不是现成API。另独立RED覆盖：accepted写入错误且提交结果不明时不得非零或伪造TaskRecord、不调用OS，改为不发业务结果/断流待查询；已可靠accepted后attempted写入错误且**提交结果不明**时必须不发业务结果/断流、不能无条件回复accepted，OS spy=0，重连通过task.get核实实际持久版本（可能是accepted或attempted），不得自动补调用；若能证明attempted未提交，允许`status_code=0`仅回复重开可见的持久accepted，OS spy=0且同ID不自动补执行。再独立RED注入OS明确Err之后 `finish(failed)` 落盘失败，断言**无非零RPC误导、无未落盘failed/unknown响应**，reopen仍attempted/不确定且重连查询不触发第二次OS；注入OS返回不明/响应丢失仍不重试；换连接/boot/重放与accepted/attempted前日志失败 OS spy=0，wire无command/path/argv。
- [ ] 失败：先建立可编译fixture/stub，再运行`cargo test -p rsetup-board-agent --test wire_03_tickets && cargo test -p rsetup-board-agent --test reboot_boundary`，逐一确认行为断言RED而非缺API。
- [ ] 实现：
```rust
if let Some(old) = journal.get(&key).await? { return idempotent_or_conflict(old, &req); }
tickets.consume_valid(&req, &peer, monotonic_now)?;
let accepted = match journal.insert_accepted(key.clone(), hash(&req)).await {
    Ok(record) => record,
    Err(error) if error.definitely_not_committed() => return reject_before_acceptance(error),
    Err(_) => return unresolved_without_false_rpc(), // accepted提交结果不明，不能宣称未接收
};
let attempted = match journal.mark_attempted(&key, boot).await {
    Ok(record) => record, // 必须可靠提交后调用OS
    Err(error) if error.definitely_not_committed() => return response_ok(accepted), // 仅证明attempted未提交时回复持久accepted；不调用OS
    Err(_) => return unresolved_without_false_rpc(), // attempted提交结果不明：不发业务结果/断流待查询；不调用OS
};
match os.reboot() {
    Err(_definite_os_error) => match journal.finish(attempted, "failed", "OS_ERROR").await {
        Ok(persisted) => response_ok(persisted), // status_code=0，可靠TaskRecord
        Err(_) => unresolved_without_false_rpc(), // 已attempted，不伪造failed/unknown或未执行错误
    },
    Ok(()) => response_ok(attempted), // OS接收不等于boot成功；随后恢复核实
}
```
示意伪码中的 `definitely_not_committed()` 是待选 journal adapter 对当前写入操作的错误分类契约：只有可靠证明该次写入未提交才能为 true，不按错误字符串猜测；对accepted写入，`reject_before_acceptance` 仅在请求未接受且OS未进边界时映射为非零业务错误且payload为空；对attempted写入，已经可靠接受，虽证明本次未提交，也只可用 `status_code=0` 回复持久accepted，绝不能调用OS或非零假拒绝。`unresolved_without_false_rpc` 是“断开/不发送业务结果并留给task.get核实”契约，不是返回业务非零或虚构持久unknown；`response_ok` 始终以 `status_code=0` 携带已可靠落盘的记录。固定OS adapter不用shell/远端参数。accepted后attempted写入错误不能删除已可靠落盘的去重记录；提交结果不明时不能假定最后持久版本仍是accepted，须不发送结果待查询，后续恢复不自动补调用。为 accepted 提交结果不明、attempted 提交结果不明及attempted确认未提交分别加故障用例：均无OS调用、无虚构持久结果、同ID不自动补执行。
- [ ] 通过：上述测试加`cargo test -p rsetup-board-agent --test wire_04_journal`，测重复/异payload/占用/崩溃不二次执行、可靠OS_ERROR失败信封与finish失败不确定性、`durable_task_journal=false`拒绝执行且OS spy=0。
- [ ] 重构/提交：`cargo fmt --all -- --check && cargo test --workspace --locked && cargo clippy --workspace --all-targets -- -D warnings`，提交`feat(board-agent): gate reboot with one-shot tickets`。在G2检查点记录目标板交叉编译target（待审批）与构建命令/结果作为证据；技术绿不解除安全门。

## Task 9：双流互操作
**Files:** `src/tunnel_binding.rs`、`crates/rsetup-board-agent/tests/interop_dual_stream.rs`、必要的`src/dispatch.rs`绑定；复用protocol tunnel与获批握手实现。01 crate 就绪后新增 `crates/rsetup-controller/src/devices/admission_events.rs`、`crates/rsetup-controller/tests/admission_tunnel.rs`，修改 controller `Cargo.toml`、`src/{main.rs,lib.rs,devices/mod.rs}` 及根 `Cargo.lock`，组装真实准入通知 adapter/集成测试；不要求03任务调度器提前存在。
- [ ] 红测试：先建立可加载的跨crate测试边界，再分别RED测双端握手→先OpenControl后OpenData→Control业务请求/响应与板端`device.task.result`事件→双流各自probe及连接PING/ACK→prepare/execute票据、accepted/attempted落盘后固定OS调用；故障路径不重复执行。新增**跨crate消息级**场景：OS尝试后切换boot，板端重启恢复持久`succeeded`/`observed_boot_id`/`boot_transition_observed`及增加的record_version；先丢`device.task.result`事件，随后测试中的中控消费端经Control `device.task.get`查询相同持久TaskRecord并检查可收敛的成功证据（身份、sub_task、journal_epoch、version、boot与无矛盾failed；真实中控持久任务 unknown→succeeded 由03 Task 3联合验收）；反向先query后重复/迟到事件不二次改变结果，同version异内容报一致性异常。另测same boot、仅accepted与日志丢失时事件或query不得伪造成功/可靠failed，也不得重发execute；已可靠failed/OS_ERROR后boot变不得覆盖。新增01提交后的`{device_id,revision}`通知→02重读DB→对应Control TYPE_SYSTEM revoke/关闭/停止重连及同公钥新连接kick/500ms旧连接关闭的互通；旧revision通知不关新代际，通知/重读失败定向关闭并补偿，审批事务内零网络。
- [ ] 失败：`cargo test -p rsetup-board-agent --test interop_dual_stream`；controller 接线另运行 `cargo test -p rsetup-controller --test admission_tunnel`，该测试使用01注入式 repository fixture 与真实 protocol 连接管理器，不需要真实 DB（DB事务原子性由01真实库测试验收）。确认每条跨crate消息级/故障断言因互操作或接线行为缺失而失败，测试模块/依赖缺失不算有效RED；获批安全修订前不宣称握手互通。
- [ ] 实现：绑定board-agent dispatch与protocol已认证双流，复用前述wire、probe、journal、ticket及Task 5通知接口，不重复定义中控侧BoardClient；查询与event共用板端持久记录和03/04版本/证据规则。controller `devices/admission_events.rs` 实现01的 `AdmissionChangeSink::after_commit`：调用02的 `on_admission_committed`，有界投递失败时调用02按device_id定向fail-closed/补偿入口；在启动组装时向审批服务注入该 adapter，覆盖 approve/reject/reopen/revoke/reauthorize 的提交后路径。`admission_tunnel` 通过真实服务调用验证提交前无通知、revoke提交后旧TCP关闭、通知失败DB决定不撤销、旧revision/代际不误关新连接；不能仅测protocol入口后声称controller已接线。
- [ ] 通过：`cargo test -p rsetup-board-agent --test interop_dual_stream && cargo test -p rsetup-controller --test admission_tunnel && cargo test --workspace --locked`；留存跨crate双端query/event丢失补偿、boot证据、kick/revoke及controller提交后通知互操作结果，获批协议修订的双方测试仍是生产前置门。
- [ ] 重构/提交：fmt+全测并复核握手/业务/探测/票据证据，提交`feat(board-agent): dual-stream interop`。

## Task 10：生产入口与两进程网络接线（条件性，非安全门替代）
**前置：** 01 中控可执行启动/配置/DB准入适配、Task 7–9持久journal与双流接线就绪；获批的协议修订及Task 3/4双方安全审查/测试未完成时，真实认证隧道与生产发布仍阻断，不以此前的跨crate消息级互操作或本任务的进程启动绕过。03 的 NTP启动关卡与持久恢复/调度尚未完成时，可以分阶段验证02网络接线，但**不能**将独立02检查宣称为最终ready/生产验收。下述完整 `process_loopback_smoke` 的只读RPC与结果断言还依赖03的生产 `BoardClient` 和轮询/任务核实接线；03未就绪时仅记录端口、握手、准入和probe的分阶段证据，不将完整smoke记PASS。
**Files:** 新增 `crates/rsetup-board-agent/src/main.rs`、条件性 `src/config.rs` 与 `tests/process_loopback_smoke.rs`，修改 board-agent `Cargo.toml`；修改 `crates/rsetup-controller/src/main.rs` 与必要的 `src/config.rs`/设备接线模块、`Cargo.toml`，依赖变更同步根 `Cargo.lock`。实际配置模块位置依01既有配置组织调整，不另造第二个中控服务进程。
- [ ] RED准备：先使可执行板端入口和01中控入口可启动、可注入测试配置/固定只读source与journal；测试启动**两个真实独立进程**而非同进程fake/仅trait组合。`process_loopback_smoke` 用例全部标 `#[ignore]`，沿用01的 `CONTROLLER_TEST_DATABASE_URL` 独占隔离空库契约，使用真实中控DB adapter，不给生产入口增加fake repository开关。测试helper核对两端待测可执行产物路径，使用隔离loopback地址、临时配置/密钥/journal目录；缺DB URL或缺任一产物报告未验证，不冒充行为RED或PASS。fixture/模块缺失不算RED。
- [ ] 独立RED→GREEN：板端可执行入口读取板端连接目标和可信中控Ed25519公钥列表，由板端**主动外联**；中控现有单进程启动中并列挂接管理HTTP与**独立安全设备TCP监听**、准入repository/02 tunnel、已认证Control/Data服务，绝不把设备入口复用成明文管理HTTP或开放未认证h2c。配置字段载体、绑定端口、附加安全参数只依已存规格和审批决定，不在此固定。双方长期Ed25519私钥首次缺失时使用安全随机生成，以防覆盖的原子创建、文件及所在目录可靠同步后落盘并限制文件权限`0600`，并发首次启动/写入失败不得以临时key继续接受连接或覆盖原key；后续重启加载复用原密钥、权限异常拒绝不安全启动；不写日志/审计或嵌入二进制。吊销时板端停止所有自动重连、持久本地未授权状态；恢复必须中控管理员重新授权**且**板端本地人工受控重置（入口仅限本机可信操作者，不添加远程动作或不经审核的新命令名），kick/临时断线可按既定退避重连。板端启动/重连后加载并恢复journal、确认`journal_epoch`/不确定任务证据，**先于接收任何execute**；不可读/丢失时按Task 7 fail-closed/unknown，不恢复自动执行。分别单独测试两端密钥首次生成/重启复用/坏权限和写入失败、板端配置不可信key拒绝、中控设备监听绑定/认证后开流、板端恢复先后顺序及吊销/人工重置路径。
- [ ] 准入接线RED→GREEN：独占空库只会初始化管理员，新板端临时公钥首次连接必须先形成 `PENDING+none`；测试先断言健康待审批但尚无Control/Data业务流。通过01真实管理HTTP完成管理员首次登录、强制改密、重新登录，携带有效session/CSRF及审批记录的当前 `expected_revision`、显式单设备公钥和 `confirm:true` 调用approve；确认DB提交为 `APPROVED+approved`、02通知链路唤醒等待连接后，才等待已认证双流和只读RPC。初始测试凭据仅从受限的测试进程输出捕获并保存在fixture内，不写普通测试报告。不得以“签名已验证”代替人工准入，不为smoke增加自动批准或绕过改密的生产开关；另用独立身份验证未批准不能进入业务流、明确reject后仍保持拒绝且不可被错误重连批准。
- [ ] 真实网络smoke RED→GREEN：在loopback上通过配置启动中控可执行产物与板端可执行产物，按上一项先完成真实管理员准入，再验证中控**实际监听独立设备TCP端口**、板端外联到该地址、前置双方签名/可信key与获批加密记录层认证、先Control后Data、双向两流probe和只读capabilities/status/clock/task.get；用错误可信key证明无法建立已认证流。完整smoke的业务请求由03生产 `BoardClient` 接线触发：连接后的能力刷新与错峰轮询发送capabilities/status/clock；fixture在隔离DB预置符合03恢复契约的待核实任务及归属锁，并在隔离板端journal预置同设备/同子任务的可靠记录，由恢复核实发送task.get（不预置可执行queued，不发送execute）。通过真实管理HTTP的缓存/任务投影及不含凭据的请求关联证据核对各次请求响应，而不是从probe成功推断业务成功；不添加原始RPC管理端点，不把Task 9库内fake消费端冒充生产BoardClient。退出并重启两个进程，核对两端长期公钥未变、板端已有可靠journal记录/`journal_epoch`重开复用且`task.get`可查询、恢复结果不触发二次执行；可通过测试本地预置隔离journal记录/boot证据来校验恢复，不经远端危险动作。smoke**绝不调用开发宿主reboot**：默认只读，若需演示prepare/execute仅使用测试隔离OS adapter和隔离运行环境，adapter只能从本机测试装配，绝不加可由远端开启的危险测试后门或让测试配置切换生产reboot边界。使用有界等待/子进程清理，失败报非零与握手/流/日志证据，不以虚拟进程替代网络。
- [ ] 有界验证：在上述条件满足后先按04已获批的资源构建顺序准备中控所需产物，再运行 `cargo build -p rsetup-controller -p rsetup-board-agent --bins --locked`，fixture须从本次构建产物取得两个可执行文件而非复用旧安装程序。仅在对应引擎URL存在时，分别运行 `CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test -p rsetup-board-agent --test process_loopback_smoke -- --ignored --nocapture --test-threads=1` 与 `CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL" cargo test -p rsetup-board-agent --test process_loopback_smoke -- --ignored --nocapture --test-threads=1`；逐引擎核对实际执行非零，缺URL标未验证，有URL却失败/零用例标失败，普通workspace测试不代替该smoke。再运行 `cargo test -p rsetup-controller --test admission_tunnel` 及最终门的workspace/双端安全测试；smoke内各阶段有界超时、结束清理子进程，外层CI超时待环境确认。未执行前只列待验证，不宣称RED/GREEN。03 Task 1 NTP关卡结束前必须同时阻断管理业务、设备接入和恢复下发；03 Task 3恢复任务锁/不确定状态先于新变更及execute，03 BoardClient消费02已认证Control流；这些与01真实启动、02进程smoke及Task 3/4安全证据**联合复验后**才考虑最终验收，Task 10先完成不得跳过03 NTP gate。
- [ ] 重构/提交：完成上述RED→GREEN与回归后仅整理启动装配，提交本任务实际改动的两端入口/配置/测试、manifest及必要的根lock，提交说明 `feat(transport): wire executable tunnel endpoints`；不把未执行的网络或双库测试记为通过。

## 覆盖矩阵与最终门
| 验收 | 任务 |
|---|---|
| WIRE-01未知version/action、缺字段/UUID、超限/非有限指标无变更；pong非零status_code/非空error_message/metadata不通过wire预检 | 1、5、6 |
| WIRE-02同流配对、旧代际、raw16B ping；匹配trace/nonce但错误status_code/error_message/metadata不健康、不清pending/失败数、不延期 | 1、5、10 |
| WIRE-03票据过期/换连接/boot、重复execute/异payload；OS_ERROR可靠failed走成功信封，finish失败保留不确定 | 8、9 |
| WIRE-04 accepted/attempted崩溃、满库、不自动重试；跨boot可靠attempted生成持久succeeded/version/observed_boot_id/evidence_kind，同boot/仅accepted/日志丢失unknown | 7、8、9 |
| WIRE-05事件重复/乱序/丢失、journal_epoch、query/event互通补偿不假称exactly-once；矛盾终态不覆盖 | 7、9 |
| pending 5/10/15s无审批TTL；首开/重开+首probe连续3失败升级连接，运行期3窗口只重开坏流 | 2、5 |
| ADM-01完整准入快照PENDING+denied为APPROVAL_DENIED、revision/状态CAS；超限资源拒绝不记denied/revoked | 2、5、9 |
| 01 DB提交后{device_id,revision}通知02、权威重读/代际保护、失败定向关闭与补偿；任何DB事务内零网络 | 2、5、9（01跨专题） |
| TYPE_SYSTEM kick/revoke仅Control、同公钥连接替换kick+500ms、指数退避与吊销停重连 | 5、9 |
| 连接级PING/ACK与流级probe分层、3窗口死链 | 5 |
| 仅reboot.execute在持久dispatching提交后发送；readonly/prepare可先行 | 8、9（03调度跨专题） |
| reason_code/验签/AEAD安全修订和双端测试 | 3、4，未完成不可生产 |
| 只读与固定OS，不暴露单板危险HTTP动作 | 6、8 |
| 握手→双流→业务→probe→票据跨crate双端互操作 | 9 |
| 板端可执行入口/外联与可信key、同进程独立设备监听、长期Ed25519密钥0600可靠落盘/重启复用、journal先恢复、吊销与人工重置 | 10（依赖01；03联合门） |
| 两进程loopback安全认证双流/只读/重启身份与journal复用，无开发宿主reboot或远端危险测试后门 | 10（3、4获批后） |

最终运行 `cargo fmt --all -- --check`、`cargo test --workspace --locked`（与`make test`的Rust命令一致）、`cargo clippy --workspace --all-targets -- -D warnings`；有界独立目标命令为 `cargo test -p rsetup-protocol --test wire_contract`、`cargo test -p rsetup-protocol --test tunnel_probe`、`cargo test -p rsetup-board-agent --test interop_dual_stream`、`cargo test -p rsetup-controller --test admission_tunnel`；`process_loopback_smoke` 则先构建两端产物，再严格按Task 10的MySQL/TiDB显式URL与 `--ignored --nocapture --test-threads=1` 命令独立运行，逐项核对实际运行测试数/失败数（01/03的真实DB测试仍需按各自门控另跑）。留存fuzz、双端互操作（Task 9跨crate与Task 10两进程真实网络分别留证）、库审查和目标板构建（Task 8/G2记录待审批target与结果）证据。**最终联合门：** 01真实中控启动/DB适配 + 02 Task 3/4获批协议双端安全审查/测试与Task 10进程网络smoke + 03 Task 1 NTP启动gate（未结束前不开放业务/设备接入/恢复下发）和Task 3持久恢复/dispatching先于execute，共同通过后方可验收；Task 10单独提前通过不解除03 gate。不得声称候选库、参数或1024设备容量已验证；本文仅计划，以上命令尚未运行，也不得据此宣称生产绿。
