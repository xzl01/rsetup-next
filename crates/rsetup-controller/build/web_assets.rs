//! 前端构建产物 -> Rust 静态资产表生成器。
//!
//! 本文件被两侧以 `#[path]` 引入**同一份源码**：
//! - `crates/rsetup-controller/build.rs`：编译期生成 `$OUT_DIR/web_assets.rs` 与其
//!   同级的字节快照目录 `$OUT_DIR/web_assets_snapshot/`；
//! - `crates/rsetup-controller/tests/web_assets.rs`：用合成构建 fixture 驱动。
//!
//! 职责：读取 `apps/controller-web/dist` 里 Vite 真实构建出的 `index.html` 与
//! `assets/` 下实际存在的文件，产出一张与磁盘现状一一对应的 Rust 资产表源码。
//! 资产名（含内容哈希）只来自磁盘现状，本仓源码里不出现任何哈希常量。
//!
//! 关键设计：TOCTOU 关闭（P1）。生成器**先**把校验过的真实字节读进内存并做完整性/
//! 一致性核验，**再**把这些字节快照写入调用方指定的私有目录（构建期即 `OUT_DIR` 子目录），
//! 生成的 Rust 只对**快照文件** `include_str!` / `include_bytes!`，绝不 `include` 可变的
//! `dist` 原路径。这样 rustc 稍后编译 `src/web.rs` 读到的一定是校验过的那一份字节，前端
//! 之后改 HTML、换哈希、删文件、把原资产换成符号链接都无法造成「HTML 新 / 资产旧」的坏包。
//! 快照读取期间做来源指纹复核（unix：dev/inode/大小/**mtime**/**ctime**；其余平台：大小 +
//! mtime）：指纹在「metadata 预检 stat → 打开句柄 fstat → 读完再 stat → 读取时指纹与预检
//! 统计指纹比对 → 生成末尾终核」五处取值并逐次比对，任一处不一致（含**同 inode 同长度的原地
//! 重写**：内容变了 mtime/ctime 必然变化）
//! 即 `Err` 失败关闭，绝不产出新旧混搭的半张生成表（生成表 `web_assets.rs` 只在全部复核
//! 通过后写出）。生成末尾的终核还会重新核对 index.html 指纹与**整个资产集合**（新增、删除、
//! 替换、mtime 变化都要在这里再挡一次）。原 `dist` 输入的 `rerun-if-changed` 监听保持不变。
//!
//! 诚实口径（不夸大防护范围）：本轮只做「失败关闭 + 让调用方重跑构建」，**不**声称抵御
//! 恶意恢复 mtime/ctime 等 metadata 的原子快照攻击，也**不**声称对无限并发写入者给出原子
//! 一致性；命中任一不确定状态一律拒绝产生产物，由构建系统重跑，绝不读入混搭字节继续构建。
//!
//! 输入/输出分离（P3）：`generate` 在任何清理或写入之前，先按解析过符号链接的真实路径判定
//! `snapshot_dir` 与 `dist_dir` 是否同一路径、别名或互为祖先后代，命中即拒（见
//! `validate_output_separation`）；`build.rs` 也**先**调用它**再**清空旧快照目录，异常调用
//! （例如把 `RSETUP_WEB_DIST` 指到 `OUT_DIR/web_assets_snapshot`）不会先删掉输入再报错。
//! 「同一目录的不同拼写」按**消解后**的真实路径判定，而消解必须**逐段对照文件系统**进行
//! （`resolve_existing_prefix`）：canonicalize 只能落到最深的现存祖先，其尾段里 `missing/..`
//! 这类拼写要折叠、`..` 折回真实目录之后**重新出现的段还要再查一次文件系统**。任何一段偷懒
//! 按字面接回，别名重叠的两个判定就会同时漏过，`build.rs` 的清旧快照与 `generate` 的写快照
//! 直接作用到输入上（P1：`missing/../alias/out` 经 `alias -> dist` 真的写进 `dist/out`）。
//!
//! 符号链接策略（P3，明确边界，不过度声明）：从**选定的 dist 根开始**严格拒绝符号链接
//! ——dist 根本身、`dist/assets`、`index.html` 以及 `assets` 内任意条目只要是指向它物的
//! 符号链接即拒；`include` 的是快照而非原路径，即便校验后原资产被换成符号链接也不会被
//! 跟随。**不**追溯 `dist` 之上的工作区祖先目录（`/tmp`、构建缓存等在 macOS 上合法地
//! 可能是符号链接）——把 dist 根当作受信任的构建根，只严格拒绝其**内部**的链接。
//! 这些门禁判定之前一律先归一路径**末端**（`normalize_dir_path_for_symlink_check`）：
//! POSIX 对以 `/` 或 `/.` 结尾的路径**强制跟随**末端符号链接，`symlink_metadata("link/")`
//! 报的是链接目标的 metadata，一个尾缀字符就能让「不得是符号链接」整体失效（实测
//! `link` 报 symlink=true，`link/`、`link//`、`link/.` 全报 symlink=false）；因此先纯词法
//! 剥掉尾部的 `/` 与 `.` 段（绝不 `canonicalize`——那恰恰会把要拒绝的链接解析掉），
//! 末端仍是 `..` 的输入（条目身份取决于逐段解析）一律拒绝。
//!
//! 失败关闭口径（任一条不满足即 `Err`，绝不静默服务旧产物或空占位）：
//! - dist 目录/入口 HTML 缺失、为空、非 UTF-8、不是普通文件或是符号链接；
//! - dist / 快照目录路径含 ASCII 控制字符（换行等），会破坏生成源码注释与 cargo 行协议；
//! - dist / 快照目录路径以 `..` 结尾（末端条目身份取决于逐段解析，符号链接门禁无从判定）；
//! - dist / 快照目录路径**解析不出真实落点**（空路径、某一段权限不足、悬空符号链接、
//!   符号链接成环）——落点未知就无法判定输入/输出是否重叠；
//! - `index.html` 引用的 `/assets/*` 在磁盘上不存在，或未引用任何 JS 入口；
//! - `dist/assets` 下存在空文件、隐藏文件、`*.map`、符号链接、越界路径名，或
//!   扩展名不在受支持 MIME 表内的文件；
//! - `index.html` 出现 `/assets/` 之外的本地引用（这类产物运行期必然 404，必须
//!   在构建期暴露而不是吞掉）；
//! - HTML 超过 `MAX_INDEX_HTML_BYTES`、单个资产超过 `MAX_ASSET_FILE_BYTES`、
//!   资产总量超过 `MAX_TOTAL_ASSET_BYTES`（预检 metadata + 有界读取 + 溢出安全累加）。

// 本模块被 build.rs（构建期）与 tests/web_assets.rs（测试期）分别 `#[path]` 引入，
// 两侧各只用 API 的一个子集，逐侧的 dead_code 告警都是误报。
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File, Metadata};
use std::io::{ErrorKind, Read};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

/// 生成文件在 `OUT_DIR` 内的文件名，`src/web.rs` 按此名 `include!`。
pub const GENERATED_FILE_NAME: &str = "web_assets.rs";

/// 字节快照子目录名：生成的 Rust 只 `include` 本目录内的稳定快照。
pub const SNAPSHOT_SUBDIR: &str = "web_assets_snapshot";

/// 前端构建产物目录相对 crate 清单目录的默认位置。
pub const DEFAULT_DIST_RELATIVE_PATH: &str = "../../apps/controller-web/dist";

/// 嵌入资产的对外 URL 前缀，与 `web::ASSET_DIR` 保持一致。
pub const ASSET_URL_PREFIX: &str = "/assets/";

/// 可用环境变量覆盖 dist 目录（仅供 CI/打包在非常规布局下指路；缺 dist 仍失败关闭）。
pub const DIST_ENV_OVERRIDE: &str = "RSETUP_WEB_DIST";

/// 构建失败时提示的下一步动作，集中一处避免文案漂移。
const BUILD_HINT: &str = "请先执行 npm --prefix apps/controller-web ci 与 npm --prefix apps/controller-web run build 构建前端产物，后重新构建 Rust 侧";

/// 递归遍历 `dist/assets` 的深度上限，防目录环与病态构建。
const MAX_ASSET_DEPTH: usize = 8;

/// `dist/assets` 下允许的文件总数上限。
const MAX_ASSET_COUNT: usize = 4_096;

/// `index.html` 字节上限。Vite 入口 HTML 实际只有几 KB，1 MiB 是极宽松的天花板，
/// 只为兜住病态/被篡改的构建，非按当前产物体量调优（当前真实产物 < 1 KiB）。
pub const MAX_INDEX_HTML_BYTES: u64 = 1 << 20; // 1 MiB

/// 单个资产字节上限。当前真实最大 chunk < 220 KiB；32 MiB 覆盖超大 wasm/字体，
/// 仍足以拒绝 1 GiB 级稀疏文件（P2：metadata 预检直接挡下，不会真读进内存）。
pub const MAX_ASSET_FILE_BYTES: u64 = 32 << 20; // 32 MiB

/// 嵌入资产总量上限。当前真实总量约 488 KiB；128 MiB 为极宽松天花板，
/// 用 `checked_add` 累加，防累加溢出绕过限制。
pub const MAX_TOTAL_ASSET_BYTES: u64 = 128 << 20; // 128 MiB

