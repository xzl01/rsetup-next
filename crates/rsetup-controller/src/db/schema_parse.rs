use crate::ControllerError;

use super::V4_SCHEMA_SQL;
use super::schema_shape::{RUNTIME_TABLES, TABLES};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct IndexMeta {
    pub(super) name: String,
    pub(super) columns: Vec<String>,
    pub(super) unique: bool,
    pub(super) sub_parts: Vec<Option<i64>>,
}

pub(super) fn validate_indexes(
    table: &str,
    expected: &[IndexMeta],
    actual: &[IndexMeta],
) -> Result<(), ControllerError> {
    if expected.len() != actual.len() {
        return Err(ControllerError::Config(format!(
            "migration incompatible index set in {table}"
        )));
    }
    for index in expected {
        if !actual.iter().any(|found| found == index) {
            return Err(ControllerError::Config(format!(
                "migration incompatible index {table}.{}",
                index.name
            )));
        }
    }
    Ok(())
}

fn ddl_parts_from(migration: &str, table: &str) -> Vec<String> {
    let prefix = format!("CREATE TABLE IF NOT EXISTS {table} (");
    let ddl = migration
        .split(';')
        .find(|statement| statement.trim().starts_with(&prefix))
        .expect("table declared in migration")
        .trim();
    let body = ddl
        .strip_prefix(&prefix)
        .unwrap()
        .rsplit_once(") CHARACTER SET")
        .unwrap()
        .0;
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut quoted = false;
    let mut start = 0;
    for (index, c) in body.char_indices() {
        match c {
            '\'' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
                parts.push(body[start..index].trim().to_owned());
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(body[start..].trim().to_owned());
    parts
}

/// `schema/v4.sql` 去掉整行 `--` 注释后的纯 DDL 文本。形状校验的列/索引声明全部
/// 从这里解析，因此注释绝不影响契约解析结果。**只支持整行注释**：行内注释由
/// [`v4_comment_form_check`] 在使用前直接拒绝。
pub(super) fn v4_schema_body() -> String {
    V4_SCHEMA_SQL
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// v4 基线的注释形态门禁：**只支持整行 `--` 注释**。
///
/// 出现行内 `--`、任何 `#` 注释或 `/* … */` 块注释都直接 fail-closed。原因是
/// [`ddl_parts_from`] 只按顶层逗号切分片段，行内注释会被并入**下一个**片段，使该片段以
/// 注释文本开头，于是形状校验报出 `v4 schema DDL missing column <表>.<列>` 这种"编译期基线
/// 与自身解析契约不兼容"的错误（真实 MySQL 8.0 只读核验正是这样失败的）。修复方式是修基线
/// SQL 的注释布局，而不是把这里扩成通用 SQL 解释器——本函数只识别 SQL 注释的三个起始标记，
/// 字符串字面量内的同名标记不算注释，绝不把合法 SQL 文本误判成注释。
pub(super) fn v4_comment_form_check(sql: &str) -> Result<(), ControllerError> {
    let bytes = sql.as_bytes();
    let mut index = 0;
    let mut line_has_code = false;
    let mut in_string = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            match byte {
                b'\\' => index += 1,
                b'\'' => {
                    if bytes.get(index + 1) == Some(&b'\'') {
                        index += 1;
                    } else {
                        in_string = false;
                    }
                }
                _ => {}
            }
            index += 1;
            continue;
        }
        match byte {
            b'\n' => {
                line_has_code = false;
                index += 1;
            }
            b' ' | b'\t' | b'\r' => index += 1,
            b'\'' => {
                line_has_code = true;
                in_string = true;
                index += 1;
            }
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                if line_has_code {
                    return Err(v4_schema_error("unsupported inline comment"));
                }
                // 整行注释：整行跳到行尾，注释体不再参与扫描（注释里的引号也不误判）。
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'#' => return Err(v4_schema_error("unsupported comment form")),
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                return Err(v4_schema_error("unsupported comment form"));
            }
            _ => {
                line_has_code = true;
                index += 1;
            }
        }
    }
    if in_string {
        return Err(v4_schema_error("unterminated string literal"));
    }
    Ok(())
}

