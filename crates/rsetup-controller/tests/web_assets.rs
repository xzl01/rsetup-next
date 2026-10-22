//! Web 静态资产嵌入契约测试。
//!
//! 两组彼此独立的预期源：
//! 1. **真实 dist 端到端**：从磁盘上的 `apps/controller-web/dist/index.html` 现场提取
//!    资产清单，要求编译期嵌入的路由逐字节交付同一份内容。断言里不出现任何哈希名，
//!    因此前端重新构建后本测试仍然成立（不靠手改哈希维持绿灯）。
//! 2. **合成构建 fixture**：直接驱动生产生成器 `build/web_assets.rs`（与 build.rs
//!    `#[path]` 引入的是同一份源码），在 `target/` 子目录内伪造两次不同哈希的前端构建，
//!    验证生成表跟随哈希、任意新增 chunk 被嵌入，以及各类拒绝路径。
//!    fixture 只落在 `env!("CARGO_TARGET_TMPDIR")` 下，绝不读写真实 dist。
//!
//! 生成器 API 为 `generate(dist_dir, snapshot_dir)`：先把校验过的字节快照写进
//! `snapshot_dir`，生成的 Rust 只 `include` 快照（P1）。本测试对快照路径、控制字符路径、
//! 大小上限、HTML tokenizer、符号链接与 rerun 监听都各有针对性断言。

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use tower::ServiceExt;

/// 生产资产生成器本体：与 `build.rs` 引入的是同一份源码，非测试专用复刻。
#[path = "../build/web_assets.rs"]
mod web_asset_gen;

// ---------------------------------------------------------------------------
// 合成构建 fixture 工具
// ---------------------------------------------------------------------------

/// 一个合成前端构建产物目录 `<target>/tmp/.../web-assets-<pid>/<name>-<seq>/{dist,snapshot}`。
struct Fixture {
    root: PathBuf,
    dist: PathBuf,
}

/// fixture 目录序号：保证同一次测试运行内每次 `Fixture::new` 独占一个目录。
static FIXTURE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Fixture {
    /// 新建一个独占的 dist 根目录；并发测试与同测试内的多次构建互不覆盖。
    fn new(name: &str) -> Self {
        let seq = FIXTURE_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("web-assets-{}", std::process::id()))
            .join(format!("{name}-{seq}"));
        if root.exists() {
            fs::remove_dir_all(&root).expect("清理旧 fixture 目录");
        }
        let dist = root.join("dist");
        fs::create_dir_all(dist.join("assets")).expect("创建 fixture dist/assets");
        Self { root, dist }
    }

    /// 本次构建的字节快照目录（传给 `generate`；与 dist 同级、独立于 dist）。
    fn snapshot(&self) -> PathBuf {
        self.root.join("snapshot")
    }

    /// 用本 fixture 的 dist + 独立快照目录驱动生产生成器。
    fn generate(&self) -> Result<web_asset_gen::Generation, String> {
        web_asset_gen::generate(&self.dist, &self.snapshot())
    }

    fn write(&self, relative: &str, content: &[u8]) -> PathBuf {
        let path = self.dist.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("创建 fixture 子目录");
        }
        fs::write(&path, content).expect("写 fixture 文件");
        path
    }

    /// 写一个指定长度的稀疏文件（不占真实磁盘），用于字节上限测试。
    fn write_sparse(&self, relative: &str, len: u64) -> PathBuf {
        let path = self.dist.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("创建 fixture 子目录");
        }
        let file = File::create(&path).expect("创建稀疏文件");
        file.set_len(len).expect("设置稀疏长度");
        path
    }

    /// 直接写入原始 index.html 字节（绕过 `index_html` 助手，供 tokenizer 专项测试）。
    fn write_raw_html(&self, html: &str) -> PathBuf {
        self.write("index.html", html.as_bytes())
    }

    fn index_html(&self, references: &[&str]) -> PathBuf {
        let mut head = String::new();
        for reference in references {
            if reference.ends_with(".css") {
                head.push_str(&format!(
                    "    <link rel=\"stylesheet\" crossorigin href=\"{reference}\">\n"
                ));
            } else {
                head.push_str(&format!(
                    "    <script type=\"module\" crossorigin src=\"{reference}\"></script>\n"
                ));
            }
        }
        let html = format!(
            "<!doctype html>\n<html lang=\"zh-CN\">\n  <head>\n    <meta charset=\"UTF-8\" />\n{head}  </head>\n  <body>\n    <div id=\"app\"></div>\n  </body>\n</html>\n"
        );
        self.write("index.html", html.as_bytes())
    }
}

/// 收集生成表里的 `(url_path, mime)`，按 url 排序，便于逐条断言。
fn generated_pairs(generation: &web_asset_gen::Generation) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = generation
        .assets
        .iter()
        .map(|asset| (asset.url_path.clone(), asset.mime.to_string()))
        .collect();
    pairs.sort();
    pairs
}

/// 断言生成失败且错误说明含指定片段（片段即"拒绝理由"契约，不比对全串）。
fn expect_generate_error(fixture: &Fixture, expected_fragment: &str) {
    match fixture.generate() {
        Ok(generation) => panic!(
            "生成器本应拒绝，实际成功嵌入 {} 条资产：{:?}",
            generation.assets.len(),
            generated_pairs(&generation)
        ),
        Err(err) => assert!(
            err.contains(expected_fragment),
            "错误说明应包含「{expected_fragment}」，实际：{err}"
        ),
    }
}

/// 新建一个可含**非 ASCII / 相对段** dist 名的独占 fixture 根。
/// 返回 `(根, dist, snapshot)`，三者互不嵌套，全部落在 `CARGO_TARGET_TMPDIR` 下。
fn fixture_root_with(name: &str, dist_rel: &str) -> (PathBuf, PathBuf, PathBuf) {
    let seq = FIXTURE_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("web-assets-{}", std::process::id()))
        .join(format!("{name}-{seq}"));
    let _ = fs::remove_dir_all(&root);
    let dist = root.join(dist_rel);
    fs::create_dir_all(dist.join("assets")).expect("创建 fixture dist/assets");
    let snapshot = root.join("snapshot");
    (root, dist, snapshot)
}

/// 往任意 dist 路径写一份最小合法前端构建（JS 入口 + CSS）。
fn write_minimal_dist(dist: &Path) {
    fs::create_dir_all(dist.join("assets")).expect("创建 dist/assets");
    fs::write(dist.join("assets/index-AAA111.js"), b"console.log(1);").expect("写 js");
    fs::write(dist.join("assets/index-AAA222.css"), b"body{margin:0}").expect("写 css");
    fs::write(
        dist.join("index.html"),
        "<!doctype html><script src=\"/assets/index-AAA111.js\"></script>\
         <link rel=\"stylesheet\" href=\"/assets/index-AAA222.css\">"
            .as_bytes(),
    )
    .expect("写 index.html");
}

/// 断言生成失败并拿回错误说明（`expect_err` 之外多带一句「本应拒绝」的上下文）。
fn expect_reject(result: Result<web_asset_gen::Generation, String>, what: &str) -> String {
    match result {
        Ok(generation) => panic!(
            "{what}：本应失败关闭，实际成功嵌入 {} 项资产",
            generation.assets.len()
        ),
        Err(err) => err,
    }
}

// ---------------------------------------------------------------------------
// 1. 生成器：跟随真实构建，不写死哈希
// ---------------------------------------------------------------------------

#[test]
fn generates_asset_table_from_synthetic_build() {
    let fixture = Fixture::new("generate-basic");
    let js = fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write("assets/index-AAA222.css", b"body{margin:0}");
    fixture.index_html(&["/assets/index-AAA111.js", "/assets/index-AAA222.css"]);

    let generation = fixture
        .generate()
        .unwrap_or_else(|err| panic!("合成构建应生成成功，实际失败：{err}"));

    assert_eq!(
        generated_pairs(&generation),
        vec![
            (
                "/assets/index-AAA111.js".to_string(),
                "text/javascript".to_string()
            ),
            (
                "/assets/index-AAA222.css".to_string(),
                "text/css".to_string()
            ),
        ]
    );
    assert_eq!(generation.index_html, fixture.dist.join("index.html"));

    // 生成源码内嵌真实哈希 URL，但 include 目标必须是稳定快照，而非可变 dist 原路径。
    let source = &generation.rust_source;
    assert!(
        source.contains("\"/assets/index-AAA111.js\""),
        "生成源码应内嵌真实 JS 资产 URL：\n{source}"
    );
    assert!(
        source.contains("\"/assets/index-AAA222.css\""),
        "生成源码应内嵌真实 CSS 资产 URL：\n{source}"
    );
    assert!(
        source.contains("include_str!"),
        "生成源码必须 include_str index.html 快照：\n{source}"
    );
    assert!(
        source.contains("include_bytes!"),
        "生成源码必须 include_bytes 资产快照：\n{source}"
    );
    assert!(
        !source.contains(&js.to_string_lossy().to_string()),
        "生成源码不得 include 可变 dist 原路径 {js:?}：\n{source}"
    );
    // 每个 include 目标都指向快照目录内的文件。
    for cap in source.match_indices("include_bytes!(") {
        let rest = &source[cap.0..];
        let line = rest.lines().next().unwrap_or_default();
        assert!(
            line.contains(&generation.snapshot_dir.to_string_lossy().to_string()),
            "include_bytes 目标必须落在快照目录内：{line}"
        );
    }
}

#[test]
fn regenerated_asset_table_tracks_new_content_hashes() {
    // 第一次构建：哈希 AAA。
    let first = Fixture::new("generate-hash-drift-first");
    first.write("assets/index-AAA111.js", b"console.log(1);");
    first.write("assets/index-AAA222.css", b"body{margin:0}");
    first.index_html(&["/assets/index-AAA111.js", "/assets/index-AAA222.css"]);
    let first_generation = first
        .generate()
        .unwrap_or_else(|err| panic!("第一次合成构建应生成成功：{err}"));

    // 第二次构建：内容变了 -> Vite 产出不同哈希；同一生成器不得残留旧哈希。
    let second = Fixture::new("generate-hash-drift-second");
    second.write("assets/index-ZZZ987.js", b"console.log(2);");
    second.write("assets/index-ZZZ986.css", b"body{margin:1}");
    second.index_html(&["/assets/index-ZZZ987.js", "/assets/index-ZZZ986.css"]);
    let second_generation = second
        .generate()
        .unwrap_or_else(|err| panic!("第二次合成构建应生成成功：{err}"));

    assert_eq!(
        generated_pairs(&second_generation),
        vec![
            (
                "/assets/index-ZZZ986.css".to_string(),
                "text/css".to_string()
            ),
            (
                "/assets/index-ZZZ987.js".to_string(),
                "text/javascript".to_string()
            ),
        ]
    );
    assert!(
        !second_generation.rust_source.contains("AAA111"),
        "重新生成后源码里不得出现上一轮构建的哈希名"
    );
    assert!(
        !first_generation.rust_source.contains("ZZZ987"),
        "上一轮生成源码也不得出现下一轮哈希名"
    );
}

#[test]
fn embeds_assets_not_referenced_by_index_html() {
    // 动态 import 的 chunk 只由 JS 在运行时请求，index.html 不引用它。
    let fixture = Fixture::new("generate-lazy-chunk");
    fixture.write("assets/index-BBB111.js", b"import('./chunk-lazy-DDD.js');");
    fixture.write("assets/index-BBB222.css", b"body{margin:0}");
    fixture.write("assets/chunk-lazy-DDD333.js", b"export const lazy = 1;");
    fixture.write("assets/nested/deep-EEE444.svg", b"<svg/>");
    fixture.index_html(&["/assets/index-BBB111.js", "/assets/index-BBB222.css"]);

    let generation = fixture
        .generate()
        .unwrap_or_else(|err| panic!("含额外 chunk 的合成构建应生成成功：{err}"));

    assert_eq!(
        generated_pairs(&generation),
        vec![
            (
                "/assets/chunk-lazy-DDD333.js".to_string(),
                "text/javascript".to_string()
            ),
            (
                "/assets/index-BBB111.js".to_string(),
                "text/javascript".to_string()
            ),
            (
                "/assets/index-BBB222.css".to_string(),
                "text/css".to_string()
            ),
            (
                "/assets/nested/deep-EEE444.svg".to_string(),
                "image/svg+xml".to_string()
            ),
        ]
    );
}

// ---------------------------------------------------------------------------
// 1a. 生成器：TOCTOU 快照（P1）——生成后改源/删源/换符号链接，被编译的仍是稳定一致包
// ---------------------------------------------------------------------------

#[test]
fn generated_rust_references_stable_snapshots_not_mutable_dist() {
    let fixture = Fixture::new("p1-snapshot-refs");
    let js = fixture.write("assets/index-AAA111.js", b"console.log(\"ORIGINAL\");");
    fixture.write("assets/index-AAA222.css", b"body{margin:0}");
    fixture.index_html(&["/assets/index-AAA111.js", "/assets/index-AAA222.css"]);

    let generation = fixture.generate().expect("应生成成功");
    assert!(
        !generation
            .rust_source
            .contains(&js.to_string_lossy().to_string()),
        "生成源码不得 include 可变 dist 原路径：\n{}",
        generation.rust_source
    );
    assert!(
        generation
            .rust_source
            .contains(&generation.index_html_snapshot.to_string_lossy().to_string()),
        "生成源码应 include index.html 快照路径"
    );
    for asset in &generation.assets {
        assert!(
            asset.snapshot.starts_with(&generation.snapshot_dir),
            "资产快照必须落在快照目录内：{:?}",
            asset.snapshot
        );
        assert_eq!(
            fs::read(&asset.snapshot).expect("读快照"),
            fs::read(&asset.file).expect("读原文件"),
            "快照字节必须与校验时的原文件一致"
        );
    }
}

/// 字节层保障（**不**等同编译层）：删源后快照文件本身仍是原始字节。
/// 真正的「编译出来的二进制里到底是哪份字节」由
/// [`generated_rust_compiles_and_serves_snapshot_after_dist_changes`] 用真实 rustc 证明。
#[test]
fn snapshot_bytes_survive_post_generate_source_deletion() {
    let fixture = Fixture::new("p1-snapshot-immutable");
    let js = fixture.write("assets/index-AAA111.js", b"console.log(\"ORIGINAL\");");
    fixture.index_html(&["/assets/index-AAA111.js"]);
    let generation = fixture.generate().expect("应生成成功");
    let asset = &generation.assets[0];
    let original = fs::read(&asset.snapshot).expect("读快照");

    // 生成之后删除原资产：`include!` 的是快照，编译结果仍是原始稳定一致包（P1 决定性钩子）。
    fs::remove_file(&js).expect("删除源资产");
    assert!(!js.exists(), "确认源资产已删除");
    assert_eq!(
        fs::read(&asset.snapshot).expect("删源后读快照"),
        original,
        "删源后快照必须仍是原始字节，编译期 include 的就是这份稳定一致包"
    );
}

/// 字节层保障（**不**等同编译层）：校验后把原资产换成符号链接，快照文件仍是原始字节。
/// 编译层的同一结论由 [`generated_rust_compiles_and_serves_snapshot_after_dist_changes`] 证明。
#[cfg(unix)]
#[test]
fn snapshot_bytes_not_followed_by_symlink_swap() {
    // 校验后把原资产换成指向别处的符号链接：include 的是快照，绝不跟随符号链接。
    let fixture = Fixture::new("p1-symlink-swap");
    let js = fixture.write("assets/index-AAA111.js", b"console.log(\"ORIGINAL\");");
    fixture.index_html(&["/assets/index-AAA111.js"]);
    let generation = fixture.generate().expect("应生成成功");
    let asset = &generation.assets[0];
    let original = fs::read(&asset.snapshot).expect("读快照");

    let secret = fixture.root.join("secret-target.txt");
    fs::write(&secret, b"passwd-like-secret").expect("写符号链接目标");
    fs::remove_file(&js).expect("删源资产");
    std::os::unix::fs::symlink(&secret, &js).expect("换成符号链接");

    assert_eq!(
        fs::read(&asset.snapshot).expect("换符号链接后读快照"),
        original,
        "被换成符号链接后，快照必须仍是原始字节（绝不跟随符号链接）"
    );
}

