# rsetup-tunnel 解析器压力验证报告：Envelope + gRPC（缩小范围轮次）

- 日期：2026-10-23（Asia/Shanghai）
- 范围：仅 `envelope` 与 `grpc` 两个解析路径的结构化变异 / 随机噪声 / 分片粘包 roundtrip 压力。
  frame、handshake、record 等其他路径本轮未做。
- 交付物：新建 `crates/rsetup-tunnel/tests/parser_stress.rs`（本报告的测试）与本报告；
  未修改任何现有源文件、既有测试、Cargo.toml / Cargo.lock。
- 与上一轮的关系：上一代理上下文耗尽，仅跑基线编译、未创建任何交付物，本轮为全新独立实现，
  未复用其产物。

## 1. 运行命令与环境

```bash
cd /home/aghost/workspace-ai/rsetup-next
CARGO_TARGET_DIR=target/tunnel-parser-stress \
  cargo test -p rsetup-tunnel --test parser_stress --locked --offline
```

- `--locked --offline`：无依赖变更、无网络；无任何新增依赖，无 nightly、无 fuzz 工具安装。
- 隔离 target 目录 `target/tunnel-parser-stress`，不与其他 writer 的构建产物互踩。
- 结果：`test result: ok. 6 passed; 0 failed; 0 ignored`（并行与 `--test-threads=1` 两种模式均通过）。

## 2. 确定性 PRNG 与固定 seed

- 测试内常驻 splitmix64（仅测试用确定性 PRNG，非生产 CSPRNG），主 seed：
  `SEED = 0x5EED_2026_1023_0001`。
- 各用例组子 seed 由主 seed 异或常量派生，完全确定：结构化变异 `SEED`；信封噪声 `SEED^0xA2`；
  gRPC 分片 roundtrip `SEED^0xB1`；gRPC 噪声流 `SEED^0xB2`；EOF 边界消息 `SEED^0xE0F`；
  action/metadata/ping/System 边界用例不使用随机（固定值构造）。
- 同一命令重复运行字节级可复现（本轮多次运行 decode_ok 计数完全一致：1537 与 4）。

## 3. 用例构成与结果（正式数字取 `--test-threads=1 -- --nocapture`）

| 测试 | 类别 | cases | 输入字节 | 单测耗时 | 结果 |
|---|---|---:|---:|---:|---|
| `envelope_structured_mutation` | 信封：合法 wire 单字节翻转/截断/追加/换位 + 10% 语义非法信元 encode 守卫 | 7 000 | 688 118 | 24 ms | ok（decode 成功 1 537 例，均过强校验） |
| `envelope_random_noise` | 信封：纯随机/结构相似噪声，含空输入 | 3 000 | 1 894 777 | 19 ms | ok（decode 成功 4 例，均过强校验） |
| `grpc_framing_roundtrip` | gRPC：合法 packets（payload 0..=300，生成器将传入上限 512 收窄为 300）拼接后按 1..64B 随机分片/粘包喂入，逐包严格等价 | 10 000 | 2 561 108 | 98 ms | ok |
| `grpc_noise_streams_and_poison` | gRPC：随机字节流 + poisoned 粘滞 / finish 合同 | 250 | 234 876 | 2 ms | ok |
| `grpc_length_prefix_boundaries` | 显式边界（下节） | 8 | 1 102 | 0 ms | ok |
| `envelope_limit_and_routing_boundaries` | 显式边界（下节） | 13 | 1 457 | 0 ms | ok |
| **合计** | | **20 271** | **5 381 438（≈5.13 MiB）** | 单测合计 143 ms | 全部通过 |

- 套件 wall time：writer 单线程整轮 0.15 s（`time` 含 harness 启动 0.20 s；未优化 debug profile）。
- 上表 **5 381 438 B 是 `note_input` 统计口径，不是 parser 全部调用的精确总字节**：
  B2 在 poisoned 后再喂合法消息的粘滞探针未纳入（复审估计约 10–20KB），边界项使用场景
  汇总公式计数。20 271 cases 中随机组按独立输入/多包流计数，额外 21 项按断言场景计数，
  不等于 20 271 次 parser 调用或消息数。
- 最大单输入：实测 4 096 B（由信封噪声组决定）；gRPC roundtrip 的分片 ≤64B，噪声流
  构造为 ≤2048B 噪声加可选合法前缀消息，故其上限不是严格 2048B。显式大长度场景只喂
  长度头或 5B+1024B filler，不代表实际喂入过 512KiB 完整消息。
- 主口径 10 000 信封变异/噪声 + 10 000 gRPC roundtrip 满足“至少 20000 case”；
  271 为额外噪声流与显式边界用例。
- 说明：并行模式下各测试打印的 `input_bytes` 因全局原子计数器交错会偏大，不作为口径；
  上表为单线程精确值。

## 4. 显式边界用例（均为断言实际合同，非随机）

Envelope：
- action 长度 64：encode→decode 严格等价；65：encode 拒绝（`ActionTooLarge`）。
- metadata 16 项 encode→decode 严格等价；17 项 encode 拒绝（`MetadataTooManyEntries`）；
  wire 级在 16 项合法编码后手工追加第 17 个 field-7 entry → `decode` 拒绝。
- `tunnel.ping`：Request/Response + payload==16 + metadata 空合法；payload 15/17、metadata 非空、
  Event 类型、Response 非零 status 均 `validate_fields` 拒绝。
- System（tunnel.kick）：`validate(Control)` Ok；`validate(Data)` 拒绝（System 数据流单独明确测试）；
  普通 Request 在 Data 流合法。