/// `index.html` 中需要按本地 URL 校验的属性名（小写）。除 `srcset` 家族外均按「单个
/// 完整 URL」校验（含空白即拒），`srcset` 家族按候选表专门切分。
const URL_ATTRIBUTES: &[&str] = &[
    "action",
    "data",
    "formaction",
    "href",
    "imagesrcset",
    "poster",
    "src",
    "srcset",
    "xlink:href",
];

/// 取值为「逗号分隔的 URL 候选表」的属性（`srcset` 家族）；其余 URL 属性按单 URL 校验。
/// 每一项**必须同时**出现在 `URL_ATTRIBUTES` 里，否则该属性根本不会被分类（漏一项就是漏嵌）。
const SRCSET_ATTRIBUTES: &[&str] = &["srcset", "imagesrcset"];

/// 元素内容为 rawtext/RCDATA：起始标签之后的内容不得按标签/属性解析（P2）。
/// 这些元素的正文里出现 `<script src=...>` 字样或 `x=1` 只是文本，不是标记。
const RAWTEXT_ELEMENTS: &[&str] = &["script", "style", "textarea", "title"];

/// 一份待嵌入的资产。
#[derive(Debug, Clone)]
pub struct AssetEntry {
    /// 对外暴露的绝对请求路径，同时是 `index.html` 中的引用形态。
    pub url_path: String,
    /// 与扩展名一致的响应 MIME。
    pub mime: &'static str,
    /// 磁盘上真实构建文件的绝对路径，仅用于 `rerun-if-changed` 监听与诊断，
    /// **不**再进入生成的 `include` 语句。
    pub file: PathBuf,
    /// 校验后写入快照目录的稳定字节副本，生成的 `include_bytes!` 只指向它。
    pub snapshot: PathBuf,
    /// 文件字节数，用于诊断输出。
    pub size: u64,
}

/// 一次生成的完整结果。
#[derive(Debug, Clone)]
pub struct Generation {
    /// 参与生成的 dist 根目录。
    pub dist_dir: PathBuf,
    /// 参与生成的入口 HTML 绝对路径（`rerun-if-changed` 目标）。
    pub index_html: PathBuf,
    /// 入口 HTML 的快照副本路径（生成的 `include_str!` 只指向它）。
    pub index_html_snapshot: PathBuf,
    /// 字节快照目录。
    pub snapshot_dir: PathBuf,
    /// 资产表，按 `url_path` 升序，保证生成源码可复现。
    pub assets: Vec<AssetEntry>,
    /// 供 `src/web.rs` `include!` 的 Rust 源码（只引用快照目录内的文件）。
    pub rust_source: String,
    /// 需要向 cargo 声明 `rerun-if-changed` 的路径（`dist` 原输入，文件与目录）。
    pub rerun_paths: Vec<PathBuf>,
}

impl Generation {
    /// 嵌入的总字节数。
    pub fn total_bytes(&self) -> u64 {
        self.assets.iter().map(|asset| asset.size).sum()
    }
}

/// 扩展名 -> 响应 MIME。只列前端构建可能真实产出的类型；
/// 未收录的扩展名一律拒绝，避免以 `application/octet-stream` 蒙混上线。
const MIME_BY_EXTENSION: &[(&str, &str)] = &[
    ("css", "text/css"),
    ("gif", "image/gif"),
    ("jpeg", "image/jpeg"),
    ("jpg", "image/jpeg"),
    ("js", "text/javascript"),
    ("json", "application/json"),
    ("mjs", "text/javascript"),
    ("otf", "font/otf"),
    ("png", "image/png"),
    ("svg", "image/svg+xml"),
    ("ttf", "font/ttf"),
    ("txt", "text/plain; charset=utf-8"),
    ("wasm", "application/wasm"),
    ("webp", "image/webp"),
    ("woff", "font/woff"),
    ("woff2", "font/woff2"),
];

/// 按扩展名取 MIME；未知扩展名返回 `None`。
pub fn mime_for(file_name: &str) -> Option<&'static str> {
    let extension = Path::new(file_name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)?;
    MIME_BY_EXTENSION
        .iter()
        .find(|(known, _)| *known == extension)
        .map(|(_, mime)| *mime)
}

// ---------------------------------------------------------------------------
// 路径安全（P2：控制字符不得进入生成的 // 注释或 cargo 行协议）
// ---------------------------------------------------------------------------

/// 路径字符串是否含 ASCII 控制字符（`< 0x20` 或 `0x7F`）。换行/回车等会截断生成的
/// 行注释，或在 `cargo:rerun-if-changed=` 之后伪造新的行协议指令，必须坚决拒绝。
///
/// **逐字节**判定，绝不以「整个串是不是 ASCII」为前提：`路径\ncargo:warning=INJECTED`
/// 这类含非 ASCII 字符的路径同样带换行，早先的 `text.is_ascii() &&` 前置条件会让它整体
/// 绕过检查，进而被 `build.rs` 原样打印进 cargo 行协议（真实 Cargo 会把它解析成注入的
/// `warning` 指令）。
fn has_control_char(text: &str) -> bool {
    text.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
}

