# Controller 设备协议核心实施计划（2026-10-05）

> **致执行代理：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐项落实。用 `- [ ]` 复选框跟踪步骤。每项是拥有独立测试周期的最小单元：新代理/新批次只做一项，该项规格+质量审查通过后才开始下一项。

**Goal:** 新建纯库 `crates/rsetup-protocol`，只实现握手帧边界（协议 §4.1）、§4.3 签名输入 raw-wire 拼接、基于已锁定并离线缓存的 `ring` 0.17.14 的真实 Ed25519 签名/验签；不含 Protobuf codec、网络、设备 DB，切片交付不构成任何握手步或“握手完成”。

**Architecture:** 三个单一职责模块——`frame.rs`（8B 帧头编解码 + 有界长度）、`sig.rs`（两条签名输入拼接，全裸字节）、`ed25519.rs`（ring 签/验薄封装）；`lib.rs` 仅导出这三个模块。严格 TDD：先放可编译桩（返回固定错误值）→ 行为 RED → 最少 GREEN → 全量测试 + fmt/clippy + 独立审查 → 提交。

**Tech Stack:** Rust 1.85（edition 2024）；依赖仅 `ring 0.17.14`（已在 Cargo.lock，经 rustls 间接存在，本计划提升为直依赖）+ 既有 workspace 依赖 `hex 0.4.3` / `thiserror 2.0.20`；零新增供应链 crate。

**Spec:** `docs/superpowers/specs/2026-10-05-controller-device-protocol-core-design.md`（用户已确认提交，下称“设计”）；字节细节、测试清单与 lock 形状取 `.superpowers/sdd/2026-10-03-controller-auth-http/device-protocol-first-slice.md`（下称 SDD）§2–§7；协议唯一依据 `docs/protocol_spec.md` §4.1/§4.3（下称“规范”）。执行者同时阅读设计、SDD、规范与本计划。

## Global Constraints

- 所有命令在 worktree 根（`.worktrees/controller-v1-01-identity`）执行。统一 cargo 前缀：`CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo +1.85.0 ...`（worktree 本地 registry + 共享构建缓存 + 固定工具链 1.85.0）。共享 target 意味着 cargo 构建需与其它同 workspace 构建串行，不并行开两个 cargo。
- **离线与 lock 纪律**：全程无网络、无新 `.crate` 落盘。唯一一次不带 `--locked` 是 Task 1 Step 4 的离线 lock 更新（新增 workspace 成员必然新增 lock 条目）；其形状审计通过后，此后所有 build/test/clippy 一律 `--offline --locked`。Task 2/3 若 `Cargo.lock` 再出现任何变化即版本漂移，停止上报。
- **串行文件所有权**：Task 1 → Task 2 → Task 3 必须逐项完成独立审查后才派下一项；Task 2/3 **禁止并行**，三项均修改同一个 `src/lib.rs`。实现代理不得 stage/commit；由主代理在独立审查后对明确 pathspec 完成秘密/index 审计及提交，计划内 Step 提交命令仅供主代理使用。
- **Rust 工具链分工**：编译/测试统一使用 `cargo +1.85.0 --offline --locked`；本机 1.85.0 未安装 cargo-fmt/clippy 组件，格式检查使用已安装的稳定版 `cargo fmt -p rsetup-protocol -- --check`，Clippy 使用稳定版 `cargo clippy --offline --locked -p rsetup-protocol --all-targets -- -D warnings`。不得因组件缺失私自联网安装。
- **帧契约（规范 §4.1，值原样）**：Magic `0x53 0x45`；Ver `0x02`（收到 `0x01` 或任何其它值 → 拒绝，不得自动降级）；FrameType 仅 `0x01..=0x06`（CLIENT_HELLO/SERVER_CHALLENGE/CLIENT_AUTH_REQUEST/SERVER_AUTH_RESPONSE/SERVER_PENDING/PENDING_PONG）；PayloadLength 4B 大端、`0..=16384`；总帧 `≤16392`B；先校验声明长度再分配；不完整片段是 `Incomplete`（可续收），不是畸形包；帧解析只核验外壳，不表示消息顺序已合法。
- **签名输入（规范 §4.3，值原样）**：客户端 = `client_random[32] || server_random[32] || client_x25519_eph_pub[32] || device_descriptor_wire_bytes`，总长 `96+D`；服务端 = `server_random[32] || client_random[32] || client_ed25519_pub[32] || client_x25519_eph_pub[32] || device_descriptor_wire_bytes || server_x25519_eph_pub[32] || status[1]`，总长 `161+D`；不加长度前缀、不填充；`D ≤ 16247`（16KiB 单帧派生的上界防御，不定义 descriptor 字段语义）；`status` 仅 0/1（裸 1 字节，不是 varint）；descriptor 是帧内原始 wire bytes 原样（禁解码后重序列化）；`reason_code` 不在签名输入内，不得私自改变签名格式；REJECTED 时服务端 eph 公钥为全零 32B 占位，本模块按传入字节原样参与拼装、不做全零假设。
- **Ed25519**：先验公钥 32B、再验签名 64B（固定顺序，两者皆错报公钥错误），再 `ring::signature::UnparsedPublicKey::new(&ED25519, key).verify(message, signature)`；错误固定、去敏，不回显 key/消息/签名内容；签名薄封装只接收真实 `ring::signature::Ed25519KeyPair`，不得以固定返回值/字节相等冒充真实验签。
- **隔离红线**：新 crate 不含 `tokio`/async/`Tcp`/`Socket`/`std::fs`/`sqlx`/任何 Protobuf 消息类型；payload 在本切片 API 中一律不透明 `&[u8]`；不接线中控 `main`；不出现公开/手动设备登记入口；交付与提交信息不用“握手完成/设备接入完成/真实设备已接入”措辞，只能说“帧/签名输入/验签纯函数已按规格通过单测”。
- **严格 RED 纪律**：RED 必须可编译、断言行为失败；不得用导入失败/缺类型/`todo!()`/`unimplemented!()`/`#[ignore]` 充当 RED。
- **测试向量**：先由真实 ring 产生本地回归向量，不能捏造预期常量；它不能证明独立互操作。Task 3 必须记录离线独立向量查找结果，若取得可信 RFC 8032/其他实现向量，则额外钉死并测试；未取得时明确保持跨实现验证未通过。
- **secret/DB**：不读 `secret/`、不连任何数据库、不触迁移；本计划作者未跑过任何 cargo 命令（planning-only）。
- **改动面**：仅根 `Cargo.toml`（members + `ring` workspace 依赖）、`Cargo.lock`（机器更新、形状受审）、`crates/rsetup-protocol/**`（新建）、本计划文档；不动其它产品代码。worktree 已有前序任务遗留的未提交改动 → 各任务提交一律显式 pathspec（`git add <paths>`），禁 `git add -A`、禁 `git add -f`。

Ruling: `&[u8; 32]` 参数由类型系统保证长度，故 `SigInputError` 只保留可达的 DescriptorTooLarge/BadStatus；Ed25519 验证先检查公钥长度再检查签名长度，双错时稳定返回公钥错误。批准的设计只规定字节契约与“先公钥 32B”，SDD 中不可达变体/列举顺序不是更高优先级规范；若判断错误，后续跨语言错误映射须变更，但绝不接受无效签名。

## 文件与职责

| 文件 | 动作 | 职责 |
| --- | --- | --- |
| `Cargo.toml`（根） | 修改 | members 加 `crates/rsetup-protocol`；`[workspace.dependencies]` 加 `ring = "0.17"` |
| `Cargo.lock` | 机器修改 | Task 1 离线更新一次；预期形状 = 仅新增 1 个 `[[package]]` |
| `crates/rsetup-protocol/Cargo.toml` | 新建 | manifest：版本/edition/rust-version/license/repository 全部 `.workspace = true`；deps = `ring`/`hex`/`thiserror`（全部 `.workspace = true`） |
| `crates/rsetup-protocol/src/lib.rs` | 新建 | 仅导出 `frame` / `sig` / `ed25519`（每任务加一行） |
| `crates/rsetup-protocol/src/frame.rs` | 新建（Task 1） | 帧头编解码 + 有界长度，仅此 |
| `crates/rsetup-protocol/src/sig.rs` | 新建（Task 2） | §4.3 两条签名输入拼接 + D/status 守卫，仅此 |
| `crates/rsetup-protocol/src/ed25519.rs` | 新建（Task 3） | ring 签/验薄封装 + 长度守卫，仅此 |
| `crates/rsetup-protocol/tests/frame.rs` | 新建（Task 1） | 帧边界行为测试（T01–T07 + S8） |
| `crates/rsetup-protocol/tests/sig.rs` | 新建（Task 2） | 签名输入布局行为测试（T08/T09 + S12 + D 边界） |
| `crates/rsetup-protocol/tests/ed25519.rs` | 新建（Task 3） | 真实 ring 行为测试（T12–T16 + S9–S11、S16–S18 + 钉死向量） |

