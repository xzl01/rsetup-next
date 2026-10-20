# Controller V1 集成验收报告

Spec: 06 §6

> 状态：初始化模板（2026-10-18）。本报告按行合同
> `| ID | status | executed_at | layer | command_or_steps | evidence |`
> 登记中控 V1 集成验收事实；`python3 scripts/check_controller_acceptance.py docs/testing/controller-v1-acceptance.md`
> 校验通过仅证明格式/完整性（ID 集合、重复、越界、合法状态、PASS 证据、Spec 引用），
> 不证明外部证据真实，也不使任何条目 PASS。

## 判定与填写规则

- 状态枚举仅四种：`PASS` / `FAIL` / `未验证` / `BLOCKED`；未运行项默认“未验证”。
- `PASS` 必须填写 `executed_at`、`layer`、`command_or_steps`、`evidence` 四个字段且非空；
  时间、层级、命令与证据路径一律去敏，不写入凭据、真实地址、原始恶意样本或未知原行内容。
- 证据分层：① 纯逻辑/组件 RED→GREEN；② 进程内 HTTP 权限矩阵、文件持久化故障注入、模拟网络/时钟；
  ③ 双引擎真实 SQLx 与真实同源双进程 smoke（真库门禁项逐引擎串行单 case）；
  ④ 专家书面决议 + 用户复审 + 获批 codec 后的真实 wire/浏览器端到端。低层证据不得冒充高层证据。
- 实质性门禁（专家书面决议、用户复审、获批 codec、真实库串行预检与真实备份确认）未齐时保持
  `BLOCKED`，不得以计划、接口存在或离线回归推断 `PASS`；不编造缺失 fixtures 或伪 PASS。
- G0/G1/G2/G3/G4/G5 分开判定；本表 12 项 T4 全过也不推定四线 execution/真实门通过；
  完整 V1 发布门禁（独立安全审查、双端负例/向量/互操作、真实 DB 迁移和备份恢复、板端持久
  journal 证据）任一缺失，发布栏维持 BLOCKED。

## 验收账本

| ID | status | executed_at | layer | command_or_steps | evidence |
| --- | --- | --- | --- | --- | --- |
| T4-01 | PASS | 2026-10-18T16:40:00+08:00 | cargo-test | cargo test -p rsetup-controller --test production_composition | tests/production_composition.rs |
| T4-02 | PASS | 2026-10-18T16:40:00+08:00 | cargo-test | cargo test -p rsetup-controller --lib tasks::http | src/tasks/http.rs |
| T4-03 | PASS | 2026-10-18T16:40:00+08:00 | cargo-test | cargo test -p rsetup-controller --test management_gateway_http | tests/management_gateway_http.rs |
| T4-04 | PASS | 2026-10-18T16:40:00+08:00 | cargo-test | cargo test -p rsetup-controller --lib tasks::adapter | src/tasks/adapter.rs |
| T4-05 | PASS | 2026-10-18T16:40:00+08:00 | cargo-test | cargo test -p rsetup-controller --lib tasks::http::decision_tests | src/tasks/http.rs |
| T4-06 | PASS | 2026-10-18T16:50:00+08:00 | cargo-test | cargo test -p rsetup-controller --lib http_device_status | src/http_device_status.rs |
| T4-07 | PASS | 2026-10-18T16:40:00+08:00 | cargo-test | cargo test -p rsetup-controller --lib tasks::recovery | src/tasks/recovery.rs |
| T4-08 | PASS | 2026-10-20T19:09:00+08:00 | cargo-test | cargo test -p rsetup-board-agent --test wire_04_execution | crates/rsetup-board-agent/tests/wire_04_execution.rs |
| T4-09 | BLOCKED | | | | |
| T4-10 | PASS | 2026-10-19T02:20:00+08:00 | headless-browser | python3 scripts/headless_e2e_verify.py | /tmp/headless_login.png & scripts/headless_e2e_verify.py |
| T4-11 | PASS | 2026-10-18T16:30:00+08:00 | cargo-test | cargo test -p rsetup-controller --test web_assets | tests/web_assets.rs |
| T4-12 | PASS | 2026-10-18T16:35:00+08:00 | real-db | cargo test -p rsetup-controller --test migration_v4 | tests/migration_v4.rs (MySQL 8.0 & TiDB v7.5) |
| AT-01 | 未验证 | | | | |
| AT-02 | 未验证 | | | | |
| AT-03 | 未验证 | | | | |
| AT-04 | 未验证 | | | | |
| AT-05 | 未验证 | | | | |
| AT-06 | 未验证 | | | | |
| AT-07 | 未验证 | | | | |
| AT-08 | 未验证 | | | | |
| AT-09 | 未验证 | | | | |
| AT-10 | 未验证 | | | | |
| AT-11 | 未验证 | | | | |
| AT-12 | 未验证 | | | | |
| AT-13 | 未验证 | | | | |
| AT-14 | 未验证 | | | | |
| AT-15 | 未验证 | | | | |
| AT-16 | 未验证 | | | | |
| AT-17 | 未验证 | | | | |
| AT-18 | 未验证 | | | | |

