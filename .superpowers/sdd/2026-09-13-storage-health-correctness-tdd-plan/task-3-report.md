# Task 3 Report: MMC 寿命边界、卡类型 guard 与 fallback

## 任务概述

本任务实现存储健康正确性 TDD 计划的 Task 3：
- 修正 eMMC 磨损寿命边界判断：从 `>= 100` 改为 `> 100`（100% 对应 `0x0A` 为 Warning，101% 对应 `0x0B` 为 Critical）。
- 在 `MmcError` 中引入 `IoCode(i32)` 保留 errno，同时保留 `Io(String)`。
- 引入可注入 EXT_CSD reader 的 `read_device_sysfs_with` 接口，默认包装函数在 `sysfs_root != "/"` 时拒绝设备读取。
- 严格限制 EXT_CSD fallback 仅在 `card_type == "MMC"`、全部健康字段在 sysfs 中缺失/无效且主块设备存在时才执行。SD 卡与 SDIO 绝不执行 CMD8 ioctl。
- 使用 `SafeFd` RAII 结构确保在 `O_RDONLY | O_CLOEXEC` 下安全打开并在所有路径关闭文件描述符，失败时立即捕获 errno。
- 支持 EXT_CSD 规范版本检查：`rev < 7` 不解析健康指标；`rev >= 7` 正常解析健康指标，且 pre-EOL 非 1/2/3 规范化为 0，保留寿命字节解析为 `None`。

---

## 阶段执行记录与 TDD 过程

### 1. 第一组 RED：`mmc_life_0a_is_not_exceeded_but_0b_is`

- **命令：** `cargo test -p rsetup-core mmc_life_0a --locked`
- **退出码：** `101`
- **失败信息：**
  ```text
  running 1 test
  test mmc::tests::mmc_life_0a_is_not_exceeded_but_0b_is ... FAILED

  failures:

  ---- mmc::tests::mmc_life_0a_is_not_exceeded_but_0b_is stdout ----

  thread 'mmc::tests::mmc_life_0a_is_not_exceeded_but_0b_is' (289) panicked at crates/rsetup-core/src/mmc.rs:317:13:
  assertion `left == right` failed
    left: true
   right: false
  ```
  在 `raw == 0x0A` 时，原实现中 `generate_warning_flags` 对 `Some(100)` 生成了 `life_time_typ_a_exceeded`（因为 `>= 100`），导致断言 `flags.iter().any(...) == (raw == 0x0B)` 失败。

### 2. 第一组 GREEN：修正 `generate_warning_flags`

- **修改：**
  在 `crates/rsetup-core/src/mmc.rs` 中，将 `life_a.map(|v| v >= 100)` 和 `life_b.map(|v| v >= 100)` 改为 `> 100`。
  同步修改 `test_generate_warning_flags` 中对 `Some(100)` 不应生成 `exceeded` flag 的断言，保留覆盖。
- **命令：** `cargo test -p rsetup-core mmc_life_0a --locked`
- **退出码：** `0`
- **输出：**
  ```text
  running 1 test
  test mmc::tests::mmc_life_0a_is_not_exceeded_but_0b_is ... ok

  test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 131 filtered out; finished in 0.00s
  ```

### 3. 第二组 RED：`sd_never_calls_ext_csd_reader`

- **测试设计：**
  使用临时目录创建 `SD` 类型的卡片 fixture（包含 `block/mmcblk0` 和 `sys/class/block/mmcblk0`），传入一旦被调用即 panic 的 injected reader `forbidden`。未加入 `card_type == "MMC"` 等守卫时，reader 被非法调用。
- **命令：** `cargo test -p rsetup-core sd_never_calls --locked`
- **退出码：** `101`
- **失败信息：**
  ```text
  running 1 test
  test mmc::sys::tests::sd_never_calls_ext_csd_reader ... FAILED

  failures:

  ---- mmc::sys::tests::sd_never_calls_ext_csd_reader stdout ----

  thread 'mmc::sys::tests::sd_never_calls_ext_csd_reader' (289) panicked at crates/rsetup-core/src/mmc/sys.rs:368:13:
  SD must not issue eMMC CMD8
  ```

### 4. 第二组 GREEN：实现 `read_device_sysfs_with` 与全套保护

- **实现要点：**
  1. `MmcError` 增加 `IoCode(i32)` 变体。
  2. `read_ext_csd_raw` 使用 `SafeFd` RAII 机制，打开参数为 `libc::O_RDONLY | libc::O_CLOEXEC`，失败立即捕获 `*libc::__errno_location()` 并返回 `MmcError::IoCode(errno)`。
  3. `read_device_sysfs` 当 `sysfs_root != "/"` 时委托返回 `MmcError::NotSupported("fixture requires an injected reader")`，防止误触宿主设备。
  4. `read_device_sysfs_with` 仅在 `card_type == "MMC" && life_a.is_none() && life_b.is_none() && pre_eol == 0 && !block_path.is_empty()` 时才调用 reader。
  5. 读取到 EXT_CSD 后，根据 `ext.rev >= 7` 判断是否解析健康指标；`pre_eol_info` 非 1/2/3 规范化为 0；寿命保留值解析为 `None`。
- **命令：** `cargo test -p rsetup-core sd_never_calls --locked`
- **退出码：** `0`
- **输出：**
  ```text
  running 1 test
  test mmc::sys::tests::sd_never_calls_ext_csd_reader ... ok

  test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 132 filtered out; finished in 0.00s
  ```

### 5. 补充矩阵测试：`ext_csd_invocation_counts_and_parser_matrix`

在 `crates/rsetup-core/src/mmc/sys.rs` 中增加了覆盖以下 8 种场景的矩阵测试：
1. **MMC 缺失健康字段**：调用 reader 恰好 1 次，解析 `rev=7`, `pre_eol=2`, `life_a=0x0A (100%)`, `life_b=0x0B (101%)` 成功，warning flags 包含 `pre_eol_warning` 与 `life_time_typ_b_exceeded`。
2. **MMC sysfs 存在有效 `life_time`**：调用 reader 0 次。
3. **MMC sysfs 存在有效 `pre_eol_info`**：调用 reader 0 次。
4. **SD 卡**：调用 reader 0 次。
5. **SDIO 设备**：在进入 fallback 前返回 `MmcError::NotSupported`，调用 reader 0 次。
6. **MMC 无主块设备**：调用 reader 0 次。
7. **MMC 仅包含 boot/rpmb/partition**：不视为主块设备，调用 reader 0 次。
8. **版本与保留值解析**：`rev < 7` 不解释健康字段（全为 0/None）；`rev >= 7` 下非法 pre-EOL 规范化为 0，保留寿命字节规范化为 None。
- **命令：** `cargo test -p rsetup-core ext_csd_invocation --locked`
- **退出码：** `0`

---

## 最终验证

### 1. `cargo test -p rsetup-core mmc --locked`
- **退出码：** `0`
- **结果：** `20 passed; 0 failed; 0 ignored; 0 measured; 114 filtered out`

### 2. `cargo test -p rsetup-core --locked`
- **退出码：** `0`
- **结果：** `132 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out`
