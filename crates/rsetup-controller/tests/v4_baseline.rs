//! v4-only 数据库基线的离线回归：不连数据库、不读凭据、不写任何真实目标。
//!
//! 覆盖四件事：
//! 1. 旧 schema 载体确实消失（`migrations/` 目录、legacy 迁移 CLI、旧符号）；
//! 2. 唯一 fresh DDL `schema/v4.sql` 的形状契约（恰好 16 表、无 FOREIGN KEY、无 CHECK）；
//! 3. 基线 SQL 的**注释形态**：只允许整行注释（行内注释会被形状解析并入下一个声明片段，
//!    在真实库上伪装成"v4 schema DDL missing column …"）；
//! 4. 唯一 schema 入口 `rsetup-controller-migrate` 的 argv 契约：legacy 模式一律拒绝、
//!    非法 UTF-8 不 panic 也不回显、门禁不全时在连接之前固定失败且不回显任何凭据。
//!
//! 真实库上的"16 表 / 0 FK / 0 CHECK / version=4"只读核验在
//! [`v4_schema_readonly_verify`](v4_schema_readonly_verify.rs)（`#[ignore]`，需操作员授权）。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const V4_SQL: &str = include_str!("../schema/v4.sql");
/// 测试专用的假口令，只用于断言"任何输出都不回显凭据"；不是任何真实库的凭据。
const FAKE_SECRET: &str = "Fa1ePassw0rd-never-real";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_source(relative: &str) -> String {
    fs::read_to_string(manifest_dir().join(relative)).unwrap_or_else(|error| {
        panic!("读取 {relative} 失败：{error}");
    })
}

fn migrate_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rsetup-controller-migrate"));
    // 清空环境：入口只允许从环境读配置，任何继承来的真实变量都可能把测试变成真连库。
    command.env_clear();
    command
}

fn run(command: &mut Command) -> Output {
    command
        .output()
        .expect("rsetup-controller-migrate 二进制必须可执行")
}

fn assert_deidentified(output: &Output) {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !text.contains(FAKE_SECRET),
        "输出回显了环境中的凭据值：{text}"
    );
    assert!(!text.contains("mysql://"), "输出回显了连接串：{text}");
    assert!(
        !text.contains("panicked"),
        "入口不得以 panic 处理任何输入：{text}"
    );
}

#[test]
fn legacy_schema_carriers_are_gone() {
    // 逐条 SQL 的旧迁移目录已被单文件 v4 基线取代。
    assert!(
        !manifest_dir().join("migrations").exists(),
        "v4-only 基线不得保留 migrations/ 旧迁移目录"
    );
    // 旧测试迁移 CLI 的源文件与注册都必须消失（否则 cargo 仍会构建出 legacy 模式入口）。
    assert!(
        !manifest_dir()
            .join("src/bin/migrate-identity-test.rs")
            .exists(),
        "legacy 迁移 CLI 源文件必须删除"
    );
    let manifest = read_source("Cargo.toml");
    assert!(
        !manifest.contains("migrate-identity-test\npath"),
        "Cargo.toml 不得再注册 legacy 迁移 CLI"
    );
    // 生产代码里不得再有任何按旧版本分支的迁移/放行路线。
    for (file, forbidden) in [
        (
            "src/db.rs",
            &[
                "TestMigrationMode",
                "check_v3_ready",
                "run_identity_test_migration",
                "create_historical_v",
                "0001_identity_devices.sql",
                "0004_tasks.sql",
            ][..],
        ),
        (
            "src/integrity.rs",
            &["MIN_SUPPORTED_SCHEMA_VERSION", "legacy_expected_version"][..],
        ),
    ] {
        let source = read_source(file);
        for token in forbidden {
            assert!(!source.contains(token), "{file} 仍含旧版本路线标记 {token}");
        }
    }
    // 库导出面同样不得再暴露 legacy 入口。
    let lib = read_source("src/lib.rs");
    assert!(
        !lib.contains("check_v3_ready"),
        "lib.rs 仍导出 check_v3_ready"
    );
    assert!(
        !lib.contains("TestMigrationMode"),
        "lib.rs 仍导出 TestMigrationMode"
    );
}

