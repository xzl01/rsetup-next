# 隧道安全协议修订：条件性文档 TDD 实施计划

> **致执行代理：** 使用 `superpowers:executing-plans`，以 `- [ ]` 跟踪步骤。本计划仅授权将来在条件满足后修改文档，不授权立即修改现行线协议、实现或发布配置。

**Goal:** 在独立外部专家书面决策和必要用户再次复审后，小步修订握手 v3、记录错误规范和 controller 文档，保持安全发布阻断。

**Architecture:** 获用户复审的设计文档是拟议方向，`docs/protocol_spec.md` 才是现行线协议规范。先锁定设计 §6 外部决议，再修握手、存真固定向量、修记录/恢复、同步 controller 门禁。握手 `Ver=0x03` 与业务 Protobuf `schema_version=1` 分属不同层。

**Tech Stack:** Markdown、Git、Python 3 标准库；未来 Rust 2024/MSRV 1.85、Ed25519/X25519/HKDF-SHA256/AES-256-GCM、Protobuf/h2c/gRPC 双方实现另须审批审查。

**Spec:** `docs/superpowers/specs/2026-10-03-controller-tunnel-security-revision-design.md` 全文；`docs/protocol_spec.md` §4/5/6.6/7/8；`docs/superpowers/specs/2026-09-23-controller-v1-00-index.md` §6；`docs/superpowers/specs/2026-09-22-controller-design.md` §12；`docs/superpowers/plans/2026-10-02-controller-02-transport-board-tdd.md` Global Constraints/Tasks 2–4/9–10/最终门；`docs/superpowers/plans/2026-10-02-controller-v1-tdd-index.md` G0/G2/G5。

## Global Constraints

- **硬关卡：** 独立外部密码/协议专家须逐项书面决定设计 §6：Client/Server 转录是否签入 `FrameType`；部分帧有界单调读取/发送堵塞 deadline 的数值、起点及缓冲；序号末值/空记录；原始 wire 固定数值向量的独立生成复核及解析器一致性；v2/v3 双端升级、停机、不能自动降级的回滚。保留真实专家身份/日期、原件和理由，不伪造审查。若改变用户已认可的设计 §2.2 转录布局，先修设计并让用户再次审阅取得明确记录。**专家决定未到，不得编辑规范性 `docs/protocol_spec.md`，不得让 02 Task 3/4 进入实现。**
- 本阶段只规划文档变更，不碰代码、业务 03 字段、旧计划无关章节或 `.cargo-home/`。新文本提交仍继续阻断安全发布：双方实现审查、真实独立固定向量、真实两进程负例/互操作/fuzz 与 01/03 联合门未通过，不得称 v3 已实现、兼容或可上线。
- 保留 Magic `53 45`、8B header、FrameType `01..06`、握手 payload ≤16KiB、非 PENDING 10s/PENDING 5s/10s/15s 单调期限；不得借握手时限代替记录部分帧 deadline。§5.1 四个逐字 HKDF info 与 §5.2 Nonce/AAD 布局保留。v3 在同一或新 TCP 都不自动降级 v2，业务 schema_version=1 不变。
- 各任务 **RED → 最小 GREEN → 原样重跑及人工逐字段核对 → 独立提交**。旧文件存在且旧语义缺新合同引发 `AssertionError` 才是有效文档 RED，路径/环境错误不算。文本/字节静态检查不构成密码学证明。

## 文件职责与任务接口

| 精确路径 | 唯一职责及下游 |
| --- | --- |
| `docs/superpowers/reviews/2026-10-03-tunnel-security-expert-decisions.md` | Task 0 仅在真实外部报告到达后索引五项决议/必要用户复审，供 Tasks 1–4。 |
| `docs/protocol_spec.md` | Task 1 改 §4.1–4.6 握手，Task 3 改 §5.2/§6.6/§7–8 记录，§5.1 不改。 |
| `docs/superpowers/vectors/2026-10-03-tunnel-v3-wire-vectors.json` | Task 2 存独立生成复核的真实 raw 帧/descriptor/签名转录/KDF/记录 hex，供未来双端测试。 |
| `docs/superpowers/specs/2026-09-23-controller-v1-00-index.md`、`docs/superpowers/specs/2026-09-22-controller-design.md` | Task 4 同步 §6/§12 的版本理由与持续发布阻断。 |
| `docs/superpowers/plans/2026-10-02-controller-02-transport-board-tdd.md` | Task 4 同步 Global Constraints/Tasks 2–4/9–10/示例/最终门，不提前解 Task 3/4 BLOCKED。 |
| `docs/superpowers/plans/2026-10-02-controller-v1-tdd-index.md` | Task 4 同步 G0/G2/G5；03 业务字段不变不改 03 规格/专题计划。 |