/// 把任意路径渲染成**单行**、可安全写进诊断信息的文本。
///
/// 非法文件名与非 UTF-8 的 `OsStr` 都可能带换行：直接 `path.display()` 会把控制字符原样
/// 吐进终端/构建日志（并可能伪造 cargo 行协议）。这里对 lossy 结果逐字符转义控制字符、
/// 反斜杠与引号，正常中文/空格/引号等可读字符保持原样，只保证「一行且无控制字符」。
fn safe_path_text(path: impl AsRef<Path>) -> String {
    let lossy = path.as_ref().display().to_string();
    let mut out = String::with_capacity(lossy.len() + 2);
    out.push('\'');
    for ch in lossy.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{{{:02x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// 拒绝任何带 ASCII 控制字符的路径（含非 UTF-8 的 OsStr 亦拒绝，无法安全嵌入）。
/// 两条诊断都走 `safe_path_text`：绝不把被拒绝路径里的控制字符原样带回错误说明。
///
/// 除生成器内部（dist/快照目录、index.html、逐个资产）之外，`build.rs` 也直接调用它
/// 校验 `CARGO_MANIFEST_DIR` 派生的构建脚本源码路径——那批路径不经过 `generate`，
/// 却与 `rerun_paths` 一起被逐行写进 cargo 行协议。
pub(crate) fn reject_control_chars(path: &Path, label: &str) -> Result<(), String> {
    let text = path.to_str().ok_or_else(|| {
        format!(
            "{label} 路径不是合法 UTF-8，无法安全嵌入生成物：{}",
            safe_path_text(path)
        )
    })?;
    if has_control_char(text) {
        return Err(format!(
            "{label} 路径含 ASCII 控制字符（换行/回车等），会破坏生成源码注释与 cargo 行协议，坚决拒绝：{}",
            safe_path_text(path)
        ));
    }
    Ok(())
}

/// 逐条校验一组**将要写进 cargo `rerun-if-changed` 行协议**的路径（P2：行协议出口闸门）。
///
/// `build.rs` 的监听路径有两个来源：① 生成器产出的 `Generation::rerun_paths`，已在
/// `generate` 内按同一条规则拒绝过控制字符；② `build.rs` 由 `CARGO_MANIFEST_DIR`
/// 环境变量 join 出的构建脚本自身源文件——**这批不经过生成器**。两个来源共用这一个
/// 出口闸门：任一路径含 ASCII 控制字符（换行/回车等）或不是合法 UTF-8 即返回 `Err`
/// （首个不合格路径为准），调用方必须失败关闭。
///
/// 为什么必须在 `println!` 之前拦住：`cargo:rerun-if-changed=` 是**行协议**，路径里
/// 只要有一个换行，其后的文本就成为一条独立 cargo 指令（如 `/p\ncargo:warning=x`
/// 会被真实 Cargo 解析成注入的 warning），单靠事后的日志脱敏无法撤销。
pub fn validate_rerun_paths(paths: &[PathBuf], label: &str) -> Result<(), String> {
    for path in paths {
        reject_control_chars(path, label)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 路径末端形态（P3：尾缀不得让 symlink_metadata「看不见」末端的符号链接）
// ---------------------------------------------------------------------------

/// 纯词法剥除路径**尾部**的分隔符与 `.` 段：不触碰文件系统、不解析符号链接、不动中间段。
///
/// 为什么必须剥：POSIX 路径 lookup 规定，以 `/` 或 `/.` 结尾的路径**强制跟随**末端符号
/// 链接，于是 `symlink_metadata("symlink_dir/")` 与 `symlink_metadata("symlink_dir/.")`
/// 返回的是**链接目标目录**的 metadata，`file_type().is_symlink()` 恒为 `false`——
/// 「dist 根 / `dist/assets` 不得是符号链接」这类门禁只要被传入 `symlink_dir/`
/// 就整体失效（本机实测：`link` 报 symlink=true，`link/`、`link//`、`link/.` 全报
/// symlink=false + dir=true）。剥掉尾缀后，末端条目才回到 `symlink_metadata` 能忠实
/// 判定的形态。
///
/// 只做词法剥除，绝不 `canonicalize`：后者会解析掉末端符号链接，恰恰把要拒绝的东西藏进
/// 真实路径里。`Path::components` 本身就丢弃尾随分隔符，因此重拼即等价于剥除。
///
/// 边界：全是 `.` 段（`.`、`./`）回落成 `.`；根 `/` 原样保留；**空路径原样返回空**
/// （退化成 `.` 会把「不存在」偷换成 cwd，那是放行而不是失败关闭）。
pub(crate) fn strip_trailing_dot_segments(path: &Path) -> PathBuf {
    let components: Vec<Component<'_>> = path.components().collect();
    let mut end = components.len();
    while end > 0 && matches!(components[end - 1], Component::CurDir) {
        end -= 1;
    }
    let mut stripped = PathBuf::new();
    for component in &components[..end] {
        stripped.push(component.as_os_str());
    }
    if stripped.as_os_str().is_empty() && !components.is_empty() {
        // 路径本身非空但全是 `.` 段（`.`、`./`、`./.`）：保留当前目录这一层。
        stripped.push(Component::CurDir.as_os_str());
    }
    stripped
}

/// 把路径整理成「末端条目可由 `symlink_metadata` 忠实判定」的形态。
///
/// 先按 [`strip_trailing_dot_segments`] 剥掉尾随分隔符与 `.` 段；剥完末端若仍是 `..`，
/// 说明该路径指向「某个目录的父目录」：它的末端条目身份取决于路径中**每一段**的解析结果
/// （POSIX 会先跟随路径里的符号链接，`symlink_dir/..` 得到的是链接目标的父目录），一次
/// `symlink_metadata` 无法判定，只能失败关闭。正常构建布局（dist 根、`index.html`、
/// 快照目录）不会以 `..` 结尾，因此这条拒绝不改变正常输入的可用性。
pub(crate) fn normalize_dir_path_for_symlink_check(
    path: &Path,
    label: &str,
) -> Result<PathBuf, String> {
    let stripped = strip_trailing_dot_segments(path);
    if stripped.components().next_back() == Some(Component::ParentDir) {
        return Err(format!(
            "{label} 路径以 `..` 结尾，其末端条目身份无法由 symlink_metadata 判定\
             （POSIX 会先跟随路径中的符号链接），拒绝：{}",
            safe_path_text(path)
        ));
    }
    Ok(stripped)
}

// ---------------------------------------------------------------------------
// HTML 引用提取（P2：最小 HTML tokenizer）
// ---------------------------------------------------------------------------

/// 判断字符串是否像一个 `scheme:` 前缀（`data:`、`https:` 等）。
fn has_url_scheme(trimmed: &str) -> bool {
    if let Some(colon) = trimmed.find(':') {
        if colon == 0 {
            return false;
        }
        let scheme = &trimmed[..colon];
        return scheme.as_bytes()[0].is_ascii_alphabetic()
            && scheme
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'-'));
    }
    false
}

/// 把「单个」URL 属性取值分类为本地资产引用、外部引用或非法本地引用。
///
/// `Ok(Some(url_path))` 表示必须嵌入的本地资产；`Ok(None)` 表示由浏览器自行取得
/// （协议相对、`scheme:` 外链、`data:`）；`Err` 表示照此上线必然 404 或路径越界。
/// 调用方负责先按 `srcset` 家族切出候选——本函数处理的是**单个完整 URL**，
/// 只要含 ASCII 空白即视为「一个属性里塞了多个/带空白的值」，坚决拒绝。
fn classify_single_reference(url_attribute: &str, value: &str) -> Result<Option<String>, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    // 单个 URL 属性里出现内部空白：浏览器把整个值当一个 URL（多半 404），嵌入器不接受。
    if trimmed.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(format!(
            "index.html 中单个 URL 属性 {url_attribute}=\"{value}\" 含空白/多个取值；\
             除 srcset 家族外的 URL 属性必须是单个无空白的 URL"
        ));
    }
    if trimmed.starts_with("//") {
        return Ok(None);
    }
    if has_url_scheme(trimmed) {
        return Ok(None);
    }
    let Some(name) = trimmed.strip_prefix(ASSET_URL_PREFIX) else {
        if trimmed.starts_with('/') {
            return Err(format!(
                "index.html 存在 /assets/ 之外的本地引用（{url_attribute}=\"{trimmed}\"）；\
                 嵌入器只应答 {ASSET_URL_PREFIX} 前缀，请调整前端构建配置使产物全部落在 assets/ 下"
            ));
        }
        return Err(format!(
            "index.html 的本地引用必须写成 {ASSET_URL_PREFIX} 绝对路径（{url_attribute}=\"{trimmed}\"），\
             相对路径在 SPA 子路由下会解析到错误位置"
        ));
    };
    validate_asset_name(name)?;
    Ok(Some(format!("{ASSET_URL_PREFIX}{name}")))
}

/// ASCII 空白（HTML 空白字符集）判定，tokenizer 与 srcset 候选切分共用。
fn is_ascii_ws_byte(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

/// 受支持的 srcset 描述符：`<数字>x`（如 `1x`/`2x`/`1.5x`）或 `<正整数>w`（如 `320w`）。
fn is_supported_descriptor(text: &str) -> bool {
    let Some((unit, number)) = text.as_bytes().split_last() else {
        return false;
    };
    if number.is_empty() {
        return false;
    }
    match unit {
        // 分辨率描述符：正浮点 + `x`
        b'x' => {
            let digits = number.iter().filter(|byte| byte.is_ascii_digit()).count();
            let dots = number.iter().filter(|byte| **byte == b'.').count();
            number
                .iter()
                .all(|byte| byte.is_ascii_digit() || *byte == b'.')
                && digits > 0
                && dots <= 1
                && number.iter().any(|byte| (b'1'..=b'9').contains(byte))
        }
        // 宽度描述符：正整数 + `w`
        b'w' => {
            number.iter().all(|byte| byte.is_ascii_digit())
                && number.iter().any(|byte| *byte != b'0')
        }
        _ => false,
    }
}

/// 按 HTML 规范「解析 srcset 属性」的**受支持子集**把候选表切成 URL：
///
/// 1. 候选的 URL = 从当前位置到**下一个 ASCII 空白**为止的整个 token；token 内部的逗号
///    **属于 URL 本身**（浏览器同解），绝不按逗号凭空拆成两条；
/// 2. 只有 token **末尾**的逗号是候选分隔符，剥掉后该候选无描述符；
/// 3. token 后到下一个逗号之间是描述符，只支持 `1x`/`2x`/`1.5x`/`320w` 这类常见形态，
///    其它描述符（多描述符、`(min-resolution: 2dppx)`、未知单位等）明确 `Err` 失败关闭，
///    绝不「猜一个 URL」后放行导致运行期 404。
///
/// 因此 `srcset="/assets/a.png,/assets/b.png"`（无空白）是一个含内部逗号的 URL：本函数
/// 不拆分，直接以「URL 含内部逗号」拒绝，而不是误报两条都存在的合法候选。
fn classify_srcset(url_attribute: &str, value: &str) -> Result<Vec<String>, String> {
    let raw = value.as_bytes();
    let n = raw.len();
    let mut urls = Vec::new();
    let mut i = 0usize;
    loop {
        while i < n && is_ascii_ws_byte(raw[i]) {
            i += 1;
        }
        if i >= n {
            break;
        }
        // 1. URL token：到下一个 ASCII 空白为止（逗号留在 URL 内）。
        let token_start = i;
        while i < n && !is_ascii_ws_byte(raw[i]) {
            i += 1;
        }
        let token = &value[token_start..i];
        // 2. 末尾逗号是候选分隔符（规范：剥掉 URL 末尾全部逗号）。
        let url = token.trim_end_matches(',');
        // 3. 描述符：token 未以逗号结尾且未到结尾时，取到下一个逗号为止。
        if !token.ends_with(',') && i < n {
            let descriptor_start = i;
            while i < n && raw[i] != b',' {
                i += 1;
            }
            let descriptor = value[descriptor_start..i].trim();
            if !descriptor.is_empty()
                && (descriptor.split_ascii_whitespace().count() != 1
                    || !is_supported_descriptor(descriptor))
            {
                return Err(format!(
                    "index.html 中 {url_attribute} 的候选描述符「{descriptor}」不在受支持子集\
                     （`1x`/`2x`/`1.5x`/`320w`）；嵌入器不猜描述符对应的候选，坚决拒绝"
                ));
            }
            if i < n {
                i += 1; // 跳过候选分隔逗号
            }
        }
        if url.is_empty() {
            continue;
        }
        if url.contains(',') {
            return Err(format!(
                "index.html 中 {url_attribute} 的候选 URL「{url}」含内部逗号；按 HTML 规范该 token \
                 就是**一个** URL（浏览器同解），逗号属于 URL 本身，嵌入器不按逗号凭空拆分，坚决拒绝"
            ));
        }
        if let Some(local) = classify_single_reference(url_attribute, url)? {
            urls.push(local);
        }
    }
    Ok(urls)
}

/// 路径名（或单段目录名）是否只含 `[A-Za-z0-9._-]` 且不以 `.` 开头。
fn is_safe_name_component(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// 校验 `/assets/` 之后的相对资产名；`Err` 说明统一以「非法资产名」开头。
fn validate_asset_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 512 {
        return Err(format!("非法资产名（长度为空或超出上限）：{name}"));
    }
    for component in name.split('/') {
        if component.is_empty() {
            return Err(format!("非法资产名（含空路径段或多余斜杠）：{name}"));
        }
        if component == "." || component == ".." {
            return Err(format!("非法资产名（含 . 或 .. 穿越段）：{name}"));
        }
        if !is_safe_name_component(component) {
            return Err(format!(
                "非法资产名（含 `[A-Za-z0-9._-]` 之外的字符或隐藏文件段）：{name}"
            ));
        }
    }
    Ok(())
}

/// 从 `from` 起查找单字节，返回其下标。
fn find_byte(hay: &[u8], from: usize, needle: u8) -> Option<usize> {
    hay[from..]
        .iter()
        .position(|&b| b == needle)
        .map(|p| from + p)
}

/// 从 `from` 起查找子序列，返回其起始下标。
fn find_sequence(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from.saturating_add(needle.len()) > hay.len() {
        // 允许 needle 恰好延伸到末尾：重新判边界。
        if needle.is_empty() || from > hay.len() {
            return None;
        }
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

/// 从 `from` 起查找匹配 `</tag` 的结束标签，返回该结束标签 `>` 之后的下标。
fn find_end_tag(hay: &[u8], from: usize, tag: &str) -> Option<usize> {
    let lower_tag = tag.to_ascii_lowercase();
    let mut i = from;
    loop {
        let lt = find_byte(hay, i, b'<')?;
        // 需要 `<` `/` `tag` 且其后为空白或 `>` 或 `/`。
        if lt + 1 >= hay.len() || hay[lt + 1] != b'/' {
            i = lt + 1;
            continue;
        }
        let start = lt + 2;
        let end = start + lower_tag.len();
        if end > hay.len() || !hay[start..end].eq_ignore_ascii_case(lower_tag.as_bytes()) {
            i = lt + 1;
            continue;
        }
        if end == hay.len() || matches!(hay[end], b'>' | b'/' | b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
        {
            return find_byte(hay, end, b'>').map(|gt| gt + 1);
        }
        i = lt + 1;
    }
}

/// 扫描 `index.html`，收集全部必须嵌入的本地资产引用（按 URL 去重升序）。
///
/// 最小 HTML tokenizer：只在**真实起始标签**里读属性，跳过 `<!--注释-->`、`<!doctype>`、
/// `</结束标签>` 与 rawtext 正文（`<script>` / `<style>` / `<textarea>` / `<title>`）。
/// 普通 URL 属性按单个完整值校验（含空白即拒），`srcset` 家族按候选表切分；
/// 合法带引号/不带引号属性、布尔属性均按明确 HTML 子集支持，诊断信息精确到属性。
pub fn collect_local_references(html: &str) -> Result<BTreeSet<String>, String> {
    const fn is_tag_name_byte(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'-'
    }
    let raw = html.as_bytes();
    let n = raw.len();
    let mut references = BTreeSet::new();
    let mut i = 0usize;

    while i < n {
        // 跳到下一个 '<'
        while i < n && raw[i] != b'<' {
            i += 1;
        }
        if i >= n {
            break;
        }
        if i + 1 >= n {
            break; // 悬空的 '<'
        }
        let next = raw[i + 1];

        // 注释 <!-- ... -->：整段跳过，注释里的 <script src=...> 不是入口。
        if raw[i..].starts_with(b"<!--") {
            match find_sequence(raw, i + 4, b"-->") {
                Some(end) => i = end + 3,
                None => i = n, // 未闭合注释吞到结尾
            }
            continue;
        }
        // <!doctype> / <?xml?> / 其它 <! ... >：跳到 '>'。
        if next == b'!' || next == b'?' {
            i = find_byte(raw, i + 2, b'>').map(|gt| gt + 1).unwrap_or(n);
            continue;
        }
        // 结束标签 </tag>：跳到 '>'。
        if next == b'/' {
            i = find_byte(raw, i + 2, b'>').map(|gt| gt + 1).unwrap_or(n);
            continue;
        }
        // 起始标签必须是 '<' + ASCII 字母，否则是文本里的裸 '<'。
        if !next.is_ascii_alphabetic() {
            i += 1;
            continue;
        }

        let mut j = i + 1;
        while j < n && is_tag_name_byte(raw[j]) {
            j += 1;
        }
        let tag = html[i + 1..j].to_ascii_lowercase();
        let mut self_closing = false;

        // 解析属性，直到 '>' 或 '/>'（引号内的 '>' 不结束标签）。
        loop {
            while j < n && is_ascii_ws_byte(raw[j]) {
                j += 1;
            }
            if j >= n {
                return Err(format!(
                    "index.html 起始标签 <{tag} 未闭合，无法确定其属性边界"
                ));
            }
            if raw[j] == b'>' {
                j += 1;
                break;
            }
            if raw[j] == b'/' {
                if j + 1 < n && raw[j + 1] == b'>' {
                    self_closing = true;
                    j += 2;
                    break;
                }
                j += 1; // 杂散斜杠，忽略
                continue;
            }
            // 属性名
            let name_start = j;
            while j < n
                && !is_ascii_ws_byte(raw[j])
                && raw[j] != b'='
                && raw[j] != b'>'
                && raw[j] != b'/'
            {
                j += 1;
            }
            if j == name_start {
                j += 1; // 非法字符起头的残片，跳过
                continue;
            }
            let attr = html[name_start..j].to_ascii_lowercase();
            // 可选 '='（布尔属性无值，直接跳过）
            let mut k = j;
            while k < n && is_ascii_ws_byte(raw[k]) {
                k += 1;
            }
            if k >= n || raw[k] != b'=' {
                continue; // 布尔属性
            }
            j = k + 1;
            while j < n && is_ascii_ws_byte(raw[j]) {
                j += 1;
            }
            // 属性值：带引号（单/双）或不带引号
            let value = if j < n && (raw[j] == b'"' || raw[j] == b'\'') {
                let quote = raw[j];
                let value_start = j + 1;
                match find_byte(raw, value_start, quote) {
                    Some(value_end) => {
                        let v = html[value_start..value_end].to_string();
                        j = value_end + 1;
                        v
                    }
                    None => {
                        return Err(format!(
                            "index.html 属性 {tag}::{attr} 的取值缺少闭合引号，无法确定引用目标"
                        ));
                    }
                }
            } else {
                // 不带引号：HTML 子集里到空白或 '>' 为止
                let value_start = j;
                while j < n && !is_ascii_ws_byte(raw[j]) && raw[j] != b'>' {
                    j += 1;
                }
                html[value_start..j].to_string()
            };

            if URL_ATTRIBUTES.contains(&attr.as_str()) {
                if SRCSET_ATTRIBUTES.contains(&attr.as_str()) {
                    for url in classify_srcset(&attr, &value)? {
                        references.insert(url);
                    }
                } else if let Some(url) = classify_single_reference(&attr, &value)? {
                    references.insert(url);
                }
            }
        }

        // rawtext 元素（非自闭合）：正文不解析，跳到匹配结束标签。
        if !self_closing && RAWTEXT_ELEMENTS.contains(&tag.as_str()) {
            i = find_end_tag(raw, j, &tag).unwrap_or(n);
        } else {
            i = j;
        }
    }
    Ok(references)
}

// ---------------------------------------------------------------------------
// 校验读取 + 来源指纹（P1：快照期一致性 / 失败关闭）
// ---------------------------------------------------------------------------

/// 来源文件指纹：unix 为 (dev, inode, 大小, mtime, ctime)；其余平台退化为 (大小, mtime)。
///
/// 只比 dev/ino/len 是**不够**的：同一 inode、同一长度、原地重写新内容（mtime 不恢复）
/// 会得到「身份未变、长度未变」的撕裂产物并被判定为成功。mtime（`SystemTime`，Linux 下
/// 纳秒精度）与 unix 的 ctime（inode 元数据变更时间）任一变化即视为来源已变，失败关闭。
/// 诚实口径：不抵御恶意把 mtime/ctime 改回旧值的攻击——那属于「来源正在被主动伪造」，
/// 本工具的正确响应是拒绝产出并让构建重跑，而不是读入混搭字节。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    ctime: i64,
    #[cfg(unix)]
    ctime_nsec: i64,
}

impl Fingerprint {
    fn of(metadata: &Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            dev: std::os::unix::fs::MetadataExt::dev(metadata),
            #[cfg(unix)]
            ino: std::os::unix::fs::MetadataExt::ino(metadata),
            #[cfg(unix)]
            ctime: std::os::unix::fs::MetadataExt::ctime(metadata),
            #[cfg(unix)]
            ctime_nsec: std::os::unix::fs::MetadataExt::ctime_nsec(metadata),
        }
    }

    /// 判断 `after`（同一来源在别处重新 stat）是否与 `self` 指向同一份未变内容。
    ///
    /// unix：(dev, inode) 判是否被替换成别的文件/符号链接；大小判是否被改写；
    /// **mtime 与 ctime 判「同 inode 同长度的原地重写」**。任一维度不同即判「已变」。
    /// 非 unix：无 inode，退化为大小 + mtime 双判。
    ///
    /// `modified` 取不到时两侧都是 `None`（视为该维度不可用），不影响其余维度判定。
    fn is_same_source(&self, after: &Fingerprint) -> bool {
        #[cfg(unix)]
        let identity_kept = self.dev == after.dev
            && self.ino == after.ino
            && self.ctime == after.ctime
            && self.ctime_nsec == after.ctime_nsec;
        #[cfg(not(unix))]
        let identity_kept = true;
        identity_kept && self.len == after.len && self.modified == after.modified
    }
}

/// 校验 `path` 是真实普通文件并返回其 `symlink_metadata`；顺带拒绝控制字符路径与超限大小。
///
/// 末端形态先归一（P3）：`index.html/`、`index.html/.` 这类尾缀会让 `symlink_metadata`
/// 跟随末端链接而看不见链接本身，因此一律先剥尾缀，末端是 `..` 则失败关闭。
fn check_regular_file(
    path: &Path,
    label: &str,
    max_bytes: u64,
) -> Result<(Metadata, Fingerprint), String> {
    reject_control_chars(path, label)?;
    let checked = normalize_dir_path_for_symlink_check(path, label)?;
    let metadata = match fs::symlink_metadata(&checked) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            return Err(format!("{label}不存在：{}", safe_path_text(path)));
        }
        Err(err) => {
            return Err(format!(
                "读取 {label} 元数据失败：{}：{err}",
                safe_path_text(path)
            ));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "{label}不得是符号链接：{}；嵌入器只接受构建目录内的真实文件",
            safe_path_text(path)
        ));
    }
    if !metadata.is_file() {
        return Err(format!("{label}不是普通文件：{}", safe_path_text(path)));
    }
    if metadata.len() == 0 {
        return Err(format!("{label}为空文件：{}", safe_path_text(path)));
    }
    if metadata.len() > max_bytes {
        return Err(format!(
            "{label} 大小 {} 字节超过上限 {max_bytes} 字节：{}",
            metadata.len(),
            safe_path_text(path)
        ));
    }
    let fingerprint = Fingerprint::of(&metadata);
    Ok((metadata, fingerprint))
}

/// 校验并**一次性有界读取**真实字节；读后做来源指纹复核，捕获读取期间的替换/改动。
fn read_regular_file(
    path: &Path,
    label: &str,
    max_bytes: u64,
) -> Result<(Vec<u8>, Fingerprint), String> {
    let (_meta, expected) = check_regular_file(path, label, max_bytes)?;
    // 打开与复核都用归一后的末端形态：带尾缀的 `path` 会让 `File::open` 跟随末端符号链接，
    // 使「预检判定为普通文件」与「实际读到的字节」不是同一个来源。
    let checked = normalize_dir_path_for_symlink_check(path, label)?;

    // 经同一 open 句柄 fstat 再取一次指纹：unix 下若预检后被换成指向它物的符号链接，
    // 打开会跟随到目标，其 (dev, ino) 与被判定为普通文件的原 inode 不符；同 inode 但
    // mtime/ctime 已变同样是「已被改写」。本函数内三个点位（预检 -> 句柄 -> 读后）任一无
    // 一致即失败关闭；调用方还要把返回的指纹与 metadata 预检统计、生成末尾终核各比一次
    // （见 `collect_asset_files` 与 `verify_sources_unchanged`），绝不带着「可能不是刚才那份
    // 字节」的读数继续走。
    let file = File::open(&checked).map_err(|err| format!("打开 {label} 失败：{err}"))?;
    let open_meta = file
        .metadata()
        .map_err(|err| format!("读取 {label} 句柄元数据失败：{err}"))?;
    if !open_meta.is_file() {
        return Err(format!(
            "{label} 在读取时被替换成非常规文件：{}",
            safe_path_text(path)
        ));
    }
    let opened = Fingerprint::of(&open_meta);
    if !expected.is_same_source(&opened) {
        return Err(format!(
            "{label} 在预检与打开之间来源指纹变化（inode/mtime/ctime/大小任一，疑似被替换或原地改写），拒绝读入：{}",
            safe_path_text(path)
        ));
    }

    // 有界读取：最多读 max_bytes + 1，超上限立即失败（防文件在校验后膨胀绕限）。
    let mut buffer = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut buffer)
        .map_err(|err| format!("读取 {label} 失败：{err}"))?;
    if buffer.len() as u64 > max_bytes {
        return Err(format!(
            "{label} 实际读取超过上限 {max_bytes} 字节：{}",
            safe_path_text(path)
        ));
    }
    if buffer.len() as u64 != expected.len {
        return Err(format!(
            "{label} 大小在读取期间变化（{} -> {}），拒绝半份产物：{}",
            expected.len,
            buffer.len(),
            safe_path_text(path)
        ));
    }

    // 读取后再复核来源指纹，捕获「读完之后又被替换/改写」。
    let after_meta =
        fs::symlink_metadata(&checked).map_err(|err| format!("复核 {label} 元数据失败：{err}"))?;
    if after_meta.file_type().is_symlink() || !after_meta.is_file() {
        return Err(format!(
            "{label} 在读取期间被替换成非普通文件：{}",
            safe_path_text(path)
        ));
    }
    let after = Fingerprint::of(&after_meta);
    if !expected.is_same_source(&after) {
        return Err(format!(
            "{label} 在读取期间来源指纹变化（疑似被替换或改写），拒绝产出可能新旧混搭的产物：{}",
            safe_path_text(path)
        ));
    }
    Ok((buffer, expected))
}