## 任务依赖与交接

| 顺序 | 交接 | 约束 |
| --- | --- | --- |
| 1 → 2 | `lib.rs` 增加 `pub mod sig;`；Task 2 不消费 Task 1 函数（纯布局，无 frame 依赖） | Task 2 仅在 Task 1 API + lock 审计审查通过后开始 |
| 2 → 3 | Task 3 消费 `client_signature_input` 构造真实签名消息 | Task 3 仅在 Task 2 布局审查通过后开始 |
| 各项自洽 | 每任务只改自己的 src 文件 + 自己的 test 文件 + `lib.rs` 一行 | 不提前修改下一任务文件、不预置下一任务桩 |

---

### Task 1: crate 骨架 + 离线 lock 更新验证 + 帧边界（frame.rs）

**Files:**
- Modify: `Cargo.toml`（根，两处：members、workspace.dependencies）
- Create: `crates/rsetup-protocol/Cargo.toml`、`crates/rsetup-protocol/src/lib.rs`、`crates/rsetup-protocol/src/frame.rs`、`crates/rsetup-protocol/tests/frame.rs`
- Audit: `Cargo.lock`（本计划唯一一次离线更新 + 形状审计，Step 4）

**Interfaces:**
- Consumes: 既有 `workspace.dependencies` 的 `hex = "0.4"` / `thiserror = "2.0"`；Cargo.lock 已锁 `ring 0.17.14`（当前经 rustls 0.23.45 间接存在）。
- Produces（精确签名，后续任务与未来板端消费）：
  ```rust
  pub const FRAME_HEADER_LEN: usize;   // 8
  pub const FRAME_MAGIC: [u8; 2];      // [0x53, 0x45]
  pub const FRAME_VERSION: u8;         // 0x02
  pub const MAX_PAYLOAD_LEN: usize;    // 16_384
  pub const MAX_FRAME_BYTES: usize;    // 16_392
  #[repr(u8)]
  pub enum FrameType { ClientHello = 0x01, ServerChallenge = 0x02, ClientAuthRequest = 0x03,
      ServerAuthResponse = 0x04, ServerPending = 0x05, PendingPong = 0x06 }
  impl FrameType { pub fn from_u8(v: u8) -> Option<FrameType>; }
  pub enum FrameError { Incomplete { have: usize, need: usize }, BadMagic, BadVersion,
      BadFrameType(u8), PayloadTooLarge { len: u32, max: u32 } }   // thiserror
  pub struct FrameHeader { pub frame_type: FrameType, pub payload_len: u32 }
  pub fn encode_frame(frame_type: FrameType, payload: &[u8]) -> Result<Vec<u8>, FrameError>;
  pub fn parse_frame_header(buf: &[u8]) -> Result<FrameHeader, FrameError>;
  pub fn frame_is_complete(buf_len: usize, payload_len: u32) -> Result<bool, FrameError>;
  ```

- [ ] **Step 1: 加 workspace 成员与 `ring` 直依赖**

根 `Cargo.toml` 改两处：`members` 数组追加 `"crates/rsetup-protocol",`（`crates/rsetup-controller` 之后）；`[workspace.dependencies]` 在 `rand = "0.8"` 行之后加 `ring = "0.17",`：

```toml
[workspace]
resolver = "2"
members = [
    "crates/rsetup-core",
    "crates/rsetup-app",
    "crates/rsetup-controller",
    "crates/rsetup-protocol",
]
exclude = ["apps/desktop/src-tauri"]
```
```toml
rand = "0.8"
ring = "0.17"
sqlx = { version = "0.8", default-features = false, features = ["mysql", "runtime-tokio", "tls-rustls", "chrono", "uuid"] }
```

- [ ] **Step 2: 建 crate manifest 与可编译桩**

`crates/rsetup-protocol/Cargo.toml`：
```toml
[package]
name = "rsetup-protocol"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
hex.workspace = true
ring.workspace = true
thiserror.workspace = true
```

`crates/rsetup-protocol/src/lib.rs`（本任务版；Task 2/3 各加一行）：
```rust
//! rsetup-protocol：板端/中控设备协议字节契约（纯函数，无 IO，无全局状态）。
//!
//! 本切片只覆盖帧边界（protocol_spec.md §4.1）、签名输入 raw wire bytes（§4.3）
//! 与 ring Ed25519 签/验薄封装；不含 Protobuf codec、网络、设备 DB。
//! 其交付不构成任何握手步骤，更不构成握手完成。

pub mod frame;
```

`crates/rsetup-protocol/src/frame.rs`（桩：常量/类型真实，三函数返回固定错误值——保证 RED 可编译）：
```rust
//! 握手帧边界（protocol_spec.md §4.1）。
//!
//! 本模块只做 8B 帧头编解码与有界长度：payload 为不透明原始字节，
//! 本模块不解析；帧解析不表示消息顺序已合法。

use thiserror::Error;

pub const FRAME_HEADER_LEN: usize = 8;
pub const FRAME_MAGIC: [u8; 2] = [0x53, 0x45];
pub const FRAME_VERSION: u8 = 0x02;
pub const MAX_PAYLOAD_LEN: usize = 16_384;
pub const MAX_FRAME_BYTES: usize = FRAME_HEADER_LEN + MAX_PAYLOAD_LEN;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    ClientHello = 0x01,
    ServerChallenge = 0x02,
    ClientAuthRequest = 0x03,
    ServerAuthResponse = 0x04,
    ServerPending = 0x05,
    PendingPong = 0x06,
}

impl FrameType {
    pub fn from_u8(v: u8) -> Option<FrameType> {
        Some(match v {
            0x01 => FrameType::ClientHello,
            0x02 => FrameType::ServerChallenge,
            0x03 => FrameType::ClientAuthRequest,
            0x04 => FrameType::ServerAuthResponse,
            0x05 => FrameType::ServerPending,
            0x06 => FrameType::PendingPong,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FrameError {
    /// 流式状态：尚未收满 8B 头；不是协议错误，接收方可继续收字节。
    #[error("incomplete frame header: have {have} bytes, need {need}")]
    Incomplete { have: usize, need: usize },
    #[error("bad magic")]
    BadMagic,
    #[error("bad version")]
    BadVersion,
    #[error("bad frame type: {0:#04x}")]
    BadFrameType(u8),
    #[error("payload too large: {len} > {max}")]
    PayloadTooLarge { len: u32, max: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub frame_type: FrameType,
    pub payload_len: u32,
}

pub fn encode_frame(frame_type: FrameType, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    // STUB（Task 1 RED）：固定错误行为，待真实实现替换。
    let _ = (frame_type, payload);
    Err(FrameError::Incomplete { have: 0, need: 8 })
}

pub fn parse_frame_header(buf: &[u8]) -> Result<FrameHeader, FrameError> {
    // STUB（Task 1 RED）：固定错误行为，待真实实现替换。
    let _ = buf;
    Err(FrameError::BadMagic)
}

pub fn frame_is_complete(buf_len: usize, payload_len: u32) -> Result<bool, FrameError> {
    // STUB（Task 1 RED）：固定错误行为，待真实实现替换。
    let _ = (buf_len, payload_len);
    Err(FrameError::PayloadTooLarge { len: 0, max: MAX_PAYLOAD_LEN as u32 })
}
```

- [ ] **Step 3: 写行为测试 `tests/frame.rs`（RED 目标；完整测试代码）**

