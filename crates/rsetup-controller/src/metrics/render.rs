//! Prometheus exposition format（text 0.0.4）渲染（规范 §5.1/§5.6）。
//!
//! 纯函数：输出逐字节确定（族顺序、标签顺序、末尾单个 `\n`），供 golden 测试钉住。
//! 不引入任何指标 SDK/注册中心，也不依赖其隐式指标（§1.3、§5.1 第 10 条）。

use crate::ControllerError;
use crate::metrics::model::ScrapeValues;

/// 逐字节固定的响应类型（含 `version=0.0.4` 前后的分号与单空格）。
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// 响应体上限：序列上限 11 条时实际 < 1 KiB，该上限只是防御性护栏（§5.1 第 9 条）。
pub const MAX_BODY_BYTES: usize = 65_536;

const NAME_BUILD_INFO: &str = "rsetup_controller_build_info";
const NAME_UPTIME: &str = "rsetup_controller_uptime_seconds";
const NAME_DEVICES: &str = "rsetup_controller_devices_total";
const NAME_TASKS: &str = "rsetup_controller_tasks_total";
const NAME_POOL: &str = "rsetup_controller_db_pool_connections";

// HELP 文本为 ASCII 英文、单行、不含换行；devices/tasks 两条同时留档"求和 = 表行数"
// 与 `_total` 后缀的 gauge 语义（§5.4 第 2 条）。
const HELP_BUILD_INFO: &str = "Build information for the rsetup-controller process, always 1.";
const HELP_UPTIME: &str =
    "Seconds elapsed since the controller process started, from the monotonic clock.";
const HELP_DEVICES: &str =
    "Devices grouped by admission state; sum over all labels equals the devices table row count.";
const HELP_TASKS: &str = "Main tasks grouped by normalized status; sum over all labels equals the main_tasks table row count.";
const HELP_POOL: &str = "Database pool connections by state at scrape time, active or idle.";

/// 标签值转义（本模块唯一一处）：严格按 0.0.4 legacy 文本规范**只**处理三个字符。
/// MUST NOT 转义 `\r`、`\t` 或其他控制字符——该格式的转义表里没有这两项。
/// 实现仍 MUST 经过本函数，不得依赖"输入总是安全的"。
pub fn escape_label_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// 渲染整份 exposition。族顺序与样本顺序固定（§5.1 第 3 条），序列集合恒定 11 条。
/// 超过 `MAX_BODY_BYTES` → 渲染层内部 fail-closed（handler 映射 503）。
pub fn render(values: &ScrapeValues) -> Result<String, ControllerError> {
    let mut out = String::with_capacity(1024);
    family(&mut out, NAME_BUILD_INFO, HELP_BUILD_INFO);
    out.push_str(&format!(
        "{NAME_BUILD_INFO}{{version=\"{}\",edition=\"{}\"}} 1\n",
        escape_label_value(values.version),
        escape_label_value(values.edition),
    ));
    family(&mut out, NAME_UPTIME, HELP_UPTIME);
    out.push_str(&format!("{NAME_UPTIME} {}\n", values.uptime_seconds));
    family(&mut out, NAME_DEVICES, HELP_DEVICES);
    out.push_str(&format!(
        "{NAME_DEVICES}{{admission=\"APPROVED\"}} {}\n",
        values.devices.approved
    ));
    out.push_str(&format!(
        "{NAME_DEVICES}{{admission=\"PENDING\"}} {}\n",
        values.devices.pending
    ));
    out.push_str(&format!(
        "{NAME_DEVICES}{{admission=\"REJECTED\"}} {}\n",
        values.devices.rejected
    ));
    family(&mut out, NAME_TASKS, HELP_TASKS);
    out.push_str(&format!(
        "{NAME_TASKS}{{status=\"pending\"}} {}\n",
        values.tasks.pending
    ));
    out.push_str(&format!(
        "{NAME_TASKS}{{status=\"running\"}} {}\n",
        values.tasks.running
    ));
    out.push_str(&format!(
        "{NAME_TASKS}{{status=\"succeeded\"}} {}\n",
        values.tasks.succeeded
    ));
    out.push_str(&format!(
        "{NAME_TASKS}{{status=\"failed\"}} {}\n",
        values.tasks.failed
    ));
    family(&mut out, NAME_POOL, HELP_POOL);
    out.push_str(&format!(
        "{NAME_POOL}{{state=\"active\"}} {}\n",
        values.pool.active
    ));
    out.push_str(&format!(
        "{NAME_POOL}{{state=\"idle\"}} {}\n",
        values.pool.idle
    ));

    if out.len() > MAX_BODY_BYTES {
        // 去敏：固定文案，不含任何指标文本片段与长度。
        return Err(ControllerError::Config(
            "metrics exposition exceeds size limit".into(),
        ));
    }
    Ok(out)
}

