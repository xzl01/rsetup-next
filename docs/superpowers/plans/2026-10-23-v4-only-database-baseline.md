# v4-only 数据库基线实施计划

> **For agentic workers:** 使用 subagent-driven-development；每个子代理只执行一次任务，中文思考，不再派发子代理。用户禁止自动提交/推送。

**Goal:** 数据库代码、测试和两个已授权开发库统一以 schema v4 为唯一基线，不保留 v1–v3 升级或旧夹具路线。

**Architecture:** 空库经显式受控入口直接创建完整 v4；已是 v4 时只读闭合校验；旧版、未知版本、部分建表或损坏状态均拒绝，不自动升级/修复。普通服务启动始终只读。真实 TiDB 备份后仅表级重建，MySQL 现有 v4 保留。

**Tech Stack:** Rust stable、SQLx MySQL/TiDB；本地可信 local-qwen 处理授权 JSON 与私有备份，真实值不回显。

**Spec:** 用户本轮明确要求“全部以 v4 为基准，移除 v4 以前测试以及迁移”；已确认“清理代码，并备份核验后重建 TiDB 为 v4”；额外授权限于此次两个专用开发库，除修改全局配置与 DROP DATABASE 外可执行数据库操作。

## Global Constraints

- 不修改数据库全局配置；不执行 DROP DATABASE 或删库变体；授权不扩大到其它目标。
- 真实重建前必须有目标身份匹配的真实一致性备份及校验，不伪造 backup_ref/ACK；未恢复演练如实记录。
- 保留 PK/UNIQUE/NOT NULL/UNSIGNED 及既有应用级 guard/CAS/数据扫描，不引入 FK/CHECK 或全局开关。
- 数据库 schema v4 不等于隧道协议 v3、业务 DTO schema_version=1；不得机械删改后两者。
- Rust stable，不引入 MSRV；make fmt-check 为格式门禁，不 cargo fmt --all。
- no commit/push/clean；所有删除/移动前核对绝对路径，工作区外修改不交子代理。
- 隧道/controller 连接修复文件由其它 writer 独占，v4-only writer 不碰 transport/main。

## 文件地图

- src/db.rs：保留 DbPool/AdmissionStore/真实写事务与严格 v4 校验；删除 legacy schema diff/fixture/续迁实现，添加空库直接初始化。
- src/integrity.rs：仅识别/接受 v4，保留全结构及应用数据完整性检查。
- src/lib.rs、显式 schema 工具与 Cargo bin 注册：删除旧 test migration/旧导出；现有 rsetup-controller-migrate 如保留名称，只提供 v4 空库初始化与 v4 只读核验，拒绝旧版。
- schema/v4.sql：唯一完整 16 表 fresh 定义，替代0001–0004历史执行链；不以先建 v1/v2/v3 再 ALTER 达到 v4。
- tests/common、schema/CLI 测试：全部以 v4 初始化/拒绝/结构/数据闭合为目标；删除旧迁移与旧夹具测试，不删除认证/授权/CAS/锁/去敏业务测试。
- 当前 README/CI/Makefile/Debian 操作文档：清除旧升级命令与测试入口；历史设计文档标明已由本计划取代，不逐份重写历史证据。
- secret/：仅可信代理处理备份/重建脚本与结果，不提交真实配置或备份。

## Task 1：v4-only 源码与离线测试收缩

- [ ] **RED：版本决策只允许空库和4。** 在生产使用的纯决策函数测试：

```rust
assert!(matches!(production_migration_plan(None), Ok(ProductionMigrationPlan::CreateV4)));
assert!(matches!(production_migration_plan(Some(4)), Ok(ProductionMigrationPlan::VerifyOnly)));
for version in [-1, 0, 1, 2, 3, 5, i32::MAX] {
    assert!(production_migration_plan(Some(version)).is_err());
}
```

- [ ] **RED：fresh schema 为一套完整 v4。** 校验16表固定集合、0 CHECK/FK、版本写入4；初始化遇任何现存表/旧元数据/部分形状在第一条DDL前拒绝。CLI旧 fixture-v1/v2/partial-v1/upgrade 模式均拒绝且零连接/零写入，非法 UTF-8 argv 不回显。
- [ ] **实现：统一 fresh V4 DDL。** 从既有最终 identity 目标形状和 runtime 五表提取完整定义，不复制旧CHECK/旧窄计数列；保留ASCII用户名、二进制ID、全部现有索引/时间列。显式入口先身份与元数据权限核验→真正空库判定→受控建表→完整形状验证→最终schema_meta(version4)→只读闭合检查；中途失败保留未就绪并报错，不自动续迁。
- [ ] **删除：历史运行路线。** 删除旧SQL文件、旧迁移bin注册/源、TestMigrationMode旧fixture、check_v3_ready和升级/续迁/旧约束删除逻辑及对应tests。保留通用授权门禁可改名，但同步所有消费者，不留死API。初始化不得“假设置v4但结构仍v2”。
- [ ] **回归：** controller --all-targets check、离线lib/CLI/identity/task/transport定向测试、make fmt-check、clippy scope；不访问secret/真DB。
- [ ] **报告：** 删除/保留清单、旧测试移除原因、替代v4证据、API变化、未验，不伪称真实库已重建。

## Task 2：fresh 独立审查

- [ ] 新GLM reviewer只读验证无旧支持入口、v4完整16表及服务启动零DDL、旧版统一拒绝、事务guard未放宽、CLI/CI/docs引用已收敛。
- [ ] 针对finding由新的writer修复，不复用已结束代理；新鲜测试证据后复审。

## Task 3：真实 TiDB 表级重建

- [ ] 等已授权只读一致性备份任务返回；CRC/hash/表行数/同MVCCsnapshot证明通过后才继续。非一致性旧dump只留档，不作门禁。
- [ ] 可信local-qwen核验实际目标/引擎/当前schema/备份绑定/独占确认，记录去敏回执。
- [ ] 只删除该库已核对的表并用Task1显式fresh入口直接建立v4，不DROP DATABASE、不SETGLOBAL、不复用升级链、不导回旧版schema。
- [ ] 确认16表、版本4、无CHECK/FK、所有形状/数据闭合；MySQL现有v4只读核验不清表。
- [ ] 清除/保留测试数据按单独用例授权和备份门禁，不把重建后的空库称已完成应用初始化/登录验收。

## Task 4：双引擎及生产双进程接回原计划

- [ ] 两引擎各串行执行真实准入/CAS/容量与v4门禁测试；凭据进程内注入、输出私有捕获不泄露。
- [ ] 用真实controller/board两个进程验证Pending→批准→Ready→只读RPC/状态轮询、Kick/Revoke、重连新密钥、GOAWAY实际上线及≥5s窗口。
- [ ] 原生产隧道计划的故障/容量/fuzz/全量门禁保持未完成，不能以本计划v4化完成替代。

## 并发与验收

Task1可与现有transport/session修复并行，文件无交叉；Task2使用唯一获授权GLM席位排队。Task3必须在Task1和真实一致性备份均完成后，且其独立审查无阻断才执行。任何库错误都去敏，不回显DSN/用户/地址/备份内容。当前已有v4的MySQL不重建；用户明确允许的TiDB旧数据将不迁移。
