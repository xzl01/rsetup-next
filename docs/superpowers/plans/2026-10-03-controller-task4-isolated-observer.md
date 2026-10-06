# Controller Task4 隔离 observer 实施计划

> **致执行代理：** 用户已选择子代理执行；主代理先自查本计划，之后使用 superpowers:subagent-driven-development，每项新代理、规格审查及质量审查。无需再询问执行模式。实现者只修改/测试/报告，不得 stage/commit；父协调代理负责最终提交。以下命令是后续离线实现步骤，本次文档任务不执行。

**Goal:** 不改变 writer/生产API/门禁，以两条业务参与连接 A/B 加独立 O 实现可审查的离线 ready 接线；没有 O 凭据也能结束为 offline-ready，但 Task4 真实验收未完成。

**Architecture:** test-only Rust support 模块由 integration tests 和本地 ignored probe 通过 path 共用，严格 observer grammar 与 writer grammar 分离。Python 新适配器持有私有配置/授权和 fd 边界，每例 reset 前独立 observer preflight；真实连接重验，首失败停两库。

**Tech Stack:** Rust2024/MSRV1.85，SQLx0.8.6；现有 Tokio、serde_json、sha2、std，Python3 stdlib；不新增依赖。

**Spec:** [Task4 observer正式规格](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/docs/superpowers/specs/2026-10-03-controller-task4-isolated-observer-design.md>)；同时阅读[原规格](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/docs/superpowers/specs/2026-10-03-controller-application-integrity-design.md>)及[原计划Task4](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/docs/superpowers/plans/2026-10-03-controller-application-integrity-tdd.md>)。

## Global Constraints

- 本次批准 `task4_observer_design_approval='批准方案及限定离线实现'` **只限offline**。所有新凭据读取/真实DB/GRANT/reset另批；不读secret、不启动live runner。操作者另准备身份与权限；不能默认supply独立身份或用fake票据当grant。
- Rust2024/MSRV1.85，SQLx0.8.6；不新增依赖。不更改生产公开API、writer/migration parser及其旧callers，不整理旧parser既有WITH GRANT OPTION行为。
- 原目标alias、备份、独占、migration ACK、失败保留及禁止DROP DATABASE不变。MySQL三表SELECT/TiDB PROCESS跨会话风险已接受；不扩大writer权限。
- O仅固定SELECT/SHOW GRANTS，无默认database/USE/SET/事务；原writer options原样。当前无显式TLS fields，不声称exactTLS；授权时明确有效传输策略，需新字段则另审。
- TiDB操作者当前窗口拓扑绑定加实际ID/start_ts关系；同host不是独立证明，不读mysql.tidb，不发明sysvar。
- RED必须可编译且因目标行为断言失败，缺函数/编译错/连接错/0测试/ignored不是RED。先加最小类型与安全拒绝stub，RED仅因合法输入被拒绝；禁止临时放开真实I/O门禁制造RED。
- 所有offline测试默认网络零调用、真文件零读取。使用临时合成资料，fake备份/拓扑只进入mock harness，不能输出可供live复用的授权物。
- ignored工具不得git add -f；计划无实现者stage/commit步骤。父协调代理只提交tracked support/wrappers/文档；本地probe/runner及其测试用去敏报告保留，不假称git记录已包含它们。
- 命令从指定worktree执行；仅使用已有离线缓存，不安装toolchain/依赖，缓存缺失报告构建blocker。任何失败先查目标断言再继续；所有任务各有审查点。

## 文件图与依赖裁决

[controller manifest](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/crates/rsetup-controller/Cargo.toml>) 与 [probe manifest](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/.superpowers/dev-db-probe/Cargo.toml>) 均有SQLx、Tokio、serde_json、sha2。共享Rust只依赖交集和std；不增加serde derive、libc、async-trait、tempfile、Tokio test-util。Python承担私有打开和pass_fds；**经用户批准的 O2 修订**：Rust 受控子进程在唯一 raw 转换边界接管经验证的整数并立即形成两个 `OwnedFd`，共享 loader 从入口接收 `OwnedFd`、由 RAII 关闭，再做单次bytes双pin；转换前验证失败时立即终止受控子进程并由父进程回收，绝不作为 loader 正常返回的“已关闭”证据。Linux本地工具已有Unix前提，其他平台此入口fail-closed。

Create（tracked）：`crates/rsetup-controller/tests/support/observer/{mod,grants,config,queries,session}.rs`，`crates/rsetup-controller/tests/observer_contract.rs`，`crates/rsetup-controller/tests/common/task4_concurrency.rs`。

Modify（tracked）：仅 `crates/rsetup-controller/tests/{common/mod.rs,mysql_identity.rs,tidb_identity.rs}` 的Task4关联及本专题文档。不修改src或manifest/lockfile。

Create（ignored/local）：`.superpowers/dev-db-probe/{observer_runner.py,test_observer_runner.py}`、`.superpowers/dev-db-probe/src/observer.rs`。

Modify（ignored/local）：[probe main](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/.superpowers/dev-db-probe/src/main.rs>)只加模块/新模式分发、保留旧函数和calls；[runner](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/.superpowers/dev-db-probe/run_dev_tests.py>)只加明确case分支/首失败停两库；[原runner测试](</home/aghost/workspace/rsetup-next/.worktrees/controller-v1-01-identity/.superpowers/dev-db-probe/test_run_dev_tests.py>)保持旧路径回归。

精确path导入：integration入口 `#[path="support/observer/mod.rs"] mod observer;`；common入口 `#[path="../support/observer/mod.rs"] mod observer;`；probe入口 `#[path="../../../crates/rsetup-controller/tests/support/observer/mod.rs"] mod observer_support;`。probe自己的observer适配器引用 `crate::observer_support`。共用模块内部相对路径只引用同目录文件。不得为共享把support注册进生产lib。

任务顺序 O1→O2→O3→O4→O5→O6；O7是独立授权后的live关卡，不在离线实施中执行。O1–O6每项结束给父协调代理一次规格/质量审查；审查不得以mock等于live放行。

## O1：独立 observer grammar 与 writer 冻结证据

**Files:** Create `tests/support/observer/{mod,grants}.rs`、`tests/observer_contract.rs`（均相对controller目录）；probe只加测试path导入。Test: 新contract、原probe `direct_grants_accept_only_database_all_and_same_account_usage`、原db `schema_metadata_grants_require_exact_direct_schema_all`。

