//! Basic Auth 解析与常量时间凭据比较（规范 §4.2/§4.3/§4.4）。
//!
//! 本模块只做两件事：把 `Authorization: Basic` 解析成**拥有所有权**的凭据对，以及
//! 用 `subtle::ConstantTimeEq` 做**不短路**的比较。它不构造 HTTP 响应、不读配置、
//! 不碰会话边界（I3）：`use crate::http_gate::single_header` 是本模块对 `http_gate`
//! 的唯一依赖，且该 helper 只做"单值 + ASCII"提取。
//!
//! 比较前先做 SHA-256：把输入长度归一为固定 32 字节，消除"长度不同提前返回"与
//! 逐字节短路两类侧信道（§4.3 第 3 条）。诚实登记的残余面（§4.3 第 4 条、§8 R3）：
//! 哈希本身耗时随输入长度线性增长；解析失败会在比较之前返回 401（提前量只与
//! "提交的是否是合法 base64 形状"有关，与凭据内容无关）。

use axum::http::{HeaderMap, header};
use subtle::ConstantTimeEq;

use crate::http_gate::single_header;
use crate::metrics::config::{BasicCredentials, REALM};

/// `Authorization: Basic` 的解析结果。**拥有所有权**：base64 解码与 UTF-8 校验
/// 产生的字符串只在解析函数内部存在，任何 `Option<(&str, &str)>` 形态都会借用
/// 一个随函数返回而销毁的临时缓冲，要么编译不过，要么靠把整张 `HeaderMap` 一路
/// 带调用链来续命。切分点又落在**解码后**的字节上，因此终态只能是 owned。
///
/// 不派生 `Debug`（凭据不外泄，§3.7）也不派生 `PartialEq`（防用短路 `==` 比较凭据）。
pub struct ParsedBasic {
    pub username: String,
    pub password: String,
}

/// §4.4 的固定挑战值；`realm` 取 §3.2 的 `REALM` 常量，二者 MUST 逐字节一致
/// （`www_authenticate_value_is_byte_fixed` 钉住）。不携带 `charset` 参数。
pub fn www_authenticate() -> String {
    format!("Basic realm=\"{REALM}\"")
}

/// §4.2 的固定七步解析。任何一步失败都返回 `None`，**不区分失败原因**（§4.2 第 7 步）。
pub fn parse_authorization(headers: &HeaderMap) -> Option<ParsedBasic> {
    // base64 只在本函数内解码一次；就近导入（与 `sha256` 内 `use sha2::Digest as _`
    // 同风格），并避免与测试模块 `use super::*` 带来的同名匿名 import 冗余。
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    // 1) 单值 + ASCII：缺失、出现多次（任意多于一个值）、非 ASCII 值一律 None。
    //    复用 http_gate 的同名 helper，绝不复制实现（"重复头绝不简化为取第一个"）。
    let value = single_header(headers, header::AUTHORIZATION)?;
    // 2) 按**第一个**空格切分 scheme 与 rest；两段都存在且 rest 非空。
    //    前导空白的行首段是空串 → 不是 basic → None；多空格使 rest 以空格开头 → 第 3 步拒绝。
    let (scheme, rest) = value.split_once(' ')?;
    // 3) scheme ASCII 大小写不敏感等于 basic（RFC 7617 §2.2）。
    if !scheme.eq_ignore_ascii_case("basic")
        || rest.is_empty()
        || rest.starts_with('=')
        || rest.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return None;
    }
    // 4) 唯一合法编码：general_purpose::STANDARD（要求 padding、拒绝尾部比特残留、
    //    不容忍空白）；STANDARD_NO_PAD / URL_SAFE / base32 / hex 一律不接受。
    let decoded = STANDARD.decode(rest).ok()?;
    // 5) 解码结果按 UTF-8 严格解码（Prometheus 官方 basic_auth 按 UTF-8 发凭据）。
    let decoded = String::from_utf8(decoded).ok()?;
    // 6) 仅**第一个**冒号是分隔符，其右侧**全部**字符（可再含冒号）都是密码。
    //    MUST NOT 用 split(':')/rsplit_once/splitn 后再校验"剩余不含冒号"——那会拒绝
    //    RFC 7617 明确允许的密码，把合法抓取方永久锁在 401 之外。
    let (username, password) = decoded.split_once(':')?;
    if username.is_empty() {
        return None;
    }
    // 7) 交 §4.3 校验。
    Some(ParsedBasic {
        username: username.to_owned(),
        password: password.to_owned(),
    })
}