顺序：外部报告/必要用户复审 → Task 0 索引 → Task 1 握手 → Task 2 真向量 → Task 3 记录 → Task 4 controller 同步。专家否定设计转录须先修设计请用户复审，不能私改测试期望。每任务只暂存自己的文件。

### Task 0：独立外部专家书面决策硬关卡

**Files:** Create `docs/superpowers/reviews/2026-10-03-tunnel-security-expert-decisions.md`（仅真实报告到达后）；Read 设计 §2.2/§6。

**Interfaces:** 消费真实专家报告、身份/日期/原件和必要用户复审；产出五项可核验决议/期限数值。

- [ ] **Step 1 RED：** 旧设计仍将问题列为开放项，索引不存在引发明确 `AssertionError`，表示门未满足；无原件即停，不造报告换 GREEN。

```sh
python3 - <<'PY'
from pathlib import Path
s=Path('docs/superpowers/specs/2026-10-03-controller-tunnel-security-revision-design.md').read_text(encoding='utf-8')
assert '## 6. 剩余独立审查决策与发布关卡' in s
assert '以下是**需要批准的开放决策**' in s
p=Path('docs/superpowers/reviews/2026-10-03-tunnel-security-expert-decisions.md')
assert p.is_file(), '独立专家书面决议未到：禁止规范编辑或 02 Task 3/4 实现'
PY
```

- [ ] **Step 2 GREEN（严格条件性）：** 真实报告到达后逐项索引结论/理由/专家身份日期/原件：双端 FrameType 具体转录字节；部分记录帧读取/发送堵塞**具体**单调时限数值/起算点/缓冲背压；`0..2^64-2`/末序号与 16B 零明文；raw wire 向量独立生成/复核人员及跨 Protobuf 库对原始嵌套字段/缺省/重复/未知/非最短 varint 一致性；双端升级顺序、停机窗口、无降级回滚。缺时限数值则停，不留占位；更改 §2.2 先请用户再次复审修改后的设计。
- [ ] **Step 3 验证：** 人工逐项对照原件、专家身份与必要用户复审；下列仅查格式，不证明专家真实性：

```sh
python3 - <<'PY'
from pathlib import Path
s=Path('docs/superpowers/reviews/2026-10-03-tunnel-security-expert-decisions.md').read_text(encoding='utf-8')
for x in ('专家','日期','原件','FrameType','单调','deadline','缓冲','序号','零明文','wire','解析器','升级','回滚','复审'):
    assert x in s,x
PY
git diff --check
```

- [ ] **Step 4 独立提交（仅真实审查后）：**

```sh
git add docs/superpowers/reviews/2026-10-03-tunnel-security-expert-decisions.md
git commit -m 'docs(protocol): record independent tunnel security decisions'
```

### Task 1：小提交修订规范 §4.1–4.6：显式握手 v3

**Files:** Modify `docs/protocol_spec.md` §4.1–4.6；Read Task 0 和设计 §2.1–2.3。

**Interfaces:** 输入专家 FrameType 决议与已获用户认可的转录，输出规范性 v3 原始 wire/签名/状态供 Task 2 向量与 Task 4 测试。专家若改变设计 §2.2，先用户再次复审再同步下述断言，不擅签 FrameType。

- [ ] **Step 1 RED：** 旧 §4 存在可读，但新合同缺失触发 `AssertionError`；非环境错误。