// ---------------------------------------------------------------------------
// 1b. 生成器：HTML tokenizer（P2）——注释/rawtext/空白/dataURL/srcset
// ---------------------------------------------------------------------------

#[test]
fn rejects_script_src_with_internal_whitespace() {
    let fixture = Fixture::new("tok-space");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write_raw_html("<script src=\"/assets/index-AAA111.js extra.js\"></script>");
    expect_generate_error(&fixture, "含空白");
}

#[test]
fn ignores_script_inside_html_comment() {
    // 唯一 JS 出现在注释里：不得被当作 JS 入口（ghost.js 不得顶替入口）。
    let fixture = Fixture::new("tok-comment");
    fixture.write("assets/ghost-CCC.js", b"console.log(1);");
    fixture.write_raw_html(
        "<!doctype html><!-- <script src=\"/assets/ghost-CCC.js\"></script> --><body></body>",
    );
    expect_generate_error(&fixture, "未引用任何 JS 入口");
}

#[test]
fn does_not_parse_attributes_inside_script_rawtext() {
    // 内联 script 正文里的 `x=1` 与字符串里的 /assets/ 字样都不是真实引用，也不报错。
    let fixture = Fixture::new("tok-rawtext");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write_raw_html(
        "<script src=\"/assets/index-AAA111.js\"></script><script>const x=1;var s=\"/assets/virtual-DDD.js\";</script>",
    );
    fixture
        .generate()
        .unwrap_or_else(|err| panic!("内联 script 正文不应被解析成属性/引用：{err}"));
}

#[test]
fn allows_unquoted_and_boolean_attributes() {
    let fixture = Fixture::new("tok-unquoted");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write("assets/index-AAA222.css", b"body{margin:0}");
    fixture.write_raw_html(
        "<script src=/assets/index-AAA111.js async></script><link rel=stylesheet href=/assets/index-AAA222.css crossorigin>",
    );
    let generation = fixture
        .generate()
        .unwrap_or_else(|err| panic!("不带引号/布尔属性应被支持：{err}"));
    assert_eq!(generated_pairs(&generation).len(), 2);
}

#[test]
fn does_not_split_data_url_on_comma() {
    let fixture = Fixture::new("tok-dataurl");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write_raw_html(
        "<link rel=\"icon\" href=\"data:image/svg+xml,%3Csvg%3E,%3C/svg%3E\"><script src=\"/assets/index-AAA111.js\"></script>",
    );
    let generation = fixture
        .generate()
        .unwrap_or_else(|err| panic!("含逗号的合法 data: URL 不得被误切：{err}"));
    assert_eq!(generated_pairs(&generation).len(), 1);
}

#[test]
fn srcset_family_splits_candidates() {
    let fixture = Fixture::new("tok-srcset");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write("assets/pic-1x.png", b"PNG1");
    fixture.write("assets/pic-2x.png", b"PNG22");
    fixture.write_raw_html(
        "<img srcset=\"/assets/pic-1x.png 1x, /assets/pic-2x.png 2x\" src=\"/assets/index-AAA111.js\">",
    );
    let generation = fixture
        .generate()
        .unwrap_or_else(|err| panic!("srcset 候选应被逐个嵌入：{err}"));
    let urls: Vec<&str> = generation
        .assets
        .iter()
        .map(|asset| asset.url_path.as_str())
        .collect();
    assert!(urls.contains(&"/assets/pic-1x.png"), "{urls:?}");
    assert!(urls.contains(&"/assets/pic-2x.png"), "{urls:?}");
}

// ---------------------------------------------------------------------------
// 1c. 生成器：字节上限（P2）——metadata 预检 + 有界读取 + checked 总量
// ---------------------------------------------------------------------------

#[test]
fn rejects_oversized_asset_without_reading_it() {
    let fixture = Fixture::new("limit-asset");
    // 1 GiB 稀疏文件：metadata 预检即拒，绝不真读进内存。
    fixture.write_sparse("assets/huge-AAA.js", 1u64 << 30);
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.index_html(&["/assets/index-AAA111.js"]);
    expect_generate_error(&fixture, "超过上限");
}

#[test]
fn index_html_size_boundary_is_inclusive() {
    let fixture = Fixture::new("limit-html-at");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    let mut html =
        String::from("<!doctype html><script src=\"/assets/index-AAA111.js\"></script><!--");
    let tail = "-->";
    let pad = web_asset_gen::MAX_INDEX_HTML_BYTES as usize - html.len() - tail.len();
    html.push_str(&"a".repeat(pad));
    html.push_str(tail);
    assert_eq!(html.len() as u64, web_asset_gen::MAX_INDEX_HTML_BYTES);
    fixture.write_raw_html(&html);
    fixture
        .generate()
        .unwrap_or_else(|err| panic!("恰好等于上限的 index.html 应通过：{err}"));
}

#[test]
fn rejects_index_html_one_byte_over_limit() {
    let fixture = Fixture::new("limit-html-over");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    let mut html =
        String::from("<!doctype html><script src=\"/assets/index-AAA111.js\"></script><!--");
    let tail = "-->";
    let pad = web_asset_gen::MAX_INDEX_HTML_BYTES as usize - html.len() - tail.len() + 1;
    html.push_str(&"a".repeat(pad));
    html.push_str(tail);
    fixture.write_raw_html(&html);
    expect_generate_error(&fixture, "超过上限");
}

#[test]
fn rejects_total_asset_bytes_over_limit() {
    // 每个稀疏文件都在单文件上限内，但 metadata 累加超过 MAX_TOTAL_ASSET_BYTES。
    let fixture = Fixture::new("limit-total");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    let per = (web_asset_gen::MAX_TOTAL_ASSET_BYTES / 5) + 1;
    assert!(per < web_asset_gen::MAX_ASSET_FILE_BYTES);
    for index in 0..5 {
        fixture.write_sparse(&format!("assets/blob-{index:03}.bin.js"), per);
    }
    fixture.index_html(&["/assets/index-AAA111.js"]);
    expect_generate_error(&fixture, "总字节数");
}

// ---------------------------------------------------------------------------
// 1d. 生成器：路径控制字符（P2）——不得注入生成源码注释或 cargo 行协议
// ---------------------------------------------------------------------------

#[test]
fn rejects_dist_path_with_control_chars() {
    let seq = FIXTURE_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("web-assets-{}", std::process::id()))
        .join(format!("limit-ctl-{seq}"));
    let _ = fs::remove_dir_all(&root);
    let evil_parent = root.join("evil\ncompile_error!(\"pwned\");\t cargo:warning=x");
    let dist = evil_parent.join("dist");
    fs::create_dir_all(dist.join("assets")).expect("建 evil dist");
    fs::write(dist.join("assets/index-AAA111.js"), b"console.log(1);").unwrap();
    fs::write(
        dist.join("index.html"),
        b"<script src=\"/assets/index-AAA111.js\"></script>",
    )
    .unwrap();
    let snapshot = root.join("snapshot");
    let err =
        web_asset_gen::generate(&dist, &snapshot).expect_err("含控制字符的 dist 路径必须失败关闭");
    assert!(err.contains("控制字符"), "应指出控制字符，实际：{err}");
    // 错误说明本身也不得原样吐出入换行的注入串（应走 Debug 转义）。
    assert!(
        !err.contains("\ncompile_error!"),
        "错误说明不得原样含注入换行串：{err}"
    );
    assert_eq!(
        err.lines().count(),
        1,
        "诊断必须只有一行（控制字符已转义），不得为 cargo 行协议注入新指令：{err:?}"
    );
}

#[test]
fn rejects_non_ascii_dist_path_that_carries_newline() {
    // 真实反例：目录名同时含**中文与非 ASCII**（早先 has_control_char 以 text.is_ascii()
    // 为前提，整串直接跳过检查）与换行 + `cargo:warning=`，一旦原样进入 build.rs 的
    // stdout，Cargo 会把它解析成注入的 warning/rerun 指令。
    let (root, dist, snapshot) =
        fixture_root_with("ctl-nonascii", "路径\ncargo:warning=INJECTED/dist");
    write_minimal_dist(&dist);
    let err = expect_reject(
        web_asset_gen::generate(&dist, &snapshot),
        "含换行的 dist 路径必须失败关闭",
    );
    assert!(err.contains("控制字符"), "应指出控制字符，实际：{err}");
    assert_eq!(err.lines().count(), 1, "诊断必须单行，实际：{err:?}");
    assert!(
        !err.contains("\ncargo:warning="),
        "诊断不得原样带回注入行：{err:?}"
    );
    let _ = root;
}

#[test]
fn allows_non_ascii_dist_path_without_control_chars() {
    // 正常中文/空格/引号/反斜杠路径必须仍然可用（拒绝只针对控制字符，不是「非 ASCII 一律拒」）。
    let (_root, dist, snapshot) =
        fixture_root_with("ctl-nonascii-ok", "前端产物 目录 '带引号' \\back\\dist");
    write_minimal_dist(&dist);
    web_asset_gen::generate(&dist, &snapshot)
        .unwrap_or_else(|err| panic!("合法的非 ASCII 路径不得被拒绝：{err}"));
}

// ---------------------------------------------------------------------------
// 1d-2. build.rs 行协议出口（P2）：`CARGO_MANIFEST_DIR` 派生路径同样不得注入 cargo 指令
//
// `build.rs` 把 `manifest_dir.join(source)`（BUILD_SCRIPT_SOURCES）与生成器的
// `rerun_paths` 合并后逐行 `println!("cargo:rerun-if-changed={}")`。后者在 `generate`
// 内已拒绝控制字符，前者**不经过生成器**；本小节锁定两侧共用的出口闸门
// `validate_rerun_paths`，以及「打印之前必须校验」这一集成点。
// ---------------------------------------------------------------------------

/// 与 `build.rs::BUILD_SCRIPT_SOURCES` 同序的清单（独立声明作为预期源，常量漂移即红）。
const BUILD_SCRIPT_SOURCE_NAMES: &[&str] = &["build.rs", "build/web_assets.rs"];

/// 复刻 `build.rs` 的合并方式：由 `CARGO_MANIFEST_DIR` join 出构建脚本源码路径。
fn build_script_paths(manifest_dir: &Path) -> Vec<PathBuf> {
    BUILD_SCRIPT_SOURCE_NAMES
        .iter()
        .map(|source| manifest_dir.join(source))
        .collect()
}

#[test]
fn rejects_build_script_source_from_newline_injected_manifest_dir() {
    // 真实攻击形态：目录名里塞换行 + cargo 指令。build.rs 若原样 println!，
    // 真实 Cargo 会把后半段解析成一条注入的 `warning`/`rerun-if-changed` 指令。
    let manifest_dir = PathBuf::from("/ws/路径\ncargo:warning=INJECTED");
    let paths = build_script_paths(&manifest_dir);
    assert_eq!(
        paths.len(),
        BUILD_SCRIPT_SOURCE_NAMES.len(),
        "构建脚本源码路径应按 BUILD_SCRIPT_SOURCES 逐条派生"
    );
    let err = web_asset_gen::validate_rerun_paths(&paths, "构建脚本源码")
        .expect_err("含换行的 CARGO_MANIFEST_DIR 派生路径必须失败关闭");
    assert!(err.contains("控制字符"), "应指出控制字符，实际：{err}");
    assert!(
        err.contains("构建脚本源码"),
        "错误说明应带上路径来源 label，实际：{err}"
    );
    assert_eq!(err.lines().count(), 1, "诊断必须单行，实际：{err:?}");
    assert!(
        !err.contains("\ncargo:warning="),
        "诊断不得原样带回注入行：{err:?}"
    );
}

