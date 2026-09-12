# Task 2.1 Review Package
Base: 505ae814d537b88a39122638a380bf4e9feedd15
Head: e3413109a9307f59d5b06691459a933f7c9e0114

## Git Log
e341310 feat(core): implement MmcManager and sysfs multi-device probing

## Git Diff
diff --git a/crates/rsetup-core/src/lib.rs b/crates/rsetup-core/src/lib.rs
index e0081d5..9c991e4 100644
--- a/crates/rsetup-core/src/lib.rs
+++ b/crates/rsetup-core/src/lib.rs
@@ -14,6 +14,7 @@ pub mod mmc;
 pub mod model;
 pub mod nvme;
 pub mod overlay;
 pub mod probe;
+pub use mmc::MmcManager;
 pub use nvme::{NvmeError, NvmeManager};
diff --git a/crates/rsetup-core/src/mmc.rs b/crates/rsetup-core/src/mmc.rs
index fcaea79..6eefdd8 100644
--- a/crates/rsetup-core/src/mmc.rs
+++ b/crates/rsetup-core/src/mmc.rs
@@ -1,3 +1,6 @@
+use crate::model::{MmcDevice, MmcHealth, MmcStatus};
+use std::path::{Path, PathBuf};
 use thiserror::Error;

+pub mod sys;
+
+#[derive(Debug, Clone)]
+pub struct MmcManager {
+    status: MmcStatus,
+    sysfs_root: PathBuf,
+}
+
+impl Default for MmcManager {
+    fn default() -> Self {
+        Self::new()
+    }
+}
+
+impl MmcManager {
+    pub fn probe_and_init(sysfs_root: Option<&Path>) -> Self {
+        let root = sysfs_root
+            .map(|p| p.to_path_buf())
+            .unwrap_or_else(|| PathBuf::from("/"));
+        let device_names = Self::probe_sysfs(&root);
+
+        let status = if device_names.is_empty() {
+            MmcStatus {
+                initialized: false,
+                devices: Vec::new(),
+                message: Some("No MMC/SD devices detected in system".into()),
+            }
+        } else {
+            let mut devices = Vec::new();
+            for name in device_names {
+                if let Ok(dev) = sys::read_device_sysfs(&root, &name) {
+                    devices.push(dev);
+                }
+            }
+            if devices.is_empty() {
+                MmcStatus {
+                    initialized: false,
+                    devices: Vec::new(),
+                    message: Some("No supported MMC/SD devices found".into()),
+                }
+            } else {
+                MmcStatus {
+                    initialized: true,
+                    devices,
+                    message: None,
+                }
+            }
+        };
+
+        Self {
+            status,
+            sysfs_root: root,
+        }
+    }
+
+    pub fn new() -> Self {
+        Self::probe_and_init(None)
+    }
+
+    pub fn sysfs_root(&self) -> &Path {
+        &self.sysfs_root
+    }
+
+    pub fn status(&self) -> MmcStatus {
+        self.status.clone()
+    }
+
+    pub fn is_initialized(&self) -> bool {
+        self.status.initialized
+    }
+
+    pub fn probe_sysfs(root: &Path) -> Vec<String> {
+        let devices_dir = if root == Path::new("/") {
+            PathBuf::from("/sys/bus/mmc/devices")
+        } else {
+            root.join("sys/bus/mmc/devices")
+        };
+
+        let mut devices = Vec::new();
+        if let Ok(entries) = std::fs::read_dir(&devices_dir) {
+            for entry in entries.flatten() {
+                let file_name = entry.file_name();
+                let name = file_name.to_string_lossy();
+                let entry_path = entry.path();
+                let type_file = entry_path.join("type");
+                if let Some(card_type) = sys::read_trimmed_attr(&type_file) {
+                    let t = card_type.trim();
+                    if t == "MMC" || t == "SD" {
+                        devices.push(name.to_string());
+                    }
+                }
+            }
+        }
+        devices.sort();
+        devices
+    }
+}
diff --git a/crates/rsetup-core/src/mmc/sys.rs b/crates/rsetup-core/src/mmc/sys.rs
new file mode 100644
index 0000000..7bb2c2a
--- /dev/null
+++ b/crates/rsetup-core/src/mmc/sys.rs
@@ -0,0 +1,114 @@
+use super::{
+    format_manufacturer, generate_warning_flags, parse_life_time_str, parse_pre_eol_info_str,
+    MmcError,
+};
+use crate::model::{MmcDevice, MmcHealth};
+use std::fs;
+use std::path::{Path, PathBuf};
+
+pub fn read_trimmed_attr(path: &Path) -> Option<String> {
+    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
+}
+
+pub fn read_block_device_size(sysfs_root: &Path, block_name: &str) -> u64 {
+    let size_path = if sysfs_root == Path::new("/") {
+        PathBuf::from(format!("/sys/class/block/{}/size", block_name))
+    } else {
+        sysfs_root.join(format!("sys/class/block/{}/size", block_name))
+    };
+
+    if let Some(size_str) = read_trimmed_attr(&size_path) {
+        if let Ok(blocks) = size_str.parse::<u64>() {
+            return blocks.saturating_mul(512);
+        }
+    }
+    0
+}
+
+pub fn read_device_sysfs(sysfs_root: &Path, dev_name: &str) -> Result<MmcDevice, MmcError> {
+    let dev_dir = if sysfs_root == Path::new("/") {
+        PathBuf::from(format!("/sys/bus/mmc/devices/{}", dev_name))
+    } else {
+        sysfs_root.join(format!("sys/bus/mmc/devices/{}", dev_name))
+    };
+
+    if !dev_dir.exists() {
+        return Err(MmcError::Io(format!(
+            "MMC device directory does not exist: {}",
+            dev_dir.display()
+        )));
+    }
+
+    let card_type = read_trimmed_attr(&dev_dir.join("type")).unwrap_or_default();
+    if card_type != "MMC" && card_type != "SD" {
+        return Err(MmcError::NotSupported(format!(
+            "Device {} has unsupported type: {}",
+            dev_name, card_type
+        )));
+    }
+
+    let model = read_trimmed_attr(&dev_dir.join("name")).unwrap_or_default();
+    let manfid_raw = read_trimmed_attr(&dev_dir.join("manfid")).unwrap_or_default();
+    let manufacturer = if !manfid_raw.is_empty() {
+        format_manufacturer(&manfid_raw)
+    } else {
+        String::new()
+    };
+    let serial = read_trimmed_attr(&dev_dir.join("serial")).unwrap_or_default();
+    let firmware = read_trimmed_attr(&dev_dir.join("fwrev"))
+        .or_else(|| read_trimmed_attr(&dev_dir.join("prv")))
+        .or_else(|| read_trimmed_attr(&dev_dir.join("hwrev")))
+        .unwrap_or_default();
+
+    let (life_a, life_b) = read_trimmed_attr(&dev_dir.join("life_time"))
+        .map(|s| parse_life_time_str(&s))
+        .unwrap_or((None, None));
+    let pre_eol = read_trimmed_attr(&dev_dir.join("pre_eol_info"))
+        .map(|s| parse_pre_eol_info_str(&s))
+        .unwrap_or(0);
+    let warning_flags = generate_warning_flags(pre_eol, life_a, life_b);
+
+    let health = MmcHealth {
+        pre_eol_info: pre_eol,
+        life_time_est_a_percent: life_a,
+        life_time_est_b_percent: life_b,
+        warning_flags,
+    };
+
+    let (block_name, total_bytes) = find_block_device(&dev_dir, sysfs_root);
+    let block_path = if !block_name.is_empty() {
+        format!("/dev/{}", block_name)
+    } else {
+        String::new()
+    };
+
+    Ok(MmcDevice {
+        name: dev_name.to_string(),
+        block_path,
+        card_type,
+        model,
+        manufacturer,
+        serial,
+        firmware,
+        total_bytes,
+        health,
+    })
+}
+
+fn find_block_device(dev_dir: &Path, sysfs_root: &Path) -> (String, u64) {
+    let block_dir = dev_dir.join("block");
+    if block_dir.exists() {
+        if let Ok(entries) = fs::read_dir(&block_dir) {
+            for entry in entries.flatten() {
+                let name = entry.file_name().to_string_lossy().to_string();
+                if name.starts_with("mmcblk") && !name.contains("boot") && !name.contains("rpmb") {
+                    let size = read_block_device_size(sysfs_root, &name);
+                    return (name, size);
+                }
+            }
+        }
+    }
+    (String::new(), 0)
+}