/// 凭据校验：先用户名、后密码，**两次比较都必须执行**，结果按位与合并（§4.3 第 1 条）。
///
/// MUST NOT 写成 `if !user_ok { return false; } …`：短路会在用户名上建立"用户名是否
/// 正确"的时序判定器。返回值也不区分"用户名错"与"密码错"（§4.3 第 5 条），不记日志。
pub fn verify(user: &str, password: &str, expected: &BasicCredentials) -> bool {
    let ok_user = ct_eq_32(
        &sha256(user.as_bytes()),
        &sha256(expected.username.as_bytes()),
    );
    let ok_password = ct_eq_32(
        &sha256(password.as_bytes()),
        &sha256(expected.password.as_bytes()),
    );
    // `&`（非 `&&`）：两侧都已求值，此处再显式保证不因折叠而短路。
    ok_user & ok_password
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).into()
}

/// `subtle::ConstantTimeEq` 的薄封装（§4.3 第 2 条）。MUST NOT 改写为手写的逐字节
/// XOR/OR 折叠循环：这类写法在 `opt-level` 提升或 LLVM 把循环识别为 `memcmp` 后不保证
/// 仍是常量时间；`subtle` 的 `Choice` 与 `black_box` 是对**编译器优化后机器码**的保证
/// （§7.8 第 7 条 grep 对 `src/metrics` 下该模式串的要求是"零命中"，故此处不复述拼法）。
///
/// `Choice` 的载荷字段私有、跨 crate 不可解构，因此经其 `From<Choice> for bool`
/// 转成 `bool`：结果**来自** `a.ct_eq(b)`（subtle 的比较）在源码上直接可指认，
/// 满足 §7.8 第 8 条 grep 对 `subtle::ConstantTimeEq` 的命中要求。
fn ct_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    #[cfg(test)]
    COUNT_CT_EQ.with(|calls| calls.set(calls.get() + 1));
    let equal: bool = a.ct_eq(b).into();
    equal
}

// ---------------------------------------------------------------------------
// 仅测试构建存在的调用计数（§7.3 `verify_runs_both_comparisons` 的注入点）
// ---------------------------------------------------------------------------