```sh
python3 - <<'PY'
from pathlib import Path
s=Path('docs/protocol_spec.md').read_text(encoding='utf-8')
h=s[s.index('## 4. 安全握手'):s.index('## 5. 传输加密')]
for x in ('Ver (0x03)','v3 首帧遇 0x02 即断开且不自动降级',
 '严格拒绝未知字段、已知字段重复、非最短 varint、错误 wire type 与非法 UTF-8',
 'BE32(descriptor_raw_len) || descriptor_raw','status(1B) || BE32(reason_code)',
 'APPROVED(status=0) 必须 reason_code=0','REJECTED(status=1) 的临时公钥为 32B 全零',
 '客户端签名失败直接关闭 TCP，不发送 REJECTED(SIGNATURE_INVALID)',
 '服务端签名失败直接关闭 TCP，不依据未认证 reason_code 停止重连'):
    assert x in h,f'§4 缺少 {x}'
PY
```

- [ ] **Step 2 GREEN：** §4.1 帧 `53 45 || 03 || FrameType || BE32(payload_len)`，每帧查 v3，v2/v1/混合帧立即断开，同一及新 TCP 不自动回退；保留六帧/16KiB/严格时序及 10s/5s/10s/15s 单调期限。§4.2 对 `ClientHello`、`ServerChallenge`、`ClientAuthRequest`、嵌套 `DeviceDescriptor`、`ServerAuthResponse` 和挂起消息从**接收到的原始帧**审计：未知字段、已知字段重复、非最短 key/length/integer varint、错误 wire type、非法 UTF-8/枚举/长度、未消费字节均断开；必要随机数/公钥 32B、签名 64B、descriptor 各恰一次，缺失已知 proto3 scalar 按零再查组合。`descriptor_raw` 只取请求 `device_descriptor` length-delimited 嵌套消息**内容原字节**，不含外层 key/长度 varint/帧头；BE32 长度等于提取字节数；客户端只编码一次并保留，服务端审计/提取/验签/语义解析使用同一原字节，不重编码。§4.3 按设计 §2.2 逐字写（ASCII 域尾单个 0x00 后单字节 0x03，除 BE32 无隐含前缀）：

```text
ClientSignInput = ASCII("rsetup-next/client-auth/v3") || 0x00 || 0x03
  || client_random(32) || server_random(32)
  || client_ed25519_pub(32) || client_x25519_eph_pub(32)
  || BE32(descriptor_raw_len) || descriptor_raw
ServerSignInput = ASCII("rsetup-next/server-auth/v3") || 0x00 || 0x03
  || server_random(32) || client_random(32)
  || client_ed25519_pub(32) || client_x25519_eph_pub(32)
  || BE32(descriptor_raw_len) || descriptor_raw
  || server_x25519_eph_pub(32) || status(1B) || BE32(reason_code)
```

`status` 恰单字节 0/1，`reason_code` 是语义 uint32 四字节 BE 而非 Protobuf wire varint。APPROVED `status=0,reason=0,有效32B eph` 且 §4.4 X25519 共享密钥非零；REJECTED `status=1,32B 全零 eph` 只可 `0=UNSPECIFIED,2=REVOKED,3=APPROVAL_DENIED,4=HANDSHAKE_TIMEOUT（适用握手步骤）,5=SERVER_ERROR`；枚举 1 SIGNATURE_INVALID 保留但 v3 不发送。§4.5 删除旧“坏签名→REJECTED(1)”图：坏客户端签名关 TCP 无拒绝帧/永久决定；坏中控签名板端关 TCP、不信原因、不置永久未授权；仅在完整已验签上下文且适用时才发已签拒绝。§4.6 只有可信签名且合法组合的 2/3 停自动重连，资源过载用 5 可重试；权威准入/连接代际复查、双方认证和密钥协商成功后才同 socket h2c。Step 1 句放真实规范段。
- [ ] **Step 3 验证：** 原样重跑 Step 1；人工逐字段对照设计 §2.1–2.3 宽度/顺序/状态/§4 时序；`git diff --check && git diff -- docs/protocol_spec.md`，本提交只改 §4、不改业务 v1。文本绿不是验签通过。
- [ ] **Step 4 独立提交：**

```sh
git add docs/protocol_spec.md
git commit -m 'docs(protocol): specify reviewed v3 handshake'
```

### Task 2：独立真实固定数值字节向量