**Interfaces:** support内部 `Engine::{Mysql,Tidb}`（Copy/PartialEq/Eq/Debug）；`type ObserverError = &'static str`；`Account { user:String, host:String }`（不derive Debug）；`validate_grants(engine:Engine, rows:&[Option<String>], expected:&Account)->Result<(),ObserverError>`；`validate_identity(engine:Engine,current:&str,writer:&str,expected:&Account,current_role:Option<&str>,mandatory_roles:Option<&str>)->Result<(),ObserverError>`。所有 `pub(crate)`，不依赖rsetup_controller。

- [ ] **Step1：可编译RED。** 定义类型，两个函数先固定 `Err("shape_rejected")`；contract写以下测试，probe以同一path模块编译同样测试，但不挪旧writer parser。

```rust
#[test]
fn observer_closed_mysql_and_tidb_grammar() {
    use observer::{Account, Engine, validate_grants};
    let a = Account { user: "o_fixture".into(), host: "%".into() };
    let mysql: Vec<Option<String>> = ["data_lock_waits", "data_locks", "threads"]
        .into_iter().map(|t| Some(format!(
            "GRANT SELECT ON `performance_schema`.`{t}` TO 'o_fixture'@'%'"))).collect();
    assert_eq!(validate_grants(Engine::Mysql, &mysql, &a), Ok(()));
    let tidb = vec![Some("GRANT PROCESS ON *.* TO 'o_fixture'@'%'".into())];
    assert_eq!(validate_grants(Engine::Tidb, &tidb, &a), Ok(()));
    assert!(validate_grants(Engine::Mysql, &tidb, &a).is_err());
    assert!(validate_grants(Engine::Tidb, &mysql, &a).is_err());
    let mut duplicate = mysql.clone(); duplicate.push(mysql[0].clone());
    assert!(validate_grants(Engine::Mysql, &duplicate, &a).is_err());
    for suffix in [" WITH GRANT OPTION", " WITH ADMIN OPTION", "; SELECT 1", " -- x", " "] {
        let mut bad = mysql.clone();
        bad[0] = bad[0].as_ref().map(|s| format!("{s}{suffix}"));
        assert!(validate_grants(Engine::Mysql, &bad, &a).is_err());
    }
}
#[test]
fn observer_identity_and_roles_are_separate_gates() {
    use observer::{Account, Engine, validate_identity};
    let a = Account { user: "o_fixture".into(), host: "%".into() };
    assert_eq!(validate_identity(Engine::Mysql,"o_fixture@%","writer@%",&a,Some("NONE"),Some("")),Ok(()));
    for (current, writer, role, mandatory) in [
        ("writer@%","writer@%",Some("NONE"),Some("")),
        ("o_fixture@%","writer@%",Some("role_x"),Some("")),
        ("o_fixture@%","writer@%",None,Some("")),
        ("o_fixture@%","writer@%",Some("NONE"),Some("forced")),
        ("o_fixture@%","writer@%",Some("NONE"),None),
    ] { assert!(validate_identity(Engine::Mysql,current,writer,&a,role,mandatory).is_err()); }
}
```

- [ ] **Step2：确认RED命令。** `cargo test --offline --locked -p rsetup-controller --test observer_contract observer_closed_mysql_and_tidb_grammar -- --exact`；必须1个目标assert合法集合失败，不能是编译错误。
- [ ] **Step3：GREEN。** 对完整前缀/作用域/account/结束位置做封闭匹配；集合去重且必须恰满足必需项；可选USAGE至多1条。account严格按spec的无转义子集解析并与expected各段精确相等。identity返回不同fixed error；TiDB无角色也必须NONE，mandatory字段不当作TiDB sysvar。无角色外部确认仍由授权gate提供。
- [ ] **Step4：扩展行为矩阵并全绿。** 两引擎分别测USAGE有/无、行序置换、backtick账户正例、缺每张表、额外任意表、schema wildcard、global SELECT、column SELECT、mixed list、wrong account、NULL、空、role/proxy/dynamic、REVOKE、comment、所有尾缀、未知quote/escaping。对writer旧函数运行原测试，验证既有WITH GRANT OPTION正例仍过、O诊断权限附加到writer仍拒绝。

```bash
cargo test --offline --locked -p rsetup-controller --test observer_contract
cargo test --offline --locked -p rsetup-controller schema_metadata_grants_require_exact_direct_schema_all
cargo test --offline --locked --manifest-path .superpowers/dev-db-probe/Cargo.toml direct_grants_accept_only_database_all_and_same_account_usage
```

- [ ] **Step5：独立审查/交付。** 父代理以任务开始时已read的文件为基准比对：probe `simple_account/has_direct_database_all/direct_database_all`、db `grant_grantee_is_simple_account/validate_schema_metadata_grants/require_schema_metadata_privilege` 以及所有旧call sites必须零diff；不能仅用测试绿代替零diff。报告两套grammar的用例数和无真实连接。实现者不提交。

## O2：双 pin、无默认库 endpoint 与私有 fd 边界

**Files:** Create `tests/support/observer/config.rs`、本地 `observer_runner.py` / `test_observer_runner.py`；Modify support导出、contract。

**Interfaces (superseding the original raw-i32 sketch with the user-approved OwnedFd revision):** `PinnedInput<'a>{writer_bytes:&'a[u8],observer_bytes:&'a[u8],writer_pin:&'a str,observer_pin:&'a str}`；`PreparedObserver`（私有secret，无Debug/Display），提供 `options(&self)->&MySqlConnectOptions`、`schema(&self)->&str`；`prepare(input:PinnedInput<'_>,engine:Engine)->Result<PreparedObserver,ObserverError>` 是不连接的纯函数。`load_inherited(writer_fd:std::os::fd::OwnedFd,observer_fd:std::os::fd::OwnedFd,writer_pin:&str,observer_pin:&str,engine:Engine)->Result<PreparedObserver,ObserverError>` 从入口拥有两 fd，任何正常返回/Err 都关闭它们；raw-i32 验证和唯一 FromRawFd 转换移至后续受控子进程入口（probe 在 O4、ignored test wrapper 在 O6），不在 O2 loader。Python `open_private(path:Path)->int` 返回caller拥有的fd，`read_pinned(fd:int,pin:str)->bytes`；新授权对象无默认值，具体接口在O5。

- [ ] **Step1：RED。** 加安全拒绝stub，使用下面合成bytes；定义helper `pin(bytes:&[u8])->String` 为 `format!("{:x}",Sha256::digest(bytes))`（导入sha2::Digest）。不读repo secret。