/// v4 唯一 fresh DDL 的白名单解析：16 条 `CREATE TABLE IF NOT EXISTS`，每条一个已知
/// 表名，顺序固定，绝不包含 FOREIGN KEY / REFERENCES / CHECK。
/// 任何偏差（少一条、多一条、重名、未知表、出现被禁止的约束）都说明编译期契约被改坏，
/// 直接 fail-closed，绝不"顺手"执行。
pub fn v4_schema_statements() -> Result<Vec<(&'static str, String)>, ControllerError> {
    let body = v4_schema_body();
    v4_comment_form_check(&body)?;
    parse_v4_statements(&body)
}

/// 纯解析器（输入已是去注释后的 DDL）：便于离线测试注入被篡改的文本来验证拒绝路径。
fn parse_v4_statements(sql: &str) -> Result<Vec<(&'static str, String)>, ControllerError> {
    let known = |name: &str| {
        TABLES
            .iter()
            .map(|(table, _, _)| *table)
            .chain(RUNTIME_TABLES.iter().map(|(table, _, _)| *table))
            .find(|table| *table == name)
    };
    let mut statements = Vec::new();
    for statement in sql
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
    {
        let table = statement
            .strip_prefix("CREATE TABLE IF NOT EXISTS ")
            .and_then(|rest| rest.split_once(" ("))
            .map(|(name, _)| name)
            .ok_or_else(|| v4_schema_error("baseline SQL shape"))?;
        let upper = statement.to_ascii_uppercase();
        let Some(known) = known(table) else {
            return Err(v4_schema_error("baseline SQL table whitelist"));
        };
        if statements.iter().any(|(seen, _)| *seen == known)
            || upper.contains("FOREIGN KEY")
            || upper.contains("REFERENCES")
            || upper.contains("CHECK (")
        {
            return Err(v4_schema_error("baseline SQL constraint whitelist"));
        }
        statements.push((known, statement.to_owned()));
    }
    let expected = TABLES.len() + RUNTIME_TABLES.len();
    if statements.len() != expected {
        return Err(v4_schema_error("baseline SQL table count"));
    }
    Ok(statements)
}

pub(super) fn v4_schema_error(code: &'static str) -> ControllerError {
    // 固定去敏文案：只含编译期字面量，绝不拼接 SQL 片段、表之外的标识符或任何凭据。
    ControllerError::Config(format!("v4 schema baseline rejected: {code}"))
}

/// 解析 v4 DDL 中某张表的列/索引声明片段（已剥离整行注释）。
/// 前置条件：调用方必须先通过 [`v4_comment_form_check`]（生产两条入口
/// [`validate_v4_shape`] 与 [`v4_schema_statements`] 都已执行），否则行内注释会被并入片段。
pub(super) fn ddl_parts(table: &str) -> Vec<String> {
    ddl_parts_from(&v4_schema_body(), table)
}

pub(super) fn expected_indexes(
    parts: &[String],
    names: &[&str],
) -> Result<Vec<IndexMeta>, ControllerError> {
    let mut expected = Vec::new();
    for &name in names {
        let definition = parts
            .iter()
            .find(|part| {
                if name == "PRIMARY" {
                    part.contains("PRIMARY KEY")
                } else {
                    part.contains(&format!(" {name} ("))
                }
            })
            .ok_or_else(|| {
                ControllerError::Config(format!("migration DDL missing index {name}"))
            })?;
        let columns = if name == "PRIMARY" && !definition.starts_with("PRIMARY KEY (") {
            vec![definition.split_whitespace().next().unwrap().to_owned()]
        } else {
            definition
                .split_once('(')
                .unwrap()
                .1
                .split_once(')')
                .unwrap()
                .0
                .split(',')
                .map(|s| s.trim().to_owned())
                .collect()
        };
        expected.push(IndexMeta {
            name: name.to_owned(),
            sub_parts: vec![None; columns.len()],
            columns,
            unique: name == "PRIMARY" || definition.contains("UNIQUE KEY"),
        });
    }
    Ok(expected)
}

#[cfg(test)]
mod tests {
    use super::super::schema_shape::{ColumnMeta, validate_column_charset, validate_column_shape};
    use super::*;

