# Task 7: 真实双进程控制器与板端加密隧道验收报告

日期：2026-10-24  
环境：已授权 TiDB 8.5 专用隔离库（保留现况）、当前构建的生产二进制、真实回环 TCP / v3 签名握手 / SecureIo / h2 双流传输。

---

## 1. 执行前置与绑定事实

所有前置门禁在执行前均已完成程序级实核（来自 `secret/two-process-rerun-20261024T1625Z/` 私有收据，无凭据外泄）：

1. **Target 目标绑定**：
   - `SELECT DATABASE()` 与凭据完全匹配，引擎为 TiDB 8.5。
   - 包含 16 张 BASE 表、122 列、40 个索引、0 外键约束、0 CHECK、`schema_version = 4`。
   - 运行前 DB 为非空现况（包含此前首次尝试留下的 1 个管理员、1 个设备、4 条审计、9 行总记录），逐表内容摘要与快照完全一致，确保执行前零额外写入。
2. **Backup 备份门禁**：
   - 现况 9 行数据的权威快照（`backup/tidb-consistent-20261024T062242Z-1bbcc0.sql.gz`）通过 gzip 全流 CRC、原始/压缩哈希校验。
   - 声明 `authoritative: true`、`consistent_snapshot: true`、`restore_drill: not_performed`（未做恢复演练，不伪称验证可恢复）。
3. **Binary 二进制绑定**：
   - 严格绑定于 `target/tunnel-v4-parser-fix/post-first-run-build.json` 登记的最新构建产物（包含 RC1 与 RC2 修复）：
     - `rsetup-controller`（sha256 前缀 `d13ac227c90e`）
     - `rsetup-board-agent`（sha256 前缀 `7d6de37514f4`）
     - `rsetup-controller-migrate`（sha256 前缀 `b432fbe57888`）
   - 绝未复用首次失败时的旧二进制（sha256 `642a2f61…`）。

---

## 2. 真实双进程执行结果

使用 `scripts/tunnel_two_process.py`（104 项离线测试 + 16 项自测全绿基线）执行：

- **Dry-run 验证**：`exit 0`，`backup_bound = true`，未启动任何子进程，私有 state 目录保持真空。
- **真实 Execute（带 `--ack ISOLATED_TARGET_AND_BACKUP_VERIFIED`）**：
  - **脚本退出码**：`exit 0`
  - **Summary 终态**：`complete = true`，`failure_codes = []`，`status_observed = true`。
  - **所有必需阶段全为 true**：
    1. `identity_ok: true`（动态生成合法 PKCS#8 v2 Ed25519 密钥，非 OpenSSL 默认 v1）
    2. `schema_version_probe: true`（控制器启动前校验版本为 4）
    3. `controller_healthy: true`（NTP 30s 预算内安全回退 SystemFallback 后 `/healthz` 响应 200，启动期 `check_identity_schema` 硬门禁成立）
    4. `login_ok: true`（使用现况管理员凭据直接登录，无伪造 session）
    5. `pending_observed: true`（新板端公钥出现在审批列表中，**RC1 修复生效，未报 503**）
    6. `approved: true`（HTTP 发起批处理 CAS 批准，目标 revision 成功推进）
    7. `ready_observed: true`（板端私有日志捕获到真实双流首探成功的 `Ready` 事件，非盲等待）
    8. `status_observed: true`（控制器状态接口获取到 `freshness = fresh` 的真实设备状态快照与指标，**非空缓存，非错误降级**）
    9. `revoked: true`（通过 HTTP 成功发起准入吊销）
    10. `board_stop: true`（板端收到签名吊销信令，写入 `UNAUTHORIZED` 标记文件并以 `exit 0` 永久停机，非被 SIGKILL 终止）
    11. `preflight_rejected: true`（板端进程重启，preflight 探测到 `UNAUTHORIZED` 标记直接以 `exit 78` 拒绝，零网络重连）

---

## 3. 运行存活周期（RC2 修复确证）

为严密验证 RC2（旧实现运行满 30s 宽限期导致内部自杀）已彻底根治：
- 在收到关闭信号前，前置挂接只读观测窗口（耗时 40.73 秒，间隔约 5 秒进行 8 次健康与审批检查）。
- 控制器在全周期内健康服务跨度达 **50.4 秒**，在关停前累计健康存活达 **75.5 秒**（完全跨越并超越旧 30 秒自杀窗口）。
- 最终由脚本发起真实 SIGTERM 信号后，控制器正常以 `exit 0` 退出，所有子进程安全回收，**零孤儿进程残留（orphaned = 0）**。

---

## 4. 数据库副作用与闭合

运行前后进行了数据库只读核对：
- **无破坏性操作**：未执行 `DROP DATABASE`、未修改全局配置、未新建账号、未修改密码。
- **数据真实推进**：
  - 总行数由 9 行变为 16 行。
  - `devices` 表由 1 个旧 PENDING 增加为 2 个（本次测试的新板端以 `REVOKED` 状态正确保留）。
  - `audit_events` 增加 3 条审批、吊销等审计轨迹，由 4 条变为 7 条。
  - `users` 表内容 SHA256 逐字节保持一致，管理员账户状态未被污染。
  - 16 张表结构定义、索引指纹完全一致。

---

## 5. 验收边界声明

本报告确证：
- 控制器与板端两个独立进程在真实操作系统环境下的 TCP 外拨、v3 签名认证、h2 双流会话、状态机流转（Pending → Approve → Ready → Status → Revoke → 永久停连）端到端通过。
- 首次运行暴露的 RC1（审批列表 JSON 503）与 RC2（生命周期 30s 自杀）已被真实运行结果证实修复闭环。

明确不包含（亦未声称）：
- 未进行真实主机重启（硬件能力如实声明 `Unsupported`）。
- 未量测线上物理网卡断开后 ≥5s 的精确 GOAWAY flush 字节时序（仅量测了受控应用层关闭）。
- 未测试真实单机 1024 设备并发接入的硬件极限。
