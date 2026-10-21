//! 构建脚本：把 `apps/controller-web/dist` 的真实前端构建产物编成 Rust 静态资产表。
//!
//! 全部生成与校验逻辑在 `build/web_assets.rs`（与 `tests/web_assets.rs` 共用同一份
//! 源码），本文件只负责：定位 dist、把校验过的字节快照写入 `OUT_DIR` 私有子目录、
//! 写 `OUT_DIR/web_assets.rs`、向 cargo 声明需要重跑本脚本的输入（始终是 **dist 原输入**，
//! 而非快照产物）。产物缺失或不合法时直接 panic 让构建失败关闭。
//!
//! 生成的 `web_assets.rs` 只 `include` 快照目录里的稳定字节副本，不 `include` 可变的
//! `dist` 原路径：这样 rustc 稍后编译 `src/web.rs` 读到的一定是本次校验过的那一份字节，
//! 前端在生成之后改 HTML、换哈希、删文件、把原资产换成符号链接都无法造成坏包（P1）。

#[path = "build/web_assets.rs"]
mod web_assets;

use std::path::{Path, PathBuf};
use web_assets::{
    DEFAULT_DIST_RELATIVE_PATH, DIST_ENV_OVERRIDE, GENERATED_FILE_NAME, SNAPSHOT_SUBDIR,
};

/// 需要 cargo 在自身变更时重跑本脚本的构建脚本源文件。
const BUILD_SCRIPT_SOURCES: &[&str] = &["build.rs", "build/web_assets.rs"];

fn env_path(name: &str) -> PathBuf {
    std::env::var(name)
        .unwrap_or_else(|err| panic!("cargo 必须提供 {name}：{err}"))
        .into()
}

/// 绝对化路径但不解析符号链接（`Path::canonicalize` 会解析，错误信息会指向构建目录之外）。
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path)
        .unwrap_or_else(|err| panic!("规范化路径 {:?} 失败：{err}", path.to_string_lossy()))
}

/// dist 目录：默认 crate 清单目录相对路径，允许 `RSETUP_WEB_DIST` 以绝对路径覆盖。
///
/// 两个来源都先剥掉尾部的 `/` 与 `.` 段（P3）：POSIX lookup 对以 `/` 或 `/.` 结尾的路径
/// **强制跟随**末端符号链接，`RSETUP_WEB_DIST=/…/dist/` 这种拼写会让生成器里「dist 根不得
/// 是符号链接」的门禁看不见链接本身。剥除是纯词法的（生成器内部同样会剥一次），在这里剥
/// 是让绝对性断言与错误信息都按剥后的形态给出。
fn resolve_dist_dir(manifest_dir: &Path) -> PathBuf {
    match std::env::var(DIST_ENV_OVERRIDE) {
        Ok(value) if !value.trim().is_empty() => {
            let path = web_assets::strip_trailing_dot_segments(&PathBuf::from(value.trim()));
            assert!(
                path.is_absolute(),
                "{DIST_ENV_OVERRIDE} 必须是绝对路径，实际：{:?}",
                path.to_string_lossy()
            );
            absolute(&path)
        }
        _ => {
            let path = web_assets::strip_trailing_dot_segments(
                &manifest_dir.join(DEFAULT_DIST_RELATIVE_PATH),
            );
            absolute(&path)
        }
    }
}

/// 尽力清空上一轮快照目录（本轮按当前资产重写，避免遗留旧哈希快照文件）。
///
/// **只允许在 `validate_output_separation` 之后调用**：清空前必须确认快照目录与 dist 输入
/// 目录分离且无别名，否则重叠调用会先删掉输入再报错（P3）。
fn clear_stale_snapshots(snapshot_dir: &Path) {
    match std::fs::remove_dir_all(snapshot_dir) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => panic!(
            "清空旧快照目录 {:?} 失败：{err}",
            snapshot_dir.to_string_lossy()
        ),
    }
}

fn main() {
    // dist 位置可由环境变量指路，就必须让 cargo 在该变量变化时重跑本脚本：
    // 只改变量而不改任何文件时，cargo 会原样复用上一轮的资产表并直接 Finished。
    println!("cargo:rerun-if-env-changed={DIST_ENV_OVERRIDE}");

    let manifest_dir = env_path("CARGO_MANIFEST_DIR");
    let dist_dir = resolve_dist_dir(&manifest_dir);
    let out_dir = env_path("OUT_DIR");
    let snapshot_dir = out_dir.join(SNAPSHOT_SUBDIR);

    // 顺序很重要（P3）：先确认「输入与输出分离且无别名」，**再**清空旧快照目录。
    // 若有人把 RSETUP_WEB_DIST 指到 OUT_DIR/web_assets_snapshot（或任何重叠位置），
    // 这里直接失败关闭，输入文件保持原样，不会先删输入再报错。
    if let Err(err) = web_assets::validate_output_separation(&dist_dir, &snapshot_dir) {
        panic!("前端构建产物的快照输出目录不合法，构建失败关闭：{err}");
    }
    clear_stale_snapshots(&snapshot_dir);
    let generation = web_assets::generate(&dist_dir, &snapshot_dir)
        .unwrap_or_else(|err| panic!("前端构建产物不可嵌入，构建失败关闭：{err}"));

    let generated_file = out_dir.join(GENERATED_FILE_NAME);
    std::fs::write(&generated_file, &generation.rust_source).unwrap_or_else(|err| {
        panic!(
            "写入生成文件 {:?} 失败：{err}",
            generated_file.to_string_lossy()
        )
    });

    // rerun 监听只声明 **dist 原输入**：目录本身 -> 新增/删除文件；逐个资产与 index.html
    // -> 内容替换；再加上构建脚本自身。快照是本轮产物，不作为重跑触发源。
    let mut rerun_paths = generation.rerun_paths.clone();
    for source in BUILD_SCRIPT_SOURCES {
        let path = manifest_dir.join(source);
        // `manifest_dir` 直接取自 `CARGO_MANIFEST_DIR`，不经过生成器校验：先逐条拒绝
        // 控制字符，才允许进入待打印列表（P2）。
        if let Err(err) = web_assets::reject_control_chars(&path, "构建脚本源码") {
            panic!("构建脚本监听路径不合法，构建失败关闭：{err}");
        }
        rerun_paths.push(path);
    }
    // 行协议出口闸门（P2）：`cargo:rerun-if-changed=` 按行解析，路径里只要有一个换行，
    // 其后的文本就成为一条独立 cargo 指令（`/p\ncargo:warning=INJECTED`）。生成器产出的
    // 那批已在 `generate` 内拒绝控制字符，这里再对**全量**路径复核一次：任一路径含
    // 换行/回车等控制字符立即 panic 失败关闭，绝不写出可注入的 cargo 行。
    if let Err(err) = web_assets::validate_rerun_paths(&rerun_paths, "rerun 监听路径") {
        panic!("构建脚本监听路径不合法，构建失败关闭：{err}");
    }
    for path in rerun_paths {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    // 成功时只在 `-vv` 下可见：记录本次真正嵌入的是哪一份构建，便于事后对账。
    eprintln!(
        "rsetup-controller 嵌入前端构建：{} 项资产、{} 字节，dist={}，快照={}",
        generation.assets.len(),
        generation.total_bytes(),
        generation.dist_dir.display(),
        generation.snapshot_dir.display()
    );
}