#[cfg(test)]
thread_local! {
    /// 线程本地计数而非全局原子量：libtest 为每个测试新建线程，因此该计数不受
    /// 同批并行测试干扰，断言 `== 2` 是确定的。计数代码只在 `cfg(test)` 下存在。
    static COUNT_CT_EQ: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn ct_eq_calls() -> u8 {
    COUNT_CT_EQ.with(std::cell::Cell::get)
}

#[cfg(test)]
fn reset_ct_eq_calls() {
    COUNT_CT_EQ.with(|calls| calls.set(0));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::config::BasicCredentials;
    use axum::http::{HeaderMap, HeaderValue, header};
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    const USER: &str = "prom_admin";
    const PASSWORD: &str = "secure_password";

    fn credentials() -> BasicCredentials {
        BasicCredentials {
            username: USER.to_string(),
            password: PASSWORD.to_string(),
        }
    }

    /// 以**原始字节**构造 Authorization 头（非 ASCII 场景必须绕开 `&str` 检查）。
    fn headers_with(raw: &[u8]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_bytes(raw).expect("合法 header 字节"),
        );
        headers
    }

    fn header_for(value: &str) -> HeaderMap {
        headers_with(value.as_bytes())
    }

    /// 按 `user:password` 组装标准 base64（带 padding）。
    fn basic(user: &[u8], password: &[u8]) -> String {
        let mut bytes = Vec::with_capacity(user.len() + password.len() + 1);
        bytes.extend_from_slice(user);
        bytes.push(b':');
        bytes.extend_from_slice(password);
        STANDARD.encode(bytes)
    }

    // ---------------------------------------------------------------------------
    // §4.2 解析
    // ---------------------------------------------------------------------------

    /// owned 结果：`ParsedBasic` 必须能脱离 `HeaderMap` 存活（先 drop 再断言）。
    #[test]
    fn parses_valid_basic_header() {
        let headers = header_for(&format!(
            "Basic {}",
            basic(USER.as_bytes(), PASSWORD.as_bytes())
        ));
        let parsed = parse_authorization(&headers).expect("合法 Basic 必须解析成功");
        drop(headers);
        assert_eq!(parsed.username, USER);
        assert_eq!(parsed.password, PASSWORD);
    }

    /// 首个冒号之后的全部字符归密码（§4.2 第 6 步 `split_once`、§3.4 规则 2）。
    #[test]
    fn parses_password_with_colons() {
        let headers = header_for(&format!("Basic {}", basic(b"prom_admin", b"a:b:c")));
        let parsed = parse_authorization(&headers).expect("密码含冒号必须解析成功");
        assert_eq!(parsed.username, "prom_admin");
        assert_eq!(parsed.password, "a:b:c");
    }

    /// 冒号串、UTF-8、`=` 全部归属密码；`"u:"` 的密码是空串。
    #[test]
    fn parses_password_with_many_and_edge_colons() {
        let cases: &[(&[u8], &[u8], &str)] = &[
            (b"u", b":::", "u::::"),
            (b"u", "日本語:パ:Sword=".as_bytes(), "u:日本語:パ:Sword="),
            (b"u", b"", "u:"),
        ];
        for (user, password, raw) in cases {
            let headers = header_for(&format!("Basic {}", basic(user, password)));
            let parsed =
                parse_authorization(&headers).unwrap_or_else(|| panic!("{raw:?} 必须解析成功"));
            assert_eq!(parsed.username.as_bytes(), *user);
            assert_eq!(parsed.password.as_bytes(), *password);
        }
    }

    /// scheme 不区分大小写；`Bas ic`（内部空格）失败。
    #[test]
    fn scheme_is_case_insensitive() {
        let encoded = basic(USER.as_bytes(), PASSWORD.as_bytes());
        for scheme in ["Basic", "basic", "BASIC", "bAsIc"] {
            let headers = header_for(&format!("{scheme} {encoded}"));
            assert!(parse_authorization(&headers).is_some(), "{scheme} 必须可用");
        }
        assert!(parse_authorization(&header_for(&format!("Bas ic {encoded}"))).is_none());
    }

    /// 其他 scheme、无 scheme、多空格、前导空白一律 None。
    #[test]
    fn rejects_other_schemes() {
        let encoded = basic(USER.as_bytes(), PASSWORD.as_bytes());
        for value in [
            format!("Bearer {encoded}"),
            format!("Digest username=\"{USER}\""),
            encoded.clone(),
            format!("Basic  {encoded}"),
            format!(" {encoded}"),
            format!(" {USER}:{PASSWORD}"),
        ] {
            assert!(
                parse_authorization(&header_for(&value)).is_none(),
                "{value:?} 必须解析失败"
            );
        }
    }

    /// 多个 Authorization 头（相同或不同值）绝不简化为取第一个。
    #[test]
    fn rejects_multiple_authorization_headers() {
        let encoded = basic(USER.as_bytes(), PASSWORD.as_bytes());
        for second in [
            format!("Basic {encoded}"),
            format!("Basic {}", basic(b"other", b"secret")),
        ] {
            let mut headers = header_for(&format!("Basic {encoded}"));
            headers.append(
                header::AUTHORIZATION,
                HeaderValue::from_str(&second).unwrap(),
            );
            assert!(parse_authorization(&headers).is_none(), "多值头必须 None");
        }
    }

    /// 缺失头、空 rest、非 ASCII 头值。
    #[test]
    fn rejects_missing_header_empty_rest_and_non_ascii() {
        assert!(parse_authorization(&HeaderMap::new()).is_none());
        assert!(parse_authorization(&header_for("Basic ")).is_none());
        assert!(parse_authorization(&header_for("Basic")).is_none());
        // 非 ASCII（UTF-8 的 é）：to_str() 失败 → None（§4.2 第 1 步）。
        assert!(parse_authorization(&headers_with("Basic é".as_bytes())).is_none());
    }

    /// 非法 base64：含 `!`、长度非 4 倍数、去掉 padding、前导 `=`、内部空白。
    #[test]
    fn rejects_invalid_base64_and_unpadded() {
        let padded = basic(USER.as_bytes(), PASSWORD.as_bytes());
        assert!(padded.ends_with('='));
        for value in [
            "Basic nothex!!".to_string(),
            format!("Basic {}", padded.trim_end_matches('=')),
            format!("Basic ={padded}"),
            format!("Basic {} {}", padded, padded),
            "Basic YQ".to_string(),
        ] {
            assert!(
                parse_authorization(&header_for(&value)).is_none(),
                "{value:?} 必须解析失败"
            );
        }
    }

    /// 解码后非法 UTF-8 字节序列 → None（§4.2 第 5 步）。
    #[test]
    fn rejects_non_utf8() {
        let headers = header_for(&format!(
            "Basic {}",
            STANDARD.encode([0xFF_u8, 0xFE, 0x00, 0x41])
        ));
        assert!(parse_authorization(&headers).is_none());
    }

    /// 无冒号、空用户名（含只有冒号）一律 None。
    #[test]
    fn rejects_missing_colon_and_empty_user() {
        for raw in ["prom_admin", ":pw", ":"] {
            let headers = header_for(&format!("Basic {}", STANDARD.encode(raw.as_bytes())));
            assert!(parse_authorization(&headers).is_none(), "{raw:?} 必须 None");
        }
    }

    // ---------------------------------------------------------------------------
    // §4.3 常量时间比较
    // ---------------------------------------------------------------------------

    #[test]
    fn verify_accepts_exact_match() {
        assert!(verify(USER, PASSWORD, &credentials()));
    }

    /// 端到端凭据形状：配置里的冒号密码不被任何一层拒绝（与 §7.5 L2 用例配对）。
    #[test]
    fn verify_accepts_password_with_colons() {
        let expected = BasicCredentials {
            username: USER.to_string(),
            password: "a:b:c".to_string(),
        };
        assert!(verify(USER, "a:b:c", &expected));
        assert!(!verify(USER, "a:b", &expected));
    }

    /// 用户名错 / 密码错 / 都错 / 都空：一律 false，响应不可区分。
    #[test]
    fn verify_rejects_each_field_independently() {
        let expected = credentials();
        assert!(!verify("wrong_user", PASSWORD, &expected));
        assert!(!verify(USER, "wrong_password", &expected));
        assert!(!verify("wrong_user", "wrong_password", &expected));
        assert!(!verify("", "", &expected));
    }

    /// 长度差 1 与差 64 的错误密码都 false：长度不进比较。
    #[test]
    fn verify_is_length_independent() {
        let expected = credentials();
        assert!(!verify(USER, &format!("{PASSWORD}x"), &expected));
        assert!(!verify(
            USER,
            &format!("{PASSWORD}{}", "x".repeat(64)),
            &expected
        ));
        assert!(!verify(&format!("{USER}x"), PASSWORD, &expected));
    }

    /// 逐位差异（首字节、末字节、单比特）都必须被检出。
    #[test]
    fn ct_eq_32_is_content_sensitive() {
        let base = sha256(b"baseline");
        assert!(ct_eq_32(&base, &base));
        let mut first = base;
        first[0] ^= 1;
        assert!(!ct_eq_32(&base, &first));
        let mut last = base;
        last[31] ^= 1;
        assert!(!ct_eq_32(&base, &last));
        assert!(!ct_eq_32(&base, &sha256(b"baselinF")));
        // 输入长度不进入比较：哈希把长度归一为 32 字节。
        assert_eq!(sha256(b"a").len(), sha256(b"aaaaaaaaaaa").len());
    }

    /// 去掉行注释后的纯代码文本（本文件的注释会解释"不许写什么"，直接扫原文会误判）。
    fn code_only(source: &str) -> String {
        source
            .lines()
            .map(|line| match line.find("//") {
                Some(index) => &line[..index],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 源码级门禁：常量时间比较必须落在 `subtle` 上，且 `subtle` 是直接依赖。
    /// 模式串取 `"diff |= a"`（比 §7.8 第 7 条的 `diff |= a\[i\]` 更短），这样本测试
    /// 自身不会成为该 grep 的命中项。
    ///
    /// 计划原文把三个禁串写成裸字面量；但本测试 `include_str!` 的是**整份 auth.rs**
    /// （含本测试自身），裸字面量会落在非注释行上被自身扫到，使断言对**正确代码**也
    /// 恒失败（自指缺陷）。故用 `concat!` 拆分拼接：编译后仍是逐字相同的 `"diff |= a"`
    /// / `"starts_with(expected"` / `"== expected"` 三个连续串，语义与门禁完全等价，
    /// 只是文件里不再出现可被自身命中的连续字面量（不是放宽门禁）。
    #[test]
    fn ct_eq_32_uses_subtle_constant_time_eq() {
        let source = include_str!("auth.rs");
        let code = code_only(source);
        assert!(
            source.contains("use subtle::ConstantTimeEq"),
            "ct_eq_32 必须由 subtle::ConstantTimeEq 实现（§7.8 第 8 条 grep 必须命中）"
        );
        assert!(
            !code.contains(concat!("diff |", "= a")),
            "MUST NOT 在指标线代码里出现手写逐字节折叠（§4.3 第 2 条）"
        );
        assert!(
            !code.contains(concat!("starts_with(", "expected"))
                && !code.contains(concat!("== ex", "pected")),
            "MUST NOT 用可短路的比较凭据（§4.3 第 2 条）"
        );
        let manifest = include_str!("../../Cargo.toml");
        assert!(
            manifest.contains("subtle.workspace = true"),
            "subtle 必须是提升而来的直接依赖（§3.1 约束 2）"
        );
    }

    /// `verify` 无条件执行两次比较：短路写法会在用户名上建时序判定器（§4.3 第 1 条）。
    #[test]
    fn verify_runs_both_comparisons() {
        reset_ct_eq_calls();
        assert!(!verify("wrong_user", PASSWORD, &credentials()));
        assert_eq!(
            ct_eq_calls(),
            2,
            "用户名不匹配时密码比较仍必须执行（计数应为 2）"
        );
        reset_ct_eq_calls();
        assert!(verify(USER, PASSWORD, &credentials()));
        assert_eq!(ct_eq_calls(), 2);
    }

    // ---------------------------------------------------------------------------
    // §4.4 401 挑战值
    // ---------------------------------------------------------------------------

    #[test]
    fn www_authenticate_value_is_byte_fixed() {
        assert_eq!(
            www_authenticate(),
            "Basic realm=\"rsetup-controller-metrics\""
        );
        assert!(www_authenticate().contains(REALM));
        assert!(!www_authenticate().contains("charset"));
    }
}