    #[test]
    fn v4_baseline_sql_is_sixteen_tables_without_checks_or_foreign_keys() {
        let statements = v4_schema_statements().expect("v4 基线 SQL 必须可解析");
        assert_eq!(statements.len(), 16, "v4 基线必须恰好 16 条 CREATE");
        let seen: Vec<&str> = statements.iter().map(|(table, _)| *table).collect();
        let mut expected: Vec<&str> = TABLES.iter().map(|(table, ..)| *table).collect();
        expected.extend(RUNTIME_TABLES.iter().map(|(table, ..)| *table));
        assert_eq!(seen, expected, "v4 基线表集合与顺序必须与契约常量逐项一致");
        for (table, statement) in &statements {
            assert!(
                statement.starts_with(&format!("CREATE TABLE IF NOT EXISTS {table} (")),
                "{table} 的 DDL 必须以受控 CREATE 前缀开始"
            );
            let upper = statement.to_ascii_uppercase();
            for forbidden in ["FOREIGN KEY", "REFERENCES", "CHECK ("] {
                assert!(
                    !upper.contains(forbidden),
                    "v4 基线不得声明 {forbidden}：{table}"
                );
            }
            assert!(
                statement
                    .lines()
                    .all(|line| !line.trim_start().starts_with("--")),
                "{table} 的 DDL 不得残留整行注释"
            );
        }
    }

    #[test]
    fn v4_baseline_parser_rejects_unknown_tables_and_constraints() {
        let base = v4_schema_body();
        let users_tail = "UNIQUE KEY uq_users_username (username)\n)";
        assert!(
            base.contains(users_tail),
            "用例前提：users DDL 尾部必须可定位，用于注入被禁止的约束"
        );
        let unknown = base.replace(
            "CREATE TABLE IF NOT EXISTS users (",
            "CREATE TABLE IF NOT EXISTS users_extra (",
        );
        assert_ne!(unknown, base);
        assert!(parse_v4_statements(&unknown).is_err(), "未知表名必须拒绝");
        let foreign_key = base.replace(
            users_tail,
            "UNIQUE KEY uq_users_username (username),\n  FOREIGN KEY (id) REFERENCES roles (id)\n)",
        );
        assert!(
            parse_v4_statements(&foreign_key).is_err(),
            "注入外键必须拒绝"
        );
        let check = base.replace(
            users_tail,
            "UNIQUE KEY uq_users_username (username),\n  CONSTRAINT ck_x CHECK (1 = 1)\n)",
        );
        assert!(parse_v4_statements(&check).is_err(), "注入 CHECK 必须拒绝");
        let duplicated = format!("{base}\n{base}");
        assert!(
            parse_v4_statements(&duplicated).is_err(),
            "重复建表（含多一条语句）必须拒绝"
        );
        let missing = base.replace(
            "CREATE TABLE IF NOT EXISTS recovery_checks (",
            "CREATE TABLE IF NOT EXISTS recovery_checks_unused (",
        );
        assert!(
            parse_v4_statements(&missing).is_err(),
            "少一张已知表必须拒绝"
        );
    }