```rust
//! 帧边界行为测试：T01–T07、S8（protocol_spec.md §4.1）。

use rsetup_protocol::frame::{
    encode_frame, frame_is_complete, parse_frame_header, FrameError, FrameType,
    FRAME_HEADER_LEN, FRAME_MAGIC, FRAME_VERSION, MAX_FRAME_BYTES, MAX_PAYLOAD_LEN,
};

fn header_bytes(magic: [u8; 2], ver: u8, ftype: u8, len: u32) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&magic);
    b.push(ver);
    b.push(ftype);
    b.extend_from_slice(&len.to_be_bytes());
    b
}

#[test]
fn frame_encode_header_exact_layout() {
    // T01：输出前 8B 恰为 53 45 02 01 00 00 00 04；总长 = 8 + payload。
    let payload = [0xAA, 0xBB, 0xCC, 0xDD];
    let frame = encode_frame(FrameType::ClientHello, &payload).unwrap();
    assert_eq!(&frame[..8], [0x53, 0x45, 0x02, 0x01, 0x00, 0x00, 0x00, 0x04]);
    assert_eq!(frame.len(), 12);
    assert_eq!(&frame[8..], &payload);
}

#[test]
fn frame_roundtrip_all_six_types() {
    // T02：6 种 FrameType 编码 → 解析，type/len 全还原。
    let types = [
        FrameType::ClientHello,
        FrameType::ServerChallenge,
        FrameType::ClientAuthRequest,
        FrameType::ServerAuthResponse,
        FrameType::ServerPending,
        FrameType::PendingPong,
    ];
    for (i, t) in types.iter().enumerate() {
        let payload = vec![i as u8; 3];
        let frame = encode_frame(*t, &payload).unwrap();
        let h = parse_frame_header(&frame).unwrap();
        assert_eq!(h.frame_type, *t);
        assert_eq!(h.payload_len, 3);
    }
}

#[test]
fn frame_reject_bad_magic_variants() {
    // S1：3 个 magic 变体 → BadMagic；字节序颠倒 0x45 0x53 不是兼容变体。
    for magic in [[0x00, 0x00], [0x53, 0x46], [0x45, 0x53]] {
        assert_eq!(
            parse_frame_header(&header_bytes(magic, FRAME_VERSION, 0x01, 4)),
            Err(FrameError::BadMagic),
        );
    }
    assert_eq!(FRAME_MAGIC, [0x53, 0x45]);
}

#[test]
fn frame_reject_ver_01_no_downgrade() {
    // S2：Ver=0x01 直接拒绝，不存在“按 v1 降级解析”分支。
    assert_eq!(
        parse_frame_header(&header_bytes(FRAME_MAGIC, 0x01, 0x01, 4)),
        Err(FrameError::BadVersion),
    );
}

#[test]
fn frame_reject_unknown_ver_and_type() {
    // S3/S4：Ver=0x03 → BadVersion；FrameType 0x00/0x07/0xFF → BadFrameType(v)，不透传。
    assert_eq!(
        parse_frame_header(&header_bytes(FRAME_MAGIC, 0x03, 0x01, 4)),
        Err(FrameError::BadVersion),
    );
    for t in [0x00u8, 0x07, 0xFF] {
        assert_eq!(
            parse_frame_header(&header_bytes(FRAME_MAGIC, FRAME_VERSION, t, 4)),
            Err(FrameError::BadFrameType(t)),
        );
    }
}

#[test]
fn frame_boundary_16kib() {
    // S5/S6/S7：16385 → PayloadTooLarge（分配前拒绝）；16384 边界通过；
    // frame_is_complete 差 1 字节 / 恰好完整。
    assert_eq!(
        parse_frame_header(&header_bytes(FRAME_MAGIC, FRAME_VERSION, 0x01, 16_385)),
        Err(FrameError::PayloadTooLarge { len: 16_385, max: MAX_PAYLOAD_LEN as u32 }),
    );
    let h = parse_frame_header(&header_bytes(FRAME_MAGIC, FRAME_VERSION, 0x01, 16_384)).unwrap();
    assert_eq!(h.payload_len, 16_384);
    assert_eq!(frame_is_complete(MAX_FRAME_BYTES - 1, 16_384), Ok(false));
    assert_eq!(frame_is_complete(MAX_FRAME_BYTES, 16_384), Ok(true));
    let big = vec![0u8; MAX_PAYLOAD_LEN + 1];
    assert_eq!(
        encode_frame(FrameType::ClientHello, &big),
        Err(FrameError::PayloadTooLarge {
            len: (MAX_PAYLOAD_LEN + 1) as u32,
            max: MAX_PAYLOAD_LEN as u32,
        }),
    );
    let full = vec![0u8; MAX_PAYLOAD_LEN];
    assert_eq!(encode_frame(FrameType::ServerPending, &full).unwrap().len(), MAX_FRAME_BYTES);
}

#[test]
fn frame_incomplete_prefixes() {
    // S7：不完整 TCP 片段是可续收的 Incomplete，不是畸形包；need = 8 - have。
    let buf = header_bytes(FRAME_MAGIC, FRAME_VERSION, 0x01, 4);
    for have in 0..FRAME_HEADER_LEN {
        assert_eq!(
            parse_frame_header(&buf[..have]),
            Err(FrameError::Incomplete { have, need: FRAME_HEADER_LEN - have }),
        );
    }
}

#[test]
fn frame_reject_huge_declared_len_no_overflow() {
    // S8：声明长度 0xFFFFFFF0 → PayloadTooLarge；`8 + len` 加法不得溢出
    // （先校验上限再相加）。
    let huge = u32::MAX - 15; // 0xFFFFFFF0，避免超过 u32 上限的错误字面量
    assert_eq!(
        parse_frame_header(&header_bytes(FRAME_MAGIC, FRAME_VERSION, 0x01, huge)),
        Err(FrameError::PayloadTooLarge { len: huge, max: MAX_PAYLOAD_LEN as u32 }),
    );
    assert_eq!(
        frame_is_complete(0, huge),
        Err(FrameError::PayloadTooLarge { len: huge, max: MAX_PAYLOAD_LEN as u32 }),
    );
}
```

- [ ] **Step 4: 离线 lock 更新与形状审计（本计划唯一一次不带 `--locked`）**

先用文件路径发现工具只读核对 ring 0.17.14 及其已知构建闭包（包括 cc、shlex、getrandom、cfg-if、libc、untrusted、windows-sys、wasi，以及 hex、thiserror、thiserror-impl、proc-macro2、quote、syn）的本地 `.crate`/源码缓存；清单不是完整依赖证明，缺项即停止并报告，最终以随后实际的 `cargo --offline` 解析构建为准。禁止用 shell `grep`、`find` 或手动创建缓存文件冒充依赖就绪。
更新 lock（不带 `--locked`；`--offline` 保证不触网）：
```bash
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target \
  cargo +1.85.0 build -p rsetup-protocol --offline
```
形状审计（以下全部通过才算更新成功，否则停止上报）：
```bash
git diff --numstat Cargo.lock
git diff Cargo.lock
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target \
  cargo +1.85.0 tree -p rsetup-protocol --offline
```
验收标准：
1. `--numstat`：仅 1 个文件 `Cargo.lock`，8–9 行插入（8 行新块 + 空行分隔），0 删除；
2. 全 diff 恰好只新增一个 `[[package]]` 块，逐字等于：
   ```toml
   [[package]]
   name = "rsetup-protocol"
   version = "0.5.0"
   dependencies = [
    "hex",
    "ring",
    "thiserror",
   ]
   ```
   `ring` 保持 `0.17.14`、checksum `a4689e6c2294d81e88dc6261c768b63bc4fcdb852be6d1352498b114f61383b7` 不变；无其它任何包版本/条目变化；
3. `cargo tree`：`rsetup-protocol → ring 0.17.14 / hex 0.4.3 / thiserror 2.0.20`；ring 传递依赖为 `cc 1.4.4, cfg-if 1.0.4, getrandom 0.2.17, libc 0.2.189, untrusted 0.9.0, windows-sys 0.52.0`，无其它版本；
4. 构建输出无 `Downloading` 行；`.cargo-home/registry/cache/` 无新 `.crate` 落盘。

> **未验证声明**：本计划作者未跑过任何 cargo 命令。上述离线可行性是基于规划时只读证据（cache/src/index 三件套齐全、共享 target 已有 ring 编译指纹、工具链 1.85.0 已装）的**预测**，不是结论；本步是首次真实验证。跑过之前不得宣称“离线构建已验证”，失败也不得放宽来源。

失败分支（任何 `--offline` 解析/下载错误）：
1. 从报错中记录缺失的精确 crate 名 + 版本 + index 条目；
2. 停止修改并保留诊断现场；任何清理/还原之前，必须逐一核对实际绝对目标路径及 Git 范围，严禁对未核对的计算路径删除或运行 `git clean`；不得将失败构建宣称为可运行。
3. 停止并上报，给用户两个选项（不代选）：(a) 从受信任机器 vendor 缺失 `.crate` + index 条目进 `.cargo-home`（需单独审查）；(b) 一次性受控联网 `cargo fetch`。

- [ ] **Step 5: 运行并确认 RED**

```bash
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target \
  cargo +1.85.0 test --offline --locked -p rsetup-protocol --test frame
```
预期：8 个测试中 7 个按行为断言失败（对桩 Err 的 `unwrap` panic 或 `assert_eq` 值不匹配）；`frame_reject_bad_magic_variants` 可能偶然通过（桩的固定返回恰为 `BadMagic`）——可接受；**零编译错误**；RED 输出原文记入任务报告。

- [ ] **Step 6: 最小实现（GREEN）**