#[test]
fn rejects_build_script_source_paths_holding_any_control_char() {
    // `\r` 一样截断行协议（CRLF），`\t`/NUL/DEL 也一并拒绝：判定逐字节 `< 0x20 || 0x7f`。
    let cases = [
        ("carriage-return", "/ws\rcargo:warning=INJECTED"),
        ("tab", "/ws\tcargo:rerun-if-changed=/etc"),
        ("nul", "/ws\0cargo:warning=x"),
        ("del", "/ws\u{7f}cargo:warning=x"),
    ];
    for (name, manifest) in cases.into_iter() {
        let paths = build_script_paths(&PathBuf::from(manifest));
        let err = web_asset_gen::validate_rerun_paths(&paths, "构建脚本源码")
            .err()
            .unwrap_or_else(|| panic!("{name}：{manifest:?} 派生的构建脚本路径必须失败关闭"));
        assert!(
            err.contains("控制字符"),
            "{name}：应指出控制字符，实际：{err}"
        );
        assert_eq!(
            err.lines().count(),
            1,
            "{name}：诊断必须单行，实际：{err:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn rejects_non_utf8_build_script_source_path() {
    // Linux 上非 UTF-8 的目录名合法存在，但 `display()` 是有损的，无法安全写进
    // 行协议：同样失败关闭，且诊断（lossy + 转义）仍须单行。
    use std::os::unix::ffi::OsStringExt;
    let manifest_dir = PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', b'w', 0xff, b's']));
    let paths = build_script_paths(&manifest_dir);
    let err = web_asset_gen::validate_rerun_paths(&paths, "构建脚本源码")
        .expect_err("非 UTF-8 的 manifest_dir 派生路径必须失败关闭");
    assert!(err.contains("UTF-8"), "应指出非法 UTF-8，实际：{err}");
    assert_eq!(err.lines().count(), 1, "诊断必须单行，实际：{err:?}");
}

#[test]
fn accepts_build_script_source_paths_without_control_chars() {
    // 中文/空格/引号/反斜杠等正常目录名必须放行——拒绝只针对控制字符。
    let manifest_dir = PathBuf::from("/工作区/My Project 'x'/rsetup-next");
    let paths = build_script_paths(&manifest_dir);
    web_asset_gen::validate_rerun_paths(&paths, "构建脚本源码")
        .unwrap_or_else(|err| panic!("合法 manifest_dir 派生路径不得被拒绝：{err}"));
    assert_eq!(
        paths,
        vec![
            manifest_dir.join("build.rs"),
            manifest_dir.join("build/web_assets.rs"),
        ],
        "构建脚本源码路径必须就是 manifest_dir 下这两个文件"
    );
}

#[test]
fn rerun_paths_of_a_real_build_pass_the_line_protocol_gate() {
    // 端到端：真实生成器产出的 rerun_paths + 本 crate 真实 manifest_dir 派生路径，
    // 合并后必须整体通过出口闸门（生产构建不得被这道闸门误伤）。
    let fixture = Fixture::new("rerun-gate");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write("assets/index-AAA222.css", b"body{margin:0}");
    fixture.index_html(&["/assets/index-AAA111.js", "/assets/index-AAA222.css"]);
    let generation = fixture
        .generate()
        .unwrap_or_else(|err| panic!("合成构建应生成成功：{err}"));

    let mut rerun_paths = generation.rerun_paths.clone();
    rerun_paths.extend(build_script_paths(Path::new(env!("CARGO_MANIFEST_DIR"))));
    web_asset_gen::validate_rerun_paths(&rerun_paths, "rerun 监听路径")
        .unwrap_or_else(|err| panic!("真实构建的 rerun 全量路径必须通过出口闸门：{err}"));
}

#[test]
fn build_script_checks_paths_before_printing_rerun_directives() {
    // 上面的用例证明校验器可靠；本用例锁住**集成点**：`build.rs` 里第一处
    // `cargo:rerun-if-changed=` 打印之前，必须已经出现控制字符校验调用。
    // （`cargo:rerun-if-env-changed=` 那条常量指令不带待校验路径，不会误命中本匹配。）
    let source = include_str!("../build.rs");
    let print_at = source
        .find("cargo:rerun-if-changed=")
        .expect("build.rs 必须向 cargo 声明 rerun-if-changed");
    let before_print = &source[..print_at];
    assert!(
        before_print.contains("reject_control_chars")
            || before_print.contains("validate_rerun_paths"),
        "build.rs 必须在 println cargo:rerun-if-changed 之前校验路径控制字符，\
         否则 CARGO_MANIFEST_DIR 派生路径可向 cargo 行协议注入指令（P2）"
    );
}

// ---------------------------------------------------------------------------
// 1e. 生成器：非法构建产物一律失败关闭
// ---------------------------------------------------------------------------

#[test]
fn rejects_missing_dist_directory() {
    let fixture = Fixture::new("reject-dist-missing");
    let missing = fixture.root.join("no-such-dist");
    let err = match web_asset_gen::generate(&missing, &fixture.snapshot()) {
        Ok(generation) => panic!(
            "dist 目录不存在时必须失败关闭，实际生成 {:?}",
            generation.assets.len()
        ),
        Err(err) => err,
    };
    assert!(
        err.contains("dist 目录不存在"),
        "错误说明应指出 dist 缺失，实际：{err}"
    );
    // 下一步提示必须是本仓**唯一**的前端构建命令（`build/web_assets.rs::BUILD_HINT`）：
    // 只断言含 "npm" 与 "run build" 会让旧的 pnpm 文案（`pnpm --filter … run build`）
    // 一并蒙混过关，因此这里逐字钉住两条完整命令，并显式排除 pnpm。
    assert!(
        err.contains("npm --prefix apps/controller-web ci"),
        "错误说明应给出安装依赖的确切命令，实际：{err}"
    );
    assert!(
        err.contains("npm --prefix apps/controller-web run build"),
        "错误说明应给出构建前端的确切命令，实际：{err}"
    );
    assert!(
        !err.to_lowercase().contains("pnpm"),
        "错误说明不得再指向已弃用的 pnpm 工作流，实际：{err}"
    );
}

#[test]
fn rejects_missing_index_html() {
    let fixture = Fixture::new("reject-index-html-missing");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    expect_generate_error(&fixture, "缺少 index.html");
}

#[test]
fn rejects_empty_index_html() {
    let fixture = Fixture::new("reject-index-html-empty");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write("index.html", b"");
    expect_generate_error(&fixture, "index.html 为空");
}

#[test]
fn rejects_index_html_that_is_not_utf8() {
    let fixture = Fixture::new("reject-index-html-not-utf8");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write("index.html", &[0xefu8, 0xbb, 0xbf, 0xff, 0xfe, 0x00, 0x80]);
    expect_generate_error(&fixture, "index.html 不是合法 UTF-8");
}

#[test]
fn rejects_index_html_without_javascript_entry() {
    let fixture = Fixture::new("reject-index-html-without-js");
    fixture.write("assets/index-AAA222.css", b"body{margin:0}");
    fixture.index_html(&["/assets/index-AAA222.css"]);
    expect_generate_error(&fixture, "未引用任何 JS 入口资产");
}

#[test]
fn rejects_referenced_asset_that_does_not_exist() {
    let fixture = Fixture::new("reject-referenced-asset-missing");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write("assets/index-AAA222.css", b"body{margin:0}");
    // 入口 HTML 引用了一个磁盘上不存在的哈希名（伪造/半成品构建）。
    fixture.index_html(&[
        "/assets/index-AAA111.js",
        "/assets/index-AAA222.css",
        "/assets/index-OLD999.js",
    ]);
    expect_generate_error(&fixture, "引用的资产在 dist/assets 下不存在");
}

#[test]
fn rejects_empty_asset_file() {
    let fixture = Fixture::new("reject-asset-empty");
    fixture.write("assets/index-AAA111.js", b"");
    fixture.write("assets/index-AAA222.css", b"body{margin:0}");
    fixture.index_html(&["/assets/index-AAA111.js", "/assets/index-AAA222.css"]);
    expect_generate_error(&fixture, "资产为空文件");
}

#[test]
fn rejects_path_traversal_in_index_reference() {
    let fixture = Fixture::new("reject-traversal-reference");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.index_html(&["/assets/../../../etc/passwd"]);
    expect_generate_error(&fixture, "非法资产名");
}

#[test]
fn rejects_absolute_asset_reference_under_asset_prefix() {
    let fixture = Fixture::new("reject-absolute-reference");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.index_html(&["/assets//etc/passwd"]);
    expect_generate_error(&fixture, "非法资产名");
}

#[test]
fn rejects_url_encoded_asset_reference() {
    let fixture = Fixture::new("reject-encoded-reference");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.index_html(&["/assets/%2e%2e%2fsecret.js"]);
    expect_generate_error(&fixture, "非法资产名");
}

#[cfg(unix)]
#[test]
fn rejects_symlink_inside_assets() {
    let fixture = Fixture::new("reject-symlink-inside-assets");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write("assets/index-AAA222.css", b"body{margin:0}");
    fixture.index_html(&["/assets/index-AAA111.js", "/assets/index-AAA222.css"]);
    std::os::unix::fs::symlink("/etc/passwd", fixture.dist.join("assets/evil-LLL.js"))
        .expect("创建 fixture 符号链接");
    expect_generate_error(&fixture, "符号链接");
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_dist_directory() {
    let fixture = Fixture::new("reject-symlinked-dist");
    let real = fixture.root.join("real-dist");
    fs::remove_dir_all(&fixture.dist).expect("移空 dist 以便放符号链接");
    fs::create_dir_all(real.join("assets")).expect("创建真实 dist");
    fs::write(real.join("assets/index-AAA111.js"), b"console.log(1);").unwrap();
    fs::write(
        real.join("index.html"),
        b"<script src=\"/assets/index-AAA111.js\"></script>",
    )
    .unwrap();
    std::os::unix::fs::symlink(&real, &fixture.dist).expect("把 dist 做成符号链接");
    expect_generate_error(&fixture, "符号链接");
}

#[cfg(unix)]
#[test]
fn allows_symlinked_ancestor_above_dist_root() {
    // P3 边界：受信任构建根之上的祖先可以是符号链接（macOS 上 /tmp 等合法），
    // 只有从选定 dist 根往下才严格拒绝内部符号链接。这里 dist 本身是真实目录。
    let fixture = Fixture::new("allow-symlink-ancestor");
    let real_parent = fixture.root.join("real-parent");
    fs::create_dir_all(real_parent.join("dist/assets")).expect("建真实父目录");
    fs::write(
        real_parent.join("dist/assets/index-AAA111.js"),
        b"console.log(1);",
    )
    .unwrap();
    fs::write(
        real_parent.join("dist/index.html"),
        b"<script src=\"/assets/index-AAA111.js\"></script>",
    )
    .unwrap();
    // 通过一个指向真实父目录的符号链接访问 dist 根：祖先链含符号链接，但 dist 根真实。
    let link_parent = fixture.root.join("link-parent");
    std::os::unix::fs::symlink(&real_parent, &link_parent).expect("父级符号链接");
    let dist_via_link = link_parent.join("dist");
    let snapshot = fixture.root.join("snapshot");
    // dist_via_link 经符号链接解析后指向真实目录；symlink_metadata(dist_via_link)
    // 落在真实目录上（中间件才是链接），因此应通过，证明不追溯工作区祖先符号链接。
    web_asset_gen::generate(&dist_via_link, &snapshot)
        .unwrap_or_else(|err| panic!("祖先符号链接应被允许（dist 根真实）：{err}"));
}

#[test]
fn rejects_unknown_asset_extension() {
    let fixture = Fixture::new("reject-unknown-extension");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write("assets/tool-BBB.exe", b"MZ");
    fixture.index_html(&["/assets/index-AAA111.js"]);
    expect_generate_error(&fixture, "MIME");
}

#[test]
fn rejects_source_map_asset() {
    let fixture = Fixture::new("reject-source-map");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write("assets/index-AAA111.js.map", b"{\"version\":3}");
    fixture.index_html(&["/assets/index-AAA111.js"]);
    expect_generate_error(&fixture, "source map");
}

#[test]
fn rejects_hidden_files_in_assets() {
    let fixture = Fixture::new("reject-hidden-file");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write("assets/.env-CCC", b"SECRET=1");
    fixture.index_html(&["/assets/index-AAA111.js"]);
    expect_generate_error(&fixture, "隐藏文件");
}

#[test]
fn rejects_local_reference_outside_asset_directory() {
    let fixture = Fixture::new("reject-local-ref-outside-assets");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write_raw_html(
        "<!doctype html><link rel=\"icon\" href=\"/favicon.ico\"><script src=\"/assets/index-AAA111.js\"></script>",
    );
    expect_generate_error(&fixture, "/assets/ 之外的本地引用");
}

#[test]
fn allows_external_and_data_references_outside_asset_directory() {
    let fixture = Fixture::new("allow-external-refs");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write_raw_html(
        "<!doctype html><link rel=\"stylesheet\" href=\"https://cdn.example/x.css\"><link rel=\"icon\" href=\"data:image/svg+xml,\"><script src=\"/assets/index-AAA111.js\"></script>",
    );

    let generation = fixture
        .generate()
        .unwrap_or_else(|err| panic!("外链与 data: 引用由浏览器取得，不得让构建失败：{err}"));
    assert_eq!(generated_pairs(&generation).len(), 1);
}

#[test]
fn emits_rerun_if_changed_for_every_embedded_input() {
    let fixture = Fixture::new("rerun-paths");
    let js = fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write("assets/index-AAA222.css", b"body{margin:0}");
    let index_html = fixture.index_html(&["/assets/index-AAA111.js", "/assets/index-AAA222.css"]);

    let generation = fixture
        .generate()
        .unwrap_or_else(|err| panic!("合成构建应生成成功：{err}"));

    // 新增/删除资产靠 dist/assets 目录本身触发重跑，替换靠逐文件与 index.html 触发。
    for expected in [&index_html, &js, &fixture.dist.join("assets")] {
        assert!(
            generation.rerun_paths.contains(expected),
            "缺少 rerun-if-changed 目标 {}；实际：{:?}",
            expected.display(),
            generation.rerun_paths
        );
    }
    assert!(
        generation
            .rerun_paths
            .iter()
            .all(|path| path.starts_with(&fixture.root)),
        "rerun 目标必须都落在本次 dist 内：{:?}",
        generation.rerun_paths
    );
    // rerun 目标不得含控制字符（会被逐行写入 cargo 行协议，P2）。
    for path in &generation.rerun_paths {
        let text = path.to_str().expect("rerun 路径应为 UTF-8");
        assert!(
            !text.bytes().any(|b| b < 0x20 || b == 0x7f),
            "rerun 目标不得含控制字符：{text:?}"
        );
    }
}

#[test]
fn emits_rerun_if_changed_for_nested_asset_source() {
    // 嵌套 rerun 回归：深层 chunk 的来源文件本身也要被监听。
    let fixture = Fixture::new("rerun-nested");
    let nested = fixture.write("assets/deep/nested-EEE444.js", b"export const x=1;");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.index_html(&["/assets/index-AAA111.js"]);

    let generation = fixture
        .generate()
        .unwrap_or_else(|err| panic!("含嵌套 chunk 的合成构建应生成成功：{err}"));
    assert!(
        generation.rerun_paths.contains(&nested),
        "嵌套资产来源必须在 rerun 目标里：{:?}",
        generation.rerun_paths
    );
    assert!(
        generation
            .rerun_paths
            .contains(&fixture.dist.join("assets")),
        "assets 目录本身必须在 rerun 目标里：{:?}",
        generation.rerun_paths
    );
}

// ---------------------------------------------------------------------------
// 1f. 生产路由构造器 + 合成构建：换一次构建就服务另一份字节
// ---------------------------------------------------------------------------

use rsetup_controller::web::{ASSET_DIR, EmbeddedAsset, web_router_from};

/// 把生成表按 include_bytes 的语义展开后注入生产路由构造器。
///
/// 生产路径经 `web_router()` 传入编译期 `include_bytes!`（只指向快照）的表；本函数
/// 把同一批快照字节在测试进程内以 `&'static` 形式交给**同一个**构造器，不复制任何
/// 路由逻辑。读的是快照，与编译期 include 的来源一致（P1）。
fn router_from_generation(generation: &web_asset_gen::Generation) -> axum::Router {
    let index_html: &'static str = Box::leak(
        fs::read_to_string(&generation.index_html_snapshot)
            .unwrap()
            .into_boxed_str(),
    );
    let assets: &'static [EmbeddedAsset] = Box::leak(
        generation
            .assets
            .iter()
            .map(|asset| EmbeddedAsset {
                path: Box::leak(asset.url_path.clone().into_boxed_str()) as &str,
                mime: asset.mime,
                bytes: Box::leak(fs::read(&asset.snapshot).unwrap().into_boxed_slice()) as &[u8],
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );
    web_router_from(index_html, assets)
}

fn first_build() -> web_asset_gen::Generation {
    let fixture = Fixture::new("router-build-one");
    fixture.write("assets/index-ONE001.js", b"console.log(\"one\");");
    fixture.write("assets/index-ONE002.css", b"body{margin:0}");
    fixture.write("assets/chunk-lazy-ONE003.js", b"export const one = 1;");
    fixture.index_html(&["/assets/index-ONE001.js", "/assets/index-ONE002.css"]);
    fixture.generate().expect("第一次合成构建应生成成功")
}

fn second_build() -> web_asset_gen::Generation {
    let fixture = Fixture::new("router-build-two");
    fixture.write("assets/index-TWO101.js", b"console.log(\"two\");");
    fixture.write("assets/index-TWO102.css", b"body{margin:1}");
    fixture.write("assets/chunk-lazy-TWO103.js", b"export const two = 2;");
    fixture.write("assets/chunk-lazy-TWO104.js", b"export const twoMore = 3;");
    fixture.index_html(&["/assets/index-TWO101.js", "/assets/index-TWO102.css"]);
    fixture.generate().expect("第二次合成构建应生成成功")
}

#[tokio::test]
async fn router_serves_newest_build_and_404s_previous_hashes() {
    let first = router_from_generation(&first_build());
    let second = router_from_generation(&second_build());

    // 上一轮构建的哈希名：只可能来自旧二进制，坚决 404，不得由新构建兜底。
    for stale in ["/assets/index-ONE001.js", "/assets/chunk-lazy-ONE003.js"] {
        let res = second
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(stale)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::NOT_FOUND,
            "陈旧资产 {stale} 必须 404"
        );
    }

    // 新构建的入口与 index.html 未引用的动态 chunk 都由同一路由交付真实字节。
    for (url, expected) in [
        (
            "/assets/index-TWO101.js",
            "console.log(\"two\");".as_bytes(),
        ),
        ("/assets/index-TWO102.css", "body{margin:1}".as_bytes()),
        (
            "/assets/chunk-lazy-TWO103.js",
            "export const two = 2;".as_bytes(),
        ),
        (
            "/assets/chunk-lazy-TWO104.js",
            "export const twoMore = 3;".as_bytes(),
        ),
    ] {
        let res = second
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(url)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "GET {url}");
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), expected, "{url} 必须交付本轮构建字节");
    }

    // 第一轮构建的路由仍服务第一轮字节（两次构建互不串味）。
    let res_first = first
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/assets/chunk-lazy-ONE003.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res_first.status(), StatusCode::OK);
    let body = to_bytes(res_first.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), "export const one = 1;".as_bytes());
}