    /// 注释形态门禁：v4 基线**只支持整行 `--` 注释**，行内注释、`#` 与 `/* … */` 一律
    /// fail-closed；字符串字面量里的同名标记不得被误判为注释。
    ///
    /// 这条门禁的存在理由来自真实缺陷：`schema/v4.sql` 曾在 `task_previews` 的列尾写行内
    /// 注释，`ddl_parts_from` 按顶层逗号切分后，下一个片段以注释文本开头，真实 MySQL 8.0
    /// 只读核验于是报出 `v4 schema DDL missing column task_previews.targets_json`。
    #[test]
    fn v4_schema_comments_must_be_whole_line_only() {
        // 基线自身两侧都必须通过：原始文件与去整行注释后的 DDL。
        v4_comment_form_check(V4_SCHEMA_SQL).expect("schema/v4.sql 的注释必须全部是整行注释");
        v4_comment_form_check(&v4_schema_body()).expect("去注释后的 DDL 不得再残留任何注释标记");

        // 行内注释：必须拒绝，且诊断是固定 ASCII 文案。
        let inline = "CREATE TABLE IF NOT EXISTS users (\n  id BINARY(16) NOT NULL PRIMARY KEY, -- 行内注释\n  display_name VARCHAR(255) NOT NULL\n) CHARACTER SET utf8mb4;";
        let error = v4_comment_form_check(inline).expect_err("行内注释必须 fail-closed");
        let message = error.to_string();
        assert!(message.is_ascii(), "固定诊断必须纯 ASCII，实际：{message}");
        assert!(
            message.contains("unsupported inline comment"),
            "行内注释必须有专属固定码，实际：{message}"
        );

        // 整行注释（含缩进）：必须放行。
        v4_comment_form_check(
            "-- 顶部注释\n  -- 缩进注释\nCREATE TABLE IF NOT EXISTS users (\n  id BINARY(16) NOT NULL PRIMARY KEY\n) CHARACTER SET utf8mb4;",
        )
        .expect("整行注释是 v4 基线唯一支持的注释形态");
        // 注释体里出现未配对引号也不能影响判定（整行注释整段跳过）。
        v4_comment_form_check(
            "-- 注释里有单引号 ' 与 -- 继续\nCREATE TABLE IF NOT EXISTS users (\n  id BINARY(16) NOT NULL PRIMARY KEY\n) CHARACTER SET utf8mb4;",
        )
        .expect("整行注释的注释体不参与扫描");

        // `#` 与块注释同样不受支持。
        for unsupported in [
            "CREATE TABLE IF NOT EXISTS users (\n  id BINARY(16) NOT NULL PRIMARY KEY\n) CHARACTER SET utf8mb4; # 尾注",
            "CREATE TABLE IF NOT EXISTS users (\n  id BINARY(16) /* 块注释 */ NOT NULL PRIMARY KEY\n) CHARACTER SET utf8mb4;",
        ] {
            let error = v4_comment_form_check(unsupported).expect_err("# 与块注释必须 fail-closed");
            assert!(
                error.to_string().contains("unsupported comment form"),
                "不受支持的注释形态必须有固定码，实际：{error}"
            );
        }

        // 字符串字面量里的 `--` 不是注释：不得把合法 SQL 文本误判。
        v4_comment_form_check(
            "CREATE TABLE IF NOT EXISTS users (\n  marker VARCHAR(8) NOT NULL DEFAULT '--'\n) CHARACTER SET utf8mb4;",
        )
        .expect("字符串字面量里的 -- 不是注释");
        // 字面量里的 `--` 之后仍要能识别真正的行内注释。
        let literal_then_comment = "CREATE TABLE IF NOT EXISTS users (\n  marker VARCHAR(8) NOT NULL DEFAULT '--', -- 真注释\n  id BINARY(16) NOT NULL PRIMARY KEY\n) CHARACTER SET utf8mb4;";
        assert!(v4_comment_form_check(literal_then_comment).is_err());
        // 未闭合字面量说明文本本身已损坏，同样拒绝。
        assert!(
            v4_comment_form_check(
                "CREATE TABLE IF NOT EXISTS users (\n  marker VARCHAR(8) NOT NULL DEFAULT 'oops\n) CHARACTER SET utf8mb4;"
            )
            .is_err(),
            "未闭合字符串字面量必须拒绝"
        );

        // 端到端：把行内注释注回基线，门禁必须在解析语句之前就拒绝（而不是解析出 16 条语句
        // 后在形状校验里伪装成"库里缺列"）。
        let polluted = v4_schema_body().replace(
            "  targets_json JSON NOT NULL,",
            "  targets_json JSON NOT NULL, -- x",
        );
        assert_ne!(polluted, v4_schema_body(), "用例前提：注入必须命中");
        assert!(
            v4_comment_form_check(&polluted).is_err(),
            "注入的行内注释必须被门禁拒绝"
        );
    }

    #[test]
    fn v4_baseline_sql_declares_every_contracted_table() {
        let statements = v4_schema_statements().expect("v4 基线 SQL 必须可解析");
        let seen: Vec<&str> = statements.iter().map(|(table, _)| *table).collect();
        for &(table, ..) in TABLES.iter().chain(RUNTIME_TABLES) {
            assert!(seen.contains(&table), "v4 基线缺少表 {table}");
        }
        assert_eq!(seen.len(), 16, "v4 基线必须恰好 16 条 CREATE");
    }