// ---------------------------------------------------------------------------
// 目录/清单收集
// ---------------------------------------------------------------------------

/// 要求 `path` 是真实目录（存在、非符号链接、确为目录）。
///
/// 末端形态先归一（P3）：`symlink_dir/`、`symlink_dir/.` 在 POSIX 下**强制跟随**末端符号
/// 链接，`symlink_metadata` 因此报「真实目录」，「dist 根 / `dist/assets` 不得是符号链接」
/// 的门禁会被一个尾缀整体绕过。剥掉尾缀后再判定；末端是 `..` 时条目身份不可判定，
/// 失败关闭（见 `normalize_dir_path_for_symlink_check`）。
fn require_real_directory(path: &Path, label: &str) -> Result<(), String> {
    reject_control_chars(path, label)?;
    let checked = normalize_dir_path_for_symlink_check(path, label)?;
    let metadata = match fs::symlink_metadata(&checked) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            return Err(format!(
                "{label}不存在：{}。{BUILD_HINT}",
                safe_path_text(path)
            ));
        }
        Err(err) => {
            return Err(format!(
                "读取 {label} 元数据失败：{}：{err}",
                safe_path_text(path)
            ));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "{label}不得是符号链接：{}；嵌入器只接受构建目录内的真实文件",
            safe_path_text(path)
        ));
    }
    if !metadata.is_dir() {
        return Err(format!("{label}不是目录：{}", safe_path_text(path)));
    }
    Ok(())
}