#[tokio::test]
async fn router_head_mime_and_cache_contract_on_synthetic_chunks() {
    let router = router_from_generation(&second_build());

    // HEAD：200 + 正确 Content-Length + 空 body（供探活与预取）。
    let res_head = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri("/assets/chunk-lazy-TWO104.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res_head.status(), StatusCode::OK);
    assert_eq!(
        res_head.headers().get("content-type").unwrap(),
        "text/javascript"
    );
    assert_eq!(res_head.headers().get("content-length").unwrap(), "25");
    assert_eq!(
        res_head.headers().get("cache-control").unwrap(),
        "public, max-age=31536000, immutable"
    );
    assert!(
        to_bytes(res_head.into_body(), usize::MAX)
            .await
            .unwrap()
            .is_empty(),
        "HEAD 响应不得带 body"
    );

    // 页面路由：回退入口 HTML + no-cache；API 与未知路径不回退 HTML。
    let res_page = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/devices/42")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res_page.status(), StatusCode::OK);
    assert_eq!(
        res_page.headers().get("content-type").unwrap(),
        "text/html; charset=utf-8"
    );
    assert_eq!(res_page.headers().get("cache-control").unwrap(), "no-cache");

    let res_api = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        res_api.status(),
        StatusCode::OK,
        "API 路径不得被 web 线接管"
    );

    let res_post = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/assets/index-TWO101.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res_post.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[test]
fn asset_url_prefix_matches_the_web_module_constant() {
    // 生成器（构建期）与 web 模块（运行期）各自持有同值字面量；一旦漂移，
    // 资产会挂到错误前缀下。两条路径分属构建期与运行期，只能在此处对齐。
    assert_eq!(web_asset_gen::ASSET_URL_PREFIX, ASSET_DIR);
}

// ---------------------------------------------------------------------------
// 1g. 第二轮整改（P2 指纹）：mtime/ctime 参与判定 + 生成末尾终核集合
// ---------------------------------------------------------------------------

/// 同 inode、同长度、mtime 未恢复的**原地重写**钩子。
///
/// 刻意不截断（`OpenOptions::write` + 等长覆写）：截断会出现短暂的 0 长度窗口，让拒绝原因
/// 漂到「空文件」上，掩盖真正要验的 mtime/ctime 维度。这正是审查指出的「首 32 字节后写新
/// 内容、同 inode 同长度」撕裂产物形态。
fn rewrite_in_place_same_length(path: &Path, bytes: &[u8], round: u64) -> std::io::Result<()> {
    let mut buffer = bytes.to_vec();
    for (index, byte) in buffer.iter_mut().enumerate() {
        *byte = b'0' + ((index + usize::try_from(round).unwrap_or(0)) % 10) as u8;
    }
    let mut file = fs::OpenOptions::new().write(true).open(path)?;
    std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(0))?;
    std::io::Write::write_all(&mut file, &buffer)?;
    std::io::Write::flush(&mut file)
}

/// 只改 index.html 尾部注释里的一个字符：长度不变、HTML 仍合法、引用不变，
/// 于是「唯一可能的变化」就是 mtime/ctime，拒绝原因不会漂到别处。
fn rewrite_html_comment_byte(path: &Path, offset: usize, round: u64) -> std::io::Result<()> {
    let mut buffer = fs::read(path)?;
    buffer[offset] = b'0' + u8::try_from(round % 10).unwrap();
    let mut file = fs::OpenOptions::new().write(true).open(path)?;
    std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(0))?;
    std::io::Write::write_all(&mut file, &buffer)?;
    std::io::Write::flush(&mut file)
}