    /// 从 v4 DDL 的列声明片段推导"该列在 `information_schema` 里应当读回的元数据"。
    ///
    /// 只按 v4 契约受控的写法推导（类型/长度/`UNSIGNED`/`NOT NULL`/
    /// `CHARACTER SET … COLLATE …`/`DEFAULT FALSE|0`），任何推导不出来或推导后仍被生产
    /// 校验器拒绝的形状，都说明基线 SQL 与自身的严格解析契约不兼容。本函数**不放宽**任何
    /// 一项：非受控的 `DEFAULT` 值原样交给 [`validate_column_default`] 判定。
    fn ideal_metadata_from_declaration(
        table: &str,
        column: &str,
        declaration: &str,
        table_collation: &str,
    ) -> ColumnMeta {
        let tokens: Vec<&str> = declaration.split_whitespace().collect();
        let head = tokens
            .first()
            .unwrap_or_else(|| panic!("{table}.{column} 的列声明片段为空：{declaration:?}"))
            .to_ascii_lowercase();
        let (base, length) = match head.split_once('(') {
            Some((base, rest)) => (
                base.to_owned(),
                rest.strip_suffix(')')
                    .unwrap_or_else(|| panic!("{table}.{column} 的类型长度写法非法：{head}"))
                    .to_owned(),
            ),
            None => (head.clone(), String::new()),
        };
        let unsigned = tokens
            .iter()
            .any(|token| token.eq_ignore_ascii_case("UNSIGNED"));
        // MySQL 读回时 BOOLEAN 是 TINYINT 别名，data_type 与 column_type 都是 tinyint。
        let (data_type, bare_type) = if base == "boolean" {
            ("tinyint".to_owned(), "tinyint".to_owned())
        } else if length.is_empty() {
            (base.clone(), base.clone())
        } else {
            (base.clone(), format!("{base}({length})"))
        };
        let column_type = if unsigned {
            format!("{bare_type} unsigned")
        } else {
            bare_type
        };
        let nullable = !tokens.windows(2).any(|pair| pair == ["NOT", "NULL"]);
        let declared_charset = tokens
            .windows(3)
            .find(|pair| {
                pair[0].eq_ignore_ascii_case("CHARACTER") && pair[1].eq_ignore_ascii_case("SET")
            })
            .map(|pair| pair[2].to_owned());
        let declared_collation = tokens
            .windows(2)
            .find(|pair| pair[0].eq_ignore_ascii_case("COLLATE"))
            .map(|pair| pair[1].to_owned());
        let character_column = matches!(
            base.as_str(),
            "varchar" | "char" | "text" | "tinytext" | "mediumtext" | "longtext"
        );
        let (character_set_name, collation_name) = match declared_charset {
            Some(set) => (Some(set), declared_collation),
            None if character_column => {
                (Some("utf8mb4".to_owned()), Some(table_collation.to_owned()))
            }
            None => (None, None),
        };
        let default = match tokens
            .windows(2)
            .find(|pair| pair[0].eq_ignore_ascii_case("DEFAULT"))
            .map(|pair| pair[1])
        {
            Some(value) if value.eq_ignore_ascii_case("FALSE") || value == "0" => {
                Some("0".to_owned())
            }
            // 非受控默认值原样保留，由生产校验器给出"unsupported default"级别的拒绝。
            Some(value) => Some(value.to_owned()),
            None => None,
        };
        ColumnMeta {
            name: column.to_owned(),
            data_type,
            column_type,
            nullable,
            character_set_name,
            collation_name,
            default,
        }
    }