```rust
#[test]
fn observer_derives_endpoint_without_database_or_writer_mutation() {
    use observer::{Engine, PinnedInput, prepare};
    let w = br#"{"schema_version":1,"engine":"mysql","connection":{"host":"fixture.invalid","port":3306,"username":"w","password":"not-real","database":"Exact_DB"}}"#;
    let o = br#"{"username":"o","password":"not-real-either"}"#;
    let wp=pin(w); let op=pin(o);
    let p=prepare(PinnedInput{writer_bytes:w,observer_bytes:o,writer_pin:&wp,observer_pin:&op},Engine::Mysql).unwrap();
    assert_eq!(p.options().get_host(),"fixture.invalid");
    assert_eq!(p.options().get_port(),3306);
    assert_eq!(p.options().get_database(),None);
    assert_eq!(p.options().get_socket(),None);
    assert_eq!(p.schema(),"Exact_DB");
    assert_eq!(p.options().get_ssl_mode(),sqlx::mysql::MySqlConnectOptions::new().get_ssl_mode());
    assert!(prepare(PinnedInput{writer_bytes:w,observer_bytes:o,writer_pin:&op,observer_pin:&op},Engine::Mysql).is_err());
    for bad in [br#"{"username":"o","password":"x","host":"override"}"#.as_slice(),
                br#"{"username":"o","username":"other","password":"x"}"#.as_slice()] {
        let bp=pin(bad);
        assert!(prepare(PinnedInput{writer_bytes:w,observer_bytes:bad,writer_pin:&wp,observer_pin:&bp},Engine::Mysql).is_err());
    }
}
```

- [ ] **Step2：确认RED。** `cargo test --offline --locked -p rsetup-controller --test observer_contract observer_derives_endpoint_without_database_or_writer_mutation -- --exact`；合法合成输入被stub拒绝，不得实际connect。
- [ ] **Step3：GREEN。** 用sha2校验原bytes双pin；O专用flat object parser用serde_json字符串token解码key/value并维护已见key集合，拒绝重复/非字符串/未知key/尾数据（不要用Value覆盖duplicate key后宣称strict）。writer由原结构提取同样host/port/schema，旧loader不改；拒绝本新入口不能表达的额外connection/TLS字段。从 `MySqlConnectOptions::new()`按原语义重建，不调用database，关闭O初始化SET与statement logging。原options若带不可复现设置则拒绝而非忽略。
- [ ] **Step4：private file/OwnedFd 行为测试（修订）。** Python unittest用TemporaryDirectory、chmod0700/0600合成文件，逐个测试owner不符（mock fstat）、文件/父目录宽权限、symlink file/dir、FIFO、非regular、超过64KiB、替换、读取中漂移、缺pin/大写pin/任一bytes变更；assert `open_private` 或 `read_pinned` 报固定异常且mock连接计数0。目录逐级openat/no-follow，允许可信非私有祖先但拒绝不可信可写祖先，直接私有父目录必须private。Rust loader 用安全 `File::into()` 生成两份不同 `OwnedFd`，在隔离子测试进程验证只读regular、seek、65536 正边界、读后关闭、错误（含 fdinfo 前检失败）亦关闭，不能从 `OwnedFd` 反向造裸整数调用不安全函数。0/1/2、关闭/缺失整数、同号原始 fd 的拒绝移到后续受控 raw 转换边界，在任何 `unsafe FromRawFd` 之前验证；转换前异常不得返回后继续迁移或 spawn，必须子进程立即结束，由父进程回收，明确这不是 loader 自己的正常关闭。fdinfo 仅读有界前缀（例如最多 4096 字节+1，超界固定拒绝），不能使用无界 `read_to_string`。

```python
def test_private_file_pin_and_no_symlink(self):
    import hashlib, os
    p = self.root / "observer.json"  # setUp: TemporaryDirectory，root chmod0700
    raw = b'{"username":"o","password":"fixture-only"}'
    p.write_bytes(raw); p.chmod(0o600)
    fd = self.observer.open_private(p)
    try:
        self.assertEqual(self.observer.read_pinned(fd, hashlib.sha256(raw).hexdigest()), raw)
        with self.assertRaises(self.observer.ObserverError):
            self.observer.read_pinned(fd, "0" * 64)
    finally: os.close(fd)
    link = self.root / "alias.json"; link.symlink_to(p)
    with self.assertRaises(self.observer.ObserverError): self.observer.open_private(link)
```

运行：`python3 -m unittest discover -s .superpowers/dev-db-probe -p 'test_observer_runner.py'`，`cargo test --offline --locked -p rsetup-controller --test observer_contract`。
- [ ] **Step5：独立审查/交付。** 审查fd所有权/TOCTOU、secret Debug/日志禁止、unknown TLS fail-closed；确认不修改writer loader和SAFE_ENV。无需真实O即可结束本任务；传输策略外部未知不是实施者补TLS的理由。

### O2B2 审查修复：先建立 OwnedFd 所有权再检查 `/proc`

**Files:** Modify `crates/rsetup-controller/tests/support/observer/config.rs`, `mod.rs`, `tests/observer_contract.rs`; no Python, production source, manifests or lockfile changes. **Consumes:** accepted `prepare(PinnedInput,Engine)` and private Python `open_private`; **produces:** safe `load_inherited(OwnedFd,OwnedFd,&str,&str,Engine)->Result<PreparedObserver,ObserverError>`. Raw-id authorization/one unsafe conversion belongs to the later controlled child entry, not this loader. This amends O2B2 review Important I-1; preconversion failures cause child exit/reap and never return to migration. Follow TDD and arrange an independent limited security review.

- [ ] **Step1: compilable behavioral RED.** Add a safe stub taking `std::os::fd::OwnedFd` and dropping both before `Err("config_invalid")`. In a child-only synthetic-file test, create `File::open(path)?.into()` for each fd (without `FromRawFd`), record `as_raw_fd()` without leaking ownership, assert valid pinned input returns a prepared schema with `options().get_database()==None`, and assert `/proc/self/fd/{n}` reports both closed afterwards. Example inside the existing isolated child fixture:

```rust
use std::{fs::File, os::fd::{AsRawFd, OwnedFd}};
let writer: OwnedFd = File::open(&writer_path).unwrap().into();
let observer_fd: OwnedFd = File::open(&observer_path).unwrap().into();
let (w, o) = (writer.as_raw_fd(), observer_fd.as_raw_fd());
let prepared = observer::load_inherited(writer, observer_fd,
    &pin(WRITER), &pin(OBSERVER), observer::Engine::Mysql).unwrap();
assert_eq!(prepared.schema(), "Exact_DB");
assert_eq!(prepared.options().get_database(), None);
assert!(!fd_open(w) && !fd_open(o));
```

Run `CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo test --offline --locked -p rsetup-controller --test observer_contract inherited_owned_fd_contract -- --exact`; RED must be the valid behavior assertion, not a compile/import failure. The current raw-`unsafe fn` test must not silently disappear: identify raw-id 0/1/2, duplicate and closed-fd cases as explicit O4/O6 controlled-entry obligations in the test/report rather than calling a safe owned loader with forged descriptors.
- [ ] **Step2: minimal GREEN and bounded flags.** Move two `OwnedFd` values into the function immediately, convert with safe `File::from(owned_fd)` or borrow until drop; all later errors including `/proc` failure then drop both. Keep the existing regular/read-only/metadata/seek/bytes/pin checks and fixed errors. Replace `fs::read_to_string("/proc/self/fdinfo/<n>")` with a bounded fdinfo read (at most 4097 bytes for a 4096-byte maximum), reject overlong/malformed input before parsing `flags:` octal. Do not reopen `/proc/self/fd/<n>` for configuration bytes, do not add any `unsafe` or dependency, and do not read actual secret paths.
- [ ] **Step3: error/lifetime matrix.** In the child test use owned writable/write-only/directory/oversized and incorrect pin inputs; assert fixed `config_invalid` or `config_changed_since_authorization` as appropriate and both fd numbers closed. Cover exactly 65536 valid JSON bytes and one byte too many; demonstrate early fdinfo failure closes the already-owned fd by a test-only injectable bounded fdinfo reader or a safe scoped synthetic failure, never by removing/mounting real `/proc`. If injection cannot be done without introducing a production backdoor, state that RAII-on-entry is statically provable and retain a targeted controlled error test without claiming a dynamic `/proc` outage test.
- [ ] **Step4: full offline verification and review.** Run observer contract, original writer/probe grammar, Rust 1.85 contract, Python private opener tests, fmt and targeted clippy using only existing offline caches; isolate any pre-existing `examples/fk_metadata.rs` warnings instead of modifying them. Recheck no changes to writer loader, production src, Cargo and frontend. Review must explicitly separate loader `OwnedFd` close-on-return from future raw-boundary fail-process cleanup, plus test/no-live limitations.



## O3：实际会话验证、固定扫描与有界观察

Caller 验收义务（待 O3 实现）：writer 必须来自实际参与连接成功查询 `CURRENT_USER()` 并非 NULL 解码的结果，查询/NULL/解码失败不得 fallback；必须核对 A/B 为同一实际 writer 或对两者分别验证 O。

**Files:** Create `tests/support/observer/{queries,session}.rs`；Modify mod/contract。

**Interfaces:** `SessionIdentity { connection_id:u64, version:String, database:Option<String>, current_user:String, server_uuid:Option<String>, global_ids:Option<bool>, pessimistic:Option<bool> }`（无Debug）；`validate_sessions(engine:Engine,schema:&str,a:&SessionIdentity,b:&SessionIdentity,o:&SessionIdentity,topology_current:bool)->Result<(),ObserverError>`。`ActorIds { waiter:u64, holder:u64 }`（derive Clone/Copy，供重复关系断言使用）；`TrxMapping { session:u64,start_ts:u64 }`；`validate_edge(ids:ActorIds,mappings:&[TrxMapping],edge:Option<(u64,u64)>)->Result<bool,ObserverError>`。`ObserverSession`只含dedicated MySqlConnection/身份，constructor私有；`open(prepared:PreparedObserver,expected:&Account,writer:&str,deadline:tokio::time::Instant)->Result<ObserverSession,ObserverError>`、`wait_edge(&mut self,engine:Engine,schema:&str,ids:ActorIds,deadline:Instant)->Result<bool,ObserverError>`；只在通过全部查询后返回verified对象。SQL对外不暴露任意string executor。

- [ ] **Step1：RED。** 纯验证先固定拒绝，contract中写：

```rust
#[test]
fn tidb_edge_requires_unique_current_start_ts() {
    use observer::{ActorIds,TrxMapping,validate_edge};
    let ids=ActorIds{waiter:22,holder:11};
    let maps=vec![TrxMapping{session:11,start_ts:101},TrxMapping{session:22,start_ts:202}];
    assert_eq!(validate_edge(ids,&maps,Some((202,101))),Ok(true));
    assert_eq!(validate_edge(ids,&maps,Some((202,999))),Ok(false));
    assert_eq!(validate_edge(ids,&maps[..1],None),Ok(false));
    assert!(validate_edge(ids,&[],None).is_err());
    let dup=vec![TrxMapping{session:11,start_ts:101},TrxMapping{session:11,start_ts:303}];
    assert!(validate_edge(ids,&dup,None).is_err());
}
```

- [ ] **Step2：目标RED命令。** `cargo test --offline --locked -p rsetup-controller --test observer_contract tidb_edge_requires_unique_current_start_ts -- --exact`。
- [ ] **Step3：GREEN纯规则。** 精确version/database；O database=None；三ID有效且互异；MySQL UUID必须全部非空相等；TiDB topology_current=false即拒绝，不用同endpoint补证，三个global_ids=true、A/B pessimistic=true。edge映射holder唯一且存在、waiter暂缺false、重复/无效start_ts error，边仅匹配当前B/A start_ts。逐字段mutation测试三身份所有拒绝分支（NULL/default-db/版本/UUID/重复ID/global-id/mode/topology失效）。
- [ ] **Step4：固定SQL接线。** 从原common wait SQL迁入queries；MySQL绑定顺序waiter/holder/schema，schema/table BINARY exact；TiDB映射先读必要ID/start_ts，然后join和服务端JSON字段检查，缺失JSON false。每张诊断表独立 `SELECT CAST(1 AS SIGNED) ... LIMIT 1`真实扫描；不输出敏感列。用内部枚举 `ReadStep::{Identity,Grants,Roles,MysqlWaits,MysqlLocks,MysqlThreads,TidbWaits,TidbTrx,Edge}` 选择常量SQL；fake记录enum和binds，断言O调用图没有SET/USE/BEGIN、MySQL三表和TiDB两视图各实际请求，NULL解码和query error不能被当空集。真实SQLx adapter和fake driver共用同一step顺序函数，不另写仅测试模型。
- [ ] **Step5：deadline RED→GREEN。** 新内部泛型 `async fn bounded<F,T>(future:F,total:Instant,stage:Duration)->Result<T,ObserverError> where F:Future<Output=Result<T,ObserverError>>`，先直接await的可编译基线只在离线fake使用；下面测试须以外层保护结束RED，随后实现 `timeout_at(min(total,now+stage),future)` 返回 `timeout`。不需tokio test-util。