**Files:** Create `docs/superpowers/vectors/2026-10-03-tunnel-v3-wire-vectors.json`；Read Task 0、规范 §4–5/§8。

**Interfaces:** 消费不同人员独立生成/复核的真实完整 hex 与错误分类；输出 raw frame/descriptor、双转录/签名、X25519/HKDF、双向记录供未来双方测试；不能用同一被测实现现场算自己的期望，报告不足即停，不造值。

- [ ] **Step 1 RED：** 先确认旧规范存在，缺向量引发明确 `AssertionError`，非 `FileNotFoundError`。

```sh
python3 - <<'PY'
from pathlib import Path
assert Path('docs/protocol_spec.md').is_file()
p=Path('docs/superpowers/vectors/2026-10-03-tunnel-v3-wire-vectors.json')
assert p.is_file(), '独立固定数值向量尚未提交'
PY
```

- [ ] **Step 2 GREEN：** JSON 顶层 `source.generator_report`/`source.independent_review_report` 为不同人员的真实可核原件；`handshake.client_auth_request_payload,descriptor_raw,client_random,server_random,client_ed25519_pub,client_x25519_eph_pub,server_x25519_eph_pub,client_sign_input,server_sign_input,client_signature,server_signature` 是完整小写 hex，`handshake.status` 为单字节整数、`handshake.reason_code` 为 uint32 整数，另外包含真实批准/合法拒绝原始帧；`kdf.shared_secret,salt,prk,c2s_key,s2c_key,c2s_iv,s2c_iv`；`records[]` 的 `direction,seq,length,nonce,aad,ciphertext_tag` 含 C2S/S2C 各 seq 0/1/获批末值和长度 16/65536；`cases[]` 的 `name,kind,wire_hex,expected`（kind=handshake/record，expected=明确成功或错误分类）。至少合法 APPROVED/REVOKED/APPROVAL_DENIED/SERVER_ERROR，修改已签 reason/status/raw descriptor/长期公钥，双向坏签名、v1/v2/混合、重复/未知/非最短/非法 UTF-8/帧序/截断、坏 tag/AAD/重放/错序号/耗尽/length 15/65537/部分帧期限/部分写。原件不齐即停，不放示意 hex、假签名或假 tag。
- [ ] **Step 3 验证：** 下述 Python 只检结构/长度/AAD，**不验证签名、HKDF、tag 或 protobuf 安全**；独立复核者还须逐字比对原请求嵌套字节、双域/BE32长度/status/reason、真实原始帧及不同解析器接受/拒绝一致性。

```sh
python3 - <<'PY'
import json,struct
from pathlib import Path
v=json.loads(Path('docs/superpowers/vectors/2026-10-03-tunnel-v3-wire-vectors.json').read_text(encoding='utf-8'))
def raw(x):
    assert isinstance(x,str) and len(x)%2==0 and x==x.lower()
    return bytes.fromhex(x)
h=v['handshake'];k=v['kdf']
for x in ('client_auth_request_payload','descriptor_raw','client_sign_input','server_sign_input'):
    assert raw(h[x]),x
assert len(raw(h['client_signature']))==len(raw(h['server_signature']))==64
D=raw(h['descriptor_raw'])
for key in ('client_random','server_random','client_ed25519_pub','client_x25519_eph_pub','server_x25519_eph_pub'):
    assert len(raw(h[key]))==32,key
assert h['status'] in (0,1) and 0<=h['reason_code']<=2**32-1
base=struct.pack('>I',len(D))+D
client=(b'rsetup-next/client-auth/v3\x00\x03'+raw(h['client_random'])+raw(h['server_random'])
        +raw(h['client_ed25519_pub'])+raw(h['client_x25519_eph_pub'])+base)
server=(b'rsetup-next/server-auth/v3\x00\x03'+raw(h['server_random'])+raw(h['client_random'])
        +raw(h['client_ed25519_pub'])+raw(h['client_x25519_eph_pub'])+base
        +raw(h['server_x25519_eph_pub'])+bytes([h['status']])+struct.pack('>I',h['reason_code']))
assert raw(h['client_sign_input'])==client and raw(h['server_sign_input'])==server
for x,n in (('shared_secret',32),('salt',64),('prk',32),('c2s_key',32),('s2c_key',32),('c2s_iv',12),('s2c_iv',12)):
    assert len(raw(k[x]))==n,x
assert v['source']['generator_report'] and v['source']['independent_review_report']
assert v['source']['generator_report']!=v['source']['independent_review_report']
assert len(v['cases'])>=14
assert {'approved','revoked','approval_denied','server_error'}<={c['name'] for c in v['cases']}
for c in v['cases']:
    w=raw(c['wire_hex']);assert c['expected'] and c['kind'] in ('handshake','record')
    if c['kind']=='handshake': assert w[:2]==b'SE'
for r in v['records']:
    assert r['direction'] in ('c2s','s2c') and 0<=r['seq']<=2**64-2
    assert r['length']==len(raw(r['ciphertext_tag'])) and 16<=r['length']<=65536
    assert len(raw(r['nonce']))==12
    assert raw(r['aad'])==struct.pack('>IQ',r['length'],r['seq'])
PY
git diff --check
```