#[test]
fn v4_sql_is_the_only_baseline_and_is_fk_and_check_free() {
    // 独立解析一遍文件文本…
    let body = V4_SQL
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    let statements: Vec<&str> = body
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .collect();
    assert_eq!(statements.len(), 16, "v4 基线必须恰好 16 条 CREATE");
    let creates = statements
        .iter()
        .filter(|statement| statement.starts_with("CREATE TABLE IF NOT EXISTS "))
        .count();
    assert_eq!(creates, 16, "v4 基线只能由 CREATE TABLE IF NOT EXISTS 组成");
    // 只判定"去掉整行注释之后"的可执行文本：文件头注释会写"无 FOREIGN KEY、无 CHECK"，
    // 那是对契约的描述，不是约束本身。
    let upper = body.to_ascii_uppercase();
    assert!(
        !upper.contains("FOREIGN KEY"),
        "v4 基线不得声明 FOREIGN KEY"
    );
    assert!(!upper.contains("REFERENCES"), "v4 基线不得声明 REFERENCES");
    assert_eq!(
        upper.matches("CHECK (").count(),
        0,
        "v4 基线不得声明 CHECK 约束"
    );
    assert!(!upper.contains("ALTER TABLE"), "v4 基线不得包含 ALTER 语句");
    // …并与生产解析器逐项对账：两侧对"16 条、表名、顺序"的理解必须完全一致。
    let parsed = rsetup_controller::db::v4_schema_statements().expect("生产解析器必须接受 v4 基线");
    let parsed_names: Vec<&str> = parsed.iter().map(|(table, _)| *table).collect();
    assert_eq!(parsed_names.len(), 16);
    for (index, table) in [
        "schema_meta",
        "users",
        "sessions",
        "roles",
        "role_permissions",
        "device_groups",
        "devices",
        "group_members",
        "grants",
        "admission_decisions",
        "audit_events",
        "task_previews",
        "main_tasks",
        "sub_tasks",
        "device_operation_locks",
        "recovery_checks",
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            parsed_names[index],
            table,
            "第 {} 张表应为 {table}，实际 {}",
            index + 1,
            parsed_names[index]
        );
        assert!(
            V4_SQL.contains(&format!("CREATE TABLE IF NOT EXISTS {table} (")),
            "v4 基线缺少表 {table}"
        );
    }
}

/// 独立扫描 SQL 文本，返回**行内注释**命中的 `(1 基行号, 该行内容)`。
///
/// 这是与生产门禁（`db::v4_comment_form_check`）互相独立的第二双眼睛：字符串字面量内的
/// `--` 不算注释，行首（前只有空白）的整行注释不算行内注释。逐行扫描依赖"基线不含跨行
/// 字符串字面量"这一前提，该前提由下面的用例用断言钉住。
fn inline_comment_hits(sql: &str) -> Vec<(usize, String)> {
    let mut hits = Vec::new();
    for (index, line) in sql.lines().enumerate() {
        let bytes = line.as_bytes();
        let mut cursor = 0;
        let mut in_string = false;
        let mut code_before = false;
        while cursor < bytes.len() {
            let byte = bytes[cursor];
            if in_string {
                if byte == b'\\' {
                    cursor += 1;
                } else if byte == b'\'' {
                    if bytes.get(cursor + 1) == Some(&b'\'') {
                        cursor += 1;
                    } else {
                        in_string = false;
                    }
                }
                cursor += 1;
                continue;
            }
            match byte {
                b'\'' => {
                    in_string = true;
                    code_before = true;
                    cursor += 1;
                }
                b'-' if bytes.get(cursor + 1) == Some(&b'-') => {
                    if code_before {
                        hits.push((index + 1, line.trim().to_owned()));
                    }
                    break;
                }
                _ => {
                    if !byte.is_ascii_whitespace() {
                        code_before = true;
                    }
                    cursor += 1;
                }
            }
        }
    }
    hits
}

#[test]
fn v4_sql_comments_are_whole_line_only() {
    // 扫描前提（钉住，避免以后加了跨行字面量还让本用例假绿）：基线不出现字符串字面量。
    assert!(
        !V4_SQL.contains('\''),
        "v4 基线出现字符串字面量，本用例的逐行注释扫描前提失效"
    );
    // 行内注释命中必须为零：整行注释（含缩进行）不计入。
    assert_eq!(
        inline_comment_hits(V4_SQL),
        Vec::<(usize, String)>::new(),
        "v4 基线只允许整行注释：列的说明必须写在对应列的上一整行"
    );
    // 去整行注释后的可执行文本里不得残留任何注释标记。
    let body = V4_SQL
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!body.contains("--"), "可执行文本残留 -- 注释标记");
    assert!(!body.contains('#'), "可执行文本残留井号注释标记");
    assert!(
        !body.contains("/*") && !body.contains("*/"),
        "可执行文本残留块注释标记"
    );
    // 交叉验证生产解析器：它接受基线，且返回的可执行片段里同样不得含注释标记
    // （注释被并入声明片段正是这次真实库失败指纹）。
    let parsed = rsetup_controller::db::v4_schema_statements().expect("生产解析器必须接受 v4 基线");
    assert_eq!(parsed.len(), 16, "生产解析器必须解析出 16 条 CREATE");
    for (table, statement) in &parsed {
        assert!(
            !statement.contains("--") && !statement.contains("/*") && !statement.contains('#'),
            "{table} 的 DDL 片段残留注释标记，形状解析会把注释当成声明的一部分"
        );
    }
}