```rust
#[tokio::test]
async fn observer_total_deadline_bounds_pending_query() {
    use std::{future::pending,time::Duration};
    use tokio::time::{Instant,timeout};
    let result=timeout(Duration::from_millis(250), observer::bounded(
        pending::<Result<(),&'static str>>(), Instant::now()+Duration::from_millis(5),
        Duration::from_secs(3))).await;
    assert!(matches!(result,Ok(Err("timeout"))));
}
```

同样断言连接10s/查询3s与总30s共用绝对deadline；实际轮询20s、100ms限流不是证据。fake Drop计数在timeout/CAS早退/query error后归零；不spawn detached任务。
- [ ] **Step6：独立审查/交付。** `cargo test --offline --locked -p rsetup-controller --test observer_contract`。审查真实SQL与fake消费相同catalog/控制流；静态SQL断言仅辅证，不把无库测试说成实际权限/行解码成功。unsupported角色机制/TLS/拓扑是external blocker。

### O5A 前置：O4/O6 连接前的离线授权封装（O3B2B I-1 修复依赖）

**Files (Python):** `.superpowers/dev-db-probe/{observer_runner.py,test_observer_runner.py}` only. **Files (Rust):** `crates/rsetup-controller/tests/support/observer/{authorization.rs,config.rs,transport.rs,mod.rs}`, `tests/observer_contract.rs`. This is a reordering of O5's already-approved authorization requirement, not permission to open real files/DB. The original O4 protocol and O5 runner/reset work still follow their respective phases.