gRPC：
- 长度前缀 512KiB+1 → `EnvelopeTooLarge(524289)` 且 poisoned；1MiB+1 → `MessageTooLarge`；
  1MiB 整 → `EnvelopeTooLarge`（两道上限顺序与文档一致）。
- flag=1 → `CompressionNotSupported` 且粘滞：随后再喂合法前缀仍 `DecoderPoisoned`。
- 512KiB 整前缀（合法上限）：先喂 5B 头再喂 1024B，`buffered_len()==1029`、无错误。
- EOF 截断（完整消息少最后 3 字节）：`next_packet` 返回 `Ok(None)`；`finish()` →
  `TruncatedMessage` 并进入 poisoned、缓冲清零（实现中 `finish` 在非空缓冲上即 poison，
  测试按真实合同断言，未擅造要求）。

## 5. 校验不变量（合法路径强等价，随机路径不强制 Err）

- 随机变异/噪声得到 `Err` 视为合法结果，不判 bug；`decode` 成功则必须：
  `validate_fields` 通过、action/metadata/payload 在预算内、canonical `encode→decode` 语义等价
  （`decode(encode(p)) == p`），随机 4 组共 1 541 例 decode 成功全部满足。
- 合法 packet 经 gRPC 分片/粘包（1..64B 随机 split）后逐包与源 packet `assert_eq!` 严格相等
  （含负数 status_code、i32::MIN/MAX 的 10 字节 varint roundtrip）。
- `InputCursor` 进度不变量：每次循环迭代要么产出包（`consumed` 严格增加）、要么以
  `remaining==0` 结束；“产 `Ok(None)` 但输入未耗尽”即断言失败（防停滞/防 hang 的 guard 上限
  `2*len+64` 全程未触发）。
- `buffered_len()` 全程 ≤ `5 + 512KiB`（对逻辑 len 断言，未对 Vec capacity 作任何声称）。
- fatal 后粘滞：poisoned 解码器再喂**完全合法**消息必须 `Err(DecoderPoisoned)` 且不消费输入
  （`consumed()==0`），250 例噪声流 + 边界用例全部满足。

## 6. 发现的缺陷

未发现实现缺陷。所有失败尝试均为本轮测试自身问题并已修正（非实现 bug，如实记录）：
1. 测试初版在 `finish()` 之后读取 `buffered_len()` 判断截断分支——`finish` 按合同会清空缓冲，
   改为调用前捕获；这是测试时序错误，实现行为与文档一致。
2. 语义非法信元生成器的一个分支可能偶然产出合法信元，已改为必然非法。

## 7. 质量门禁与越权检查

- `rustfmt --edition 2024 --check crates/rsetup-tunnel/tests/parser_stress.rs` 通过（仅格式化该文件，
  未跑 `cargo fmt --all`，在途文件零触碰）。
- clippy：直接对测试目标执行 `-D warnings` 会先在 lib 的**在途他人代码**
  （`src/session/driver.rs` 的 `collapsible_if`、lib 的 `large_enum_variant`，共 3 处，非本轮引入，
  本轮不修改）报错中断。临时 `-A clippy::collapsible_if -A clippy::large_enum_variant` 后整条链
  （含 `parser_stress` 测试目标）在 `-D warnings` 下 0 诊断退出码 0，即本测试文件自身无任何 clippy/
  rustc 警告。
- 普通 `cargo test` 编译本测试目标 0 警告。
- writer 声明本轮仅新增测试与本报告，未 commit/push/clean，未访问 secret/、DB、真实服务。
  注意 `git status` 可能只显示整个新 crate 未跟踪，**不能仅凭目录级状态证明本轮只改两个
  文件**；该范围结论来自任务交付及内容核对，而非目录状态本身。

## 8. 诚实边界

- 这是**固定 seed 确定性随机/变异压力测试**，不是 coverage-guided fuzz，无双进程/真实网络/
  容量压测语义；随机流中的合法 envelope 命中依赖运气，覆盖率未度量。
- 本轮未覆盖：frame、handshake、record、session 路径的随机压力。本轮调用 `next_packet`
  生产增量入口，**没有执行** `push`、`next_packets`、`fill_batch` 或 `PushOutcome` 包装层；
  共享解码核心得到覆盖不等于这些包装层已测。64 包预算与 rest 合同仍由既有
  `grpc_memory_budget.rs` 测试负责。
- 测试文件 668 行，略超 250–400 行目标区间；主要成因是 rustfmt 对 Rust 块/结构体字面量的强制
  多行格式与 6 个测试函数 + PRNG + 构造器的固有体量；未扩功能范围。
- debug profile 下全套件 ~0.15 s 量级，性能数字无基准意义，仅记录耗时事实。

## 9. 独立复审（e43fcfa7）

**Spec PASS / Quality PASS**，未发现生产实现缺陷。复审者只读交付，真实编译当前源码、
串行两轮六项测试通过，计数和生成输入一致，格式检查通过。payload 实际范围、包装层未覆盖、
噪声流长度、字节计数与 case 口径等文案已按复审纠正；没有为通过审查修改生产代码。

主代理也独立串行复跑六项通过；这些结果都不是 coverage-guided fuzz 或容量/双进程证据。
复审期间 session writer 在途改动曾导致编译错误，故 reviewer 未独立验证报告的并行模式与
clippy 声明；工作区最终门禁仍待所有在途修改收敛后统一执行。

本轮确定性压力验证及其独立审查完成，整体生产隧道和 Task7 全部验收仍未完成。