- [ ] **Step 4 独立提交：**

```sh
git add docs/superpowers/vectors/2026-10-03-tunnel-v3-wire-vectors.json
git commit -m 'docs(protocol): add independently reviewed wire vectors'
```

### Task 3：小提交修订 §5.2/§6.6/§7–8：记录致命错误

**Files:** Modify `docs/protocol_spec.md` §5.2/§6.6/§7–8；Read Task 0 的**具体**期限/序号/零记录决议、Task 2 真向量、设计 §3–5。

**Interfaces:** 输入专家审定 deadline 数值/起算点/缓冲预算；输出整 TCP 致命错误/重连新密钥/双方故障负例供 Task 4。不能仅写“有界”而省略数值。

- [ ] **Step 1 RED：** 旧 §5.2 缺下界及关闭合同，以下 `AssertionError` 因旧内容而非环境问题发生。

```sh
python3 - <<'PY'
from pathlib import Path
s=Path('docs/protocol_spec.md').read_text(encoding='utf-8')
r=s[s.index('### 5.2 '):s.index('## 6. ',s.index('### 5.2 '))]
f=s[s.index('### 6.6 '):s.index('### 6.7 ')]
a=s[s.index('## 7. '):]
for x in ('16 ≤ Record Length ≤ 65536（含 16B tag）','记录部分帧有界读取 deadline',
 '发送部分写入即关闭整条 TCP','错误 tag 或 AAD 即关闭整条 TCP',
 '失败记录不交付明文、不增加 RcvSeq，且不在原连接重同步','序号 0..2^64-2'):
    assert x in r,f'§5.2 缺少 {x}'
assert '记录层致命错误关闭整个 TCP 并结清当前连接代际所有流的未决请求' in f
for x in ('新 TCP、新完整 v3 握手、新随机数和新临时密钥',
 '不自动重放可能已执行的设备变更或重启','双端固定数值向量','两进程负向互操作','fuzz'):
    assert x in a,f'§7–8 缺少 {x}'
PY
```