/// 校验后的资产条目（快照路径由 `generate` 分配）。
pub(crate) struct CollectedAsset {
    url_path: String,
    mime: &'static str,
    file: PathBuf,
    size: u64,
    bytes: Vec<u8>,
    /// 读取时确认过的来源指纹（dev/inode/大小/mtime/ctime），供生成末尾终核比对。
    fingerprint: Fingerprint,
}

/// 单趟 metadata 预检得到的资产统计（此趟**不读字节**，让总量限制在读取前生效）。
///
/// 类型可见性与返回它的 `pub(crate)` 入口一致；字段一律私有，测试只能整体取用
/// `enumerate_asset_stats` 的结果，无法伪造其中的指纹。
#[derive(Debug)]
pub(crate) struct AssetStat {
    relative: String,
    url_path: String,
    mime: &'static str,
    path: PathBuf,
    len: u64,
    /// 预检时的来源指纹（终核按同一规则再取一次并比对）。
    fingerprint: Fingerprint,
}

/// 第一趟（P2 字节上限）：递归遍历 `dist/assets` 只做 metadata 预检——符号链接/名/深度/
/// MIME/数量/单文件上限/`checked_add` 总量上限，**不读字节**，因此 1 GiB 稀疏文件在预检
/// 即被挡下，绝不真读进内存。通过预检的文件逐个带上来源指纹，供读取后与生成末尾终核比对。
///
/// 生成末尾的终核（`verify_sources_unchanged`）**复用这一份遍历与预检规则**再走一遍，
/// 避免两条路径的遍历口径分叉。
///
/// `pub(crate)` 不是为了对外暴露遍历细节，而是让测试能拿到**这一趟**的预检结果，
/// 在「预检 → 读取」两个阶段之间注入变更（见 `tests/web_assets.rs` 的
/// `rejects_fingerprint_change_between_precheck_and_read`）；返回的 `AssetStat` 字段全部
/// 私有，调用方无法伪造指纹。
pub(crate) fn enumerate_asset_stats(assets_dir: &Path) -> Result<Vec<AssetStat>, String> {
    require_real_directory(assets_dir, "前端构建产物 dist/assets 目录")?;

    // 遍历 + 校验 + metadata 预检 + 溢出安全的总量累加。
    let mut stats: Vec<AssetStat> = Vec::new();
    let mut total_bytes: u64 = 0;
    let mut pending: Vec<(PathBuf, String, usize)> =
        vec![(assets_dir.to_path_buf(), String::new(), 0)];
    while let Some((dir, prefix, depth)) = pending.pop() {
        if depth > MAX_ASSET_DEPTH {
            return Err(format!(
                "dist/assets 目录层级超过 {MAX_ASSET_DEPTH} 层：{}",
                safe_path_text(&dir)
            ));
        }
        let read = fs::read_dir(&dir)
            .map_err(|err| format!("读取 {} 失败：{err}", safe_path_text(&dir)))?;
        for entry in read {
            let entry =
                entry.map_err(|err| format!("读取 {} 条目失败：{err}", safe_path_text(&dir)))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|err| format!("读取 {} 类型失败：{err}", safe_path_text(&path)))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if file_type.is_symlink() {
                return Err(format!(
                    "dist/assets 内不得存在符号链接：{}；符号链接可越出构建目录，嵌入器坚决拒绝",
                    safe_path_text(&path)
                ));
            }
            if !is_safe_name_component(&name) {
                return Err(format!(
                    "dist/assets 内存在非法名（隐藏文件，或含 `[A-Za-z0-9._-]` 之外的字符或控制字符）：{}",
                    safe_path_text(&path)
                ));
            }
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if file_type.is_dir() {
                pending.push((path, relative, depth + 1));
                continue;
            }
            if !file_type.is_file() {
                return Err(format!(
                    "dist/assets 内存在非常规文件（设备/FIFO/socket）：{}",
                    safe_path_text(&path)
                ));
            }
            if name.to_ascii_lowercase().ends_with(".map") {
                return Err(format!(
                    "dist/assets 内存在 source map：{}；生产构建不应发布 source map，请在 vite.config 关闭 build.sourcemap",
                    safe_path_text(&path)
                ));
            }
            validate_asset_name(&relative)?;
            let mime = mime_for(&name).ok_or_else(|| {
                format!(
                    "dist/assets 内资产 {} 的扩展名不在受支持 MIME 表内；请改用可明确给出 Content-Type 的资源类型，\
                     或按该产物的真实 MIME 扩充生成器 MIME 表",
                    safe_path_text(&path)
                )
            })?;
            if stats.len() >= MAX_ASSET_COUNT {
                return Err(format!("dist/assets 内文件数超过上限 {MAX_ASSET_COUNT}"));
            }
            // metadata 预检：非空、非符号链接、单文件不超上限（此处**不读字节**）。
            let (metadata, fingerprint) =
                check_regular_file(&path, "dist/assets 内资产", MAX_ASSET_FILE_BYTES)?;
            total_bytes = total_bytes.checked_add(metadata.len()).ok_or_else(|| {
                format!(
                    "dist/assets 资产总字节数溢出，疑似病态构建：{}",
                    safe_path_text(&path)
                )
            })?;
            if total_bytes > MAX_TOTAL_ASSET_BYTES {
                return Err(format!(
                    "dist/assets 资产总字节数 {} 超过上限 {MAX_TOTAL_ASSET_BYTES}：{}",
                    total_bytes,
                    safe_path_text(&path)
                ));
            }
            stats.push(AssetStat {
                url_path: format!("{ASSET_URL_PREFIX}{relative}"),
                relative,
                mime,
                len: metadata.len(),
                path,
                fingerprint,
            });
        }
    }
    Ok(stats)
}