`frame.rs` 三个函数体替换为（常量/类型/`from_u8` 不动）：
```rust
pub fn encode_frame(frame_type: FrameType, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    // 先校验上限再分配。
    if payload.len() > MAX_PAYLOAD_LEN {
        return Err(FrameError::PayloadTooLarge {
            len: payload.len() as u32,
            max: MAX_PAYLOAD_LEN as u32,
        });
    }
    let mut out = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
    out.extend_from_slice(&FRAME_MAGIC);
    out.push(FRAME_VERSION);
    out.push(frame_type as u8);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

pub fn parse_frame_header(buf: &[u8]) -> Result<FrameHeader, FrameError> {
    if buf.len() < FRAME_HEADER_LEN {
        return Err(FrameError::Incomplete {
            have: buf.len(),
            need: FRAME_HEADER_LEN - buf.len(),
        });
    }
    // 检查顺序固定：magic → version → frame type → payload length。
    if buf[0..2] != FRAME_MAGIC {
        return Err(FrameError::BadMagic);
    }
    if buf[2] != FRAME_VERSION {
        return Err(FrameError::BadVersion);
    }
    let frame_type = FrameType::from_u8(buf[3]).ok_or(FrameError::BadFrameType(buf[3]))?;
    let payload_len = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if payload_len as usize > MAX_PAYLOAD_LEN {
        return Err(FrameError::PayloadTooLarge {
            len: payload_len,
            max: MAX_PAYLOAD_LEN as u32,
        });
    }
    Ok(FrameHeader { frame_type, payload_len })
}

pub fn frame_is_complete(buf_len: usize, payload_len: u32) -> Result<bool, FrameError> {
    // 先校验上限再相加，防大声明长度的 `8 + len` 溢出。
    if payload_len as usize > MAX_PAYLOAD_LEN {
        return Err(FrameError::PayloadTooLarge {
            len: payload_len,
            max: MAX_PAYLOAD_LEN as u32,
        });
    }
    Ok(buf_len >= FRAME_HEADER_LEN + payload_len as usize)
}
```

- [ ] **Step 7: 运行并确认 GREEN + 全量 + fmt/clippy**

```bash
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target \
  cargo +1.85.0 test --offline --locked -p rsetup-protocol --test frame
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target \
  cargo +1.85.0 test --offline --locked -p rsetup-protocol
CARGO_HOME=$PWD/.cargo-home cargo fmt -p rsetup-protocol -- --check
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target \
  cargo clippy --offline --locked -p rsetup-protocol --all-targets -- -D warnings
```
预期：frame 8/8 PASS；crate 全量 PASS（当前仅 frame 一个测试目标）；fmt/clippy 无输出（干净）。

- [ ] **Step 8: 安全审查（实现者之外独立进行）**

核对清单（逐项勾选，全过才算过）：
1. `git show --stat` 仅覆盖 `Cargo.toml`、`Cargo.lock`、`crates/rsetup-protocol/**`（6 个文件）；
2. 常量逐字对规范：`0x53 0x45` / `0x02` / 6 个 FrameType 值 / 16384 / 16392 / 8；
3. 检查顺序固定 magic→ver→type→len；`Ver=0x01` 无降级分支；`0x00`、`0x07..=0xFF` 全拒绝，无“未知类型透传”；
4. `PayloadTooLarge` 在任何缓冲分配之前拒绝；`0xFFFFFFF0` 不溢出（`frame_reject_huge_declared_len_no_overflow` GREEN）；
5. `Incomplete` 作为流式状态返回（含 have/need），不是丢弃流的错误分支；
6. 帧解析不校验消息顺序（代码中无顺序状态）；payload 未做任何解释；
7. `Cargo.lock` diff 形状与 Step 4 验收一致（提交前已审计）；
8. 新 crate 依赖恰为 `{ring, hex, thiserror}`，源码无 `Tcp`/`Socket`/`tokio`/`sqlx`/`std::fs`。

- [ ] **Step 9: 提交**

```bash
git add Cargo.toml Cargo.lock crates/rsetup-protocol
git commit -m "feat(protocol): add rsetup-protocol crate with handshake frame boundary"
```

---

### Task 2: §4.3 签名输入 raw wire bytes（sig.rs）

**Files:**
- Create: `crates/rsetup-protocol/src/sig.rs`、`crates/rsetup-protocol/tests/sig.rs`
- Modify: `crates/rsetup-protocol/src/lib.rs`（加一行 `pub mod sig;`，置于 `pub mod frame;` 之后）
- Audit: Task 2 开始前记录 `sha256sum Cargo.lock` 的完整值到任务报告，Step 5 结束后重新计算并与该入口快照逐字比较；不以 `git status` 判断漂移（Task 1 的改动即使尚未提交亦不应误报）。任一额外变化即停止上报。

**Interfaces:**
- Consumes: 不消费 Task 1 任何函数（纯布局，无 frame 依赖）；workspace `thiserror`。
- Produces（精确签名）：
  ```rust
  pub const MAX_DESCRIPTOR_WIRE_BYTES: usize;  // 16_247
  pub enum SigInputError { DescriptorTooLarge { len: usize, max: usize }, BadStatus(u8) }  // thiserror
  pub fn client_signature_input(
      client_random: &[u8; 32],
      server_random: &[u8; 32],
      client_x25519_eph_pub: &[u8; 32],
      device_descriptor_wire_bytes: &[u8],
  ) -> Result<Vec<u8>, SigInputError>;   // 输出恰 96 + D 字节
  pub fn server_signature_input(
      server_random: &[u8; 32],
      client_random: &[u8; 32],
      client_ed25519_pub: &[u8; 32],
      client_x25519_eph_pub: &[u8; 32],
      device_descriptor_wire_bytes: &[u8],
      server_x25519_eph_pub: &[u8; 32],
      status: u8,                        // 0=APPROVED, 1=REJECTED；>1 → BadStatus
  ) -> Result<Vec<u8>, SigInputError>;   // 输出恰 161 + D 字节
  ```
  > **裁定差异（规范文本 vs 签名）**：SDD 草案在 `&[u8; 32]` 签名下仍列出 `BadClientRandom(usize)` 等不可达长度变体。本计划取数组签名（32B 字段长度违例编译期不可达，S13 的 31B/33B 情形由类型系统 + Task 3 的 `VerifyError` 长度守卫共同覆盖），`SigInputError` 收窄为上列两个可达变体。若审查者坚持 slice 签名 + 运行时长错误，Task 2 须按该结论重做并重新审查。

- [ ] **Step 1: 可编译桩 + lib.rs 导出**

`crates/rsetup-protocol/src/sig.rs`（桩：常量/类型真实，两函数返回固定错误值）：
```rust
//! §4.3 签名输入 raw wire bytes（纯函数，不透明字节）。
//!
//! 两条布局均为直接拼接：不加长度前缀、不填充。`device_descriptor_wire_bytes`
//! 是帧内 DeviceDescriptor 的原始 wire bytes（不透明 `&[u8]`），本模块
//! 不解析、不重序列化。

use thiserror::Error;

/// 派生上限：ClientAuthRequest 固定开销 134B + 嵌套 tag 1B + 2B varint
/// ⇒ `134 + 1 + 2 + D ≤ 16384` ⇒ `D ≤ 16247`。本切片只作上界防御，
/// 不定义 descriptor 字段语义。
pub const MAX_DESCRIPTOR_WIRE_BYTES: usize = 16_247;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SigInputError {
    #[error("descriptor wire bytes too large: {len} > {max}")]
    DescriptorTooLarge { len: usize, max: usize },
    #[error("bad status byte: {0:#04x} (only 0x00/0x01 allowed)")]
    BadStatus(u8),
}

/// §4.3.1 客户端签名输入（板端 Ed25519 长期私钥签）：
/// `client_random(32) || server_random(32) || client_x25519_eph_pub(32) || descriptor_wire(D)`。
pub fn client_signature_input(
    client_random: &[u8; 32],
    server_random: &[u8; 32],
    client_x25519_eph_pub: &[u8; 32],
    device_descriptor_wire_bytes: &[u8],
) -> Result<Vec<u8>, SigInputError> {
    // STUB（Task 2 RED）：固定错误行为，待真实实现替换。
    let _ = (client_random, server_random, client_x25519_eph_pub, device_descriptor_wire_bytes);
    Err(SigInputError::DescriptorTooLarge {
        len: 0,
        max: MAX_DESCRIPTOR_WIRE_BYTES,
    })
}

/// §4.3.2 服务端签名输入（中控 Ed25519 长期私钥签）：
/// `server_random(32) || client_random(32) || client_ed25519_pub(32) || client_x25519_eph_pub(32)`
/// `|| descriptor_wire(D) || server_x25519_eph_pub(32) || status(1B)`。
/// REJECTED 时 `server_x25519_eph_pub` 为全零 32B 占位；本函数按传入字节原样
/// 参与拼装，不做全零假设。
pub fn server_signature_input(
    server_random: &[u8; 32],
    client_random: &[u8; 32],
    client_ed25519_pub: &[u8; 32],
    client_x25519_eph_pub: &[u8; 32],
    device_descriptor_wire_bytes: &[u8],
    server_x25519_eph_pub: &[u8; 32],
    status: u8,
) -> Result<Vec<u8>, SigInputError> {
    // STUB（Task 2 RED）：固定错误行为，待真实实现替换。
    let _ = (
        server_random,
        client_random,
        client_ed25519_pub,
        client_x25519_eph_pub,
        device_descriptor_wire_bytes,
        server_x25519_eph_pub,
        status,
    );
    Err(SigInputError::BadStatus(0))
}
```