/// 生成期间持续原地重写 `target`（同 inode 同长度），断言生成器必须失败关闭。
///
/// 各处指纹点位（metadata 预检 stat -> 打开句柄 fstat -> 读后 stat -> 预检统计与读取的
/// 跨阶段比对 -> 生成末尾终核）比对的是同一份判定规则，重写落在哪个窗口就由哪个点位挡下
/// （本机实测常落在「预检与打开之间」），Err 说明均含「来源指纹」。只比 dev/ino/len 的旧
/// 实现全程返回 Ok（见 RED 探针）；「预检与读取之间」这一格的确定性覆盖见
/// `rejects_in_place_rewrite_between_precheck_and_read`。
fn assert_concurrent_rewrite_is_rejected<F>(
    target: &Path,
    dist: &Path,
    snapshot: &Path,
    rewrite: F,
) -> String
where
    F: Fn(&Path, u64) -> std::io::Result<()> + Send + Sync + 'static,
{
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let flag = std::sync::Arc::clone(&stop);
    let counter = std::sync::Arc::clone(&writes);
    let target = target.to_path_buf();
    let writer = std::thread::spawn(move || {
        let mut round = 0u64;
        while !flag.load(std::sync::atomic::Ordering::Relaxed) {
            if rewrite(&target, round).is_ok() {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            round = round.wrapping_add(1);
            std::thread::yield_now();
        }
    });

    let result = web_asset_gen::generate(dist, snapshot);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    writer.join().expect("重写线程回收");

    assert!(
        writes.load(std::sync::atomic::Ordering::Relaxed) > 0,
        "钩子必须真的在生成期间重写过来源，否则本测试没有意义"
    );
    let err = expect_reject(result, "生成期间来源被同 inode 同长度原地改写");
    assert!(
        err.contains("来源指纹"),
        "错误说明应指出来源指纹（mtime/ctime）变化，实际：{err}"
    );
    err
}

#[test]
fn rejects_in_place_rewrite_of_same_length_asset() {
    // 反例：读 1 MiB 资产期间/之后，同一 inode 同长度写新内容且不恢复 mtime。
    // 只比 dev/ino/len 的实现会得到「身份未变、长度未变」的撕裂 JS 并返回 Ok。
    let fixture = Fixture::new("fp-asset-rewrite");
    let target = fixture.write("assets/aardvark-AAA111.js", b"console.log(\"ORIGINAL\");");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    // 长尾：把整体生成时长拉到毫秒级，让重写落在读取之后的窗口里。
    let tail = vec![b'x'; 1 << 20];
    for index in 0..20 {
        fixture.write(&format!("assets/tail-{index:03}.js"), &tail);
    }
    let mut references = vec!["/assets/aardvark-AAA111.js", "/assets/index-AAA111.js"];
    let names: Vec<String> = (0..20)
        .map(|index| format!("/assets/tail-{index:03}.js"))
        .collect();
    for name in &names {
        references.push(name.as_str());
    }
    fixture.index_html(&references);

    let snapshot = fixture.snapshot();
    let err = assert_concurrent_rewrite_is_rejected(
        &target,
        &fixture.dist,
        &snapshot,
        |path: &Path, round: u64| {
            rewrite_in_place_same_length(path, b"console.log(\"ORIGINAL\");", round)
        },
    );
    assert!(
        err.contains("aardvark-AAA111.js"),
        "错误说明必须指认被改写的具体来源，实际：{err}"
    );
}

#[test]
fn rejects_in_place_rewrite_of_index_html() {
    // HTML 指纹同样不能丢：生成期间原地改写 index.html（长度不变、只动注释里一个字节）
    // 必须失败关闭，否则「HTML 新 / 资产表旧」的坏包重新可能出现。
    let fixture = Fixture::new("fp-html-rewrite");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    let tail = vec![b'x'; 1 << 20];
    for index in 0..20 {
        fixture.write(&format!("assets/tail-{index:03}.js"), &tail);
    }
    let names: Vec<String> = (0..20)
        .map(|index| format!("/assets/tail-{index:03}.js"))
        .collect();
    let mut references = vec!["/assets/index-AAA111.js"];
    for name in &names {
        references.push(name.as_str());
    }
    // 直接构造带尾部注释的 HTML，注释内字节可安全改写。
    let mut html = String::from("<!doctype html><script src=\"/assets/index-AAA111.js\"></script>");
    for name in &names {
        html.push_str(&format!("<link rel=\"stylesheet\" href=\"{name}\">"));
    }
    let comment_start = html.len() + "<!--".len() + 8;
    html.push_str("<!--");
    html.push_str(&"p".repeat(64));
    html.push_str("-->");
    assert!(comment_start < html.len() - 2);
    let index_path = fixture.write_raw_html(&html);

    let snapshot = fixture.snapshot();
    let err = assert_concurrent_rewrite_is_rejected(
        &index_path,
        &fixture.dist,
        &snapshot,
        move |path: &Path, round: u64| rewrite_html_comment_byte(path, comment_start, round),
    );
    assert!(err.contains("index.html"), "应指认 index.html，实际：{err}");
}

// ---------------------------------------------------------------------------
// 1h. 第二轮整改（P2）：srcset 家族按「URL token + 描述符」真实边界切分
// ---------------------------------------------------------------------------

/// 用任意起始标签驱动 tokenizer，返回本地引用集合（走生产 `collect_local_references`）。
fn references_of(tag: &str) -> Result<std::collections::BTreeSet<String>, String> {
    web_asset_gen::collect_local_references(&format!("<!doctype html>{tag}"))
}

#[test]
fn classifies_both_srcset_family_attributes() {
    // 分类覆盖：`srcset` 与 `imagesrcset` 都必须既在候选表家族里、也在外层 URL 属性表里。
    // 少一项的后果是该属性整体不被分类 -> preload 缺图照样受理、运行期 404。
    for attribute in ["srcset", "imagesrcset"] {
        let tag =
            format!("<link rel=\"preload\" as=\"image\" {attribute}=\"/assets/pic-1x.png 1x\">");
        let references = references_of(&tag)
            .unwrap_or_else(|err| panic!("{attribute} 必须被分类为候选表：{err}"));
        assert!(
            references.contains("/assets/pic-1x.png"),
            "{attribute} 的候选未被收集：{references:?}"
        );
    }
}

#[test]
fn rejects_image_srcset_reference_to_missing_asset() {
    // `<link rel=preload as=image imagesrcset=...>` 指向不存在的图：必须构建期拒绝。
    let fixture = Fixture::new("srcset-imagesrcset-missing");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write_raw_html(
        "<script src=\"/assets/index-AAA111.js\"></script>\
         <link rel=\"preload\" as=\"image\" imagesrcset=\"/assets/ghost-1x.png 1x, /assets/ghost-2x.png 2x\">",
    );
    expect_generate_error(&fixture, "不存在");
}

#[test]
fn embeds_image_srcset_candidates() {
    let fixture = Fixture::new("srcset-imagesrcset-ok");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write("assets/pic-1x.png", b"PNG1");
    fixture.write("assets/pic-2x.png", b"PNG22");
    fixture.write_raw_html(
        "<script src=\"/assets/index-AAA111.js\"></script>\
         <link rel=\"preload\" as=\"image\" imagesrcset=\"/assets/pic-1x.png 1x, /assets/pic-2x.png 2x\">",
    );
    let generation = fixture
        .generate()
        .unwrap_or_else(|err| panic!("imagesrcset 候选应被逐个嵌入：{err}"));
    let urls: Vec<&str> = generation
        .assets
        .iter()
        .map(|asset| asset.url_path.as_str())
        .collect();
    assert!(urls.contains(&"/assets/pic-1x.png"), "{urls:?}");
    assert!(urls.contains(&"/assets/pic-2x.png"), "{urls:?}");
}

#[test]
fn rejects_srcset_url_with_internal_comma() {
    // 真实反例：`srcset="/assets/a.png,/assets/b.png"` 无任何空白/descriptor。
    // 按 HTML 规范浏览器把它当成**一个**含内部逗号的 URL；按逗号凭空拆成两条、
    // 再因为「两个文件都在」而放行的实现是错的（它证明不了构建产物可用）。
    let fixture = Fixture::new("srcset-internal-comma");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write("assets/pic-1x.png", b"PNG1");
    fixture.write("assets/pic-2x.png", b"PNG22");
    fixture.write_raw_html(
        "<script src=\"/assets/index-AAA111.js\"></script>\
         <img srcset=\"/assets/pic-1x.png,/assets/pic-2x.png\">",
    );
    let err = expect_reject(fixture.generate(), "srcset URL 内含逗号");
    assert!(
        err.contains("内部逗号"),
        "应说明不得凭空拆逗号，实际：{err}"
    );
}

#[test]
fn rejects_image_srcset_url_with_internal_comma() {
    // imagesrcset 走同一规则。
    let fixture = Fixture::new("imagesrcset-internal-comma");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write("assets/pic-1x.png", b"PNG1");
    fixture.write("assets/pic-2x.png", b"PNG22");
    fixture.write_raw_html(
        "<script src=\"/assets/index-AAA111.js\"></script>\
         <link rel=\"preload\" as=\"image\" imagesrcset=\"/assets/pic-1x.png,/assets/pic-2x.png\">",
    );
    let err = expect_reject(fixture.generate(), "imagesrcset URL 内含逗号");
    assert!(err.contains("内部逗号"), "实际：{err}");
}

#[test]
fn supports_common_srcset_descriptors() {
    // 常见合法候选形态必须继续支持：1x / 2x / 1.5x / 320w（含无空格分隔）。
    let fixture = Fixture::new("srcset-descriptors");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write("assets/pic-1x.png", b"PNG1");
    fixture.write("assets/pic-2x.png", b"PNG22");
    fixture.write("assets/pic-15x.png", b"PNG15");
    fixture.write("assets/pic-w.png", b"PNGW");
    fixture.write_raw_html(
        "<script src=\"/assets/index-AAA111.js\"></script>\
         <img srcset=\"/assets/pic-1x.png 1x,/assets/pic-2x.png 2x, /assets/pic-15x.png 1.5x , /assets/pic-w.png 320w,\">",
    );
    let generation = fixture
        .generate()
        .unwrap_or_else(|err| panic!("常见 1x/2x/1.5x/320w 候选应支持：{err}"));
    let urls: Vec<&str> = generation
        .assets
        .iter()
        .map(|asset| asset.url_path.as_str())
        .collect();
    for expected in [
        "/assets/pic-1x.png",
        "/assets/pic-2x.png",
        "/assets/pic-15x.png",
        "/assets/pic-w.png",
    ] {
        assert!(urls.contains(&expected), "{expected} 缺失：{urls:?}");
    }
}

#[test]
fn rejects_unsupported_srcset_descriptors() {
    // 不支持的描述符子集：明确 failclosed，不猜一个候选后放行。
    for (case, value) in [
        ("多描述符", "/assets/pic-1x.png 1x 2x"),
        ("媒体查询式", "/assets/pic-1x.png (min-resolution: 2dppx)"),
        ("未知单位", "/assets/pic-1x.png 2h"),
        ("零宽度", "/assets/pic-1x.png 0w"),
    ] {
        let fixture = Fixture::new(&format!("srcset-descriptor-{case}"));
        fixture.write("assets/index-AAA111.js", b"console.log(1);");
        fixture.write("assets/pic-1x.png", b"PNG1");
        fixture.write_raw_html(&format!(
            "<script src=\"/assets/index-AAA111.js\"></script><img srcset=\"{value}\">"
        ));
        let err = expect_reject(fixture.generate(), "{case} 不在受支持子集");
        assert!(
            err.contains("描述符"),
            "{case} 的错误说明应指向描述符，实际：{err}"
        );
    }
}

#[test]
fn keeps_plain_url_attributes_unsplit() {
    // 普通 src/href 退回「单个完整 URL」：含逗号即非法资产名，绝不退化成按逗号拆分。
    let fixture = Fixture::new("url-attr-comma");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.write("assets/pic-1x.png", b"PNG1");
    fixture.write("assets/pic-2x.png", b"PNG22");
    fixture.write_raw_html(
        "<script src=\"/assets/index-AAA111.js\"></script>\
         <img src=\"/assets/pic-1x.png,/assets/pic-2x.png\">",
    );
    let err = expect_reject(fixture.generate(), "单个 URL 属性含逗号");
    assert!(err.contains("非法资产名"), "实际：{err}");
}

// ---------------------------------------------------------------------------
// 1i. 第二轮整改（P3）：输入/输出重叠必须在任何清理之前被拒绝且不破坏输入
// ---------------------------------------------------------------------------

/// 断言 dist 输入完好（拒绝路径绝不先删输入再报错）。
fn assert_dist_inputs_intact(dist: &Path) {
    assert!(
        dist.join("index.html").is_file(),
        "index.html 必须仍在：输入被清理破坏"
    );
    assert!(
        dist.join("assets/index-AAA111.js").is_file(),
        "资产必须仍在：输入被清理破坏"
    );
    assert!(
        dist.join("assets/index-AAA222.css").is_file(),
        "资产必须仍在：输入被清理破坏"
    );
}

#[test]
fn rejects_snapshot_dir_identical_to_dist() {
    // generate(dist, dist)：快照 index.html 会直接覆盖输入。必须在写任何东西之前拒绝。
    let (root, dist, _snapshot) = fixture_root_with("overlap-same", "dist");
    write_minimal_dist(&dist);
    let err = expect_reject(
        web_asset_gen::generate(&dist, &dist),
        "快照目录与 dist 同一",
    );
    assert!(err.contains("同一目录"), "实际：{err}");
    assert_dist_inputs_intact(&dist);
    let _ = root;
}

#[test]
fn rejects_snapshot_dir_inside_dist() {
    let (root, dist, _snapshot) = fixture_root_with("overlap-inside", "dist");
    write_minimal_dist(&dist);
    let nested = dist.join("web_assets_snapshot");
    let err = expect_reject(
        web_asset_gen::generate(&dist, &nested),
        "快照目录在 dist 之内",
    );
    assert!(err.contains("之内"), "实际：{err}");
    assert_dist_inputs_intact(&dist);
    assert!(!nested.exists(), "被拒的输出目录不得被创建");
    let _ = root;
}

#[test]
fn rejects_dist_inside_snapshot_dir() {
    // 清空旧快照 = 删输入，必须先拒。
    let (root, _dist, snapshot) = fixture_root_with("overlap-inverse", "unused");
    let dist = snapshot.join("inner-dist");
    write_minimal_dist(&dist);
    let err = expect_reject(
        web_asset_gen::generate(&dist, &snapshot),
        "dist 在快照目录之内",
    );
    assert!(err.contains("之内"), "实际：{err}");
    assert_dist_inputs_intact(&dist);
    let _ = root;
}

#[cfg(unix)]
#[test]
fn rejects_overlapping_paths_through_symlink_alias() {
    // 字面量看不出的重叠：快照目录经符号链接别名落进 dist 之内 / 与 dist 同一。
    let (root, dist, _snapshot) = fixture_root_with("overlap-alias", "dist");
    write_minimal_dist(&dist);
    let alias = root.join("dist-alias");
    std::os::unix::fs::symlink(&dist, &alias).expect("建 dist 符号链接别名");

    let nested_via_alias = alias.join("web_assets_snapshot");
    let err = expect_reject(
        web_asset_gen::generate(&dist, &nested_via_alias),
        "符号链接别名使快照落进 dist 之内",
    );
    assert!(err.contains("之内"), "实际：{err}");
    assert_dist_inputs_intact(&dist);

    let same_via_alias = alias.clone();
    let err = expect_reject(
        web_asset_gen::generate(&dist, &same_via_alias),
        "符号链接别名使快照与 dist 同一",
    );
    assert!(
        err.contains("同一目录") || err.contains("快照目录不得是符号链接"),
        "别名重叠应被拒绝（同目录判定或符号链接输出判定），实际：{err}"
    );
    assert_dist_inputs_intact(&dist);
    let _ = root;
}

#[test]
fn accepts_disjoint_output_and_keeps_inputs() {
    // 对照组：正常分离的输入/输出必须照常成功（拒绝策略不是「一律失败」）。
    let (root, dist, snapshot) = fixture_root_with("overlap-ok", "dist");
    write_minimal_dist(&dist);
    let generation = web_asset_gen::generate(&dist, &snapshot)
        .unwrap_or_else(|err| panic!("分离的输入/输出必须成功：{err}"));
    assert_eq!(generation.assets.len(), 2);
    assert_dist_inputs_intact(&dist);
    let _ = root;
}

// ---------------------------------------------------------------------------
// 1k. 第三轮整改（P3）：路径尾缀不得绕过末端符号链接门禁；尾段 `..` 必须参与消解
//
// 两条独立根因：
// 1. POSIX lookup 对以 `/` 或 `/.` 结尾的路径**强制跟随**末端符号链接，
//    `symlink_metadata("symlink_dir/")` 因此返回链接目标的目录 metadata，
//    `is_symlink()` 恒 false —— 一个尾缀字符就整体绕过「dist 根/快照目录不得是符号链接」。
// 2. `resolve_existing_prefix` 只把「canonicalize 成功的最深祖先」之后的尾段**原样接回**，
//    尾段里的 `..` 不被折叠，于是 `missing/../dist` 这类拼写看不出与 dist 重叠，
//    输入/输出分离判定（含「先清理再报错」的 P3 保证）被绕过。
// ---------------------------------------------------------------------------

#[test]
fn strips_path_tails_that_hide_the_final_symlink() {
    // 纯函数口径：尾随分隔符与 `.` 段一律剥掉；中间段、根、空路径均不得被改写语义。
    let cases: &[(&str, &str)] = &[
        ("/b/dist", "/b/dist"),
        ("/b/dist/", "/b/dist"),
        ("/b/dist//", "/b/dist"),
        ("/b/dist/.", "/b/dist"),
        ("/b/dist/./", "/b/dist"),
        ("/b/dist/./.", "/b/dist"),
        ("/b/../dist/", "/b/../dist"),
        ("/", "/"),
        (".", "."),
        ("./", "."),
        ("/b/dist/..", "/b/dist/.."),
        ("", ""),
    ];
    for (input, expected) in cases {
        let stripped = web_asset_gen::strip_trailing_dot_segments(Path::new(input));
        assert_eq!(stripped, Path::new(expected), "剥除尾缀：{input:?}");
    }
    // 空路径绝不得退化成 `.`：那会把「不存在」偷换成 cwd（放行而不是失败关闭）。
    assert_ne!(
        web_asset_gen::strip_trailing_dot_segments(Path::new("")),
        Path::new("."),
        "空路径必须保持为空"
    );
}

#[test]
fn rejects_paths_ending_in_parent_dir_for_symlink_check() {
    // 末端是 `..` 时，被检查的条目身份取决于路径里每一段的解析（含符号链接跟随），
    // 一次 symlink_metadata 判不了 → 失败关闭。
    let err =
        web_asset_gen::normalize_dir_path_for_symlink_check(Path::new("/b/dist/.."), "dist 目录")
            .expect_err("末端 `..` 的条目身份无法由 symlink_metadata 判定，必须失败关闭");
    assert!(err.contains("以 `..` 结尾"), "实际：{err}");
    assert_eq!(err.lines().count(), 1, "诊断必须单行，实际：{err:?}");
    // 可剥尾缀的正常拼写必须照常放行。
    for ok in ["/b/dist", "/b/dist/", "/b/dist/.", "/b/../dist/", "/"] {
        web_asset_gen::normalize_dir_path_for_symlink_check(Path::new(ok), "dist 目录")
            .unwrap_or_else(|err| panic!("{ok:?} 应放行：{err}"));
    }
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_dist_directory_with_trailing_slash() {
    // 漏洞形态本体：dist 根是符号链接，拼写成 `dist/`、`dist//`、`dist/.`、`dist/./`。
    // 只比 is_symlink() 的实现下这些拼写全部通过门禁，实际读的是链接目标目录。
    for tail in ["/", "//", "/.", "/./"] {
        let fixture = Fixture::new("reject-symlinked-dist-tail");
        let real = fixture.root.join("real-dist");
        fs::remove_dir_all(&fixture.dist).expect("移空 dist 以便放符号链接");
        fs::create_dir_all(real.join("assets")).expect("创建真实 dist");
        fs::write(real.join("assets/index-AAA111.js"), b"console.log(1);").unwrap();
        fs::write(
            real.join("index.html"),
            b"<script src=\"/assets/index-AAA111.js\"></script>",
        )
        .unwrap();
        std::os::unix::fs::symlink(&real, &fixture.dist).expect("把 dist 做成符号链接");

        let spelled = PathBuf::from(format!("{}{}", fixture.dist.display(), tail));
        let err = expect_reject(
            web_asset_gen::generate(&spelled, &fixture.snapshot()),
            &format!("尾缀 {tail:?} 不得绕过 dist 根符号链接门禁"),
        );
        assert!(
            err.contains("符号链接"),
            "拼写 {spelled:?} 应被拒绝，实际：{err}"
        );
        assert!(
            !fixture.snapshot().exists(),
            "被拒调用不得写出任何快照：{spelled:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_dist_assets_directory_with_trailing_slash() {
    // 同一绕过对 `dist/assets` 同样成立：assets 是符号链接时 `assets/` 让
    // require_real_directory 看不见链接，越出构建目录的整棵目录树都会被当真资产嵌入。
    let fixture = Fixture::new("reject-symlinked-assets-tail");
    let outside = fixture.root.join("outside-assets");
    fs::create_dir_all(&outside).expect("外部资产目录");
    fs::write(outside.join("index-AAA111.js"), b"console.log(1);").unwrap();
    fs::remove_dir_all(fixture.dist.join("assets")).expect("移空 assets 以便放符号链接");
    std::os::unix::fs::symlink(&outside, fixture.dist.join("assets")).expect("assets 符号链接");
    fixture.index_html(&["/assets/index-AAA111.js"]);

    // 直接驱动预检入口：`assets/` 这一拼写必须仍被认成符号链接并拒绝。
    let spelled = PathBuf::from(format!("{}/", fixture.dist.join("assets").display()));
    let err = web_asset_gen::enumerate_asset_stats(&spelled)
        .expect_err("尾随斜杠不得绕过 dist/assets 符号链接门禁");
    assert!(err.contains("符号链接"), "实际：{err}");
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_snapshot_directory_with_trailing_slash() {
    // 输出侧同一绕过：`snapshot-link/` 让「快照目录不得是符号链接」失效，
    // 后果是把校验过的字节写进链接指向的目录（可指向 dist 之外任意可写位置）。
    let fixture = Fixture::new("reject-symlinked-snapshot-tail");
    write_minimal_dist(&fixture.dist);
    let real_out = fixture.root.join("real-out");
    fs::create_dir_all(&real_out).expect("真实输出目录");
    let link = fixture.root.join("snapshot-link");
    std::os::unix::fs::symlink(&real_out, &link).expect("快照目录符号链接");

    for tail in ["", "/", "/."] {
        let spelled = PathBuf::from(format!("{}{}", link.display(), tail));
        let err = expect_reject(
            web_asset_gen::generate(&fixture.dist, &spelled),
            "快照目录符号链接",
        );
        assert!(
            err.contains("快照目录不得是符号链接"),
            "拼写 {spelled:?} 应被拒绝，实际：{err}"
        );
    }
    let leaked: Vec<String> = fs::read_dir(&real_out)
        .expect("读链接目标目录")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        leaked.is_empty(),
        "被拒调用不得把快照写进链接目标目录：{leaked:?}"
    );
}

#[test]
fn rejects_dist_path_ending_in_parent_dir() {
    // 末端 `..`：条目身份取决于路径里每一段的解析（可能被符号链接带走），门禁无法判定，
    // 只能失败关闭；且必须发生在任何写入之前。
    let (_root, dist, snapshot) = fixture_root_with("dist-dotdot", "a/dist");
    write_minimal_dist(&dist);
    let spelled = dist.join("..");
    let err = expect_reject(
        web_asset_gen::generate(&spelled, &snapshot),
        "dist 路径以 .. 结尾",
    );
    assert!(err.contains("以 `..` 结尾"), "实际：{err}");
    assert!(!snapshot.exists(), "被拒调用不得写出快照");
    assert_dist_inputs_intact(&dist);
}

#[test]
fn rejects_snapshot_path_ending_in_parent_dir() {
    // 输出侧同一口径：`out/..` 让「先清理再报错」的 P3 保证失去可判定性。
    let (_root, dist, snapshot) = fixture_root_with("snapshot-dotdot", "dist");
    write_minimal_dist(&dist);
    let spelled = snapshot.join("..");
    let err = expect_reject(
        web_asset_gen::generate(&dist, &spelled),
        "快照目录以 .. 结尾",
    );
    assert!(err.contains("以 `..` 结尾"), "实际：{err}");
    assert!(!snapshot.exists(), "被拒调用不得写出快照");
    assert_dist_inputs_intact(&dist);
}

#[test]
fn accepts_trailing_slash_on_real_dist_and_snapshot() {
    // 对照组：真实目录 + 尾随斜杠必须照常成功，且生成物里的 include 路径不得带尾缀
    // （`include_bytes!("…/snap/")` 会让 rustc 以 ENOTDIR 失败）。
    let (root, dist, snapshot) = fixture_root_with("tail-ok", "dist");
    write_minimal_dist(&dist);
    let dist_slash = PathBuf::from(format!("{}/", dist.display()));
    let snapshot_slash = PathBuf::from(format!("{}/./", snapshot.display()));
    let generation = web_asset_gen::generate(&dist_slash, &snapshot_slash)
        .unwrap_or_else(|err| panic!("真实目录的尾随斜杠拼写必须可用：{err}"));

    assert_eq!(generation.dist_dir, dist, "dist 字段应为剥掉尾缀的拼写");
    assert_eq!(
        generation.snapshot_dir, snapshot,
        "快照字段应为剥掉尾缀的拼写"
    );
    let include_target = format!("{:?}", snapshot.join("index.html"));
    assert!(
        generation
            .rust_source
            .contains(&format!("include_str!({include_target})")),
        "include_str! 必须指向剥掉尾缀的快照文件"
    );
    assert!(
        !generation
            .rust_source
            .contains(&format!("{}/\"", snapshot.display())),
        "生成物内不得出现带尾随斜杠的目录字面量"
    );
    assert_dist_inputs_intact(&dist);
    let _ = root;
}

#[test]
fn rejects_output_overlap_through_unresolved_parent_segments() {
    // 尾段 `..` 必须参与消解：`missing` 不存在时 canonicalize 只能退到 root，
    // 旧实现把 `missing/../dist` 原样接回，两个重叠判定都看不出这是 dist 本身。
    let (root, dist, _snapshot) = fixture_root_with("overlap-dotdot", "dist");
    write_minimal_dist(&dist);
    assert!(!root.join("missing").exists(), "前提：中间段必须不存在");

    // 同一目录（经 `..` 拼写）：旧实现放行后会把 index.html 快照直接覆盖到输入上。
    let same_via_dotdot = root.join("missing").join("..").join("dist");
    let err = web_asset_gen::validate_output_separation(&dist, &same_via_dotdot)
        .expect_err("`missing/../dist` 消解后就是 dist 本身，必须判定为同一目录");
    assert!(err.contains("同一目录"), "实际：{err}");
    assert_dist_inputs_intact(&dist);

    // 快照落进 dist 之内（同一拼写技巧）。
    let nested_via_dotdot = root
        .join("missing")
        .join("..")
        .join("dist")
        .join("web_assets_snapshot");
    let err = web_asset_gen::validate_output_separation(&dist, &nested_via_dotdot)
        .expect_err("`missing/../dist/web_assets_snapshot` 消解后落在 dist 之内，必须拒绝");
    assert!(err.contains("之内"), "实际：{err}");
    assert_dist_inputs_intact(&dist);

    // 端到端同一形态：generate 自己也得在写任何东西之前拒绝，且输入完好。
    let err = expect_reject(
        web_asset_gen::generate(&dist, &same_via_dotdot),
        "端到端：同一目录别名必须由 generate 拒绝",
    );
    assert!(err.contains("同一目录"), "实际：{err}");
    assert_dist_inputs_intact(&dist);
    let _ = root;
}

#[test]
fn keeps_disjoint_paths_with_parent_segments_allowed() {
    // 对照组：消解 `..` 之后仍然分离的路径不得被误伤。
    let (root, dist, _snapshot) = fixture_root_with("overlap-dotdot-ok", "dist");
    write_minimal_dist(&dist);
    let out = root.join("missing").join("..").join("out");
    let generation = web_asset_gen::generate(&dist, &out)
        .unwrap_or_else(|err| panic!("`missing/../out` 消解后与 dist 分离，必须成功：{err}"));
    assert_eq!(generation.assets.len(), 2);
    assert_dist_inputs_intact(&dist);
    assert!(out.join("index.html").is_file(), "快照必须落在输出目录");
    let _ = root;
}

// ---------------------------------------------------------------------------
// 1m. 第四轮整改（P1）：`..` 折回真实目录**之后**的符号链接别名必须重新解析
//
// 旧实现只做「最深现存祖先 canonicalize + 其余尾段纯词法接回」。`missing` 不存在时祖先
// 只能退到 `root`，尾段 `missing/../alias/out` 里 `missing/..` 折回 `root` 之后，`alias`
// 被**按字面**接回：比较用的 `snapshot_real` 是 `root/alias/out`，与 `dist_real = root/dist`
// 既不等同也不是前缀，「同一目录」「落在 dist 之内」两个判定同时漏过。而真正写入时
// （本机实测）`create_dir_all` 会先把 `root/missing` 真建出来，此后内核就能解析
// `missing/..` 得到 `root`，再跟随 `alias -> dist`，把 index.html 快照直接落进
// `root/dist/out/` —— 输出穿透写进输入目录。
//
// 修复口径：真实路径必须**逐段**对照文件系统解析；`..` 一旦把路径折回一个真实存在的目录，
// 其后每一段都要重新查文件系统（符号链接段跟随到最终落点）。以下测试覆盖：穿透本体、
// 折回更深层真实目录后的别名、链接链、相对目标、`..` 与别名交替、解析不动的落点（悬空
// 链接 / 成环），以及「别名指向 dist 之外必须照常放行」的对照组。
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn rejects_snapshot_escaping_into_dist_through_alias_after_missing_segment() {
    // 漏洞形态本体：`missing/../alias/out`（`alias -> dist`）绝不能穿透写进 dist。
    let (root, dist, _snapshot) = fixture_root_with("p1-alias-dotdot", "dist");
    write_minimal_dist(&dist);
    std::os::unix::fs::symlink(&dist, root.join("alias")).expect("建 alias -> dist");
    assert!(
        !root.join("missing").exists(),
        "前提：中间段必须不存在（穿透的全部筹码都在这里）"
    );

    // 同一目录：`missing/../alias` 逐段解析后就是 dist 本身。
    let same_via_alias = root.join("missing").join("..").join("alias");
    let err = web_asset_gen::validate_output_separation(&dist, &same_via_alias)
        .expect_err("`missing/../alias` 解析后就是 dist 本身，必须判定为同一目录");
    assert!(err.contains("同一目录"), "实际：{err}");

    // 落在 dist 之内：`missing/../alias/out`。
    let nested_via_alias = root.join("missing").join("..").join("alias").join("out");
    let err = web_asset_gen::validate_output_separation(&dist, &nested_via_alias)
        .expect_err("`missing/../alias/out` 解析后落在 dist 之内，必须拒绝");
    assert!(err.contains("之内"), "实际：{err}");

    // 端到端：generate 必须在动任何文件之前拒绝，且绝不经 alias 穿透写进 dist。
    let err = expect_reject(
        web_asset_gen::generate(&dist, &nested_via_alias),
        "端到端：`missing/../alias/out` 别名穿透必须由 generate 拒绝",
    );
    assert!(err.contains("之内"), "实际：{err}");
    assert_dist_inputs_intact(&dist);
    assert!(
        !dist.join("out").exists(),
        "绝不允许经 alias 在 dist 之内建出输出目录"
    );
    assert!(
        !dist.join("out/index.html").is_file(),
        "绝不允许经 alias 把 index.html 快照写进 dist"
    );
    assert!(
        !root.join("missing").exists(),
        "拒绝必须发生在任何写入之前：不得先建出 `missing` 把路径变成内核可解析"
    );
    let _ = root;
}

#[cfg(unix)]
#[test]
fn revalidates_alias_after_popping_back_to_deeper_existing_dir() {
    // `..` 折回的不是根，而是真实存在的 `root/real`：其后的 `alias` 同样必须重新跟随。
    // 链接目标是**相对**路径（`../dist`），顺带覆盖 canonicalize 的相对目标解析。
    let (root, dist, _snapshot) = fixture_root_with("p1-alias-dotdot-deep", "dist");
    write_minimal_dist(&dist);
    let real = root.join("real");
    fs::create_dir_all(&real).expect("创建 root/real");
    std::os::unix::fs::symlink("../dist", real.join("alias")).expect("建 real/alias -> ../dist");
    let dist_real = dist.canonicalize().expect("dist 可 canonicalize");

    let nested = real.join("missing").join("..").join("alias").join("out");
    // 解析口径本身：落点必须是 `dist/out`，不是字面的 `real/alias/out`。
    let resolved =
        web_asset_gen::resolve_existing_prefix(&nested, "快照目录").expect("深层拼写应可解析");
    assert_eq!(
        resolved,
        dist_real.join("out"),
        "`real/missing/../alias/out` 必须解析到 dist 之内，而不是字面拼写"
    );

    let err = web_asset_gen::validate_output_separation(&dist, &nested)
        .expect_err("深层别名穿透落在 dist 之内，必须拒绝");
    assert!(err.contains("之内"), "实际：{err}");

    // 同一目录拼写（弹掉 alias 这一层就是 dist 本身）。
    let same = real.join("missing").join("..").join("alias");
    let err = web_asset_gen::validate_output_separation(&dist, &same)
        .expect_err("`real/missing/../alias` 解析后就是 dist 本身，必须判定同一目录");
    assert!(err.contains("同一目录"), "实际：{err}");

    let err = expect_reject(
        web_asset_gen::generate(&dist, &nested),
        "端到端：深层别名穿透必须由 generate 拒绝",
    );
    assert!(err.contains("之内"), "实际：{err}");
    assert_dist_inputs_intact(&dist);
    assert!(!dist.join("out").exists(), "不得经别名在 dist 之内建目录");
    assert!(!real.join("missing").exists(), "拒绝必须早于任何写入");
    let _ = root;
}

#[cfg(unix)]
#[test]
fn resolves_paths_component_by_component() {
    // 逐段解析的口径表：别名跟随、`..` 折回后重新解析、未创建尾段按字面保留。
    let (root, dist, _snapshot) = fixture_root_with("resolve-table", "dist");
    write_minimal_dist(&dist);
    let root_real = root.canonicalize().expect("fixture root 可 canonicalize");
    let dist_real = dist.canonicalize().expect("dist 可 canonicalize");
    std::os::unix::fs::symlink(&dist, root.join("alias")).expect("建 alias -> dist");
    std::os::unix::fs::symlink("alias", root.join("alias2")).expect("建 alias2 -> alias");
    std::os::unix::fs::symlink("dist", root.join("alias3")).expect("建 alias3 -> dist（相对目标）");
    fs::create_dir_all(root.join("real")).expect("创建 root/real");
    std::os::unix::fs::symlink("../dist", root.join("real/alias"))
        .expect("建 real/alias -> ../dist（链接目标按内核语义解析）");

    let resolve = |path: &Path| -> PathBuf {
        web_asset_gen::resolve_existing_prefix(path, "快照目录")
            .unwrap_or_else(|err| panic!("{} 应可解析：{err}", path.display()))
    };
    let cases: Vec<(&str, PathBuf, PathBuf)> = vec![
        (
            "别名段在开头：直接跟随",
            root.join("alias").join("out"),
            dist_real.join("out"),
        ),
        (
            "链接链 alias2 -> alias -> dist",
            root.join("missing").join("..").join("alias2").join("out"),
            dist_real.join("out"),
        ),
        (
            "相对目标 alias3 -> dist（链接目标也按内核语义解析）",
            root.join("missing").join("..").join("alias3"),
            dist_real.clone(),
        ),
        (
            "P1 本体：`missing/../alias/out`",
            root.join("missing").join("..").join("alias").join("out"),
            dist_real.join("out"),
        ),
        (
            "折回深层真实目录后的别名",
            root.join("real")
                .join("missing")
                .join("..")
                .join("alias")
                .join("..")
                .join("alias")
                .join("out"),
            dist_real.join("out"),
        ),
        (
            "别名与 `..` 交替：alias/../real/../alias",
            root.join("alias")
                .join("..")
                .join("real")
                .join("..")
                .join("alias"),
            dist_real.clone(),
        ),
        (
            "`.` 段忽略",
            root.join("./missing/../alias"),
            dist_real.clone(),
        ),
        (
            "末端 `..` 折回真实父目录",
            dist.join(".."),
            root_real.clone(),
        ),
        (
            "未创建段之后没有 `..`：按字面保留",
            root.join("missing").join("x").join("y"),
            root_real.join("missing/x/y"),
        ),
        (
            "未创建段之后的 `..` 只弹未创建层，仍在未创建状态",
            root.join("missing").join("x").join("..").join("y"),
            root_real.join("missing/y"),
        ),
        (
            "弹回真实目录后未创建层重新起算",
            root.join("missing")
                .join("x")
                .join("..")
                .join("..")
                .join("out")
                .join("z"),
            root_real.join("out/z"),
        ),
    ];
    for (what, input, expected) in cases {
        assert_eq!(
            resolve(&input),
            expected,
            "{what}：{} 的解析落点不符",
            input.display()
        );
    }
    let _ = root;
}

#[cfg(unix)]
#[test]
fn rejects_alias_penetration_spellings_end_to_end() {
    // 各种别名/穿透拼写：每一个都必须在 generate 写任何东西之前被挡下，
    // 且 dist 目录的条目集合不得发生任何变化。
    let (root, dist, _snapshot) = fixture_root_with("p1-combos", "dist");
    write_minimal_dist(&dist);
    fs::create_dir_all(root.join("real")).expect("创建 root/real");
    std::os::unix::fs::symlink(&dist, root.join("alias")).expect("建 alias -> dist");
    std::os::unix::fs::symlink("alias", root.join("alias2")).expect("建 alias2 -> alias");
    std::os::unix::fs::symlink("../dist", root.join("real/alias")).expect("建 real/alias");
    let entries = |dir: &Path| -> Vec<PathBuf> {
        let mut names: Vec<PathBuf> = fs::read_dir(dir)
            .unwrap_or_else(|err| panic!("读 {} 失败：{err}", dir.display()))
            .map(|entry| entry.expect("读目录项").path())
            .collect();
        names.sort();
        names
    };
    let before = entries(&dist);

    let spellings: Vec<PathBuf> = vec![
        root.join("missing/../alias/out"),
        root.join("missing/../alias"),
        root.join("missing/../alias2/assets"),
        root.join("real/missing/../alias/snap"),
        root.join("alias/missing/../../alias/snap"),
        root.join("missing/../alias/../real/alias/snap/deeper"),
        dist.join("missing/../alias/snap"),
        dist.join("assets/../../missing/../alias/snap"),
    ];
    for spelling in spellings {
        let err = match web_asset_gen::generate(&dist, &spelling) {
            Ok(generation) => panic!(
                "别名穿透必须拒绝：{}（实际成功嵌入 {} 项资产）",
                spelling.display(),
                generation.assets.len()
            ),
            Err(err) => err,
        };
        assert_eq!(err.lines().count(), 1, "诊断必须单行：{err:?}");
        assert!(
            err.contains("同一目录") || err.contains("之内"),
            "应命中重叠判定，实际：{err}"
        );
        assert_dist_inputs_intact(&dist);
        assert_eq!(
            entries(&dist),
            before,
            "被拒调用不得改动 dist 条目：{}",
            spelling.display()
        );
        assert!(
            !root.join("missing").exists(),
            "拒绝必须早于任何写入：不得建出 `missing` 让内核可解析：{}",
            spelling.display()
        );
    }
    let _ = root;
}

#[cfg(unix)]
#[test]
fn rejects_output_whose_landing_point_cannot_be_resolved() {
    // 落点解析不出来（悬空链接 / 成环）时绝不带着「落点未知」进入比较，一律失败关闭。
    let (root, dist, _snapshot) = fixture_root_with("resolve-unresolvable", "dist");
    write_minimal_dist(&dist);

    // 悬空符号链接：跟随之后落不到任何真实条目，与 dist 的关系无从判定。
    std::os::unix::fs::symlink(root.join("nowhere"), root.join("dangling")).expect("建悬空链接");
    let dangling = root.join("missing").join("..").join("dangling").join("out");
    let err = expect_reject(
        web_asset_gen::generate(&dist, &dangling),
        "悬空符号链接的输出目录必须失败关闭",
    );
    assert!(err.contains("真实路径失败"), "实际：{err}");
    assert_eq!(err.lines().count(), 1, "诊断必须单行，实际：{err:?}");
    assert!(!root.join("nowhere").exists(), "拒绝不得把链接目标建出来");

    // 符号链接成环：内核返回 ELOOP，即失败关闭（无需自设跳数上限）。
    std::os::unix::fs::symlink(root.join("loop-b"), root.join("loop-a")).expect("建 loop-a");
    std::os::unix::fs::symlink(root.join("loop-a"), root.join("loop-b")).expect("建 loop-b");
    let looping = root.join("missing").join("..").join("loop-a").join("out");
    let err = expect_reject(
        web_asset_gen::generate(&dist, &looping),
        "符号链接成环的输出目录必须失败关闭",
    );
    assert!(err.contains("真实路径失败"), "实际：{err}");
    assert_eq!(err.lines().count(), 1, "诊断必须单行，实际：{err:?}");
    assert_dist_inputs_intact(&dist);
    assert!(!root.join("missing").exists(), "拒绝必须早于任何写入");
    let _ = root;
}

#[cfg(unix)]
#[test]
fn accepts_aliased_output_dir_that_lands_outside_dist() {
    // 对照组：逐段跟随别名之后仍然分离的输出必须照常成功（修复不是一律拒绝）。
    let (root, dist, _snapshot) = fixture_root_with("p1-alias-ok", "dist");
    write_minimal_dist(&dist);
    let outside = root.join("outside");
    fs::create_dir_all(&outside).expect("创建 root/outside");
    std::os::unix::fs::symlink(&outside, root.join("alias-out")).expect("建 alias-out -> outside");

    let snapshot = root
        .join("missing")
        .join("..")
        .join("alias-out")
        .join("snap")
        .join("deep");
    let generation = web_asset_gen::generate(&dist, &snapshot)
        .unwrap_or_else(|err| panic!("别名指向 dist 之外的输出必须可用：{err}"));
    assert_eq!(generation.assets.len(), 2);
    assert!(
        outside.join("snap/deep/index.html").is_file(),
        "快照必须落在别名指向的真实目录"
    );
    assert_dist_inputs_intact(&dist);
    assert!(!dist.join("snap").exists(), "dist 之内不得出现输出目录");
    let _ = root;
}

#[test]
fn rejects_empty_paths_instead_of_degrading_to_cwd() {
    // 空路径的落点无从确定：绝不退化成当前工作目录（那是放行，不是失败关闭）。
    let err = web_asset_gen::resolve_existing_prefix(Path::new(""), "快照目录")
        .expect_err("空路径必须失败关闭");
    assert!(err.contains("空路径"), "实际：{err}");
    assert_eq!(err.lines().count(), 1, "诊断必须单行，实际：{err:?}");
    let err = web_asset_gen::validate_output_separation(Path::new("/tmp/any-dist"), Path::new(""))
        .expect_err("空快照目录必须失败关闭");
    assert!(err.contains("空路径"), "实际：{err}");

    // 相对路径锚定到 cwd 的 canonical 形态，与绝对拼写落在同一比较基准上。
    let resolved = web_asset_gen::resolve_existing_prefix(Path::new("nested/./sub"), "快照目录")
        .expect("相对路径应可解析");
    let cwd = Path::new(".").canonicalize().expect("cwd 可解析");
    assert!(resolved.is_absolute(), "解析结果必须是绝对路径");
    assert_eq!(
        resolved,
        cwd.join("nested/sub"),
        "实际：{}",
        resolved.display()
    );
}

// ---------------------------------------------------------------------------
// 1l. 第三轮整改（P1）：预检与读取**之间**的来源变化必须由跨阶段指纹挡下
//
// `generate` 分两趟：`enumerate_asset_stats` 只做 metadata 预检（让总量上限在读取前生效），
// `collect_asset_files` 才真的有界读取。窗口落在两趟之间时，读取自身**完全自洽**：
// 句柄 fstat 与读后 stat 都取自改写之后，长度也一样，所以只比 `bytes.len() != stat.len`
// 什么都看不见——必须把预检那份指纹与读取时的指纹按同一条 `is_same_source` 比一次。
// 测试直接驱动这两个入口，把变更精确落在该窗口（并发用例无法锁定这一格）。
// ---------------------------------------------------------------------------

#[test]
fn rejects_in_place_rewrite_between_precheck_and_read() {
    let fixture = Fixture::new("fp-precheck-rewrite");
    let target = fixture.write("assets/aardvark-AAA111.js", b"console.log(\"ORIGINAL\");");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.index_html(&["/assets/aardvark-AAA111.js", "/assets/index-AAA111.js"]);

    let stats = web_asset_gen::enumerate_asset_stats(&fixture.dist.join("assets"))
        .expect("合成构建的预检应通过");
    assert!(
        !stats.is_empty(),
        "预检必须拿到资产统计，否则本测试没有意义"
    );

    // 同 inode、同长度、内容不同且不恢复 mtime/ctime：唯一可见的变化维度就是指纹本身。
    rewrite_in_place_same_length(&target, b"console.log(\"ORIGINAL\");", 7)
        .expect("同长度原地重写");
    assert_eq!(
        fs::symlink_metadata(&target).expect("读元数据").len(),
        24,
        "前提：改写前后长度必须一致，否则拒绝会漂到长度判定上"
    );

    let err = match web_asset_gen::collect_asset_files(&stats) {
        Ok(files) => panic!(
            "预检与读取之间来源被原地改写，本应拒绝半份产物，实际读入 {} 项",
            files.len()
        ),
        Err(err) => err,
    };
    assert!(
        err.contains("来源指纹"),
        "应指出跨阶段来源指纹变化，实际：{err}"
    );
    assert!(
        err.contains("aardvark-AAA111.js"),
        "错误说明必须指认被改写的具体来源，实际：{err}"
    );
}

#[test]
fn rejects_same_length_replacement_between_precheck_and_read() {
    let fixture = Fixture::new("fp-precheck-replace");
    let target = fixture.write("assets/aardvark-AAA111.js", b"console.log(\"ORIGINAL\");");
    fixture.write("assets/index-AAA111.js", b"console.log(1);");
    fixture.index_html(&["/assets/aardvark-AAA111.js", "/assets/index-AAA111.js"]);

    let assets_dir = fixture.dist.join("assets");
    let stats = web_asset_gen::enumerate_asset_stats(&assets_dir).expect("合成构建的预检应通过");
    // 正向对照：来源没动时，同一份预检结果必须照常读得出来（拒绝只针对变化）。
    let untouched = web_asset_gen::collect_asset_files(&stats)
        .unwrap_or_else(|err| panic!("未变更的预检结果必须可读：{err}"));
    assert_eq!(untouched.len(), 2);

    // 另一个 inode 的**等长**替换：长度维度完全看不出来，只能靠 dev/ino（及 mtime/ctime）。
    let replacement = fixture.root.join("payload.js");
    fs::write(&replacement, b"console.log(\"PAYLOAD!\");").expect("写替换文件");
    assert_eq!(
        fs::symlink_metadata(&replacement)
            .expect("读替换文件")
            .len(),
        fs::symlink_metadata(&target).expect("读原文件").len(),
        "前提：替换文件必须等长"
    );
    fs::rename(&replacement, &target).expect("等长替换来源");

    let err = match web_asset_gen::collect_asset_files(&stats) {
        Ok(files) => panic!(
            "预检与读取之间来源被等长替换成另一 inode，本应拒绝，实际读入 {} 项",
            files.len()
        ),
        Err(err) => err,
    };
    assert!(err.contains("来源指纹"), "实际：{err}");
}

#[test]
fn build_script_strips_dist_path_tails_before_use() {
    // 集成点：`build.rs` 定位 dist 时必须先剥掉尾部的 `/` 与 `.` 段，否则
    // `RSETUP_WEB_DIST=/…/dist/` 会让生成器看不见「dist 根其实是符号链接」。
    let source = include_str!("../build.rs");
    let resolve_at = source
        .find("fn resolve_dist_dir")
        .expect("build.rs 必须自己定位 dist 目录");
    let body = &source[resolve_at..];
    let body_end = body.find("\nfn ").unwrap_or(body.len());
    assert!(
        body[..body_end].contains("strip_trailing_dot_segments"),
        "resolve_dist_dir 必须剥掉 dist 路径尾部的分隔符与 `.` 段（P3）"
    );
}

// ---------------------------------------------------------------------------
// 1j. 第二轮整改（P3）：生成物**真实 rustc 编译并执行**，证的是二进制里的字节
// ---------------------------------------------------------------------------

/// FNV-1a 64 位摘要：与探针二进制内的实现逐字一致，用来比对「编译进去的字节」。
fn fnv1a64(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// 探针二进制源码：`include!` 生成表本体，打印 HTML 与每个资产的摘要。
const SNAPSHOT_PROBE_MAIN: &str = r#"#[derive(Clone, Copy)]
pub struct EmbeddedAsset {
    pub path: &'static str,
    pub mime: &'static str,
    pub bytes: &'static [u8],
}

include!("generated_web_assets.rs");

fn digest(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn main() {
    println!("index.html {}", digest(INDEX_HTML.as_bytes()));
    for asset in ASSETS {
        println!("{} {}", asset.path, digest(asset.bytes));
    }
}
"#;

/// 找一个可用的 rustc：优先 `CARGO` 同级的 rustc，其次 PATH 上的 `rustc`。
fn rustc_program() -> PathBuf {
    if let Ok(cargo) = std::env::var("CARGO")
        && let Some(sibling) = Path::new(&cargo).parent().map(|dir| dir.join("rustc"))
        && sibling.is_file()
    {
        return sibling;
    }
    PathBuf::from("rustc")
}

/// 用真实 rustc 编译探针 crate（workspace target 下自有小 crate，零依赖）并运行，
/// 返回其二进制逐行输出的「路径 摘要」清单。
fn compile_and_run_snapshot_probe(probe_dir: &Path, generated_source: &str) -> Vec<String> {
    fs::create_dir_all(probe_dir).expect("创建探针 crate 目录");
    fs::write(probe_dir.join("generated_web_assets.rs"), generated_source).expect("写生成表副本");
    fs::write(probe_dir.join("main.rs"), SNAPSHOT_PROBE_MAIN).expect("写探针 main.rs");

    let binary = probe_dir.join("snapshot_probe");
    let rustc = rustc_program();
    let output = std::process::Command::new(&rustc)
        .current_dir(probe_dir)
        .arg("--edition")
        .arg("2021")
        .arg("--crate-type")
        .arg("bin")
        .arg("--crate-name")
        .arg("snapshot_probe")
        .arg("-o")
        .arg(&binary)
        .arg("main.rs")
        .output()
        .unwrap_or_else(|err| panic!("运行 rustc（{}）失败：{err}", rustc.display()));
    assert!(
        output.status.success(),
        "rustc 编译生成 Rust 失败：\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let run = std::process::Command::new(&binary)
        .output()
        .expect("运行编译出的探针二进制");
    assert!(run.status.success(), "探针二进制运行失败");
    String::from_utf8_lossy(&run.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// 测试侧按「生成当时校验过的字节」算出期望摘要（与探针同一算法，独立于快照文件）。
fn expected_probe_lines(index_html: &[u8], assets: &[(&str, &[u8])]) -> Vec<String> {
    let mut lines = vec![format!("index.html {}", fnv1a64(index_html))];
    for (url, bytes) in assets {
        lines.push(format!("{url} {}", fnv1a64(bytes)));
    }
    lines
}

#[test]
fn generated_rust_compiles_and_serves_snapshot_after_dist_changes() {
    // 决定性证明（不是「读快照文件」的字节层断言）：
    // 1. 正常生成；2. **之后**改内容 / 删原源 / 换符号链接 / 换 index.html / 新增资产；
    // 3. 污染完成后**才**用真实 rustc 编译生成表并运行二进制；
    // 4. 二进制里的摘要必须仍是生成当时校验过的那一份字节。
    // 若生成物引用的是可变 dist 原路径，第 3 步的 rustc 就会把污染后的字节编进去（RED）。
    let fixture = Fixture::new("p1-compiled-snapshot");
    let js: &[u8] = b"console.log(\"SNAPSHOT-ORIGINAL\");";
    let css: &[u8] = b"body{margin:0}";
    fixture.write("assets/index-AAA111.js", js);
    fixture.write("assets/index-AAA222.css", css);
    fixture.index_html(&["/assets/index-AAA111.js", "/assets/index-AAA222.css"]);
    // 先固定「生成当时磁盘上的 HTML 字节」，作为期望摘要的独立来源。
    let html_at_generation = fs::read(fixture.dist.join("index.html")).expect("读 index.html");
    let generation = fixture.generate().expect("应生成成功");
    let expected = expected_probe_lines(
        &html_at_generation,
        &[
            ("/assets/index-AAA111.js", js),
            ("/assets/index-AAA222.css", css),
        ],
    );

    // 污染原 dist：四种形态一起上。
    fs::write(
        fixture.dist.join("assets/index-AAA111.js"),
        b"console.log(\"MUTATED-AFTER-GENERATE\");",
    )
    .expect("改原资产内容");
    fs::write(fixture.root.join("evil-target.txt"), b"EVIL").expect("写符号链接目标");
    fs::remove_file(fixture.dist.join("assets/index-AAA222.css")).expect("删原资产");
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        fixture.root.join("evil-target.txt"),
        fixture.dist.join("assets/index-AAA222.css"),
    )
    .expect("换成符号链接");
    #[cfg(not(unix))]
    fs::write(fixture.dist.join("assets/index-AAA222.css"), b"EVIL").expect("替换原资产");
    fs::write(
        fixture.dist.join("index.html"),
        b"<!doctype html><script src=\"/assets/index-AAA111.js\"></script><!--REPLACED-->",
    )
    .expect("换 index.html");
    fs::write(
        fixture.dist.join("assets/index-NEW999.js"),
        b"console.log(9);",
    )
    .expect("新增资产");

    let actual =
        compile_and_run_snapshot_probe(&fixture.root.join("probe-crate"), &generation.rust_source);
    assert_eq!(
        actual, expected,
        "编译出的二进制必须是生成当时校验过的快照字节（污染 dist 后编译）"
    );
}

// ---------------------------------------------------------------------------
// 2. 真实 dist 端到端（预期源 = 磁盘上的真实构建，断言不含哈希）
// ---------------------------------------------------------------------------

fn real_dist_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("workspace 根目录")
        .join("apps/controller-web/dist")
}

/// 从真实 dist/index.html 现场提取全部 `/assets/…` 引用（哈希无关）。
fn real_dist_references() -> (String, Vec<String>) {
    let html = fs::read(real_dist_dir().join("index.html"))
        .expect("真实前端构建产物 apps/controller-web/dist/index.html 必须存在");
    let html = String::from_utf8(html).expect("dist/index.html 必须是 UTF-8");
    let mut references = Vec::new();
    for token in html.split(['"', '\'']) {
        if token.starts_with("/assets/") && !references.contains(&token.to_string()) {
            references.push(token.to_string());
        }
    }
    (html, references)
}

#[tokio::test]
async fn real_dist_build_is_served_byte_for_byte() {
    let router = rsetup_controller::web::web_router();
    let (expected_html, references) = real_dist_references();
    assert!(
        expected_html.contains("<div id=\"app\">"),
        "磁盘上的构建入口必须挂载 SPA 根节点"
    );
    assert!(
        references
            .iter()
            .any(|reference| reference.ends_with(".js")),
        "Vite 入口必须引用哈希 JS 资产"
    );

    // 1. SPA 页面路由返回 200 + no-cache，且 HTML 字节与磁盘构建入口逐字节全等。
    for uri in ["/devices", "/"] {
        let req = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        let res = router.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers().get("content-type").unwrap(),
            "text/html; charset=utf-8"
        );
        assert_eq!(res.headers().get("cache-control").unwrap(), "no-cache");
        let html_bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            html_bytes.as_ref(),
            expected_html.as_bytes(),
            "GET {uri} 必须逐字节交付磁盘上真实 dist/index.html"
        );
    }

    // 2. 全部资产引用都要命中，且字节来自磁盘上的真实构建文件；任一缺失即失败。
    assert!(
        !references.is_empty(),
        "dist/index.html 未引用任何 /assets/ 资产"
    );
    for url in &references {
        let on_disk = fs::read(real_dist_dir().join(url.trim_start_matches('/')))
            .unwrap_or_else(|err| panic!("磁盘上应存在 {url}: {err}"));
        assert!(!on_disk.is_empty(), "{url} 不允许是空文件");
        // 期望 MIME 直接取自生成器的同一张 MIME 表（对 font/svg/图片等一视同仁，
        // 不再对非 js/css 直接 unreachable，避免合法 preload 资源炸测）。
        let file_name = url.rsplit('/').next().unwrap_or(url);
        let expected_mime = web_asset_gen::mime_for(file_name)
            .unwrap_or_else(|| panic!("真实 dist 资产 {url} 的扩展名应受生成器 MIME 表覆盖"));
        let req_asset = Request::builder()
            .method(Method::GET)
            .uri(url)
            .body(Body::empty())
            .unwrap();
        let res_asset = router.clone().oneshot(req_asset).await.unwrap();
        assert_eq!(res_asset.status(), StatusCode::OK, "GET {url}");
        assert_eq!(
            res_asset.headers().get("content-type").unwrap(),
            expected_mime,
            "{url} 的 Content-Type 必须与生成器 MIME 表一致"
        );
        assert_eq!(
            res_asset.headers().get("cache-control").unwrap(),
            "public, max-age=31536000, immutable"
        );
        let asset_bytes = to_bytes(res_asset.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            asset_bytes.as_ref(),
            on_disk.as_slice(),
            "{url} 必须交付真实构建字节，而不是旧哈希产物或空占位"
        );
    }

    // 3. 缺失静态资产坚决 404，绝不回退 HTML
    let req_missing_asset = Request::builder()
        .method(Method::GET)
        .uri("/assets/does-not-exist-in-any-build.js")
        .body(Body::empty())
        .unwrap();
    let res_missing_asset = router.clone().oneshot(req_missing_asset).await.unwrap();
    assert_eq!(res_missing_asset.status(), StatusCode::NOT_FOUND);

    // 4. API 404 JSON 由 production_composition 在完整生产合并 router 上断言，
    // 此孤立 web_router 用例只验证不能把 API 当 SPA 成功回退。
    let req_api = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/not-found")
        .body(Body::empty())
        .unwrap();
    let res_api = router.clone().oneshot(req_api).await.unwrap();
    assert_ne!(res_api.status(), StatusCode::OK);

    // 5. 非 GET/HEAD 尝试访问 SPA 页面路由返回 405 或 404，不得 200
    let req_post_spa = Request::builder()
        .method(Method::POST)
        .uri("/devices")
        .body(Body::empty())
        .unwrap();
    let res_post_spa = router.oneshot(req_post_spa).await.unwrap();
    assert_ne!(res_post_spa.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// 2a. CSP 回归防护：真实 dist 入口不得有内联脚本 / 内联样式属性
//
// `web.rs::CSP_HTML` 的 `script-src 'self'` 与 `style-src 'self'` 只有在产物本身
// 完全外联时才成立。本小节把这一前提钉成测试：真实构建一旦引入内联 `<script>`、
// `<style>` 元素或 `style=` 属性，本用例立刻变红，提醒**显式评审**是否真的要
// 放宽 CSP，并同时同步 `web.rs::CSP_HTML` 与本文件的 `SECURITY_CSP_HTML`。
// ---------------------------------------------------------------------------

/// 返回 HTML 中每一处以 `<name` 起始的标签头文本（到最近的 `>` 之前），已转小写。
///
/// `to_ascii_lowercase()` 不改变字节长度、只改 ASCII 字节，因此下标与原串一一对应；
/// `needle` 是 ASCII，命中位置必落在字符边界上。
fn tag_heads(html: &str, name: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let needle = format!("<{name}");
    let mut heads = Vec::new();
    let mut cursor = 0usize;
    while let Some(offset) = lower[cursor..].find(&needle) {
        let start = cursor + offset;
        let end = lower[start..]
            .find('>')
            .map(|index| start + index)
            .unwrap_or(html.len());
        heads.push(lower[start..end].to_string());
        cursor = end;
    }
    heads
}

/// 找出 HTML 里作为**独立属性名**出现的 `style=`（属性名前一名须是空白、`<` 或引号），
/// 避免把 `<link rel="stylesheet">` 之类误判成内联样式属性；返回命中的字节偏移。
fn inline_style_attribute_offsets(html: &str) -> Vec<usize> {
    let lower = html.to_ascii_lowercase();
    let mut offsets = Vec::new();
    for (offset, _) in lower.match_indices("style=") {
        let at_attribute_boundary = offset.checked_sub(1).is_some_and(|previous| {
            matches!(
                lower.as_bytes()[previous],
                b' ' | b'\t' | b'\n' | b'\r' | b'"' | b'\'' | b'<'
            )
        });
        if at_attribute_boundary {
            offsets.push(offset);
        }
    }
    offsets
}

#[test]
fn real_dist_html_has_no_inline_script_or_style() {
    let (html, references) = real_dist_references();

    // 1. 每个 `<script` 标签都必须外联（带 `src=`）：内联脚本需要 `'unsafe-inline'`
    //    或 nonce/hash，而本仓的 CSP 两者都不给。
    let script_tags = tag_heads(&html, "script");
    assert!(
        !script_tags.is_empty(),
        "真实 dist 入口必须引用脚本，否则本用例没有意义：{html}"
    );
    for tag in &script_tags {
        assert!(
            tag.contains("src="),
            "真实 dist 入口出现内联 <script>（标签头没有 src=），与 CSP 的 \
             `script-src 'self'` 冲突：{tag}"
        );
    }

    // 2. 不得出现 `<style>` 元素：内联样式表同样需要 `style-src 'unsafe-inline'`。
    let style_tags = tag_heads(&html, "style");
    assert!(
        style_tags.is_empty(),
        "真实 dist 入口出现 <style> 元素，与 CSP 的 `style-src 'self'` 冲突：{style_tags:?}"
    );

    // 3. 不得出现 `style=` 内联属性（同样需要 `style-src 'unsafe-inline'`）。
    let inline_styles = inline_style_attribute_offsets(&html);
    assert!(
        inline_styles.is_empty(),
        "真实 dist 入口出现内联 style 属性，与 CSP 的 `style-src 'self'` 冲突：\
         命中偏移 {inline_styles:?}，入口 HTML：{html}"
    );

    // 4. 产物侧全部外联成立，策略侧就不得再放开任何 inline 例外；本文件的预期串
    //    与 `web.rs::CSP_HTML` 由 security_headers_on_html_assets_and_error_responses
    //    逐字比对，这里只钉「不许出现 inline 例外」这一条方向。
    for directive in ["script-src", "style-src"] {
        assert!(
            !SECURITY_CSP_HTML.contains("unsafe-inline"),
            "`{directive}` 的产物侧证据是零内联，{SECURITY_CSP_HTML:?} 仍含 inline 例外"
        );
        assert!(
            SECURITY_CSP_HTML.contains(&format!("{directive} 'self'")),
            "`{directive}` 必须是 'self'，实际：{SECURITY_CSP_HTML:?}"
        );
    }
    // 交付的样式确实只来自外部 CSS（外部 CSS 由 style-src 'self' 允许）。
    assert!(
        references
            .iter()
            .any(|reference| reference.ends_with(".css")),
        "dist 入口应通过外部 CSS 引入样式，实际引用：{references:?}"
    );
}

#[test]
fn inline_script_or_style_detector_is_not_vacuous() {
    // 自检：上面那条防护不得是恒真的。三种违规形态（内联脚本、`<style>` 元素、
    // `style=` 属性）必须各自被检出，而真实 Vite 形态（外联 + 外链 CSS，含
    // `rel="stylesheet"` 这种容易误判的拼写）必须零命中。
    let violating = "<!doctype html><html><head><script>document.write(1)</script>\
                     <style>body{margin:0}</style></head>\
                     <body><div style=\"color:red\"></div></body></html>";
    let scripts = tag_heads(violating, "script");
    assert_eq!(scripts.len(), 1, "内联 <script> 必须被扫到：{scripts:?}");
    assert!(
        scripts.iter().all(|tag| !tag.contains("src=")),
        "该样本里的 script 标签必须是无 src 的内联形态：{scripts:?}"
    );
    assert_eq!(tag_heads(violating, "style").len(), 1, "<style> 必须被扫到");
    assert_eq!(
        inline_style_attribute_offsets(violating),
        vec![
            violating
                .to_ascii_lowercase()
                .find("style=")
                .expect("样本里必须有 style= 属性")
        ],
        "内联 style 属性必须被检出"
    );

    let compliant = "<!doctype html><script type=\"module\" crossorigin src=\"/assets/i-1.js\">\
                     </script><link rel=\"stylesheet\" crossorigin href=\"/assets/i-2.css\">";
    let scripts = tag_heads(compliant, "script");
    assert_eq!(scripts.len(), 1);
    assert!(
        scripts.iter().all(|tag| tag.contains("src=")),
        "外联脚本不得被判成内联：{scripts:?}"
    );
    assert!(tag_heads(compliant, "style").is_empty());
    assert!(
        inline_style_attribute_offsets(compliant).is_empty(),
        "`rel=\"stylesheet\"` 不是内联 style 属性，不得误判"
    );
}

// ---------------------------------------------------------------------------
// 3. L4 纵深防御：静态资产与 SPA 响应的安全响应头
//
// 预期源是 `web.rs` 的头契约（HTML 四项、非 HTML 与错误响应两项），走真实
// `web_router()`（编译期嵌入的那一份 dist 构建），不引入第二套路由逻辑。
// ---------------------------------------------------------------------------

/// HTML 文档应有的 CSP（与 `web.rs::CSP_HTML` 逐字对齐，独立声明作为预期源）。
///
/// `style-src` 只有 `'self'`：当前构建产物的样式全部来自外部 CSS，脚本全部外联，
/// 两侧都不需要 `'unsafe-inline'`。若产物真的引入内联样式而本常量被改回去，
/// [`real_dist_html_has_no_inline_script_or_style`] 会先把产物侧的证据报红。
const SECURITY_CSP_HTML: &str = "default-src 'self'; script-src 'self'; \
                                 style-src 'self'; img-src 'self' data:; \
                                 connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; \
                                 form-action 'self'";

/// 向 web 路由发一次空 body 请求，拿回完整响应（含头）。
async fn web_request(router: &axum::Router, method: Method, uri: &str) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("web 路由不应产生服务错误")
}

/// 取响应头的字符串值（`None` = 该头不存在）。
fn header_of(res: &axum::response::Response, name: &str) -> Option<String> {
    res.headers()
        .get(name)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
}

/// 断言响应头存在且取值逐字相等（带上下文，便于定位是哪个 URI 少了头）。
fn assert_header(res: &axum::response::Response, uri: &str, name: &str, expected: &str) {
    let actual = header_of(res, name)
        .unwrap_or_else(|| panic!("{uri} 必须带响应头 {name}，实际头：{:?}", res.headers()));
    assert_eq!(actual, expected, "{uri} 的 {name} 取值不符");
}

/// 断言响应头不存在（用于「CSP 只加给 HTML」这条边界）。
fn assert_no_header(res: &axum::response::Response, uri: &str, name: &str) {
    assert_eq!(
        header_of(res, name),
        None,
        "{uri} 不是 HTML，不应带 {name}，实际：{:?}",
        res.headers()
    );
}

#[tokio::test]
async fn security_headers_on_html_assets_and_error_responses() {
    let router = rsetup_controller::web::web_router();
    let (_html, references) = real_dist_references();
    let asset_url = references
        .iter()
        .find(|reference| reference.ends_with(".js"))
        .cloned()
        .expect("真实 dist 入口必须引用 JS 资产");

    // 1. SPA 页面（GET /）：200 + HTML 四个安全头全量，既有缓存/MIME 语义不变。
    let res_page = web_request(&router, Method::GET, "/").await;
    assert_eq!(res_page.status(), StatusCode::OK);
    assert_header(&res_page, "/", "content-type", "text/html; charset=utf-8");
    assert_header(&res_page, "/", "cache-control", "no-cache");
    assert_header(&res_page, "/", "x-content-type-options", "nosniff");
    assert_header(&res_page, "/", "referrer-policy", "no-referrer");
    assert_header(&res_page, "/", "x-frame-options", "DENY");
    assert_header(&res_page, "/", "content-security-policy", SECURITY_CSP_HTML);

    // 2. 真实资产（GET）：nosniff + Referrer-Policy，但 CSP/X-Frame-Options 只给 HTML。
    let res_asset = web_request(&router, Method::GET, &asset_url).await;
    assert_eq!(res_asset.status(), StatusCode::OK, "GET {asset_url}");
    assert_header(&res_asset, &asset_url, "x-content-type-options", "nosniff");
    assert_header(&res_asset, &asset_url, "referrer-policy", "no-referrer");
    assert_no_header(&res_asset, &asset_url, "content-security-policy");
    assert_no_header(&res_asset, &asset_url, "x-frame-options");
    // 加固不得改动资产自身的缓存与长度语义。
    assert_header(
        &res_asset,
        &asset_url,
        "cache-control",
        "public, max-age=31536000, immutable",
    );

    // 3. HEAD：与 GET 同样的安全头，body 仍为空、Content-Length 仍是真实字节数。
    let res_head_page = web_request(&router, Method::HEAD, "/").await;
    assert_eq!(res_head_page.status(), StatusCode::OK);
    assert_header(
        &res_head_page,
        "HEAD /",
        "x-content-type-options",
        "nosniff",
    );
    assert_header(&res_head_page, "HEAD /", "referrer-policy", "no-referrer");
    assert_header(&res_head_page, "HEAD /", "x-frame-options", "DENY");
    assert_header(
        &res_head_page,
        "HEAD /",
        "content-security-policy",
        SECURITY_CSP_HTML,
    );
    assert!(
        to_bytes(res_head_page.into_body(), usize::MAX)
            .await
            .unwrap()
            .is_empty(),
        "HEAD / 响应不得带 body"
    );

    let res_head_asset = web_request(&router, Method::HEAD, &asset_url).await;
    assert_eq!(res_head_asset.status(), StatusCode::OK);
    assert_header(
        &res_head_asset,
        &format!("HEAD {asset_url}"),
        "x-content-type-options",
        "nosniff",
    );
    assert_header(
        &res_head_asset,
        &format!("HEAD {asset_url}"),
        "referrer-policy",
        "no-referrer",
    );
    assert_no_header(
        &res_head_asset,
        &format!("HEAD {asset_url}"),
        "content-security-policy",
    );
    let on_disk = fs::read(real_dist_dir().join(asset_url.trim_start_matches('/')))
        .expect("磁盘上应存在该资产");
    assert_eq!(
        header_of(&res_head_asset, "content-length").as_deref(),
        Some(on_disk.len().to_string().as_str()),
        "HEAD 的 Content-Length 必须仍是真实字节数"
    );

    // 4. 不存在的资产/未知页面路径：404 行为逐字节不变（坚决 404、绝不回退 HTML）。
    //    实测这些路径**不匹配 `web_router` 的任何路由**，响应由 axum `Router` 的默认
    //    404 服务产生（请求根本没进过 `serve_static`），因此当前**不可能**由 web 层给
    //    它们加头。本用例锁定两条不变量（状态码 + 不回退 HTML）；web.rs 自己的错误
    //    响应头契约由 `web::tests::error_responses_carry_base_headers_without_html_directives`
    //    锁定，未匹配路径的头归属见文件末尾「已知边界」。
    for uri in [
        "/assets/does-not-exist-in-any-build.js",
        "/api/v1/not-found",
        "/nope",
        "/admin/",
        "/devices/1/2",
    ] {
        let res = web_request(&router, Method::GET, uri).await;
        assert_eq!(
            res.status(),
            StatusCode::NOT_FOUND,
            "GET {uri} 必须坚决 404"
        );
        assert!(
            res.headers().get("content-type").is_none(),
            "GET {uri} 不得被回退成 HTML 交付"
        );
    }

    // 5. 非 GET/HEAD 命中白名单：405 由 web.rs 自己产出，必须带两个基础头，Allow 不变。
    let res_post = web_request(&router, Method::POST, "/").await;
    assert_eq!(res_post.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_header(&res_post, "POST /", "x-content-type-options", "nosniff");
    assert_header(&res_post, "POST /", "referrer-policy", "no-referrer");
    assert_header(&res_post, "POST /", "allow", "GET, HEAD");
    assert_no_header(&res_post, "POST /", "content-security-policy");
}

// 已知边界（本测试刻意锁定的现状）：
//
// 第 4 段里的路径**不匹配 `web_router` 的任何路由**（含 `/admin/`、`/devices/1/2`
// 这类「看起来在白名单附近」的拼写），响应由 axum `Router` 的默认 404 服务产生，
// 裸 `web_router` 上实测零响应头——请求根本没进过 `web.rs::serve_static`，web 层
// 无从给它加头。生产装配已在 `production.rs` 的 `compose_component_routers` 上挂
// 统一后置头层，为这类响应补 `X-Content-Type-Options`/`Referrer-Policy`；该行为由
// `production.rs` 与 `tests/production_composition.rs` 的 base_security_headers 测试
// 锁定，本测试只覆盖**裸 web_router** 的边界，不能在 web_router 上注册全局 fallback
// （会改变路由白名单与 404 语义）。web.rs 自己产出的错误响应（405 见第 5 段；
// `not_found()` 见 `web.rs` 的单元测试，当前路由表下不可达）都已按同一契约带头。