/// 第二趟：只对**第一趟那份**预检结果做**有界读取** + 读取期来源指纹复核（P1/P2），
/// 结果落进按相对名（即 URL 尾段）升序的表，保证生成源码可复现。
///
/// 读取完成后必须把「预检指纹」与「读取时指纹」按同一条 `is_same_source` 规则比一次
/// （P1）：两趟之间来源被换成另一 inode 的**同长度**文件、或被**同 inode 同长度的原地
/// 重写**（mtime/ctime 必变）时，读取结果自身完全自洽——长度一致、句柄复核一致，只看
/// 字节长度什么都看不出来，只有跨阶段指纹比对才能挡住这份「预检是 A、读回来是 B」的
/// 半份产物。
pub(crate) fn collect_asset_files(
    stats: &[AssetStat],
) -> Result<BTreeMap<String, CollectedAsset>, String> {
    let mut files: BTreeMap<String, CollectedAsset> = BTreeMap::new();
    for stat in stats {
        let (bytes, fingerprint) =
            read_regular_file(&stat.path, "dist/assets 内资产", MAX_ASSET_FILE_BYTES)?;
        if !stat.fingerprint.is_same_source(&fingerprint) {
            return Err(format!(
                "dist/assets 内资产在预检与读取之间来源指纹变化（inode/mtime/ctime/大小任一，\
                 疑似被替换或同长度原地改写），拒绝半份产物：{}",
                safe_path_text(&stat.path)
            ));
        }
        if bytes.len() as u64 != stat.len {
            return Err(format!(
                "dist/assets 内资产在预检与读取之间大小变化（{} -> {}），拒绝半份产物：{}",
                stat.len,
                bytes.len(),
                safe_path_text(&stat.path)
            ));
        }
        files.insert(
            stat.relative.clone(),
            CollectedAsset {
                url_path: stat.url_path.clone(),
                mime: stat.mime,
                file: stat.path.clone(),
                size: stat.len,
                bytes,
                fingerprint,
            },
        );
    }
    Ok(files)
}

// ---------------------------------------------------------------------------
// 输入/输出分离（P3：绝不在关系检查之前清理，绝不把输出写进输入）
// ---------------------------------------------------------------------------

/// 对**应当真实存在**的路径做 `canonicalize`；失败一律 `Err` 失败关闭（绝不退回字面拼写）。
///
/// `source` 是最初传入的那条拼写，只用于诊断：报错里同时给出「哪一段解析不动」与
/// 「来自哪条输入」，否则 canonical 结果会把发起方信息全部吞掉。
fn canonical_existing_path(path: &Path, source: &Path, label: &str) -> Result<PathBuf, String> {
    fs::canonicalize(path).map_err(|err| {
        format!(
            "解析 {label} 的真实路径失败（{err}）：{}；来源 {}；该路径无法确定真实落点，拒绝",
            safe_path_text(path),
            safe_path_text(source)
        )
    })
}

/// 解析路径的**真实落点**：逐段对照文件系统解析（跟随符号链接、逐段折叠 `..`），产出
/// 「canonical 真实前缀 + 确定不存在的字面尾段」。返回值恒为绝对路径，可直接用 `==`
/// 与 `starts_with` 比较两端是否同一目录/互为祖先后代。
///
/// 为什么必须**逐段**解析，而不是「最深现存祖先 canonicalize + 尾段词法接回」（P1）：
/// 尾段里 `..` 折回一个真实存在的目录之后，**其后的每一段又回到「可能存在符号链接」的
/// 世界**；按字面接回就把别名藏进了真实路径里。`root/missing/../alias/out`
/// （`missing` 不存在、`alias -> dist`）在旧实现下：祖先只能退到 `root`，词法折叠
/// `missing/..` 回到 `root`，再把 `alias` **按字面** push 进去，得到 `root/alias/out`；
/// 与 `dist_real = root/dist` 前缀比对不相等，两个重叠判定同时判「分离」。而实际写入
/// 时（本机实测）`create_dir_all` 会先把 `root/missing` 真建出来，此后内核就能解析
/// `missing/..`，跟随 `alias` 把 index.html 快照直接写进 `root/dist/out/`——输入被输出污染。
/// 结论：**一旦 `..` 把路径退回一个真实存在的目录，其后每一段都必须重新查文件系统**。
///
/// 算法（维护一条真实路径栈，栈内**始终不含任何符号链接**）：
/// - 锚点：绝对路径从根（Windows 上还有盘符前缀）开始；相对路径以 cwd 的 canonical 形态
///   为锚点，让「一条绝对、一条相对」的拼写也落在同一比较基准上。`CurDir` 直接忽略。
/// - `Normal(name)`：对「栈 + name」取 `symlink_metadata`，按条目形态三分。存在且**不是**
///   符号链接 → 直接入栈：canonical 前缀接一段非链接条目，结果仍 canonical，不必再 stat
///   一次。存在且**是**符号链接 → `canonicalize` 跟随到最终落点：链接链、相对目标与多级
///   链接都由内核一次解完，成环时内核返回 `ELOOP` 即失败关闭（无需自设跳数上限），悬空
///   链接根本落不到任何真实条目，同样拒绝。不存在（`NotFound`）→ 进入「尚未创建」状态：
///   父目录已确认不存在，其下任何条目都不可能存在，后续段只按字面累积（这正是「本轮还没
///   建出来的输出目录」的正常形态）。
/// - `ParentDir`：处于「尚未创建」状态时先弹掉一个未创建段；**若因此弹空、路径回到真实
///   存在的前缀，立刻对该前缀重新 `canonicalize`**——既恢复「已存在」状态（其后的段重新
///   走符号链接判定，别名无处遁形），也顺手挡住解析期间的目录替换。否则直接弹掉真实栈
///   一段（canonical 路径的父路径仍 canonical）；已在锚点时 `..` 即锚点本身，与 POSIX
///   的 `/..` == `/` 一致。
///
/// 失败关闭（`Err`）：空路径、相对路径但 cwd 已不可解析、路径中任一段查询失败（权限、
/// `ENOTDIR`、悬空链接、链接成环）。空路径尤其**不得**退化成 cwd：那会把「不存在」偷换成
/// 当前工作目录，是放行而不是失败关闭。
pub(crate) fn resolve_existing_prefix(path: &Path, label: &str) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err(format!(
            "{label} 为空路径，无法确定其真实落点，拒绝（绝不退化成当前目录）"
        ));
    }
    let anchored = matches!(
        path.components().next(),
        Some(Component::Prefix(_) | Component::RootDir)
    );
    let mut resolved = if anchored {
        PathBuf::new()
    } else {
        canonical_existing_path(Path::new("."), path, label)?
    };
    // 「尚未创建」的尾段：其父目录已确认不存在，只按字面累积，绝不查文件系统、绝不折叠。
    let mut pending: Vec<OsString> = Vec::new();
    for component in path.components() {
        match component {
            // 前缀/根只可能出现在开头；命中即重置锚点（也顺带清掉不该存在的 pending）。
            Component::Prefix(prefix) => {
                resolved = PathBuf::from(prefix.as_os_str());
                pending.clear();
            }
            Component::RootDir => {
                resolved.push(component.as_os_str());
                pending.clear();
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if pending.is_empty() {
                    // canonical 前缀的父路径仍 canonical；到锚点即止（`/..` == `/`）。
                    resolved.pop();
                    continue;
                }
                pending.pop();
                if pending.is_empty() {
                    // 弹掉最后一个未创建段 = 回到一个真实存在的目录：必须立刻重新规范化，
                    // 让**其后**的段重新对照文件系统解析。P1 的绕过点正是漏了这一步。
                    resolved = canonical_existing_path(&resolved, path, label)?;
                }
            }
            Component::Normal(name) => {
                if !pending.is_empty() {
                    pending.push(name.to_os_string());
                    continue;
                }
                let candidate = resolved.join(name);
                match fs::symlink_metadata(&candidate) {
                    // 符号链接段：跟随到最终落点，绝不把别名按字面留在待比较的路径里。
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        resolved = canonical_existing_path(&candidate, path, label)?;
                    }
                    Ok(_) => {
                        resolved = candidate;
                    }
                    Err(err) if err.kind() == ErrorKind::NotFound => {
                        pending.push(name.to_os_string());
                    }
                    Err(err) => {
                        return Err(format!(
                            "解析 {label} 的真实路径失败（{err}）：{}；来源 {}；无法确定真实落点，拒绝",
                            safe_path_text(&candidate),
                            safe_path_text(path)
                        ));
                    }
                }
            }
        }
    }
    for name in pending {
        resolved.push(name);
    }
    Ok(resolved)
}