    /// v4 基线的**契约自闭合**回归（离线、不连库）：把生产形状校验
    /// [`validate_v4_shape`] 实际消费的同一套 helper——[`ddl_parts`]、
    /// [`expected_indexes`]、[`validate_column_shape`]、[`validate_column_charset`]
    /// （以及其内部调用的 [`validate_column_default`]）——套到 16 张表的**每一个契约列名与
    /// 每一个契约索引名**上，逐列逐索引起真实作用，而不是只数 `CREATE` 条数或源码出现次数。
    ///
    /// 断言内容：
    /// 1. 每张契约表都在白名单解析结果里；
    /// 2. 每个契约列都能按**与生产逐字相同的规则**（片段以 `列名 + 空格` 开头）在 DDL 里
    ///    找到恰好一个声明；
    /// 3. 该声明喂给生产校验器后，类型/长度/UNSIGNED/NULL/字符集/collation/默认值全部通过，
    ///    并且**四类反向变异**（可空性取反、长度 ±1、默认值改成 `1`、字符集/collation 改成
    ///    latin1）都必须被拒绝——证明这个 `Ok` 不是空判定，校验确实逐条在跑；
    /// 4. 每个契约索引都能从 DDL 解析出列集合、唯一性与无前缀长度，并与命名前缀吻合；篡改
    ///    其中任一索引的列集合后 [`validate_indexes`] 必须拒绝；
    /// 5. 顶层片段数恰好等于 契约列数 + 独立索引定义数（内联 `PRIMARY KEY` 不额外产生片段），
    ///    且没有任何片段以 `--` 开头（行内注释被并入下一片段的指纹）。
    ///
    /// 覆盖规模（16 表 / 122 列 / 40 索引）也硬编码断言，防止有人靠缩小契约常量蒙过本用例。
    #[test]
    fn v4_schema_contract_is_self_consistent_for_all_sixteen_tables() {
        const TABLE_COLLATION: &str = "utf8mb4_0900_ai_ci";
        const EXPECTED_COLUMNS: usize = 122;
        const EXPECTED_INDEXES: usize = 40;
        let statements = v4_schema_statements().expect("v4 基线 SQL 必须可解析");
        let mut tables = 0usize;
        let mut columns_covered = 0usize;
        let mut indexes_covered = 0usize;
        for &(table, columns, indexes) in TABLES.iter().chain(RUNTIME_TABLES) {
            assert!(
                statements.iter().any(|(seen, _)| *seen == table),
                "白名单解析结果缺少契约表 {table}"
            );
            let parts = ddl_parts(table);
            for &column in columns {
                // 与 validate_v4_shape 完全同一条查找规则；固定文案也与生产逐字一致，
                // 因此这条离线回归失败时给出的正是真实库上会出现的那句话。
                let found: Vec<&String> = parts
                    .iter()
                    .filter(|part| part.starts_with(&format!("{column} ")))
                    .collect();
                assert_eq!(
                    found.len(),
                    1,
                    "v4 schema DDL missing column {table}.{column}（片段匹配数 {}）",
                    found.len()
                );
                let declaration = found[0]
                    .strip_prefix(&format!("{column} "))
                    .expect("已按 `列名 + 空格` 前缀匹配");
                let actual =
                    ideal_metadata_from_declaration(table, column, declaration, TABLE_COLLATION);
                validate_column_shape(table, declaration, &actual).unwrap_or_else(|error| {
                    panic!("{table}.{column} 的声明无法通过生产形状校验：{error}")
                });
                validate_column_charset(table, declaration, &actual, TABLE_COLLATION)
                    .unwrap_or_else(|error| {
                        panic!("{table}.{column} 的字符集元数据无法通过生产校验：{error}")
                    });
                let mut flipped = actual.clone();
                flipped.nullable = !flipped.nullable;
                assert!(
                    validate_column_shape(table, declaration, &flipped).is_err(),
                    "{table}.{column} 的形状校验对可空性不敏感，说明校验没有真正运行"
                );
                // 其余三类元数据也必须**真的**参与判定（防止"声明被解析器读懂但校验被跳过"）：
                // 长度、默认值、字符集/collation 各做一次变异，必须被拒绝。
                let head = declaration
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let declared_charset_in_sql =
                    declaration.to_ascii_uppercase().contains("CHARACTER SET");
                let character_column = matches!(
                    head.split('(').next().unwrap_or_default(),
                    "varchar" | "char" | "text" | "tinytext" | "mediumtext" | "longtext"
                );
                let width = actual.column_type.find('(').and_then(|open| {
                    actual.column_type[open + 1..].find(')').and_then(|digits| {
                        actual.column_type[open + 1..open + 1 + digits]
                            .parse::<i64>()
                            .ok()
                            .map(|value| (open, digits, value))
                    })
                });
                if let Some((open, digits, value)) = width {
                    let mut mutated = actual.clone();
                    let end = open + 1 + digits;
                    mutated.column_type = format!(
                        "{}{}{}",
                        &actual.column_type[..open + 1],
                        value + 1,
                        &actual.column_type[end..]
                    );
                    assert!(
                        validate_column_shape(table, declaration, &mutated).is_err(),
                        "{table}.{column} 的长度判定没有真正运行"
                    );
                }
                if declaration.to_ascii_uppercase().contains("DEFAULT") {
                    let mut mutated = actual.clone();
                    mutated.default = Some("1".to_owned());
                    assert!(
                        validate_column_shape(table, declaration, &mutated).is_err(),
                        "{table}.{column} 的默认值判定没有真正运行"
                    );
                }
                if character_column || declared_charset_in_sql {
                    let mut mutated = actual.clone();
                    mutated.character_set_name = Some("latin1".to_owned());
                    mutated.collation_name = Some("latin1_bin".to_owned());
                    assert!(
                        validate_column_shape(table, declaration, &mutated)
                            .and(validate_column_charset(
                                table,
                                declaration,
                                &mutated,
                                TABLE_COLLATION,
                            ))
                            .is_err(),
                        "{table}.{column} 的字符集/collation 判定没有真正运行"
                    );
                }
                columns_covered += 1;
            }
            let parsed = expected_indexes(&parts, indexes)
                .unwrap_or_else(|error| panic!("{table} 的索引契约无法从 DDL 解析：{error}"));
            assert_eq!(parsed.len(), indexes.len(), "{table} 索引数量不吻合");
            for (name, meta) in indexes.iter().zip(parsed.iter()) {
                assert_eq!(meta.name, *name, "{table} 索引顺序不吻合");
                assert!(!meta.columns.is_empty(), "{table}.{name} 未解析出任何列");
                assert!(
                    meta.columns
                        .iter()
                        .all(|column| columns.contains(&column.as_str())),
                    "{table}.{name} 引用了契约外列 {:?}",
                    meta.columns
                );
                assert!(
                    meta.sub_parts.iter().all(|part| part.is_none()),
                    "{table}.{name} 不应存在前缀长度"
                );
                let unique = *name == "PRIMARY" || name.starts_with("uq_");
                assert_eq!(
                    meta.unique, unique,
                    "{table}.{name} 的唯一性与命名前缀不吻合"
                );
                indexes_covered += 1;
            }
            // 索引校验器同样要证明"真的在比对"：改掉解析出的一个列集合必须被拒绝。
            let mut tampered = parsed.clone();
            tampered[0].columns = vec!["not_a_contract_column".to_owned()];
            tampered[0].sub_parts = vec![None];
            assert!(
                validate_indexes(table, &parsed, &tampered).is_err(),
                "{table} 的索引校验对列集合不敏感，说明校验没有真正运行"
            );
            for part in &parts {
                assert!(
                    !part.starts_with("--"),
                    "{table} 的 DDL 片段以注释开头，说明 schema/v4.sql 存在行内注释：{part:?}"
                );
            }
            let inline_primary = parts.iter().any(|part| {
                columns
                    .iter()
                    .any(|column| part.starts_with(&format!("{column} ")))
                    && part.contains("PRIMARY KEY")
            });
            assert_eq!(
                parts.len(),
                columns.len() + indexes.len()
                    - usize::from(inline_primary && indexes.contains(&"PRIMARY")),
                "{table} 的 DDL 顶层片段数与契约列/索引数不吻合"
            );
            tables += 1;
        }
        assert_eq!(tables, 16, "必须覆盖 11 identity + 5 runtime 共 16 张表");
        assert_eq!(
            columns_covered, EXPECTED_COLUMNS,
            "契约列覆盖规模必须恰好 122 列"
        );
        assert_eq!(
            indexes_covered, EXPECTED_INDEXES,
            "契约索引覆盖规模必须恰好 40 个"
        );
    }
}
