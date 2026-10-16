use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// 纯标准库临时目录守卫；Drop 时在验证独立临时根绝对路径后递归清理，防误删。
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let seq = COUNTER.fetch_add(1, Ordering::SeqCst);
        let prefix = format!("rsetup-board-agent-{}-{}-{seq}", tag, std::process::id());
        let root = std::env::temp_dir();
        let path = root.join(&prefix);
        let abs_path = if path.is_absolute() {
            path
        } else {
            root.canonicalize().unwrap_or(root).join(path)
        };
        std::fs::create_dir_all(&abs_path).expect("create temp dir");
        Self { path: abs_path }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // 安全防护门禁：严格验证绝对路径位于系统临时目录内且持有专用前缀，防误删任意主机目录
        let temp_root = std::env::temp_dir();
        let is_safe = self.path.is_absolute()
            && self.path.starts_with(&temp_root)
            && self
                .path
                .file_name()
                .and_then(|f| f.to_str())
                .map(|name| name.starts_with("rsetup-board-agent-"))
                .unwrap_or(false);

        if is_safe {
            if let Err(err) = std::fs::remove_dir_all(&self.path) {
                // 错误可诊断：在非 NotFound 情况下打印诊断信息，不静默吞掉真实 IO 错误
                if err.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "diagnostics: failed to clean up TempDir at {:?}: {err}",
                        self.path
                    );
                }
            }
        } else {
            eprintln!(
                "safety violation: refusing to remove path {:?} not matching safe temp root",
                self.path
            );
        }
    }
}