- [ ] **Step 2 GREEN：** §5.2 写 `16 ≤ Record Length ≤ 65536（含 16B tag）`，16B 零明文细节依专家决定；先校验长度和缓冲预算再分配，写入专家审定的**具体**部分帧读/写堵塞单调 deadline 数值、起算点和缓冲背压上限（不得借用握手 10s 或悄悄省略）。长度 15/65537、长度字段/体截断或超时、坏 tag/AAD、期望序号耗尽、发送部分写/整记录写出不确定，均立即关闭**整条 TCP**、失效双向 key/IV/seq/缓冲/h2c 全流；整记录认证前无明文交付，失败不增 RcvSeq、不在原连接继续读取、补发或扫描帧头重同步。每方向串行加密和发送，整记录成功写出才推进 TxSeq，部分写不复用 key/nonce；按专家末值 checked-add（设计拟 `0..2^64-2`，不使用 `2^64-1`），耗尽关闭、不回绕。保留 §5.1 SharedSecret/salt/四个逐字 HKDF info/32/32/12/12B 与 §5.2 Nonce `IV_Base[0:4]||(IV_Base[4:12] XOR BE64(SeqID))`、AAD `BE32(Record Length)||BE64(SeqID)`。§6.6 记录错误高于单流 RST，结清当前连接代际**所有流** pending。§7 保留 `1s,2s,4s,…≤60s + [0,1s)` 抖动后新 TCP/完整 v3/新随机数与 eph，旧 key/seq/PENDING token 不复用；变更/重启可能已生效不自动重放，查持久结果。§8 保留旧双流/审批/容量/时间测试，改 v3 拒绝 v2/v1/混合无回退，加独立真固定向量双端对照、原始解析器差异、双向坏签名/改原因、16/65536 与 15/65537 边界、截断/deadline/部分写/tag/AAD/重放/乱序/耗尽/零明文交付、真实两进程负例与 fuzz，观察 TCP 关闭及新密钥。Step 1 句落实际章节。
- [ ] **Step 3 验证：** 原样重跑 Step 1；人工查专家实际时限**数值/起点/预算**、旧 §8 测试保留、§4/§5.1 不倒退；`git diff --check && git diff -- docs/protocol_spec.md`。文本绿不是 AEAD 实测。
- [ ] **Step 4 独立提交：**

```sh
git add docs/protocol_spec.md
git commit -m 'docs(protocol): make record errors connection fatal'
```

### Task 4：同步 controller 00/设计/02/G0/G2/G5

**Files:** Modify `docs/superpowers/specs/2026-09-23-controller-v1-00-index.md` §6、`docs/superpowers/specs/2026-09-22-controller-design.md` §12、`docs/superpowers/plans/2026-10-02-controller-02-transport-board-tdd.md` Global Constraints/Tasks 2–4/9–10/矩阵/最终门、`docs/superpowers/plans/2026-10-02-controller-v1-tdd-index.md` G0/G2/G5；Read 03 业务规格但不修改。

**Interfaces:** 消费 Tasks 0–3 审查、规范、真向量；输出握手/业务分层与**持续**的双方实现/发布阻断。

- [ ] **Step 1 RED：** 旧 02 `SE/v2/BE长度` 与含混 `v1不得自动降级v2` 不能满足新合同；四文件存在可读。

```sh
python3 - <<'PY'
from pathlib import Path
r=Path('docs/superpowers')
f={'base':r/'specs/2026-09-23-controller-v1-00-index.md',
 'design':r/'specs/2026-09-22-controller-design.md',
 'p02':r/'plans/2026-10-02-controller-02-transport-board-tdd.md',
 'index':r/'plans/2026-10-02-controller-v1-tdd-index.md'}
s={k:p.read_text(encoding='utf-8') for k,p in f.items()}
assert '握手 Ver=0x03；业务 schema_version=1' in s['p02']
assert 'SE/v2/BE长度' not in s['p02'] and 'v1不得自动降级v2' not in s['p02']
for k in ('base','design'): assert '握手 v3 文档提交不解除生产安全阻断' in s[k]
for k in ('G0 ', 'G2 ', 'G5 '): assert k in s['index']
assert '双端安全审查及两进程负向互操作完成前不得发布' in s['index']
PY
```