/// 校验「输出（快照目录）与输入（dist 目录）分离且无别名」。**必须在任何清理或写入之前**
/// 调用（`build.rs` 亦先调它再清空旧快照目录），否则异常调用会先删掉输入再报错。
///
/// 拒绝三类重叠（一律按 `resolve_existing_prefix` **逐段解析**出的真实落点判定，命中即拒
/// 且不动任何文件）：
/// - 同一目录或其符号链接别名（例如 `generate(dist, dist)`）：快照 index.html 会覆盖输入；
/// - 快照目录在 dist 之内：输出污染输入，新写的快照下一轮会被当成资产嵌进产物；
/// - dist 在快照目录之内：清空旧快照等于删输入。
///
/// 判定所用的真实落点是**逐段**解析出来的：路径里每一段都对照文件系统查过一次（符号链接段
/// 跟随到最终落点，`..` 折回真实目录后其后的段再重新查一次），所以 `missing/../alias/out`
/// 这种「先穿过一个不存在的段、再借符号链接别名落回 dist」的拼写同样按 `dist/out` 判定，
/// 而不是按字面拼写判成分离（P1）。两端解析不动（空路径、权限、悬空链接、链接成环）时
/// 一律 `Err` 失败关闭，绝不带着「落点未知」的路径进入比较。
///
/// 符号链接策略与全模块一致：dist **自身**不得是符号链接（既有策略），输出目录若已是
/// 指向别处的符号链接同样拒绝（输出必须是调用方私有目录，构建期即 `OUT_DIR` 子目录）。
pub fn validate_output_separation(dist_dir: &Path, snapshot_dir: &Path) -> Result<(), String> {
    reject_control_chars(dist_dir, "dist 目录")?;
    reject_control_chars(snapshot_dir, "快照目录")?;
    // 末端形态先归一（P3）：`snapshot-link/` 会让 `symlink_metadata` 看不见末端链接，
    // 末端 `..` 让条目身份完全不可判定；两者都必须在任何比较（以及调用方的清理）之前
    // 失败关闭，绝不能带着「看不见的链接」进入下面的真实路径比对。
    let dist_dir = &normalize_dir_path_for_symlink_check(dist_dir, "dist 目录")?;
    let snapshot_dir = &normalize_dir_path_for_symlink_check(snapshot_dir, "快照目录")?;
    // 用 `matches!` + 守卫折叠这一次判定：结果是个具名布尔量，下面的错误信息直接复用，
    // 也不会再给 clippy::collapsible_if 留下可折的嵌套。
    let snapshot_is_symlink = matches!(
        fs::symlink_metadata(snapshot_dir),
        Ok(metadata) if metadata.file_type().is_symlink()
    );
    if snapshot_is_symlink {
        return Err(format!(
            "快照目录不得是符号链接：{}；输出必须是调用方私有的真实目录",
            safe_path_text(snapshot_dir)
        ));
    }
    let dist_real = resolve_existing_prefix(dist_dir, "dist 目录")?;
    let snapshot_real = resolve_existing_prefix(snapshot_dir, "快照目录")?;
    if dist_real == snapshot_real {
        return Err(format!(
            "快照目录与 dist 输入目录是同一目录（含符号链接别名）：dist={}，快照={}；两者必须分离，\
             否则清理旧快照会先删除输入。请把快照目录指向 OUT_DIR 之类的私有输出目录",
            safe_path_text(&dist_real),
            safe_path_text(&snapshot_real)
        ));
    }
    if snapshot_real.starts_with(&dist_real) {
        return Err(format!(
            "快照目录在 dist 输入目录之内（{} 在 {} 之下）：输出会污染输入并被当成资产嵌入，坚决拒绝",
            safe_path_text(&snapshot_real),
            safe_path_text(&dist_real)
        ));
    }
    if dist_real.starts_with(&snapshot_real) {
        return Err(format!(
            "dist 输入目录在快照目录之内（{} 在 {} 之下）：清空旧快照会删除输入，坚决拒绝",
            safe_path_text(&dist_real),
            safe_path_text(&snapshot_real)
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 生成末尾终核（P1：最后一个核验点）
// ---------------------------------------------------------------------------

/// 生成末尾的**终核**：在快照全部写完之后、渲染生成表之前，把「index.html 的来源指纹」与
/// 「整个资产集合（相对名集合 + 每个来源的 dev/inode/大小/mtime/ctime）」再核一遍。
///
/// 覆盖的跨文件窗口：写完快照后原 HTML 被改、某资产被换成另一 inode、某资产被**同 inode
/// 同长度的原地重写**（mtime/ctime 必变）、资产目录新增或删除文件。任一命中即 `Err` 失败
/// 关闭，由调用方重跑构建；本函数**不**尝试「读最新的一份」补救，也不声称对并发写入者给出
/// 原子快照。
fn verify_sources_unchanged(
    index_html: &Path,
    html_fingerprint: &Fingerprint,
    assets_dir: &Path,
    sources: &[(String, PathBuf, Fingerprint)],
) -> Result<(), String> {
    // 终核 1：index.html 指纹（HTML 指纹绝不能丢，否则「HTML 新 / 资产表旧」重新可能出现）。
    let html_meta = fs::symlink_metadata(index_html)
        .map_err(|err| format!("复核 index.html 元数据失败：{err}"))?;
    if html_meta.file_type().is_symlink() || !html_meta.is_file() {
        return Err(format!(
            "index.html 在生成期间被替换成非普通文件：{}",
            safe_path_text(index_html)
        ));
    }
    let html_now = Fingerprint::of(&html_meta);
    if !html_fingerprint.is_same_source(&html_now) {
        return Err(format!(
            "index.html 在生成期间来源指纹变化（被替换、原地改写或 mtime/ctime 变化），拒绝产出可能新旧混搭的产物：{}",
            safe_path_text(index_html)
        ));
    }

    // 终核 2：资产集合 + 逐个来源指纹（复用第一趟同一套遍历与预检规则，避免口径分叉）。
    let after = enumerate_asset_stats(assets_dir)?;
    let after_map: BTreeMap<&str, &Fingerprint> = after
        .iter()
        .map(|stat| (stat.relative.as_str(), &stat.fingerprint))
        .collect();
    let before_names: BTreeSet<&str> = sources.iter().map(|(r, _, _)| r.as_str()).collect();
    let after_names: BTreeSet<&str> = after_map.keys().copied().collect();
    if before_names != after_names {
        let added: Vec<&str> = after_names.difference(&before_names).copied().collect();
        let removed: Vec<&str> = before_names.difference(&after_names).copied().collect();
        return Err(format!(
            "dist/assets 资产集合在生成期间变化（新增 {added:?}、消失 {removed:?}），拒绝产出可能新旧混搭的产物"
        ));
    }
    for (relative, file, fingerprint) in sources {
        let Some(now) = after_map.get(relative.as_str()) else {
            return Err(format!("资产来源在生成期间消失：{}", safe_path_text(file)));
        };
        if !fingerprint.is_same_source(now) {
            return Err(format!(
                "资产来源 {} 在生成期间来源指纹变化（被替换、原地改写或 mtime/ctime 变化），拒绝产出可能新旧混搭的产物",
                safe_path_text(file)
            ));
        }
    }
    Ok(())
}

/// 渲染供 `src/web.rs` `include!` 的 Rust 源码：只 `include` 快照目录内的稳定副本。
fn render_rust_source(
    dist_dir: &Path,
    snapshot_dir: &Path,
    index_html_snapshot: &Path,
    assets: &[AssetEntry],
) -> String {
    let total: u64 = assets.iter().map(|asset| asset.size).sum();
    let mut source = String::new();
    source.push_str("// @generated by crates/rsetup-controller/build.rs —— 请勿手改，请勿提交。\n");
    // 路径经 `{:?}`（Debug）转义后写入行注释：换行等控制字符渲染成 `\n`，无法截断注释注入代码（P2）。
    source.push_str(&format!("// 前端构建产物根目录：{dist_dir:?}\n"));
    source.push_str(&format!(
        "// 字节快照目录（生成物只引用此目录内文件）：{snapshot_dir:?}\n"
    ));
    source.push_str(&format!(
        "// 嵌入资产 {} 份、共 {} 字节；清单随前端构建变化，资产哈希不进入本仓源码。\n\n",
        assets.len(),
        total
    ));
    source.push_str(
        "/// 编译期嵌入的 Vite 入口 HTML（校验过的字节快照副本，非可变 dist 原路径）。\n",
    );
    source.push_str(&format!(
        "const INDEX_HTML: &str = include_str!({index_html_snapshot:?});\n\n"
    ));
    source.push_str("/// 编译期资产表：与生成时的前端构建逐项对应，新增 chunk 自动纳入。\n");
    source.push_str("const ASSETS: &[EmbeddedAsset] = &[\n");
    for asset in assets {
        source.push_str(&format!(
            "    EmbeddedAsset {{\n        path: {:?},\n        mime: {:?},\n        bytes: include_bytes!({:?}),\n    }},\n",
            asset.url_path,
            asset.mime,
            asset.snapshot
        ));
    }
    source.push_str("];\n");
    source
}

/// 依据 `dist_dir` 磁盘现状生成资产表：校验 -> 读入字节 -> 写快照到 `snapshot_dir`
/// -> 复核来源指纹 -> 渲染只引用快照的 Rust 源码。任何不一致都以 `Err(说明)` 失败关闭。
pub fn generate(dist_dir: &Path, snapshot_dir: &Path) -> Result<Generation, String> {
    if !dist_dir.is_absolute() {
        return Err(format!(
            "dist 目录必须是绝对路径，实际：{}",
            safe_path_text(dist_dir)
        ));
    }
    if !snapshot_dir.is_absolute() {
        return Err(format!(
            "快照目录必须是绝对路径，实际：{}",
            safe_path_text(snapshot_dir)
        ));
    }
    // 生成的注释与 include 路径都要嵌入这些路径，先拒控制字符（P2）。
    reject_control_chars(dist_dir, "dist 目录")?;
    reject_control_chars(snapshot_dir, "快照目录")?;
    // 剥掉两个目录的尾部 `/` 与 `.` 段（P3）：带着这层尾缀，POSIX lookup 会强制跟随末端
    // 符号链接（`symlink_dir/` 被当成真实目录），生成物里的 `include_bytes!("…/snap/")`
    // 也会因指向目录而以 ENOTDIR 编译失败。剥除是纯词法的，末端 `..` 由下面的分离校验
    // 与 `require_real_directory` 一并失败关闭。
    let dist_dir: &Path = &strip_trailing_dot_segments(dist_dir);
    let snapshot_dir: &Path = &strip_trailing_dot_segments(snapshot_dir);
    // 任何读取/写入之前先确认输入与输出分离且无别名（P3）：本函数自己**不动任何文件**，
    // 调用方（含 `build.rs` 清空旧快照）也必须先过这道门再清理。
    validate_output_separation(dist_dir, snapshot_dir)?;

    require_real_directory(dist_dir, "前端构建产物 dist 目录")?;

    let index_html = dist_dir.join("index.html");
    let assets_dir = dist_dir.join("assets");

    // 1. 入口 HTML：校验 + 有界读取 + 复核，并校验 UTF-8。
    //    label 带尾随空格，使 `{label}为空文件` 读作「index.html 为空文件」，与既有错误契约片段一致。
    let (html_bytes, html_fingerprint) =
        read_regular_file(&index_html, "index.html ", MAX_INDEX_HTML_BYTES).map_err(|err| {
            if err.contains("index.html不存在") || err.contains("index.html 不存在") {
                format!(
                    "缺少 index.html：{}；前端尚未构建。{BUILD_HINT}",
                    safe_path_text(&index_html)
                )
            } else {
                err
            }
        })?;
    let html = String::from_utf8(html_bytes).map_err(|_| {
        format!(
            "index.html 不是合法 UTF-8：{}；前端构建产物编码异常",
            safe_path_text(&index_html)
        )
    })?;

    // 2. 从**真实起始标签**提取本地引用（跳过注释/rawtext；P2）。
    let references = collect_local_references(&html)?;

    // 3. 递归校验 assets 目录，同时把每个文件的字节快照读入内存；来源指纹随字节一起留下，
    //    供第 6 步终核比对（指纹不能在读完之后就丢掉）。预检与读取分两趟，但**只遍历一次**：
    //    读取用的正是预检那份统计，两趟之间的来源变化由 `collect_asset_files` 按指纹挡下。
    let stats = enumerate_asset_stats(&assets_dir)?;
    let files = collect_asset_files(&stats)?;
    let source_fingerprints: Vec<(String, PathBuf, Fingerprint)> = files
        .iter()
        .map(|(relative, collected)| {
            (
                relative.clone(),
                collected.file.clone(),
                collected.fingerprint,
            )
        })
        .collect();

    // 4. 一致性：入口 HTML 引用的每个 /assets/* 必须在快照集合内（读到的字节自洽）。
    if !references
        .iter()
        .any(|url| url.to_ascii_lowercase().ends_with(".js"))
    {
        return Err(format!(
            "index.html 未引用任何 JS 入口资产（实际本地引用：{references:?}）；前端构建产物不完整"
        ));
    }
    for url in &references {
        if !files.contains_key(&url[ASSET_URL_PREFIX.len()..]) {
            return Err(format!(
                "index.html 引用的资产在 dist/assets 下不存在：{url}；构建产物不完整，或 index.html 与产物不同步"
            ));
        }
    }

    // 5. 写快照：先建目录，按 url 升序把校验过的字节落盘，include 只引用它们。
    fs::create_dir_all(snapshot_dir)
        .map_err(|err| format!("创建快照目录 {} 失败：{err}", safe_path_text(snapshot_dir)))?;
    let index_html_snapshot = snapshot_dir.join("index.html");
    fs::write(&index_html_snapshot, html.as_bytes()).map_err(|err| {
        format!(
            "写入 index.html 快照 {} 失败：{err}",
            safe_path_text(&index_html_snapshot)
        )
    })?;

    let mut assets: Vec<AssetEntry> = Vec::with_capacity(files.len());
    for (index, collected) in files.into_values().enumerate() {
        let snapshot_path = snapshot_dir.join(format!("asset-{index:05}"));
        fs::write(&snapshot_path, &collected.bytes).map_err(|err| {
            format!(
                "写入资产快照 {} 失败：{err}",
                safe_path_text(&snapshot_path)
            )
        })?;
        assets.push(AssetEntry {
            url_path: collected.url_path,
            mime: collected.mime,
            file: collected.file,
            snapshot: snapshot_path,
            size: collected.size,
        });
    }

    // 6. 终核（P1 的最后一个核验点）：写完快照后，index.html 指纹与整个资产集合（含新增、
    //    删除、替换、同 inode 同长度的原地改写导致的 mtime/ctime 变化）必须仍与读取时一致。
    //    命中任一不确定状态即 Err，由调用方重跑构建；绝不「读最新的一份」继续产出。
    verify_sources_unchanged(
        &index_html,
        &html_fingerprint,
        &assets_dir,
        &source_fingerprints,
    )?;

    // 7. 全部通过后才渲染生成表（失败时上面已 Err 返回，绝不产出半张表）。
    let rust_source = render_rust_source(dist_dir, snapshot_dir, &index_html_snapshot, &assets);

    // rerun 监听：原输入目录本身 -> 新增/删除；逐个资产与 index.html -> 内容替换。
    let mut rerun_paths = vec![index_html.clone(), assets_dir.clone()];
    for asset in &assets {
        rerun_paths.push(asset.file.clone());
    }

    Ok(Generation {
        dist_dir: dist_dir.to_path_buf(),
        index_html,
        index_html_snapshot,
        snapshot_dir: snapshot_dir.to_path_buf(),
        assets,
        rust_source,
        rerun_paths,
    })
}