/// 族头：`# HELP <name> <text>` + `# TYPE <name> gauge`（族之间不插入空行）。
fn family(out: &mut String, name: &str, help: &str) {
    out.push_str(&format!("# HELP {name} {help}\n"));
    out.push_str(&format!("# TYPE {name} gauge\n"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ControllerError;
    use crate::metrics::EDITION;
    use crate::metrics::model::{DeviceCounts, PoolCounts, ScrapeValues, TaskCounts};

    /// §5.6 的 golden 文本。`@VERSION@` 是版本占位符：规范 §5.2.1 要求测试**不硬编码**
    /// 版本号（改断言"等于 CARGO_PKG_VERSION"），渲染逐字节比对仍以替换后的定本文执行。
    const GOLDEN: &str = r#"# HELP rsetup_controller_build_info Build information for the rsetup-controller process, always 1.
# TYPE rsetup_controller_build_info gauge
rsetup_controller_build_info{version="@VERSION@",edition="2024"} 1
# HELP rsetup_controller_uptime_seconds Seconds elapsed since the controller process started, from the monotonic clock.
# TYPE rsetup_controller_uptime_seconds gauge
rsetup_controller_uptime_seconds 4213
# HELP rsetup_controller_devices_total Devices grouped by admission state; sum over all labels equals the devices table row count.
# TYPE rsetup_controller_devices_total gauge
rsetup_controller_devices_total{admission="APPROVED"} 7
rsetup_controller_devices_total{admission="PENDING"} 2
rsetup_controller_devices_total{admission="REJECTED"} 1
# HELP rsetup_controller_tasks_total Main tasks grouped by normalized status; sum over all labels equals the main_tasks table row count.
# TYPE rsetup_controller_tasks_total gauge
rsetup_controller_tasks_total{status="pending"} 1
rsetup_controller_tasks_total{status="running"} 2
rsetup_controller_tasks_total{status="succeeded"} 40
rsetup_controller_tasks_total{status="failed"} 3
# HELP rsetup_controller_db_pool_connections Database pool connections by state at scrape time, active or idle.
# TYPE rsetup_controller_db_pool_connections gauge
rsetup_controller_db_pool_connections{state="active"} 2
rsetup_controller_db_pool_connections{state="idle"} 3
"#;

    /// §5.6 fixture：approved 7 / pending 2 / revoked 1；pending 1 / running 2 / succeeded 40 / failed 3。
    fn fixture() -> ScrapeValues {
        ScrapeValues {
            version: env!("CARGO_PKG_VERSION"),
            edition: EDITION,
            uptime_seconds: 4213,
            devices: DeviceCounts {
                approved: 7,
                pending: 2,
                rejected: 1,
            },
            tasks: TaskCounts {
                pending: 1,
                running: 2,
                succeeded: 40,
                failed: 3,
            },
            pool: PoolCounts { active: 2, idle: 3 },
        }
    }

    fn golden() -> String {
        GOLDEN.replace("@VERSION@", env!("CARGO_PKG_VERSION"))
    }

    #[test]
    fn render_matches_golden_exposition() {
        let rendered = render(&fixture()).expect("fixture 必须可渲染");
        assert_eq!(rendered, golden(), "golden 逐字节不一致");
        assert!(rendered.ends_with('\n'), "必须以换行结尾");
        assert!(!rendered.ends_with("\n\n"), "末尾不得有多余空行");
        assert_eq!(
            env!("CARGO_PKG_VERSION").len(),
            env!("CARGO_PKG_VERSION").chars().count(),
            "版本标签必须是 ASCII"
        );
    }

    /// 全 0 fixture 仍输出全部 11 条序列（缺序列会让 PromQL 出现空洞、absent() 误报）。
    #[test]
    fn render_emits_zero_valued_series() {
        let values = ScrapeValues {
            version: env!("CARGO_PKG_VERSION"),
            edition: EDITION,
            uptime_seconds: 0,
            devices: DeviceCounts::default(),
            tasks: TaskCounts::default(),
            pool: PoolCounts::default(),
        };
        let rendered = render(&values).unwrap();
        assert_eq!(sample_lines(&rendered).len(), 11);
        for line in sample_lines(&rendered) {
            assert!(
                line.ends_with(" 0") || line.ends_with("} 0") || line.ends_with(" 1"),
                "{line}"
            );
        }
        assert!(golden_lines_present(&rendered));
    }

    #[test]
    fn render_series_count_and_sum_invariants() {
        let values = fixture();
        let rendered = render(&values).unwrap();
        let lines = sample_lines(&rendered);
        assert_eq!(lines.len(), 11, "§5.1 第 4 条：序列集合恒定为 11 条");
        let devices: u64 = lines
            .iter()
            .filter(|line| line.starts_with("rsetup_controller_devices_total"))
            .map(|line| value_of(line))
            .sum();
        let tasks: u64 = lines
            .iter()
            .filter(|line| line.starts_with("rsetup_controller_tasks_total"))
            .map(|line| value_of(line))
            .sum();
        assert_eq!(devices, 10, "三桶之和必须等于设备表行数");
        assert_eq!(tasks, 46, "四桶之和必须等于主任务表行数");
        assert_eq!(value_of(&lines[0]), 1, "build_info 恒为 1");
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("rsetup_controller_devices_total"))
                // 标签值片段截到取值之前（`admission="APPROVED"} 7` → `"APPROVED"}`）：
                // 本断言钉的是固定标签顺序，不与取值耦合。
                .map(|line| {
                    line.split('{')
                        .nth(1)
                        .unwrap()
                        .split('=')
                        .nth(1)
                        .unwrap()
                        .split(' ')
                        .next()
                        .unwrap()
                })
                .collect::<Vec<_>>()
                .join(","),
            "\"APPROVED\"},\"PENDING\"},\"REJECTED\"}",
            "§5.1 第 3 条固定标签顺序"
        );
    }

    #[test]
    fn render_declares_every_family_as_gauge() {
        let rendered = render(&fixture()).unwrap();
        let types: Vec<&str> = rendered
            .lines()
            .filter(|line| line.starts_with("# TYPE"))
            .collect();
        assert_eq!(types.len(), 5, "五个族各一条 TYPE");
        assert!(types.iter().all(|line| line.ends_with(" gauge")));
        assert_eq!(
            types.join("\n"),
            "# TYPE rsetup_controller_build_info gauge\n\
             # TYPE rsetup_controller_uptime_seconds gauge\n\
             # TYPE rsetup_controller_devices_total gauge\n\
             # TYPE rsetup_controller_tasks_total gauge\n\
             # TYPE rsetup_controller_db_pool_connections gauge"
        );
    }

    /// 三项转义（§5.1 第 6 条）：`\\`、`\"`、`\n`。
    #[test]
    fn render_escapes_label_values() {
        let values = ScrapeValues {
            version: "a\"b\\c\nd",
            edition: EDITION,
            uptime_seconds: 1,
            devices: DeviceCounts::default(),
            tasks: TaskCounts::default(),
            pool: PoolCounts::default(),
        };
        let rendered = render(&values).unwrap();
        assert!(
            rendered.contains("version=\"a\\\"b\\\\c\\nd\""),
            "转义不符合 0.0.4 转义表：{rendered}"
        );
        assert!(!rendered.contains("a\"b\\c\nd"));
    }

    /// `\r`、`\t` 与其他控制字符 MUST 逐字节原样输出（0.0.4 转义表只有三项）。
    #[test]
    fn render_does_not_escape_carriage_return_or_tab() {
        let values = ScrapeValues {
            version: "a\r\tb\u{1}",
            edition: EDITION,
            uptime_seconds: 1,
            devices: DeviceCounts::default(),
            tasks: TaskCounts::default(),
            pool: PoolCounts::default(),
        };
        let rendered = render(&values).unwrap();
        assert!(
            rendered.contains("version=\"a\r\tb\u{1}\""),
            "CR/TAB 被额外转义"
        );
        assert!(!rendered.contains("\\r"));
        assert!(!rendered.contains("\\t"));
    }

    /// 数值一律无符号十进制（等价 `^[a-z_]+(\{[^}]*\})? [0-9]+$`，不引入 regex）。
    #[test]
    fn render_values_are_unsigned_decimal() {
        let rendered = render(&fixture()).unwrap();
        // §5.1 第 5 条只约束**数值记法**（不得用科学计数法/NaN/+Inf/负号）：HELP 文本
        // 按 §5.6 golden 必含小写字母 `e`（"the"、"controller"），指标名与标签名同样
        // 含 `e`（`edition`、`devices`），因此逐条样本行只断言其**取值字面量**。
        for line in sample_lines(&rendered) {
            assert!(is_unsigned_sample_line(&line), "非法样本行：{line:?}");
            let value = line.rsplit_once(' ').expect("样本行必须带取值").1;
            for marker in ["e", "NaN", "+Inf", "-1"] {
                assert!(
                    !value.contains(marker),
                    "取值含非法数值记法 {marker:?}：{line:?}"
                );
            }
        }
    }

    #[test]
    fn render_is_deterministic() {
        let values = fixture();
        assert_eq!(render(&values).unwrap(), render(&values).unwrap());
    }

    #[test]
    fn content_type_is_the_byte_constant() {
        assert_eq!(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8");
        assert_eq!(MAX_BODY_BYTES, 65_536);
    }

    /// 渲染层内部 fail-closed：超限返回 Err，消息不含任何指标文本片段（I5）。
    #[test]
    fn render_rejects_oversized_body() {
        // 标签值是唯一可被外部拉长的输入（正常路径由 §3.4/§5.2.1 限定）。
        let huge: &'static str = Box::leak("x".repeat(MAX_BODY_BYTES + 1).into_boxed_str());
        match render(&values_with_version(huge)) {
            Err(ControllerError::Config(text)) => {
                assert_eq!(text, "metrics exposition exceeds size limit");
                assert!(
                    !text.contains("rsetup_controller"),
                    "错误消息含指标文本：{text}"
                );
            }
            other => panic!("超限必须 Err(Config)，实得 {other:?}"),
        }
        // 恰好等于上限的输入不得被拒绝（边界是"超过"）。定长骨架（不含 version）实测
        // 1_401 字节，故边界由 `base` 反算，使渲染结果恰为 MAX_BODY_BYTES 字节。
        let base = render(&values_with_version("")).expect("空标签值必须可渲染");
        let edge_len = MAX_BODY_BYTES - base.len();
        let edge: &'static str = Box::leak("x".repeat(edge_len).into_boxed_str());
        let rendered = render(&values_with_version(edge)).expect("恰好等于上限不得拒绝");
        assert_eq!(rendered.len(), MAX_BODY_BYTES, "边界输入必须恰好等于上限");
        let over: &'static str = Box::leak("x".repeat(edge_len + 1).into_boxed_str());
        assert!(
            render(&values_with_version(over)).is_err(),
            "超出上限一字节必须拒绝"
        );
    }

    fn values_with_version(version: &'static str) -> ScrapeValues {
        ScrapeValues {
            version,
            edition: EDITION,
            uptime_seconds: 1,
            devices: DeviceCounts::default(),
            tasks: TaskCounts::default(),
            pool: PoolCounts::default(),
        }
    }

    /// 非 `#` 开头的行即样本行。
    fn sample_lines(rendered: &str) -> Vec<String> {
        rendered
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_string)
            .collect()
    }

    fn golden_lines_present(rendered: &str) -> bool {
        [
            "rsetup_controller_build_info",
            "rsetup_controller_uptime_seconds",
            "rsetup_controller_devices_total",
            "rsetup_controller_tasks_total",
            "rsetup_controller_db_pool_connections",
        ]
        .iter()
        .all(|name| rendered.contains(&format!("# TYPE {name} gauge")))
    }

    fn value_of(line: &str) -> u64 {
        line.rsplit_once(' ')
            .unwrap()
            .1
            .parse()
            .unwrap_or_else(|_| panic!("取值非法：{line:?}"))
    }

    /// 手写最小判据（等价 `^[a-z_]+(\{[^}]*\})? [0-9]+$`）：本仓库不引入 regex 依赖。
    fn is_unsigned_sample_line(line: &str) -> bool {
        let Some((head, value)) = line.rsplit_once(' ') else {
            return false;
        };
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
        let (name, labels) = match head.split_once('{') {
            Some((name, tail)) => match tail.strip_suffix('}') {
                Some(labels) => (name, Some(labels)),
                None => return false,
            },
            None => (head, None),
        };
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        {
            return false;
        }
        if let Some(labels) = labels
            && (labels.is_empty() || labels.contains('}') || !labels.contains('='))
        {
            return false;
        }
        true
    }
}