#[test]
fn legacy_cli_modes_are_rejected_before_any_connection() {
    // 旧入口的全部模式：都必须当作未知参数拒绝，且在任何配置读取与连接之前退出。
    for legacy in [
        vec!["--mode", "upgrade"],
        vec!["--mode", "fixture-v1"],
        vec!["--mode", "fixture-partial-v1"],
        vec!["--mode", "fixture-v2"],
        vec!["--mode"],
        vec!["--check", "--apply"],
        vec!["--check", "--mode", "upgrade"],
        vec!["--dsn", "mysql://x/y"],
        vec![],
    ] {
        let output = run(migrate_command().args(&legacy));
        assert!(!output.status.success(), "参数 {legacy:?} 必须被拒绝");
        let text = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(
            text.contains("unexpected arguments"),
            "拒绝必须是固定文案，实际：{text}"
        );
        assert!(text.is_ascii(), "拒绝文案必须纯 ASCII：{text}");
        // 未知模式绝不回显收到的 token。
        assert!(!text.contains("--mode"), "不得回显参数 token：{text}");
    }
}

#[test]
fn non_utf8_argv_is_rejected_without_panic_or_echo() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    for raw in [
        OsStr::from_bytes(b"\xff\xfe"),
        OsStr::from_bytes(b"--mode\xFF"),
        OsStr::from_bytes(b"\x80\x81\x82"),
    ] {
        let output = run(migrate_command().arg(raw));
        assert!(
            !output.status.success(),
            "非法 UTF-8 参数必须被拒绝而不是 panic"
        );
        assert_ne!(
            output
                .status
                .code()
                .expect("入口必须以退出码结束，不得被信号终止"),
            101,
            "退出码 101 说明标准库因非法 UTF-8 argv panic"
        );
        assert_deidentified(&output);
    }
}

#[test]
fn incomplete_gates_fail_fixed_before_connecting_and_never_echo_credentials() {
    // 门禁缺一即固定失败：这些用例都不会连库（配置/门禁在连接之前），因此可离线运行。
    let fake_url = format!("mysql://svc:{FAKE_SECRET}@db.invalid:3306/rsc_prod_ctl");
    let complete = [
        ("CONTROLLER_DATABASE_URL", fake_url.as_str()),
        ("CONTROLLER_MIGRATION_ALLOW_DESTRUCTIVE", "1"),
        ("CONTROLLER_MIGRATION_EXPECTED_DATABASE", "rsc_prod_ctl"),
        (
            "CONTROLLER_MIGRATION_BACKUP_REF",
            "/srv/backup/FAKE-ref-hashes.sha256",
        ),
        (
            "CONTROLLER_MIGRATION_ACK",
            "isolated-exclusive-backed-up-disposable",
        ),
    ];
    for missing in [
        &complete[..1],
        &complete[..2],
        &complete[..3],
        &complete[..4],
        &complete[1..],
    ] {
        let output = run(migrate_command()
            .arg("--apply")
            .envs(missing.iter().copied()));
        assert!(
            !output.status.success(),
            "门禁不全（{} 项）必须拒绝",
            missing.len()
        );
        assert_deidentified(&output);
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).trim(),
            "migrate-controller: configuration: production migration authorization required",
            "门禁失败必须是唯一固定文案（{} 项）",
            missing.len()
        );
    }
}

#[test]
fn check_without_configuration_fails_without_echoing_anything_secret() {
    let output = run(migrate_command().arg("--check"));
    assert!(
        !output.status.success(),
        "缺 CONTROLLER_DATABASE_URL 必须拒绝"
    );
    assert_deidentified(&output);
}

#[test]
fn help_documents_the_v4_only_contract() {
    let output = run(migrate_command().arg("--help"));
    assert!(output.status.success(), "--help 必须成功退出");
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        text.contains("v4-only"),
        "--help 必须写明 v4-only 语义：{text}"
    );
    assert!(
        text.contains("no upgrade path"),
        "--help 必须写明没有升级路线：{text}"
    );
    assert!(
        text.contains("CONTROLLER_MIGRATION_ACK"),
        "--help 必须列出确认门禁"
    );
}

#[test]
fn startup_path_stays_read_only_and_the_entry_point_is_the_only_writer() {
    // 普通服务启动必须零 DDL：main.rs 只调用只读门禁，不引用任何建库/迁移入口。
    let main = read_source("src/main.rs");
    assert!(
        main.contains("check_identity_schema"),
        "普通启动必须继续调用只读门禁 check_identity_schema"
    );
    assert!(
        !main.contains("run_production_migration") && !main.contains("create_v4_schema"),
        "普通启动绝不得调用显式建库/迁移入口"
    );
    // db.rs 内 v4 建库只由显式入口链路调用（run_production_migration_at）。
    let db = read_source("src/db.rs");
    let callers: Vec<&str> = db
        .lines()
        .filter(|line| line.contains("create_v4_schema(") && !line.contains("async fn "))
        .collect();
    assert_eq!(
        callers.len(),
        1,
        "v4 建库必须只有一个调用点（显式入口），实际：{callers:?}"
    );
    let schema_dir = manifest_dir().join("schema");
    assert!(
        Path::new(&schema_dir).is_dir() && schema_dir.join("v4.sql").is_file(),
        "唯一 fresh DDL 必须固定位于 schema/v4.sql"
    );
}
