//! 测试专用 fixture：生产 SQL 常量的文本形状门禁。只解析字符串，不连引擎、不伪造
//! `MySqlRow`，因此不声称任何真实引擎行为（真库端到端由受控 executor 复跑）。

/// 空白归一：SQL 常量里的换行续行与缩进不参与形状比较。
pub(crate) fn normalize_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 生产源码文本（`#[cfg(test)]` 之前），供"执行位必须消费被钉常量"的门禁使用。
pub(crate) fn production_source(src: &str) -> &str {
    src.split("#[cfg(test)]").next().unwrap_or(src)
}

/// 把一条 `SELECT ... FROM ...` 语句二分为（顶层 select 列表逐项，`FROM` 起整段尾部）。
/// 括号与单引号内的逗号不算顶层分隔；本仓被门禁的生产 SQL 都是 ASCII 单表投影，
/// 解析失败直接 panic —— 形状门禁的解析失败本身就是必须暴露的缺陷。
pub(crate) fn split_select(sql: &str) -> (Vec<String>, String) {
    let sql = normalize_sql(sql);
    assert!(sql.is_ascii(), "SQL 形状门禁只支持 ASCII 常量：{sql}");
    assert!(sql.starts_with("SELECT "), "不是 SELECT 语句：{sql}");
    let bytes = sql.as_bytes();
    let mut depth = 0i32;
    let mut in_quote = false;
    let mut splits: Vec<usize> = Vec::new();
    let mut from_at: Option<usize> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        if in_quote {
            if bytes[i] == b'\'' {
                in_quote = false;
            }
            i += 1;
            continue;
        }
        match bytes[i] {
            b'\'' => in_quote = true,
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => splits.push(i),
            _ => {}
        }
        if depth == 0 && sql[i..].starts_with(" FROM ") {
            from_at = Some(i);
            break;
        }
        i += 1;
    }
    let from_at =
        from_at.unwrap_or_else(|| panic!("SQL 常量必须带顶层 FROM 才能做形状门禁：{sql}"));
    let mut items = Vec::new();
    let mut start = "SELECT ".len();
    for split in splits {
        items.push(sql[start..split].trim().to_string());
        start = split + 1;
    }
    items.push(sql[start..from_at].trim().to_string());
    (items, sql[from_at + 1..].trim().to_string())
}

/// 取出源码里的 `SELECT` 字符串字面量（`\` 续行拼接、空白归一；本仓生产 SQL 常量
/// 不含 `\\n` 之类转义与 raw string，故只做最小扫描）。
pub(crate) fn select_string_literals(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] != '"' {
            i += 1;
            continue;
        }
        let mut raw = String::new();
        let mut j = i + 1;
        let mut closed = false;
        while j < chars.len() {
            match chars[j] {
                '\\' if j + 1 < chars.len() => {
                    raw.push(chars[j + 1]);
                    j += 2;
                }
                '"' => {
                    closed = true;
                    break;
                }
                c => {
                    raw.push(c);
                    j += 1;
                }
            }
        }
        let normalized = normalize_sql(&raw);
        if closed && normalized.starts_with("SELECT ") {
            out.push(normalized);
        }
        i = j + 1;
    }
    out
}

/// `devices.descriptor_json` 读 SQL 的统一形状门禁：该列在 v4 schema 是 `JSON`
/// （MySQL/TiDB wire 245），而 workspace 未启用 sqlx `json` feature，`?` 绑定按文本
/// 写入可以、按文本读出会被 sqlx 以类型不匹配拒绝并固定去敏为 503
/// `STORAGE_UNAVAILABLE`，所以投影必须是 `CAST(descriptor_json AS CHAR) AS
/// descriptor_json`。除投影形态外，本函数把整条 statement 全部纳入断言：select
/// 列表逐项（列顺序 + 其余 CAST 口径）、`FROM` 之后尾部（表名/WHERE/ORDER BY/
/// LIMIT/行锁）逐字一致、绑定参数数量不漂移。
pub(crate) fn assert_device_descriptor_text_projection(
    label: &str,
    sql: &str,
    expected_items: &[&str],
    expected_tail: &str,
    expected_binds: usize,
) {
    const CHAR_PROJECTION: &str = "CAST(descriptor_json AS CHAR) AS descriptor_json";
    let (items, tail) = split_select(sql);
    let got: Vec<&str> = items.iter().map(String::as_str).collect();
    assert_eq!(
        got,
        expected_items.to_vec(),
        "{label} 的 select 列表必须逐项一致（列顺序、descriptor_json 投影形态、archived 布尔 CAST 口径）：{got:?}"
    );
    assert_eq!(
        tail, expected_tail,
        "{label} 的 FROM 尾部（表名/WHERE/ORDER BY/LIMIT/行锁）必须逐字一致：{tail}"
    );
    assert_eq!(
        normalize_sql(sql).matches('?').count(),
        expected_binds,
        "{label} 的绑定参数数量不得漂移"
    );
    let descriptor_items: Vec<&str> = got
        .iter()
        .copied()
        .filter(|item| item.contains("descriptor_json"))
        .collect();
    assert_eq!(
        descriptor_items,
        vec![CHAR_PROJECTION],
        "{label} 的 descriptor_json 必须是唯一一项 CAST(... AS CHAR) 文本投影，否则 decode_device_admin_view 的 \
         Option<String> 取列在真实引擎上被 sqlx 拒绝（RC1 真库根因）：{descriptor_items:?}"
    );
}