## 条目说明（非账本，不构成状态）

当前 Task 4 收敛验收项（映射 runbook 表 1–12，不改变验收要求/门禁）：

- T4-01 同源生产装配：`lib/main` 唯一独立集成 owner，四线只提供子 router
  （auth ⊕ management ⊕ tasks，路径集互斥，不重复注册 `/healthz`、`/readyz`）；
  真实同源 smoke 不以“非 404”判通过。
- T4-02 共享 HTTP 边界与强制改密应用于 9 条 Runtime 业务路由（单值 Host、严格 Cookie、权威会话、
  Origin/CSRF、1 MiB、限速）；认证后 `must_change_password` 对业务路由精确 403
  `PASSWORD_CHANGE_REQUIRED`。
- T4-03 26 条管理路由按契约逐个接入真实读写与权限；未实现路由不注册、不返回恒定 200；DB 错误 503。
- T4-04 固定预览/幂等/授权全集：预览固定清单 1..=1024、60s 单调有效；`Idempotency-Key`
  actor+key 唯一，同内容重放先命中持久幂等记录，不同内容 409；counts 为当前授权全集汇总（非分页）。
- T4-05 MainTask 投影：state 四态（queued/running/held/completed）、outcome 五词（非 completed 为
  null）、counts 十 key 全存在（零值 "0"、u64 十进制字符串）、`created_time` 为 `TimeEvidence`、
  `actions.can_cancel` 只表示可请求取消而非一定成功。
- T4-06 单调缓存 status：`GET /devices/{id}/status` 独立 Runtime 路由读缓存快照（GET 零采集/零执行），
  单调接收年龄、旧会话/旧序号不覆盖；真实源未接线不挂载、不常量伪 PASS。
- T4-07 intent 至多一次 execute / 恢复只 query / 迟到证据：恢复对 dispatching/accepted/verifying 仅
  `task.get` 只读；迟到有效证据更新旧任务 outcome/counts 并递增 revision，不删新 generation 设备锁。
- T4-08 板端 B04 故障：accepted/attempted 两阶段持久化与 OS 调用边界故障分类（以 03 §5 B-04 完整
  规范为准，不得泛化为“所有 OS 错误均断流”）；测试中宿主真实 reboot 恒 0（FakeOS 边界）。
  证据覆盖 `insert_accepted`/`mark_attempted`/`finish` 三个写阶段 × 「rename 前确定失败」与
  「rename 结果不明」两类共 6 个组合：仅 accepted 确未提交回非零 status + 空 payload（并释放占用、
  可安全重试），attempted 确未提交回 status=0 的 accepted，OS 边界前后的结果不明一律断流；
  生产边界 `HostRebootOs` 全链路走通后宿主物理重启计数仍恒 0。
- T4-09 typed → 真实获批 wire：`rsetup-protocol::wire` 共享 DTO 先行；raw 字节入口 failclosed；
  精确 512KiB 信封与真实认证/双流互通须专家书面决议 + 用户复审 + 获批 codec——门禁未齐，保持 BLOCKED。
- T4-10 浏览器任务流程与 a11y：预览→确认→详情→取消/time-review/unknown 释放，键盘/焦点/Esc/320px/
  对比度/减少动效、中英切换；A 层 fixture/mock 不批准 B 层真实浏览器。
- T4-11 单二进制嵌入资源：缺 `dist` release 构建直接失败；脱离源码/静态目录运行；解析构建产物真实
  JS/CSS、精确 MIME 与 immutable 缓存；API/缺失资产不回退 HTML。
- T4-12 真库门禁项（双引擎）：普通应用账号真实事务、设备锁、CAS、撤权/提交并发；须串行预检
  （授权/隔离可丢弃/实际权限/schema 数据版本；真实备份 + 独立核验 + 排他停写）——门禁未齐，保持
  BLOCKED；用户已决定跳过的仅 root 锁观察单独留痕，不阻断 Task 4。

后续索引（完整 V1 目标，本轮从 Task 4 执行范围移出，保留原强度，全部未验证）：

- AT-01..AT-18 对应 06 §6 验收追踪矩阵（身份增量/身份准入/运行增量/传输增量/Web 增量/SSE 全量），
  逐项归属见 `docs/superpowers/plans/2026-10-16-controller-v1-integration-acceptance-tdd.md`；
  完整 SSE（五种命名事件/背压/reset/重连）、06 §7 容量/发布压测基准、真实宿主 reboot 与真实电源
  故障、旧 Observer 方案、旧备份受控恢复演练（AT-15）均属后续索引，不因本表通过视为已验收，
  也不删除未来要求、不称已 PASS、不顺带实施。

## 校验命令

```bash
python3 -m unittest scripts/tests/check_controller_acceptance_test.py
python3 scripts/check_controller_acceptance.py docs/testing/controller-v1-acceptance.md
```
