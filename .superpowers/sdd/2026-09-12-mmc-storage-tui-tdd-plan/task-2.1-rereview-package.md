# Task 2.1 Fix Round 1 Re-Review Package
Base: e341310012c30ebfc0be9bbe29e4927f05287727
Head: 9237ef6885bf24b575e8374cc5ea3b1eac670812

## Git Log
9237ef6 fix(core): filter for primary mmc block device node only

## Git Diff
diff --git a/crates/rsetup-core/src/mmc.rs b/crates/rsetup-core/src/mmc.rs
index e019477..44e505b 100644
--- a/crates/rsetup-core/src/mmc.rs
+++ b/crates/rsetup-core/src/mmc.rs
@@ -380,11 +380,48 @@ mod tests {
         assert_eq!(d1.name, "mmc1:59b4");
         assert_eq!(d1.card_type, "SD");
         assert_eq!(d1.model, "SC64G");
         assert_eq!(d1.manufacturer, "SanDisk (0x000045)");
         assert_eq!(d1.serial, "0x87654321");
         assert_eq!(d1.block_path, "/dev/mmcblk1");
         assert_eq!(d1.total_bytes, 124735488 * 512);
 
         let _ = std::fs::remove_dir_all(&root);
     }
+
+    #[test]
+    fn test_mmc_primary_block_device_selection() {
+        let root = std::env::temp_dir().join(format!("rsetup-mmc-blk-filter-{}", uuid::Uuid::new_v4()));
+        let dev_dir = root.join("sys/bus/mmc/devices/mmc0:0001");
+        std::fs::create_dir_all(&dev_dir).expect("create dev_dir");
+        std::fs::write(dev_dir.join("type"), "MMC\n").unwrap();
+        std::fs::write(dev_dir.join("name"), "TEST_MMC\n").unwrap();
+
+        let block_dir = dev_dir.join("block");
+        // Create boot0 as the ONLY file first to see if it mistakenly matches
+        std::fs::create_dir_all(block_dir.join("mmcblk0boot0")).unwrap();
+
+        let class_blk0boot0 = root.join("sys/class/block/mmcblk0boot0");
+        std::fs::create_dir_all(&class_blk0boot0).unwrap();
+        std::fs::write(class_blk0boot0.join("size"), "10\n").unwrap();
+
+        // If only boot0 is present, it shouldn't match mmcblk0
+        let dev = sys::read_device_sysfs(&root, "mmc0:0001").expect("read_device_sysfs");
+        assert_eq!(dev.block_path, "");
+
+        // Now add boot0, boot1, rpmb, p1, and mmcblk0
+        std::fs::create_dir_all(block_dir.join("mmcblk0boot1")).unwrap();
+        std::fs::create_dir_all(block_dir.join("mmcblk0rpmb")).unwrap();
+        std::fs::create_dir_all(block_dir.join("mmcblk0p1")).unwrap();
+        std::fs::create_dir_all(block_dir.join("mmcblk0")).unwrap();
+
+        let class_blk0 = root.join("sys/class/block/mmcblk0");
+        std::fs::create_dir_all(&class_blk0).unwrap();
+        std::fs::write(class_blk0.join("size"), "1000\n").unwrap();
+
+        let dev2 = sys::read_device_sysfs(&root, "mmc0:0001").expect("read_device_sysfs");
+        assert_eq!(dev2.block_path, "/dev/mmcblk0");
+        assert_eq!(dev2.total_bytes, 1000 * 512);
+
+        let _ = std::fs::remove_dir_all(&root);
+    }
 }
diff --git a/crates/rsetup-core/src/mmc/sys.rs b/crates/rsetup-core/src/mmc/sys.rs
index 38fbc27..2a1d7e1 100644
--- a/crates/rsetup-core/src/mmc/sys.rs
+++ b/crates/rsetup-core/src/mmc/sys.rs
@@ -17,20 +17,30 @@ pub fn read_block_device_size(sysfs_root: &Path, block_name: &str) -> u64 {
     };
 
     if let Ok(size_str) = fs::read_to_string(&size_file) {
         if let Ok(blocks) = size_str.trim().parse::<u64>() {
             return blocks.saturating_mul(512);
         }
     }
     0
 }
 
+/// Check if a block device name is a primary mmcblk device (e.g. "mmcblk0", "mmcblk1"),
+/// excluding partitions ("mmcblk0p1"), boot partitions ("mmcblk0boot0"), rpmb ("mmcblk0rpmb"), etc.
+pub fn is_primary_mmcblk(name: &str) -> bool {
+    if let Some(suffix) = name.strip_prefix("mmcblk") {
+        !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit())
+    } else {
+        false
+    }
+}
+
 /// Read sysfs MMC/SD device info and construct an MmcDevice.
 pub fn read_device_sysfs(sysfs_root: &Path, dev_name: &str) -> Result<MmcDevice, MmcError> {
     let dev_dir = if sysfs_root == Path::new("/") {
         PathBuf::from(format!("/sys/bus/mmc/devices/{}", dev_name))
     } else {
         sysfs_root.join(format!("sys/bus/mmc/devices/{}", dev_name))
     };
 
     if !dev_dir.exists() {
         return Err(MmcError::Io(format!(
@@ -71,34 +81,34 @@ pub fn read_device_sysfs(sysfs_root: &Path, dev_name: &str) -> Result<MmcDevice,
 
     // Determine block device
     // Check dev_dir/block or entries matching block/mmcblk*
     let mut block_name: Option<String> = None;
 
     let block_dir = dev_dir.join("block");
     if block_dir.exists() {
         if let Ok(entries) = fs::read_dir(&block_dir) {
             for entry in entries.flatten() {
                 let name = entry.file_name().to_string_lossy().to_string();
-                if name.starts_with("mmcblk") {
+                if is_primary_mmcblk(&name) {
                     block_name = Some(name);
                     break;
                 }
             }
         }
     }
 
-    // Fallback: check dev_dir entries directly for mmcblk*
+    // Fallback: check dev_dir entries directly for primary mmcblk*
     if block_name.is_none() {
         if let Ok(entries) = fs::read_dir(&dev_dir) {
             for entry in entries.flatten() {
                 let name = entry.file_name().to_string_lossy().to_string();
-                if name.starts_with("mmcblk") {
+                if is_primary_mmcblk(&name) {
                     block_name = Some(name);
                     break;
                 }
             }
         }
     }
 
     let (block_path, total_bytes) = if let Some(blk) = block_name {
         let size = read_block_device_size(sysfs_root, &blk);
         (format!("/dev/{}", blk), size)