`lib.rs` 追加（保持字母序）：
```rust
pub mod frame;
pub mod sig;
```

- [ ] **Step 2: 写行为测试 `tests/sig.rs`（完整测试代码）**

```rust
//! §4.3 签名输入布局行为测试：T08/T09、S12、D 边界。

use rsetup_protocol::sig::{
    client_signature_input, server_signature_input, SigInputError, MAX_DESCRIPTOR_WIRE_BYTES,
};

/// 确定性模式字节：从 `base` 起 32 字节，逐字节 +1。
fn ramp(base: u8) -> [u8; 32] {
    let mut a = [0u8; 32];
    for (i, b) in a.iter_mut().enumerate() {
        *b = base.wrapping_add(i as u8);
    }
    a
}

/// SDD §2.4 参考 raw wire bytes：DeviceDescriptor field1 = "ABCD"（0A 04 41 42 43 44）。
const DESC: [u8; 6] = [0x0A, 0x04, b'A', b'B', b'C', b'D'];

#[test]
fn client_sig_input_exact_bytes() {
    // T08：输出恰 96+6=102B，与逐段拼接逐字节相等；
    // 关键位置 hex 钉死（client_random@0、server_random@32、eph@64、descriptor@96）。
    let cr = ramp(0x00);
    let sr = ramp(0x20);
    let eph = ramp(0x40);
    let got = client_signature_input(&cr, &sr, &eph, &DESC).unwrap();
    assert_eq!(got.len(), 102);
    let mut want = Vec::new();
    want.extend_from_slice(&cr);
    want.extend_from_slice(&sr);
    want.extend_from_slice(&eph);
    want.extend_from_slice(&DESC);
    assert_eq!(got, want);
    assert_eq!(&hex::encode(&got[..10]), "00010203040506070809");
    assert_eq!(&hex::encode(&got[32..42]), "20212223242526272829");
    assert_eq!(&hex::encode(&got[96..102]), "0a0441424344");
}

#[test]
fn server_sig_input_exact_bytes() {
    // T09：恰 161+6=167B；status 0/1 仅末 1B 不同；
    // 段序按 §4.3.2（server_random 开头，与客户端布局故意不同）。
    let sr = ramp(0x00);
    let cr = ramp(0x20);
    let ed = ramp(0x40);
    let eph = ramp(0x60);
    let server_eph = ramp(0x80);

    let approved = server_signature_input(&sr, &cr, &ed, &eph, &DESC, &server_eph, 0).unwrap();
    let rejected = server_signature_input(&sr, &cr, &ed, &eph, &DESC, &server_eph, 1).unwrap();
    // 独立完整期望：钉死 ed_pub@64、client_eph_pub@96 及全部尾段；
    // 单纯核对总长/首尾字节无法发现两个安全关键段的互换。
    let mut want = Vec::new();
    want.extend_from_slice(&sr);
    want.extend_from_slice(&cr);
    want.extend_from_slice(&ed);
    want.extend_from_slice(&eph);
    want.extend_from_slice(&DESC);
    want.extend_from_slice(&server_eph);
    want.push(0);
    assert_eq!(approved, want);
    assert_eq!(rejected[..166], want[..166]);
    assert_eq!(approved.len(), 167);
    assert_eq!(rejected.len(), 167);
    assert_eq!(&approved[..166], &rejected[..166]);
    assert_eq!(approved[166], 0);
    assert_eq!(rejected[166], 1);

    // 位置钉死：sr@0、cr@32、descriptor@128、server_eph@134、status 为末 1B。
    assert_eq!(&hex::encode(&approved[..10]), "00010203040506070809");
    assert_eq!(&hex::encode(&approved[32..42]), "20212223242526272829");
    assert_eq!(&hex::encode(&approved[128..134]), "0a0441424344");
    assert_eq!(&hex::encode(&approved[134..138]), "80818283");

    // 两条布局在同输入下不同（前 64B 顺序互换），防“共用一个顺序”的实现 bug。
    let client_in = client_signature_input(&cr, &sr, &eph, &DESC).unwrap();
    assert_ne!(&client_in[..96], &approved[..96]);
}

#[test]
fn server_sig_input_rejects_bad_status() {
    // S12：`status` 是裸 1 字节，不是 varint；0x02/0xFF 不产生签名输入。
    let z = [0u8; 32];
    assert_eq!(
        server_signature_input(&z, &z, &z, &z, b"", &z, 2),
        Err(SigInputError::BadStatus(2)),
    );
    assert_eq!(
        server_signature_input(&z, &z, &z, &z, b"", &z, 0xFF),
        Err(SigInputError::BadStatus(0xFF)),
    );
    // 同时违反 status 与 D 上界时必须优先报告 BadStatus。
    assert_eq!(
        server_signature_input(&z, &z, &z, &z, &vec![0u8; MAX_DESCRIPTOR_WIRE_BYTES + 1], &z, 2),
        Err(SigInputError::BadStatus(2)),
    );
}

#[test]
fn descriptor_length_bounds() {
    // D=0 字节层合法；D=16247 上界通过；D=16248 拒绝（16KiB 单帧派生上限）。
    let z = [0u8; 32];
    assert_eq!(client_signature_input(&z, &z, &z, b"").unwrap().len(), 96);
    let max_desc = vec![0u8; MAX_DESCRIPTOR_WIRE_BYTES];
    assert_eq!(
        client_signature_input(&z, &z, &z, &max_desc).unwrap().len(),
        96 + MAX_DESCRIPTOR_WIRE_BYTES,
    );
    assert_eq!(
        client_signature_input(&z, &z, &z, &vec![0u8; MAX_DESCRIPTOR_WIRE_BYTES + 1]),
        Err(SigInputError::DescriptorTooLarge {
            len: MAX_DESCRIPTOR_WIRE_BYTES + 1,
            max: MAX_DESCRIPTOR_WIRE_BYTES,
        }),
    );
    assert_eq!(
        server_signature_input(
            &z,
            &z,
            &z,
            &z,
            &vec![0u8; MAX_DESCRIPTOR_WIRE_BYTES + 1],
            &z,
            0,
        ),
        Err(SigInputError::DescriptorTooLarge {
            len: MAX_DESCRIPTOR_WIRE_BYTES + 1,
            max: MAX_DESCRIPTOR_WIRE_BYTES,
        }),
    );
}
```

- [ ] **Step 3: 运行并确认 RED**

```bash
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target \
  cargo +1.85.0 test --offline --locked -p rsetup-protocol --test sig
```
预期：4 个测试全按行为断言失败（桩固定 Err → `unwrap` panic 或 `assert_eq` 值不匹配）；零编译错误；RED 输出原文记入任务报告。

- [ ] **Step 4: 最小实现（GREEN）**

`sig.rs` 两函数体替换为（常量/类型不动）：
```rust
pub fn client_signature_input(
    client_random: &[u8; 32],
    server_random: &[u8; 32],
    client_x25519_eph_pub: &[u8; 32],
    device_descriptor_wire_bytes: &[u8],
) -> Result<Vec<u8>, SigInputError> {
    check_descriptor(device_descriptor_wire_bytes)?;
    let mut out = Vec::with_capacity(96 + device_descriptor_wire_bytes.len());
    out.extend_from_slice(client_random);
    out.extend_from_slice(server_random);
    out.extend_from_slice(client_x25519_eph_pub);
    out.extend_from_slice(device_descriptor_wire_bytes);
    Ok(out)
}

pub fn server_signature_input(
    server_random: &[u8; 32],
    client_random: &[u8; 32],
    client_ed25519_pub: &[u8; 32],
    client_x25519_eph_pub: &[u8; 32],
    device_descriptor_wire_bytes: &[u8],
    server_x25519_eph_pub: &[u8; 32],
    status: u8,
) -> Result<Vec<u8>, SigInputError> {
    // 检查顺序固定：先 status，后 descriptor 上限。
    if status > 1 {
        return Err(SigInputError::BadStatus(status));
    }
    check_descriptor(device_descriptor_wire_bytes)?;
    let mut out = Vec::with_capacity(161 + device_descriptor_wire_bytes.len());
    out.extend_from_slice(server_random);
    out.extend_from_slice(client_random);
    out.extend_from_slice(client_ed25519_pub);
    out.extend_from_slice(client_x25519_eph_pub);
    out.extend_from_slice(device_descriptor_wire_bytes);
    out.extend_from_slice(server_x25519_eph_pub);
    out.push(status);
    Ok(out)
}

fn check_descriptor(d: &[u8]) -> Result<(), SigInputError> {
    if d.len() > MAX_DESCRIPTOR_WIRE_BYTES {
        return Err(SigInputError::DescriptorTooLarge {
            len: d.len(),
            max: MAX_DESCRIPTOR_WIRE_BYTES,
        });
    }
    Ok(())
}
```