**Envelope (exact shared offline wire shape, UTF-8 JSON ≤65536 bytes):** `engine` (`mysql|tidb`), `writer_pin`, `observer_pin` (independent lowercase SHA-256), `expected_account` (`user`,`host`), `run_id`, `window_start`, `window_end` (UTC RFC3339), `transport_policy_ref`, `topology_ref`, `operator_confirmed` (true), and `scope` (`observer-capabilities` or one of O6's four exact case names). No path, password or URL goes into this envelope. A nonempty reference or true flag alone is **not** operator authority: the Python parent must compare the entire binding to an external, current-window operator record supplied separately, not generated by parsing the secret or synthesizing a passed marker. Synthetic records exist only inside offline tests and are not reusable live approvals.

- [ ] **Step1: Python authorization RED→GREEN.** Add a no-repr all-required authorization object and a separate no-repr current-window operator record input; start with a fail-closed envelope stub. Synthetic temporary-file/fake-record tests must show a matching externally supplied record can yield only the above fields, while missing/mismatched record, target pin/identity/scope/transport/topology/window, false flag, reused/stale window and inherited observer env reject before any open/spawn/reset. Observe a behavior assertion RED for valid fake input, then implement just this parent-bound generation; keep O2 private opener and original runner untouched. This phase does not create a real operator record.
- [ ] **Step2: Rust strict envelope and connection gate RED→GREEN.** Add `read_authorization(input:impl Read)->Result<AuthorizedRun,ObserverError>` with private non-Debug fields, bounded read, strict decoded-key duplicate/unknown/type rejection and nested exact account. Keep expected engine, both pins, account, run/window/scope/transport/topology binding private; `PreparedObserver` privately retains its already checked input pins and verified engine. `ObserverSession::open` takes `&AuthorizedRun`, checks all binding/window/scope and `expected` **before** `connect_with`, then uses the existing bounded connect/shared preflight. Invalid/missing/expired/wrong-scope/pin/account/engine must leave a fake connector invocation count at 0. Valid synthetic authorization can only exercise an injected no-network connector; never call the real connector in offline tests. Observe a valid fake behavior RED on an initial safe refusal and then GREEN. The Rust parser checks the parent's declaration/shape, not an independently verified operator act; O4/O5 must still prove the trusted parent record and real writer `CURRENT_USER()` source before live.
- [ ] **Step3: cross-language contract/review.** Python builds one synthetic envelope; Rust strict parser accepts that exact shape with matching synthetic pin/account/time while mutations and duplicate decoded keys reject. Independent safety review traces parent record→stdin envelope→`AuthorizedRun`→pre-connect gate and confirms no real secret read, DB connection, GRANT, reset or production API edit. Only after both slices pass can O4/O6 consume this gate; raw fd handoff and fixed-output protocol remain separate tasks.

## O4：observer-only probe 入口和固定输出协议

**Files:** Create local `src/observer.rs`；Modify local main的新模式注册/早分发（writer旧callers保持原文）。Test: local observer模块测试及共享contract。

**Interfaces:** `async fn run_observer(engine:Engine)->serde_json::Value`（只从显式fd/pin/授权输入载入）；新mode `observer-capabilities`，原四mode不变。唯一成功输出字段：`{engine,mode:"observer-capabilities",result:"passed",writes_executed:false,capabilities:{identity:true,grants:true,roles:true,scans:true}}`；失败仅固定engine/mode/result/stage/error_class/writes_executed；无 `direct_database_all`。成功只证明preflight，不附可复用live授权票据。

- [ ] **Step1：RED。** 先定义 `observer_result(engine, outcome:Result<(),(&'static str,&'static str)>)->Value` 固定failed。模块测试：

```rust
#[test]
fn observer_protocol_is_not_writer_probe_success() {
    let ok=observer_result("mysql",Ok(()));
    assert_eq!(ok["mode"],"observer-capabilities");
    assert_eq!(ok["result"],"passed");
    assert_eq!(ok["writes_executed"],false);
    assert!(ok.get("direct_database_all").is_none());
    let err=observer_result("mysql",Err(("evil-secret","raw-secret")));
    assert!(!err.to_string().contains("secret"));
    assert_eq!(err["result"],"failed");
}
```

- [ ] **Step2：RED命令。** `cargo test --offline --locked --manifest-path .superpowers/dev-db-probe/Cargo.toml observer_protocol_is_not_writer_probe_success`。
- [ ] **Step3：GREEN。** stage/class双白名单serialize，不format SQLx errors。新mode直接进入共享config/session；任何失败exit非0；总30s outer timeout包括SHOW GRANTS/roles/扫描，45s父进程兜底。旧probe/reset/backup-empty/lock-capabilities和writer direct_database_all不改。A/B预检只持有原writer权限，不让O的passed绕过原writer preflight。**OwnedFd 接线补充：**仅本受控 observer 子进程在授权 envelope 已核对、fd 数字 >=3/不同/有效且确系本次 Python pass_fds 移交、无其他本进程所有者/并发close/reuse 后，使用一处写明前提的 `unsafe FromRawFd` 转为两个 `OwnedFd`，立刻移交共享安全 `load_inherited`；裸 fd 校验若在转换前失败，立即失败退出整个子进程并由父进程监督回收，不返回之后继续 spawn 或运行迁移。invalid/duplicate/stdin/out/err、前置 `/proc` 失败、已转换后 fdinfo 失败各用隔离 fake 子进程验证无继续动作，并将内核进程回收与 loader 的 RAII 关闭分开报告。
- [ ] **Step4：无网络行为验证。** 入口缺fd/缺pin/无授权时返回固定失败，connection counter=0；fake全部stage分别query/decode/timeout错误，输出不含合成user/password/schema/UUID/ID/grants；所有grants正例仍必须执行scans，扫描空集可passed但edge不标true。原writer `probe()`收到新mode成功JSON必须仍拒绝。
- [ ] **Step5：审查/交付。** 重跑O1两套grammar与probe全离线单测；主代理逐字检查旧函数与调用点。报告ignored入口修改，不强制入库。

## O5：runner cleanenv、每例reset前短路及首失败停两库

**Files:** local `observer_runner.py`、`test_observer_runner.py`；最小修改原run_dev_tests的新分支；原测试继续跑。

**Interfaces:** `ObserverAuthorization`（Python dataclass无repr，所有字段必填）包含engine、writer_path、observer_path、writer_pin、observer_pin、expected_account(user,host)、run_id、window_start/end、transport_policy_ref、topology_ref、operator_confirmed；私有记录来自本次外部授权，不由工具生成。runner只把已验证记录的必要字段序列化到新observer子进程的专用stdin（最多64KiB），不含密码/连接URL；该通道只承载授权绑定，不作为配置凭据fallback。字段固定为engine、双pin、expected_account、run_id、window_start/end、transport_policy_ref、topology_ref、operator_confirmed与操作范围；Rust `read_authorization(input:impl std::io::Read)->Result<AuthorizedRun,ObserverError>` 校验严格类型、scope、window及双pin并消费后关闭stdin引用；`AuthorizedRun`无Debug，构造不公开。ref非空本身不代表授权：runner必须先确认其对应操作者当前窗口记录与目标/身份绑定，未提供即拒绝。`validate_authorization(auth,engine,now)->None`；`observer_preflight(binary,auth,env,root)->dict`；`observer_case_environment(base,auth,writer_fd,observer_fd)->dict`；`is_observer_case(engine,name)->bool`只接受O6精确四名。`execute(..., observer_authorizations=None)` 新可选参数默认不提供；选择O case无授权直接fail-closed，不能绕回旧self-observation。旧模式无O env，原probe函数不修改。

- [ ] **Step1：RED fake reset短路。** 在原RunnerTests基础建立子类 `ObserverRunnerTests`，setUp替换case_names为O6 mysql两例、fixture source对应ignored函数，并用纯内存合成auth；`run_mock`转发observer_authorizations。mock仅证明顺序，禁止真实I/O；所有subprocess均patch。先安全stub总拒绝，对合法fake全链应失败RED：

```python
def test_observer_preflight_before_each_reset(self):
    result = self.run_mock(observer_authorizations=self.synthetic_authorizations)
    self.assertEqual(result["status"], "passed")
    modes = [args[-1] for args, _ in self.calls
             if args[0] == str(self.probe_bin)]
    self.assertEqual(modes, ["probe", "observer-capabilities", "reset-tables",
                             "observer-capabilities", "reset-tables"])

def test_preflight_denial_never_resets(self):
    def deny(args, kwargs):
        if args[-1:] == ["observer-capabilities"]:
            return subprocess.CompletedProcess(args, 1, json.dumps({
                "engine":"mysql", "mode":"observer-capabilities", "result":"failed",
                "stage":"grants", "error_class":"shape_rejected", "writes_executed":False}), "")
    self.inject = deny
    result = self.run_mock(observer_authorizations=self.synthetic_authorizations)
    self.assertEqual(result["status"], "failed")
    self.assertFalse(result["engines"]["mysql"]["reset_attempted"])
    self.assertFalse(any(args[-1:] == ["reset-tables"] or "--exact" in args for args,_ in self.calls))
```

- [ ] **Step2：RED命令。** `python3 -m unittest discover -s .superpowers/dev-db-probe -p 'test_observer_runner.py'`。失败必须合法mock链不通过，不是缺属性或fixture未构造。补setUp/fake_run的observer协议分支后才记录RED。
- [ ] **Step3：GREEN。** authorize→双pin/private→原writer preflight→O preflight→reset→wrapper；每例重新开fd/校验，不能前例成功后缓存授权能力。case_env只显式加入本次O fd引用、双pin和无秘密授权绑定，不加入URL/password。新observer spawn helper 经独立受信 Cargo 构建记录核对 artifact 路径/sha256/owner/inode/mode，以 no-follow 打开的 executable fd 固定 inode，调用 `Popen(args=[binary,engine,'observer-capabilities'],executable='/proc/self/fd/<artifact_fd>',pass_fds=(writer_fd,observer_fd,artifact_fd),...)`；第三 fd 仅供执行目标固定，O 配置 fd仍独立验证/关闭。组可写工作树或可由同UID原位改写的 artifact **不得因此放行 live**，需受信不可修改的部署位置；不能从待执行二进制自己计算指纹当成独立构建证据。旧模式无 fd转交，O6 后续任何 migration child 前也必须关闭 artifact fd。cargo/list/reset/migration/普通cases均0 O引用/0 O fd。新mode输出严格键与bool校验，错engine/mode/字段/未知label/非零返回码拒绝，不调用旧probe去接受它。
- [ ] **Step4：cleanenv及进程fake矩阵。** 注入 `RSETUP_OBSERVER_URL/PATH/WRITER_FD/OBSERVER_FD/WRITER_PIN/OBSERVER_PIN`、`RUST_LOG`、`SQLX_LOG`、原DATABASE_URL等，assert clean_environment全去除；SAFE_ENV不扩。env变量前缀在实现中固定为 `RSETUP_OBSERVER_`；在四项 O 配置 FD/pin 字段之外，仅新 observer child 再加入 `RSETUP_OBSERVER_ARTIFACT_FD` 的非秘密整数用于关闭已继承的第三 executable fd；不接受它作为授权/构建证明，raw adoption 前先核对三 fd 均有效/互异。其他继承环境绝不信任。敏感auth metadata只存本次进程内/专用stdin，不作为命令行参数；stdin envelope缺失、错误scope/窗口/pin时即使有合法O fd也不得连接。检查child完成后fd关闭；migration child在Command构造器上逐个 `env_remove` 这五个变量并用 `stdin(Stdio::null())`，且已关闭配置fd和artifact fd。不要在Rust2024多线程Tokio进程调用unsafe `std::env::remove_var`。为此只给现有 `run_explicit_identity_test_command` 的子进程构造增加这五个删除项与null stdin，不改mode、writer环境或授权门禁，也不改其callers。error output含合成秘密/编码变体也不得进入报告。用户新增的父进程中断裁决：在私有 fd 获取/登记、原 artifact fd 移交等窗口遭 `KeyboardInterrupt`/`SystemExit`，已登记 fd 仅关闭一次、已启动child尽力kill/reap后**立即进程级退出**，内核回收未登记 fd；不能把中断转成可捕获的普通错误后继续reset/下个引擎。该路径可能没有 JSON/结果日志，fixture状态未知且必须人工核对/重新授权，不得声称本函数正常关闭所有 raw fd。所有相关注入测试在隔离测试子进程执行，不能杀主测试进程。

```python
def test_clean_environment_never_inherits_observer_secrets(self):
    unsafe={"RSETUP_OBSERVER_URL":"fixture-secret", "RSETUP_OBSERVER_PATH":"fixture-secret",
            "RSETUP_OBSERVER_WRITER_PIN":"fixture-secret", "RUST_LOG":"trace", "SQLX_LOG":"trace"}
    with patch.dict(os.environ, unsafe): env=self.runner.clean_environment()
    self.assertTrue(set(unsafe).isdisjoint(env))
```

- [ ] **Step5：两库stop与timeout。** 构造mysql失败+tidb合法的两engine mock execute；断言无tidb probe/reset/case、tidb记录not_run。覆盖首/第二例preflight失败、双pin替换、expired topology、transport缺失、capability45s超时、case300s父超时、中断、Popen失败、wrong summary；0/1执行计数不混淆。复用原killpg/drain行为fake证明组杀和reap发生，不用真实sleep。任何失败后reset次数不再增长；不生成伪passed引擎。新observer批次缺授权也须双库停，不受旧engine内catch继续行为影响。
- [ ] **Step6：独立审查/交付。** `python3 -m unittest discover -s .superpowers/dev-db-probe -p 'test_*.py'`；报告原runner回归与observer新增测试分别计数。父代理审查授权输入不自授、reset_attempted标记时点、fixture保持未知/已保留区分。不提交ignored tools。

## O6：真实case接线、必要拆分及offline-ready汇总

**Files:** Create `tests/common/task4_concurrency.rs`；Modify common module声明/重导出、mysql/tidb四wrapper；contract补行为/源码冻结辅助测试。本地runner四名allowlist与case discovery测试同步。

**Interfaces:** common原两个公有测试helper迁移后增加消费 `PreparedObserver`/授权绑定，不改变生产CAS签名；明确 `pub async fn admission_deactivation_holds_guard_cas_waits_then_denied(db:&DbPool,engine:ExpectedIdentityEngine,prepared:PreparedObserver)` 及反序同签名。wrapper先consume O inherited fd并关闭/清除传递信息，才执行原fixture upgrade；实际sessions仍在fixture之后重新验证。test-only共享协调器在session模块定义原生async trait `EdgeSampler { async fn sample(&mut self)->Result<bool,ObserverError>; }`（不使用async-trait），以及 `async fn await_positive_edge<T,S:EdgeSampler>(sampler:&mut S,cas:&mut tokio::sync::oneshot::Receiver<T>,deadline:tokio::time::Instant)->Result<(),ObserverError>`。真实O adapter实现sample并持有已验证O、schema、actor IDs；Case A必须调用该协调器成功后才tx.commit，真实waiter仍直接b.compare_and_set。没有生产CAS替身或库API变化。

原位替换的四个完整名称（只加 `_observer`，不是再复制四例）：

```text
admission_deactivation_holds_guard_cas_waits_then_denied_on_fresh_v3_mysql_observer
admission_cas_commits_before_deactivation_then_new_cas_denied_on_fresh_v3_mysql_observer
admission_deactivation_holds_guard_cas_waits_then_denied_on_fresh_v3_tidb_observer
admission_cas_commits_before_deactivation_then_new_cas_denied_on_fresh_v3_tidb_observer
```

- [ ] **Step1：可编译接线RED。** 先迁出原Task4 1060–1494行必要helpers（不改业务断言），共享协调器暂固定拒绝；下面合法edge断言失败为RED，错误/提前CAS仍安全拒绝。fake直接实现真实协调器的EdgeSampler，不能另建测试专用状态机。辅助源码审查确认Case A仅在协调器Ok后commit，传入的是O adapter而不是A tx。

```rust
struct FakeSampler { edge:bool, fail:bool }
impl observer::EdgeSampler for FakeSampler {
    async fn sample(&mut self)->Result<bool,&'static str> {
        if self.fail { Err("query_error") } else { Ok(self.edge) }
    }
}
#[tokio::test]
async fn wiring_requires_o_edge_before_holder_commit() {
    use tokio::{sync::oneshot,time::{Instant,Duration}};
    let (_tx,mut rx)=oneshot::channel::<()>();
    let mut sampler=FakeSampler{edge:true,fail:false};
    assert_eq!(observer::await_positive_edge(&mut sampler,&mut rx,
        Instant::now()+Duration::from_millis(30)).await,Ok(()));
    sampler.edge=false;
    assert!(observer::await_positive_edge(&mut sampler,&mut rx,
        Instant::now()+Duration::from_millis(5)).await.is_err());
    let (tx,mut early)=oneshot::channel::<()>(); tx.send(()).unwrap();
    sampler.edge=true;
    assert!(observer::await_positive_edge(&mut sampler,&mut early,
        Instant::now()+Duration::from_millis(30)).await.is_err());
    sampler.fail=true;
    assert!(observer::await_positive_edge(&mut sampler,&mut rx,
        Instant::now()+Duration::from_millis(30)).await.is_err());
}
```

- [ ] **Step2：RED命令。** `cargo test --offline --locked -p rsetup-controller --test observer_contract wiring_requires_o_edge_before_holder_commit -- --exact`。GREEN使用biased select优先检测CAS完成/通道关闭，反复有界sample直到positive，返回Ok前再try_recv必须Empty；100ms限流同受总deadline约束。该函数本身不commit，真实caller在Ok后再次检查并commit；fake不能证明真实数据库调度，O7负责该证据。
- [ ] **Step3：GREEN真实接线。** A/B callback保留database/version/ID/禁重连、MySQL UUID、TiDB真实session pessimistic/global-ID，移除三张P_S/两张TiDB诊断扫描至O。O连接不得包装DbPool。Case A持A tx同时await独立O采样，检查CAS未提前返回再commit；Case B原两次commit顺序与持久断言保持。给每个错误路径结构化取消、O关闭、A rollback/池关闭并固定错误，不能panic前遗留spawn任务。原task4_event_rows/meta/fixture支持仅按必要可见性供子模块使用。
- [ ] **Step4：wrapper及发现编译核对。** 两engine各两名都ignored；原四名不再出现在source_cases/--list。wrapper顺序必须 `受控 ignored test 子进程入口接管raw fd为OwnedFd → load_inherited/close fd → 原required_fresh_identity_db → 原upgrade → strict_v3 → observer并发helper`。本地runnerfake发现实源码四名，旧名选择拒绝，suffix近似名拒绝；没有O creds运行普通tests无网络，明确运行ignored wrapper缺O授权必须在DB fixture之前拒绝（子进程fake test）。对compile-only二进制执行 --list不注入任何DB/O env。

```bash
cargo test --offline --locked -p rsetup-controller --test observer_contract
cargo test --offline --locked -p rsetup-controller --test mysql_identity --no-run
cargo test --offline --locked -p rsetup-controller --test tidb_identity --no-run
cargo test --offline --locked --manifest-path .superpowers/dev-db-probe/Cargo.toml
python3 -m unittest discover -s .superpowers/dev-db-probe -p 'test_*.py'
cargo +1.85.0 test --offline --locked -p rsetup-controller --test observer_contract
cargo fmt --all -- --check
cargo clippy --offline --locked -p rsetup-controller --all-targets -- -D warnings
```

- [ ] **Step5：独立冻结与secret审查。** 审查src/manifest/lockfile全部零diff、writer旧函数与所有calls零diff（probe本地文件以起始读取快照比较，不只git）；新SQL无DATABASE过滤、无泄漏敏感投影，A/B无诊断扫描；tracked模块路径引用编译成立。无真实credentials/SQL运行记录时不得写PASS/live；纯源码检查只能辅证call wiring，真实wait边在O7。
- [ ] **Step6：offline-ready交付。** 新本地实施报告逐任务写RED目标断言、GREEN命令/非零用例数、MSRV/编译范围、两阶段审查、tracked候选与ignored本地文件分列。结论仅 `offline-ready; Task4 live acceptance pending`。O凭据缺失不阻止O1–O6结束。父协调代理在其授权范围内统一提交tracked support/wrappers/docs，实现者无stage/commit；不将ignored helper缺失隐瞒为可独立从git重现live runner。

## O7：仅另获授权后进入的live验收（当前不得执行）

**Files:** 只产生本地去敏结果与后续已授权报告；不自动改代码或申请权限。**Consumes:** O1–O6审查完成、实际操作者准备的身份/权限、每份新secret读取授权与pin、当前窗口传输策略/拓扑核验、原target/独占/真实backup/ACK以及独立live操作授权。

- [ ] **Gate L1：身份资料授权。** 操作者明确指认每引擎O私有配置路径/当次内容pin/用途与对应原目标；分别允许读取。实际SHOW GRANTS不同shape、CURRENT_ROLE不确定、新TLS字段需求等均停止另审，不宽松parser、不授GRANT。
- [ ] **Gate L2：真实只读preflight。** 另获只读连接授权后先运行独立observer-capabilities、原writer preflight；确认三会话目标/身份/roles/真实扫描。TiDB拓扑记录绑定当次窗口且实际三session global-ID满足；MySQL实例UUID满足。未通过不得reset。一引擎失败停两库；保留去敏stage/class，不抄原输出。
- [ ] **Gate L3：reset/fixture/四例许可。** 另获逐例reset/迁移/并发执行授权后，父协调代理才调用safe runner指定四名；不直接cargo --ignored越过runner。每例再做O preflight，原reset-tables→upgrade→v3→实际A/B/O重验→业务断言；任何失败停止两个目标且保留fixture，不继续下一例/下一库。
- [ ] **Gate L4：真实证据审查。** MySQL8.0.46/TiDB8.0.11-TiDB-v8.5.8分别四名中的各两例必须唯一汇总1passed/0failed/0ignored；Case A必须有O B→A正向边及PermissionDenied/不变快照，Case B两次commit顺序与持久性。未运行、空扫描、只见pending/STATE、sleep后CAS拒绝均不计通过。精确版本/实际用例数/首失败/未执行分开写；不声称MySQL8.4或整个controller完成。

O7没有默认执行命令包含真实路径/凭据，也不由离线fake生成可供它消费的授权票据。其所有未满足项是external blockers；本计划不要求实现者在缺Ocredentials时无限等待或越权补齐。

## 主代理自查清单

- [ ] 需求覆盖：grammar/writer冻结→O1；双pin/private/endpoint/TLS边界→O2；identity/roles/topology/SQL/timeout→O1/O3；probe输出→O4；cleanenv/reset-before/两库停→O5；真实wrappers/取消/拆分/MSRV→O6；live授权分层→O7。
- [ ] 所有RED可编译、无DB；fake调用真实协调器/catalog而非平行模型；无生产API或依赖新增。
- [ ] 没有把前置条件当implementation TODO，没有新sysvar/mysql.tidb权限/exactTLS承诺。
- [ ] 所有真实执行等待授权；主代理之后按已选子代理方式实施，不再询问模式，不让实现者stage/commit。