- [ ] **Step 2 GREEN：** 00 §6/设计 §12 只说明规范文本消除 reason 未签、验签失败动作冲突、坏 tag 后策略不明三个**文本**缺口；双端审核/测试和管理 HTTP 风险仍在。02 Global Constraints 把 `v1不得自动降级v2` 拆为“握手 v3 不得自动降级握手 v2（含新 TCP）”及“业务 schema_version=1 不接受未知业务版本”；Task 2 `SE/v2/BE长度` 改 `SE/v3/BE长度`，补严格 raw wire 与 `v1_hello()`/`v2_hello()` 混合负例，PENDING/CAS 不变。Task 3 以 Task 2 已独立复核的真实向量替代“reason 布局未定”和暂定自造向量，测双域/BE32(reason)/合法组合、坏客户端签名不发 `REJECTED(SIGNATURE_INVALID)`、坏中控签名不误停重连/不启 h2c；仍 **BLOCKED** 至专家/必要用户复审、密码库与双方实现审查/真向量门。Task 4 补下界、认证前无明文、截断/deadline/tag/序号/部分写整 TCP 销毁，旧仅返回 `RecordError` 的伪码不能让连接继续；仍 **BLOCKED** 至双方审核/负例。Task 9 跨 crate 互通不充当 Task 10 两独立进程坏签名/tag/截断/错序号网络证据；Task 10 仍依赖 01/03 DB/NTP/恢复。02 矩阵/最终门及总 G0/G2/G5 分开专家/用户必要审阅、文档合同、真向量、双方实现/负例/互操作和生产联合门，不因文档提交解阻断。03 业务字段未变不改 03 规格/专题计划。Step 1 句置于相应正文。
- [ ] **Step 3 验证：** 原样重跑 Step 1；人工核对 02 Global Constraints/Tasks 2–4/9–10/示例/最终门、总 G0/G2/G5、00 §6 与设计 §12；运行下方跨文档检查和 `git diff --check`，业务 v1 不变。
- [ ] **Step 4 独立提交：**

```sh
git add docs/superpowers/specs/2026-09-23-controller-v1-00-index.md docs/superpowers/specs/2026-09-22-controller-design.md docs/superpowers/plans/2026-10-02-controller-02-transport-board-tdd.md docs/superpowers/plans/2026-10-02-controller-v1-tdd-index.md
git commit -m 'docs(controller): align v3 transport safety gates'
```

## 四层验收（不可互相替代）

- [ ] **文件静态一致性：** 每任务 Python RED 因旧语义缺新要求失败，GREEN 后退出 0，人工逐字段核对字节/期限/跨文档接口及 `git diff --check`；不是密码学证明。
- [ ] **独立专家审核：** 核查真实专家身份/原件、五组决议与必要用户再次复审；缺失不得编辑规范。
- [ ] **双方实现/真实两进程负向测试：** 未来另获授权，板端/中控分别审核密码库与实现、真向量双端逐字对照；真实独立进程 loopback 注入坏签名/tag/长度/截断/deadline/错序号/部分写及 fuzz，观察 TCP 关闭和零明文泄露；库内 fake 不算网络证据。
- [ ] **生产发布：** 双端证据与 01/03 DB/NTP/任务恢复、G5 联合门全通过方可评估；新文本不解阻断，不用 v2 自动降级当回滚。

未来所有条件性文档任务完成后才运行（当前作者不声称专家、向量、实现或实测已完成）：

```sh
python3 - <<'PY'
from pathlib import Path
p=Path('docs/protocol_spec.md').read_text(encoding='utf-8')
a=Path('docs/superpowers/plans/2026-10-02-controller-02-transport-board-tdd.md').read_text(encoding='utf-8')
i=Path('docs/superpowers/plans/2026-10-02-controller-v1-tdd-index.md').read_text(encoding='utf-8')
b=Path('docs/superpowers/specs/2026-09-23-controller-v1-00-index.md').read_text(encoding='utf-8')
d=Path('docs/superpowers/specs/2026-09-22-controller-design.md').read_text(encoding='utf-8')
s=Path('docs/superpowers/specs/2026-09-23-controller-v1-03-device-protocol.md').read_text(encoding='utf-8')
assert 'Ver (0x03)' in p and 'BE32(reason_code)' in p and '16 ≤ Record Length ≤ 65536' in p
assert '两进程负向互操作' in p
assert 'SE/v2/BE长度' not in a and 'v1不得自动降级v2' not in a
assert '握手 Ver=0x03；业务 schema_version=1' in a
assert 'schema_version必须等于1' in s and 'schema_version=1' in a
assert '握手 v3 文档提交不解除生产安全阻断' in b
assert '握手 v3 文档提交不解除生产安全阻断' in d
assert '双端安全审查及两进程负向互操作完成前不得发布' in i
PY
git diff --check
git status --short
```

人工再逐项对照设计 §2–6 覆盖、占位词 `TBD`/`TODO`、未定义接口/时限、前后合同。旧 `Ver=0x02` 的**规范性**句须改，明确仅为历史背景的旧版本描述可保留。发现冲突停在相应任务重新 RED→GREEN，不带冲突进入实现。