- [ ] **Step 5: 运行并确认 GREEN + 全量 + fmt/clippy + lock 漂移检查**

```bash
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target \
  cargo +1.85.0 test --offline --locked -p rsetup-protocol
CARGO_HOME=$PWD/.cargo-home cargo fmt -p rsetup-protocol -- --check
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target \
  cargo clippy --offline --locked -p rsetup-protocol --all-targets -- -D warnings
sha256sum Cargo.lock  # compare full value with Task 2 entry snapshot recorded in report
```
预期：sig 4/4 + frame 8/8 PASS；fmt/clippy 干净；`Cargo.lock` 的 SHA-256 与 Task 2 开始时逐字一致。

- [ ] **Step 6: 安全审查（实现者之外独立进行）**

核对清单：
1. 两条布局字节序与规范 §4.3.1/§4.3.2 逐字一致（客户端 client_random 开头、服务端 server_random 开头；`server_sig_input_exact_bytes` 含“两布局必不同”断言）；
2. 无长度前缀/无填充：总长恰 `96+D` / `161+D`（测试断言）；
3. descriptor 不透明 `&[u8]`：本模块无解码/重序列化（源码审查无字段解析）；
4. `D=0/16247` 通过、`D=16248` 拒绝；`status` 为 `u8` 且仅 0/1（非 varint）；
5. `server_x25519_eph_pub` 按传入字节原样参与拼装（无全零假设；S14 的语义校验留给后续 codec/状态机）；
6. `reason_code` 不在签名输入（S15/O2：如实记录规范现状，不改规范）；
7. 检查顺序固定（status → descriptor）；本任务产出唯一消费者为 Task 3 与未来板端。

- [ ] **Step 7: 提交**

```bash
git add crates/rsetup-protocol/src/sig.rs crates/rsetup-protocol/src/lib.rs crates/rsetup-protocol/tests/sig.rs
git commit -m "feat(protocol): add raw-wire signature input assembly (spec 4.3)"
```

---

### Task 3: ring Ed25519 薄封装 + 真实向量钉死（ed25519.rs）

**Files:**
- Create: `crates/rsetup-protocol/src/ed25519.rs`、`crates/rsetup-protocol/tests/ed25519.rs`
- Modify: `crates/rsetup-protocol/src/lib.rs`（加一行 `pub mod ed25519;`，置于 `pub mod frame;` 之前保持字母序）
- Audit: Task 3 开始前记录 `sha256sum Cargo.lock` 完整值到报告，Step 5 后重新计算并与入口快照比对；不以 `git status` 判断（Task 1 lock 改动可能尚未提交）。

**Interfaces:**
- Consumes: Task 2 的 `client_signature_input`（构造真实签名消息）；`ring 0.17.14`（API 已对照本地缓存源码核对：`signature::ED25519` 静态项、`UnparsedPublicKey::new(&'static dyn VerificationAlgorithm, B).verify(msg, sig) -> Result<(), error::Unspecified>`、`Ed25519KeyPair::from_seed_unchecked(&[u8]) -> Result<Self, KeyRejected>`、`KeyPair::public_key() -> &PublicKey`、`Signature: AsRef<[u8]>`（Ed25519 为 64B））。
- Produces（精确签名）：
  ```rust
  pub const PUBLIC_KEY_LEN: usize;   // 32
  pub const SIGNATURE_LEN: usize;    // 64
  pub enum VerifyError { PublicKeyLen { len: usize }, SignatureLen { len: usize }, InvalidSignature }  // thiserror
  pub fn verify_ed25519(message: &[u8], signature: &[u8], public_key: &[u8]) -> Result<(), VerifyError>;
  pub fn sign_ed25519(key: &ring::signature::Ed25519KeyPair, message: &[u8]) -> [u8; 64];
  ```
  长度守卫顺序固定：先公钥后签名（两者皆错 → 报公钥错误，`ring_verify_length_guards` 钉死）。

- [ ] **Step 1: 可编译桩 + lib.rs 导出**

`crates/rsetup-protocol/src/ed25519.rs`（桩：sign 返回全 `0x55` 模式——必非真实签名，保证 T16 在 RED 必失败；verify 恒 `InvalidSignature`）：
```rust
//! ring 0.17.14 Ed25519 薄封装（本 crate 唯一真实密码学入口）。
//!
//! 本切片生产路径只用 `verify_ed25519`；`sign_ed25519` 供测试向量生成与
//! 未来中控响应签名。密钥生成/落盘/加载（0600 等）不在本切片。

use ring::signature::Ed25519KeyPair;
use thiserror::Error;

pub const PUBLIC_KEY_LEN: usize = 32;
pub const SIGNATURE_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum VerifyError {
    #[error("bad public key length: {len} (want {PUBLIC_KEY_LEN})")]
    PublicKeyLen { len: usize },
    #[error("bad signature length: {len} (want {SIGNATURE_LEN})")]
    SignatureLen { len: usize },
    #[error("invalid signature")]
    InvalidSignature,
}

/// 验签：先长度守卫（公钥 32B，再签名 64B，固定顺序），后真实 ring 验签。
/// 错误固定、去敏：不回显 key/消息/签名内容。
pub fn verify_ed25519(message: &[u8], signature: &[u8], public_key: &[u8]) -> Result<(), VerifyError> {
    // STUB（Task 3 RED）：固定错误行为，待真实实现替换。
    let _ = (message, signature, public_key);
    Err(VerifyError::InvalidSignature)
}

/// 签名：ring 薄封装，只接收真实 `Ed25519KeyPair`。
pub fn sign_ed25519(key: &Ed25519KeyPair, message: &[u8]) -> [u8; SIGNATURE_LEN] {
    // STUB（Task 3 RED）：固定错误行为（全 0x55 模式，必非真实签名），
    // 待真实实现替换。
    let _ = (key, message);
    [0x55; SIGNATURE_LEN]
}
```

`lib.rs` 更新为：
```rust
pub mod ed25519;
pub mod frame;
pub mod sig;
```

- [ ] **Step 2: 写行为测试 `tests/ed25519.rs`（完整测试代码）**

```rust
//! 真实 ring Ed25519 行为测试：T12–T16、S9–S11、S16–S18（protocol_spec.md §4.3/§4.2）。

use ring::signature::{Ed25519KeyPair, KeyPair};
use rsetup_protocol::ed25519::{sign_ed25519, verify_ed25519, VerifyError};
use rsetup_protocol::sig::client_signature_input;

/// 测试 fixture：固定种子（非秘密），模式字节 01 02 03 ... 20。
fn test_seed() -> [u8; 32] {
    let mut a = [0u8; 32];
    for (i, b) in a.iter_mut().enumerate() {
        *b = (i as u8).wrapping_add(1);
    }
    a
}

fn keypair_from(seed: &[u8; 32]) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(seed).expect("fixed 32B seed must be accepted")
}

fn ramp(base: u8) -> [u8; 32] {
    let mut a = [0u8; 32];
    for (i, b) in a.iter_mut().enumerate() {
        *b = base.wrapping_add(i as u8);
    }
    a
}

/// 固定 fixture：(client_random, server_random, client_x25519_eph_pub)，确定性模式字节。
fn fixture() -> ([u8; 32], [u8; 32], [u8; 32]) {
    (ramp(0x20), ramp(0x40), ramp(0x60))
}

/// SDD §2.4 参考 raw wire bytes：DeviceDescriptor field1 = "ABCD"。
const DESC: [u8; 6] = [0x0A, 0x04, b'A', b'B', b'C', b'D'];

#[test]
fn ring_verify_happy_path() {
    // T12：完整客户端签名输入上真实签名 + 真实验签（固定种子，确定性）。
    let (cr, sr, eph) = fixture();
    let key = keypair_from(&test_seed());
    let msg = client_signature_input(&cr, &sr, &eph, &DESC).unwrap();
    let sig = sign_ed25519(&key, &msg);
    assert_eq!(verify_ed25519(&msg, &sig, key.public_key().as_ref()), Ok(()));
}

#[test]
fn ring_verify_tamper_variants() {
    // S16：消息翻 1 bit / 签名改 1 字节 / 公钥改 1 字节 → InvalidSignature（3 个独立变体）。
    let (cr, sr, eph) = fixture();
    let key = keypair_from(&test_seed());
    let msg = client_signature_input(&cr, &sr, &eph, &DESC).unwrap();
    let sig = sign_ed25519(&key, &msg);
    let pk = key.public_key().as_ref().to_vec();

    let mut msg_t = msg.clone();
    msg_t[0] ^= 0x01;
    assert_eq!(verify_ed25519(&msg_t, &sig, &pk), Err(VerifyError::InvalidSignature));

    // 单独钉死 descriptor 原始 wire 区任意 1B 改动必使签名失效。
    let mut descriptor_t = msg.clone();
    descriptor_t[96] ^= 0x01;
    assert_eq!(verify_ed25519(&descriptor_t, &sig, &pk), Err(VerifyError::InvalidSignature));

    let mut sig_t = sig;
    sig_t[10] ^= 0xFF;
    assert_eq!(verify_ed25519(&msg, &sig_t, &pk), Err(VerifyError::InvalidSignature));

    let mut pk_t = pk.clone();
    pk_t[3] ^= 0x80;
    assert_eq!(verify_ed25519(&msg, &sig, &pk_t), Err(VerifyError::InvalidSignature));
}

#[test]
fn ring_verify_length_guards() {
    // S18/S13：长度错误类型化且不进入 ring 调用；
    // 守卫顺序固定：先公钥后签名（两者皆错 → 报公钥错误）。
    let pk = [7u8; 32];
    let sig64 = [9u8; 64];
    let msg = [1u8; 5];
    assert_eq!(
        verify_ed25519(&msg, &sig64[..63], &pk),
        Err(VerifyError::SignatureLen { len: 63 }),
    );
    assert_eq!(
        verify_ed25519(&msg, &[0u8; 65], &pk),
        Err(VerifyError::SignatureLen { len: 65 }),
    );
    assert_eq!(
        verify_ed25519(&msg, &sig64, &pk[..31]),
        Err(VerifyError::PublicKeyLen { len: 31 }),
    );
    assert_eq!(
        verify_ed25519(&msg, &sig64, &[0u8; 33]),
        Err(VerifyError::PublicKeyLen { len: 33 }),
    );
    assert_eq!(
        verify_ed25519(&msg, &sig64[..63], &pk[..31]),
        Err(VerifyError::PublicKeyLen { len: 31 }),
    );
}

#[test]
fn ring_verify_wrong_keypair_fails() {
    // S17：密钥对 A 的签名 + 密钥对 B 的公钥（各自合法、只是不匹配）→ InvalidSignature。
    let (cr, sr, eph) = fixture();
    let msg = client_signature_input(&cr, &sr, &eph, &DESC).unwrap();
    let k1 = keypair_from(&test_seed());
    let mut seed2 = test_seed();
    seed2[0] = 0xFF;
    let k2 = keypair_from(&seed2);
    let sig = sign_ed25519(&k1, &msg);
    assert_eq!(
        verify_ed25519(&msg, &sig, k2.public_key().as_ref()),
        Err(VerifyError::InvalidSignature),
    );
}

#[test]
fn sig_input_order_is_binding() {
    // S9/T10：对正确布局输入签名，用 (client_random, server_random) 互换后的输入
    // 验签 → 必败；两段装配输出必不同字节。
    let (cr, sr, eph) = fixture();
    let key = keypair_from(&test_seed());
    let correct = client_signature_input(&cr, &sr, &eph, &DESC).unwrap();
    let swapped = client_signature_input(&sr, &cr, &eph, &DESC).unwrap();
    assert_ne!(correct, swapped);
    let sig = sign_ed25519(&key, &correct);
    assert_eq!(
        verify_ed25519(&swapped, &sig, key.public_key().as_ref()),
        Err(VerifyError::InvalidSignature),
    );
}

#[test]
fn descriptor_raw_bytes_rule() {
    // S10/T11：用 descriptor A（canonical 字段序）签名；用 B（字段序不同、
    // 同逻辑值）拼装验签 → 必败。A/B 均为手写 raw wire bytes；
    // 本测试不得出现“解码再编码”代码路径。
    let (cr, sr, eph) = fixture();
    let key = keypair_from(&test_seed());
    let a: [u8; 10] = [0x0A, 0x04, b'A', b'B', b'C', b'D', 0x12, 0x02, b'M', b'1'];
    let b: [u8; 10] = [0x12, 0x02, b'M', b'1', 0x0A, 0x04, b'A', b'B', b'C', b'D'];
    let msg_a = client_signature_input(&cr, &sr, &eph, &a).unwrap();
    let msg_b = client_signature_input(&cr, &sr, &eph, &b).unwrap();
    let sig = sign_ed25519(&key, &msg_a);
    assert_eq!(verify_ed25519(&msg_a, &sig, key.public_key().as_ref()), Ok(()));
    assert_eq!(
        verify_ed25519(&msg_b, &sig, key.public_key().as_ref()),
        Err(VerifyError::InvalidSignature),
    );
}

#[test]
fn cross_session_replay_fails() {
    // S11/T15：会话 1（server_random=R1）输入签名；会话 2（R2）输入验签 → 必败。
    let (cr, _sr, eph) = fixture();
    let key = keypair_from(&test_seed());
    let r1 = ramp(0xA0);
    let r2 = ramp(0xB0);
    let s1 = client_signature_input(&cr, &r1, &eph, &DESC).unwrap();
    let s2 = client_signature_input(&cr, &r2, &eph, &DESC).unwrap();
    let sig = sign_ed25519(&key, &s1);
    assert_eq!(verify_ed25519(&s1, &sig, key.public_key().as_ref()), Ok(()));
    assert_eq!(
        verify_ed25519(&s2, &sig, key.public_key().as_ref()),
        Err(VerifyError::InvalidSignature),
    );
}

#[test]
fn vector_pinning() {
    // T16：固定种子 + 固定消息（102B 客户端签名输入）→ 签名 hex 必须等于钉死常量。
    // 该向量是任何第二实现（含未来 board agent）的互操作基准。
    // RED：常量为全零哨兵；首次运行必失败，并从 `PINNED_SIG=` 输出读出真实值。
    // GREEN：用 ring 输出的真实值替换哨兵、删除 eprintln、重跑 PASS，
    // 并把最终向量记入任务报告。
    const ZERO_SENTINEL: &str = concat!(
        "0000000000000000",
        "0000000000000000",
        "0000000000000000",
        "0000000000000000",
        "0000000000000000",
        "0000000000000000",
        "0000000000000000",
        "0000000000000000",
    ); // 8 × 16 = 128 个 0 = 64 字节签名
    let (cr, sr, eph) = fixture();
    let key = keypair_from(&test_seed());
    let msg = client_signature_input(&cr, &sr, &eph, &DESC).unwrap();
    let sig = sign_ed25519(&key, &msg);
    let hex_sig = hex::encode(sig);
    eprintln!("PINNED_SIG={hex_sig}"); // GREEN 钉死后删除本行
    assert_eq!(hex_sig, ZERO_SENTINEL);
}
```

- [ ] **Step 3: 运行并确认 RED**

```bash
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target \
  cargo +1.85.0 test --offline --locked -p rsetup-protocol --test ed25519
```
预期：8 个测试中 5 个必按行为断言失败：`ring_verify_happy_path`（桩 verify 恒 Err）、`ring_verify_length_guards`（桩不区分长度错误）、`descriptor_raw_bytes_rule`/`cross_session_replay_fails`（首个 `Ok` 断言失败）、`vector_pinning`（桩全 `0x55` 签名 ≠ 全零哨兵）；`ring_verify_tamper_variants`/`ring_verify_wrong_keypair_fails`/`sig_input_order_is_binding` 在桩阶段可能偶然成立（负向断言与桩一致）——可接受，GREEN 门禁是全部通过 + 向量为真实值；零编译错误；RED 输出原文记入任务报告。

- [ ] **Step 4: 最小实现（GREEN）**

`ed25519.rs` 文件头 `use` 行更新为：
```rust
use ring::signature::{Ed25519KeyPair, UnparsedPublicKey, ED25519};
```
两函数体替换为（常量/`VerifyError` 不动）：
```rust
pub fn verify_ed25519(message: &[u8], signature: &[u8], public_key: &[u8]) -> Result<(), VerifyError> {
    if public_key.len() != PUBLIC_KEY_LEN {
        return Err(VerifyError::PublicKeyLen { len: public_key.len() });
    }
    if signature.len() != SIGNATURE_LEN {
        return Err(VerifyError::SignatureLen { len: signature.len() });
    }
    let pk = UnparsedPublicKey::new(&ED25519, public_key);
    pk.verify(message, signature)
        .map(|_| ())
        .map_err(|_| VerifyError::InvalidSignature)
}

pub fn sign_ed25519(key: &Ed25519KeyPair, message: &[u8]) -> [u8; SIGNATURE_LEN] {
    let sig = key.sign(message);
    let mut out = [0u8; SIGNATURE_LEN];
    out.copy_from_slice(sig.as_ref());
    out
}
```

- [ ] **Step 5: 运行 GREEN + T16 向量钉死 + 全量 + fmt/clippy + lock 漂移检查**

1. 全量运行（真实实现已就位；`vector_pinning` 此时仍因全零哨兵而失败，同时 `eprintln` 输出真实值——读该输出）：
   ```bash
   CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target \
     cargo +1.85.0 test --offline --locked -p rsetup-protocol -- --nocapture
   ```
2. 从 `PINNED_SIG=<128 位 hex>` 输出行取真实值：把 `tests/ed25519.rs` 中 `ZERO_SENTINEL` 常量改名为 `PINNED_SIG_HEX`、`concat!` 体替换为该 128 字符单条字面量、同步更新 `assert_eq` 引用，并删除 `eprintln` 行；
3. 重跑确认：
   ```bash
   CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target \
     cargo +1.85.0 test --offline --locked -p rsetup-protocol
   CARGO_HOME=$PWD/.cargo-home cargo fmt -p rsetup-protocol -- --check
   CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target \
     cargo clippy --offline --locked -p rsetup-protocol --all-targets -- -D warnings
   sha256sum Cargo.lock  # 与 Task 3 入口报告中的完整 SHA-256 逐字比对
   ```
   预期：20/20 PASS（frame 8 + sig 4 + ed25519 8）；fmt/clippy 干净；`Cargo.lock` SHA-256 与 Task 3 入口快照一致；钉死的向量 hex 与种子/消息 fixture 一并记入任务报告。

- [ ] **Step 6: 安全审查（实现者之外独立进行）**

核对清单：
1. **真实密码学门禁**：T16 钉死向量可由任何装有 ring 的环境按报告中记录的种子+消息复算；全零/全 `0x55` 等固定值已被真实 hex 替换；
2. S16 三种篡改 + S17 不同密钥对 + S18 长度守卫（守卫顺序：公钥→签名，双错报公钥错误）全 GREEN；
3. T10 顺序绑定 / T11 原样字节（测试中无“解码再编码”路径）/ T15 跨会话重放，反例全 GREEN；
4. 错误去敏：`VerifyError` 各变体只带长度值，不回显 key/消息/签名内容；
5. `sign_ed25519` 为薄封装（只接真实 `Ed25519KeyPair`、`copy_from_slice` 取 64B），非固定返回；
6. 源码无 `Tcp`/`Socket`/`tokio`/`sqlx`/`std::fs`；crate 依赖恰为 `{ring, hex, thiserror}`；
7. 交付措辞不含“握手完成/设备接入”（操作化定义：G4+G6 之前不得用）。

- [ ] **Step 7: 提交**

```bash
git add crates/rsetup-protocol/src/ed25519.rs crates/rsetup-protocol/src/lib.rs crates/rsetup-protocol/tests/ed25519.rs
git commit -m "feat(protocol): add ring Ed25519 wrapper with pinned test vector"
```

---

## 覆盖核对（规格 → 任务）

| 设计要求 | 任务 |
| --- | --- |
| 帧边界：8B 头 / 6 帧类型 / 16KiB / Incomplete / 无越界分配（设计“固定字节契约”、SDD T01–T07、S1–S8） | Task 1（含 `frame_reject_huge_declared_len_no_overflow`） |
| 签名输入 raw-wire：96+D / 161+D、无长度前缀、D≤16247、status 0/1、原样字节（设计、SDD §2.2/§2.3、T08/T09、S12） | Task 2 |
| ring Ed25519 真实签/验：32/64 守卫、去敏错误、钉死向量、顺序/重放/原样字节绑定（设计、SDD T12–T16、S9–S11、S16–S18） | Task 3（另加 `ring_verify_wrong_keypair_fails` 覆盖 S17） |
| 离线 lock 纪律：首次仅 `--offline` 更新 → 形状审计 → 全链路 `--locked`（设计“接口与隔离”、SDD §7.4） | Task 1 Step 4 建立；Task 2/3 以 `--locked` + 漂移检查维持 |
| 隔离：无 codec/网络/DB/main 接线/登记入口（设计“目的与非目标”、SDD §8） | 各任务 Files 限定 + 各自安全审查 |
| 严格 TDD（可编译 RED / 最少 GREEN）、fmt/clippy（设计“测试和未验门禁”） | 每任务 Step 2–7 |
| 交付措辞门禁（“不能称握手完成”操作化定义） | Task 3 Step 6 第 7 条 + 全局约束 |

## 自查记录（写计划时完成，执行时复核）

1. **覆盖**：SDD T01–T16 与 S1–S18 逐项映射到上述测试函数，无遗漏项；设计文档三段（固定字节契约/接口与隔离/测试和未验门禁）均有任务承接。
2. **占位扫描**：全文无 `TBD`/`TODO`/“以后实现”。唯一两阶段常量是 T16 的 `ZERO_SENTINEL`（SDD §7.2 规定 RED 态即“常量位占位 + 断言框架就位”），其替换程序已钉死在 Task 3 Step 5；GREEN 后必须删除哨兵与 `eprintln`。
3. **类型/签名一致性**：三处出现的 `FrameError`/`FrameHeader`/`FrameType`、`SigInputError`、`VerifyError` 签名与 `Interfaces` 完全一致；`ramp`/`DESC` fixture 在 `tests/sig.rs` 与 `tests/ed25519.rs` 中定义相同（各自文件自包含，不共享测试模块）；hex 锚点（`00010203040506070809`、`20212223242526272829`、`0a0441424344`、`80818283`）已按模式字节手工核算。
4. **已裁定的规范不一致**：
   - SDD `SigInputError` 的长度变体在 `&[u8; 32]` 签名下不可达 → 收窄为 `DescriptorTooLarge`/`BadStatus`（Task 2 Interfaces 已注明）；
   - SDD `VerifyError` 变体列举顺序（SignatureLen 在前）与设计“先要求公钥 32B、签名 64B”不一致 → 本计划固定**公钥先**（Task 3 `ring_verify_length_guards` 含双错用例钉死）。

## 未决风险（规划时只读证据不能消除的部分）

1. **离线 lock 更新是预测而非已验证结论**：本计划作者未跑过任何 cargo 命令。只读证据支持可行性（`.cargo-home` 的 cache/src/index 三件套含 ring 0.17.14 及其全部传递依赖、共享 target 已有 ring 编译指纹、工具链 1.85.0 已装），但 Task 1 Step 4 才是首次真实验证；失败则走该步的降级分支（vendor 或一次性受控联网，需用户选择）。
2. **`SigInputError` 收窄是计划裁定**：若审查者坚持 slice 签名 + 运行时长错误 API，Task 2 需重做并重审（Task 3 不受影响）。
3. **跨实现向量未钉死**：T16 仅钉死本地 ring 生成向量，证明回归/确定性，**不证明 RFC 8032 互操作**。执行 Task 3 时先只读检查本地是否存在可核实的 RFC 8032 或独立实现向量，记录实际查找范围与结果；**若取得可信独立向量，必须额外加对应断言**，不得用同一 ring 输出冒充独立证据。当前未获此证据，不能声称跨实现验收。
4. **ring 0.17.14 安全公告状态未核实**（全程禁网）。缓解：该版本与现有 rustls 0.23.45 生产依赖完全相同，供应链风险已被现有依赖树接受；后续切片联网时一次性核对。
5. **feature 统一可能触发 ring 重编译**：`rsetup-protocol` 以默认 features 直依赖 ring，若与 rustls 的 feature 集不同，ring 会重新编译一次（仍在离线范围内，仅一次性耗时）；本计划不承诺指纹复用。
6. **worktree 有前序任务遗留的 45 处未提交改动**：各任务提交用显式 pathspec 即可隔离；`Cargo.lock` 的形状审计用 `git diff` 精确到文件，不受影响。
7. **语义层问题留给协议 owner**（本切片不改规范）：descriptor 字段长度上限与 `device_sn` 可否为空（O1）、`reason_code` 未被签名覆盖（O2）、REJECTED 时 `server_x25519_eph_pub` 全零占位的语义校验（O4）。
8. **T16 固定种子与消息是 fixture 而非秘密**：记录在测试代码（将被提交的 `tests/ed25519.rs`）与任务报告中；任务报告保持去敏惯例即可。

## 执行交接

计划已保存到 `docs/superpowers/plans/2026-10-05-controller-device-protocol-core.md`。两种执行方式：
1. 子代理驱动（推荐）：使用 superpowers:subagent-driven-development，每任务新代理，任务间两阶段审查；
2. 当前会话执行：使用 superpowers:executing-plans，分批落实并保留审查检查点。
