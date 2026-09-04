use crate::{ActionRun, ActionStatus, HardwareError};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};
use uuid::Uuid;

const FLASHCP: &str = "/usr/sbin/flashcp";
const FLASH_ERASE: &str = "/usr/sbin/flash_erase";
const BACKUP_DIRECTORY: &str = "/var/lib/rsetup-next/spi-backups";
const WORK_DIRECTORY: &str = "/run/rsetup-next/spi";
const RK3399_IMAGE_SIZE: u64 = 4 * 1024 * 1024;
const RK35_IMAGE_SIZE: u64 = 16 * 1024 * 1024;
const RK33_IMAGE_SIZE: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpiFlashStatus {
    pub collected_at: DateTime<Utc>,
    pub synthetic: bool,
    pub supported: bool,
    pub mutable: bool,
    pub revision: String,
    pub devices: Vec<SpiFlashDevice>,
    pub images: Vec<SpiBootImage>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpiFlashDevice {
    pub id: String,
    pub path: String,
    pub name: String,
    pub kind: String,
    pub size_bytes: u64,
    pub erase_size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpiBootImage {
    pub id: String,
    pub product_id: String,
    pub title: String,
    pub layout: String,
    pub size_bytes: u64,
    pub components: Vec<SpiBootComponent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpiBootComponent {
    pub file_name: String,
    pub source_path: String,
    pub offset_bytes: u64,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpiFlashRequest {
    pub operation: String,
    pub target_id: String,
    pub image_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpiFlashPlan {
    pub synthetic: bool,
    pub revision: String,
    pub plan_token: String,
    pub request: SpiFlashRequest,
    pub target: SpiFlashDevice,
    pub image: Option<SpiBootImage>,
    pub warnings: Vec<String>,
    pub requires_root: bool,
    pub backup_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpiFlashApplyResult {
    pub run: ActionRun,
    pub plan: SpiFlashPlan,
    pub backup_path: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct SpiFlashManager {
    root: PathBuf,
    synthetic: bool,
}

impl SpiFlashManager {
    pub(crate) fn new(synthetic: bool) -> Self {
        Self {
            root: PathBuf::from("/"),
            synthetic,
        }
    }

    #[cfg(test)]
    fn at_root(root: PathBuf) -> Self {
        Self {
            root,
            synthetic: false,
        }
    }

    pub(crate) fn status(&self) -> Result<SpiFlashStatus, HardwareError> {
        if self.synthetic {
            return Ok(demo_status());
        }
        let devices = discover_devices(&self.root)?;
        let images = discover_images(&self.root)?;
        let supported = !devices.is_empty();
        let tools_available = rooted_path(&self.root, FLASHCP).is_file()
            && rooted_path(&self.root, FLASH_ERASE).is_file();
        let mutable = supported && tools_available;
        let unavailable_reason = if !supported {
            Some("No SPI NOR MTD device was detected.".into())
        } else if !tools_available {
            Some("Install mtd-utils to write or erase SPI boot flash.".into())
        } else {
            None
        };
        Ok(SpiFlashStatus {
            collected_at: Utc::now(),
            synthetic: false,
            supported,
            mutable,
            revision: status_revision(&self.root, &devices, &images)?,
            devices,
            images,
            unavailable_reason,
        })
    }

    pub(crate) fn plan(&self, request: &SpiFlashRequest) -> Result<SpiFlashPlan, HardwareError> {
        validate_request(request)?;
        let status = self.status()?;
        if !status.supported {
            return Err(HardwareError::Unsupported(
                status
                    .unavailable_reason
                    .unwrap_or_else(|| "SPI boot flash is unavailable".into()),
            ));
        }
        if !self.synthetic && !status.mutable {
            return Err(HardwareError::Unsupported(
                status
                    .unavailable_reason
                    .unwrap_or_else(|| "SPI boot flash is read-only".into()),
            ));
        }
        let target = status
            .devices
            .iter()
            .find(|device| device.id == request.target_id)
            .cloned()
            .ok_or_else(|| {
                HardwareError::InvalidInput(format!(
                    "unknown SPI flash target {}",
                    request.target_id
                ))
            })?;
        let image = match request.operation.as_str() {
            "install" => {
                let image_id = request.image_id.as_deref().ok_or_else(|| {
                    HardwareError::InvalidInput(
                        "an installed boot image is required for install".into(),
                    )
                })?;
                let image = status
                    .images
                    .iter()
                    .find(|image| image.id == image_id)
                    .cloned()
                    .ok_or_else(|| {
                        HardwareError::InvalidInput(format!(
                            "unknown installed boot image {image_id}"
                        ))
                    })?;
                if image.size_bytes > target.size_bytes {
                    return Err(HardwareError::Conflict(format!(
                        "{} needs {} bytes but {} has {} bytes",
                        image.title, image.size_bytes, target.path, target.size_bytes
                    )));
                }
                Some(image)
            }
            "erase" => None,
            _ => unreachable!("validated operation"),
        };
        let plan_token = plan_token(&status.revision, request);
        Ok(SpiFlashPlan {
            synthetic: self.synthetic,
            revision: status.revision,
            plan_token,
            request: request.clone(),
            target,
            image,
            warnings: vec![
                "power_loss_can_make_sbc_unbootable".into(),
                "wrong_boot_image_can_make_sbc_unbootable".into(),
                "current_flash_is_backed_up_before_change".into(),
            ],
            requires_root: true,
            backup_required: true,
        })
    }

    pub(crate) fn apply_live(
        &self,
        request: &SpiFlashRequest,
        supplied_token: &str,
    ) -> Result<SpiFlashApplyResult, HardwareError> {
        if self.synthetic {
            return Err(HardwareError::RootRequired);
        }
        let plan = self.plan(request)?;
        if supplied_token.trim().is_empty() {
            return Err(HardwareError::PlanRequired);
        }
        if plan.plan_token != supplied_token {
            return Err(HardwareError::StalePlan);
        }
        let started_at = Utc::now();
        let target_path = rooted_path(&self.root, &plan.target.path);
        let backup = self.backup_target(&plan, &target_path)?;
        let operation_result = match plan.request.operation.as_str() {
            "install" => {
                let image = plan.image.as_ref().expect("install plan includes image");
                let composite = self.build_composite_image(image)?;
                let result = run_fixed_tool(
                    rooted_path(&self.root, FLASH_ERASE),
                    &[path_text(&target_path)?, "0", "0"],
                )
                .and_then(|erase_output| {
                    run_fixed_tool(
                        rooted_path(&self.root, FLASHCP),
                        &["-v", path_text(&composite)?, path_text(&target_path)?],
                    )
                    .map(|write_output| {
                        [erase_output, write_output]
                            .into_iter()
                            .filter(|value| !value.is_empty())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                })
                .and_then(|output| {
                    verify_equal_prefix(&composite, &target_path, image.size_bytes).map(|()| output)
                });
                let _ = fs::remove_file(&composite);
                result
            }
            "erase" => run_fixed_tool(
                rooted_path(&self.root, FLASH_ERASE),
                &[path_text(&target_path)?, "0", "0"],
            )
            .and_then(|output| {
                verify_erased(&target_path, plan.target.size_bytes).map(|()| output)
            }),
            _ => unreachable!("validated operation"),
        };
        let output = match operation_result {
            Ok(output) => output,
            Err(error) => {
                let rollback = run_fixed_tool(
                    rooted_path(&self.root, FLASHCP),
                    &["-v", path_text(&backup)?, path_text(&target_path)?],
                )
                .and_then(|_| verify_equal_prefix(&backup, &target_path, plan.target.size_bytes));
                return Err(match rollback {
                    Ok(()) => HardwareError::Io(format!(
                        "SPI flash operation failed and the previous image was restored: {error}"
                    )),
                    Err(rollback) => HardwareError::Io(format!(
                        "SPI flash operation failed: {error}; automatic restore also failed: {rollback}"
                    )),
                });
            }
        };
        let action_title = if plan.request.operation == "install" {
            "Install SPI boot image"
        } else {
            "Erase SPI boot flash"
        };
        let summary = if plan.request.operation == "install" {
            "SPI boot image installed, verified, and backed up."
        } else {
            "SPI boot flash erased, verified, and backed up."
        };
        Ok(SpiFlashApplyResult {
            run: ActionRun {
                id: Uuid::new_v4().to_string(),
                action_id: format!("hardware.spi-flash.{}", plan.request.operation),
                action_title: action_title.into(),
                status: ActionStatus::Succeeded,
                synthetic: false,
                summary: summary.into(),
                output: (!output.is_empty()).then_some(output),
                started_at,
                finished_at: Some(Utc::now()),
            },
            plan,
            backup_path: Some(display_path(&self.root, &backup)),
        })
    }

    fn backup_target(
        &self,
        plan: &SpiFlashPlan,
        target_path: &Path,
    ) -> Result<PathBuf, HardwareError> {
        let directory = rooted_path(&self.root, BACKUP_DIRECTORY);
        secure_directory(&directory)?;
        let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
        let path = directory.join(format!(
            "{}-{stamp}-{}-{}.bin",
            plan.target.id,
            &plan.revision[plan.revision.len().saturating_sub(12)..],
            Uuid::new_v4().simple()
        ));
        copy_exact_to_new_file(target_path, &path, plan.target.size_bytes)?;
        Ok(path)
    }

    fn build_composite_image(&self, image: &SpiBootImage) -> Result<PathBuf, HardwareError> {
        let directory = rooted_path(&self.root, WORK_DIRECTORY);
        secure_directory(&directory)?;
        let output_path = directory.join(format!("{}.img", Uuid::new_v4()));
        let mut output = new_private_file(&output_path)?;
        write_erased_bytes(&mut output, image.size_bytes)?;
        for component in &image.components {
            let source = rooted_path(&self.root, &component.source_path);
            let mut input = File::open(&source).map_err(|error| {
                HardwareError::Io(format!(
                    "unable to open trusted boot component {}: {error}",
                    component.source_path
                ))
            })?;
            output
                .seek(SeekFrom::Start(component.offset_bytes))
                .map_err(io_error)?;
            let copied = io::copy(&mut input, &mut output).map_err(io_error)?;
            if copied != component.size_bytes {
                let _ = fs::remove_file(&output_path);
                return Err(HardwareError::Io(format!(
                    "boot component {} changed while preparing the image",
                    component.file_name
                )));
            }
        }
        output.sync_all().map_err(io_error)?;
        Ok(output_path)
    }
}

fn discover_devices(root: &Path) -> Result<Vec<SpiFlashDevice>, HardwareError> {
    let directory = root.join("sys/class/mtd");
    let Ok(entries) = fs::read_dir(directory) else {
        return Ok(Vec::new());
    };
    let mut devices = Vec::new();
    for entry in entries.flatten() {
        let id = entry.file_name().to_string_lossy().into_owned();
        if !valid_mtd_id(&id) {
            continue;
        }
        let kind = read_trimmed(entry.path().join("type")).unwrap_or_default();
        if !kind.eq_ignore_ascii_case("nor") {
            continue;
        }
        let path = format!("/dev/{id}");
        if !rooted_path(root, &path).exists() {
            continue;
        }
        let Some(size_bytes) = read_u64(entry.path().join("size")) else {
            continue;
        };
        let Some(erase_size_bytes) = read_u64(entry.path().join("erasesize")) else {
            continue;
        };
        if size_bytes == 0 || erase_size_bytes == 0 {
            continue;
        }
        devices.push(SpiFlashDevice {
            id,
            path,
            name: read_trimmed(entry.path().join("name")).unwrap_or_else(|| "SPI NOR".into()),
            kind,
            size_bytes,
            erase_size_bytes,
        });
    }
    devices.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(devices)
}

fn discover_images(root: &Path) -> Result<Vec<SpiBootImage>, HardwareError> {
    let directory = root.join("usr/lib/u-boot");
    let Ok(entries) = fs::read_dir(&directory) else {
        return Ok(Vec::new());
    };
    let mut images = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let product_id = entry.file_name().to_string_lossy().into_owned();
        if !valid_asset_id(&product_id) {
            continue;
        }
        if let Some(image) = image_from_directory(root, &product_id, &entry.path())? {
            images.push(image);
        }
    }
    images.sort_by(|left, right| left.title.cmp(&right.title));
    Ok(images)
}

fn image_from_directory(
    root: &Path,
    product_id: &str,
    directory: &Path,
) -> Result<Option<SpiBootImage>, HardwareError> {
    let rk3399 = [("idbloader-spi_spl.img", 0), ("u-boot.itb", 512 * 768)];
    if components_exist(directory, &rk3399) {
        return build_image(
            root,
            product_id,
            "rockchip-rk3399",
            RK3399_IMAGE_SIZE,
            directory,
            &rk3399,
        )
        .map(Some);
    }
    let idbloader = if trusted_regular_file(&directory.join("idbloader-sd_nand.img")) {
        "idbloader-sd_nand.img"
    } else {
        "idbloader.img"
    };
    let rk35 = [(idbloader, 512 * 64), ("u-boot.itb", 512 * 16384)];
    if components_exist(directory, &rk35) {
        return build_image(
            root,
            product_id,
            "rockchip-rk35",
            RK35_IMAGE_SIZE,
            directory,
            &rk35,
        )
        .map(Some);
    }
    let rk33 = [
        ("idbloader-spi.img", 0),
        ("uboot.img", 512 * 4096),
        ("trust.img", 512 * 6144),
    ];
    if components_exist(directory, &rk33) {
        return build_image(
            root,
            product_id,
            "rockchip-rk33",
            RK33_IMAGE_SIZE,
            directory,
            &rk33,
        )
        .map(Some);
    }
    Ok(None)
}

fn build_image(
    root: &Path,
    product_id: &str,
    layout: &str,
    image_size: u64,
    directory: &Path,
    recipe: &[(&str, u64)],
) -> Result<SpiBootImage, HardwareError> {
    let mut components = Vec::new();
    for (file_name, offset_bytes) in recipe {
        let path = directory.join(file_name);
        let size_bytes = fs::metadata(&path).map_err(io_error)?.len();
        if size_bytes == 0 || offset_bytes.saturating_add(size_bytes) > image_size {
            return Err(HardwareError::Io(format!(
                "trusted boot component {} does not fit the {} layout",
                path.display(),
                layout
            )));
        }
        components.push(SpiBootComponent {
            file_name: (*file_name).into(),
            source_path: display_path(root, &path),
            offset_bytes: *offset_bytes,
            size_bytes,
        });
    }
    Ok(SpiBootImage {
        id: format!("{product_id}:{layout}"),
        product_id: product_id.into(),
        title: humanize_product_id(product_id),
        layout: layout.into(),
        size_bytes: image_size,
        components,
    })
}

fn components_exist(directory: &Path, recipe: &[(&str, u64)]) -> bool {
    recipe
        .iter()
        .all(|(file_name, _)| trusted_regular_file(&directory.join(file_name)))
}

fn trusted_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_file() && metadata.len() > 0)
}

fn validate_request(request: &SpiFlashRequest) -> Result<(), HardwareError> {
    if !matches!(request.operation.as_str(), "install" | "erase") {
        return Err(HardwareError::InvalidInput(
            "SPI flash operation must be install or erase".into(),
        ));
    }
    if !valid_mtd_id(&request.target_id) {
        return Err(HardwareError::InvalidInput(
            "invalid SPI flash target identifier".into(),
        ));
    }
    if let Some(image_id) = &request.image_id
        && (!valid_asset_id(image_id) || !image_id.contains(':'))
    {
        return Err(HardwareError::InvalidInput(
            "invalid installed boot image identifier".into(),
        ));
    }
    if request.operation == "erase" && request.image_id.is_some() {
        return Err(HardwareError::InvalidInput(
            "erase does not accept a boot image".into(),
        ));
    }
    Ok(())
}

fn valid_mtd_id(value: &str) -> bool {
    value.strip_prefix("mtd").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn valid_asset_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && !value.contains("..")
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-' | b':')
        })
}

fn status_revision(
    root: &Path,
    devices: &[SpiFlashDevice],
    images: &[SpiBootImage],
) -> Result<String, HardwareError> {
    let mut hash = StableHash::new();
    for device in devices {
        hash.update(device.id.as_bytes());
        hash.update(device.name.as_bytes());
        hash.update(&device.size_bytes.to_le_bytes());
        hash.update(&device.erase_size_bytes.to_le_bytes());
    }
    for image in images {
        hash.update(image.id.as_bytes());
        hash.update(&image.size_bytes.to_le_bytes());
        for component in &image.components {
            hash.update(component.source_path.as_bytes());
            hash.update(&component.offset_bytes.to_le_bytes());
            hash.update(&component.size_bytes.to_le_bytes());
            let path = rooted_path(root, &component.source_path);
            let mut input = File::open(&path).map_err(|error| {
                HardwareError::Io(format!(
                    "unable to fingerprint trusted boot component {}: {error}",
                    component.source_path
                ))
            })?;
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let read = input.read(&mut buffer).map_err(io_error)?;
                if read == 0 {
                    break;
                }
                hash.update(&buffer[..read]);
            }
        }
    }
    Ok(format!("spi-{:016x}", hash.finish()))
}

fn plan_token(revision: &str, request: &SpiFlashRequest) -> String {
    let mut hash = StableHash::new();
    hash.update(revision.as_bytes());
    hash.update(request.operation.as_bytes());
    hash.update(request.target_id.as_bytes());
    if let Some(image_id) = &request.image_id {
        hash.update(image_id.as_bytes());
    }
    format!("spi-plan-{:016x}", hash.finish())
}

struct StableHash(u64);

impl StableHash {
    fn new() -> Self {
        Self(0xcbf29ce484222325)
    }

    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }

    fn finish(self) -> u64 {
        self.0
    }
}

fn secure_directory(path: &Path) -> Result<(), HardwareError> {
    fs::create_dir_all(path).map_err(io_error)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io_error)
}

fn new_private_file(path: &Path) -> Result<File, HardwareError> {
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .read(true)
        .mode(0o600)
        .open(path)
        .map_err(io_error)
}

fn write_erased_bytes(file: &mut File, size: u64) -> Result<(), HardwareError> {
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let block = [0xffu8; 64 * 1024];
    let mut remaining = size;
    while remaining > 0 {
        let write = remaining.min(block.len() as u64) as usize;
        file.write_all(&block[..write]).map_err(io_error)?;
        remaining -= write as u64;
    }
    Ok(())
}

fn copy_exact_to_new_file(
    source: &Path,
    destination: &Path,
    size: u64,
) -> Result<(), HardwareError> {
    let mut input = File::open(source).map_err(io_error)?;
    let mut output = new_private_file(destination)?;
    let copied = io::copy(
        &mut std::io::Read::by_ref(&mut input).take(size),
        &mut output,
    )
    .map_err(io_error)?;
    if copied != size {
        let _ = fs::remove_file(destination);
        return Err(HardwareError::Io(format!(
            "SPI flash backup was incomplete: expected {size} bytes, copied {copied}"
        )));
    }
    output.sync_all().map_err(io_error)
}

fn run_fixed_tool(program: PathBuf, arguments: &[&str]) -> Result<String, HardwareError> {
    let output = Command::new(&program)
        .args(arguments)
        .output()
        .map_err(|error| {
            HardwareError::Io(format!("unable to start {}: {error}", program.display()))
        })?;
    if !output.status.success() {
        return Err(HardwareError::Io(format!(
            "{} failed: {}",
            program.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .trim()
    .chars()
    .take(8_000)
    .collect())
}

fn verify_equal_prefix(expected: &Path, actual: &Path, size: u64) -> Result<(), HardwareError> {
    let mut expected = File::open(expected).map_err(io_error)?;
    let mut actual = File::open(actual).map_err(io_error)?;
    let mut expected_buffer = [0u8; 64 * 1024];
    let mut actual_buffer = [0u8; 64 * 1024];
    let mut remaining = size;
    while remaining > 0 {
        let read_size = remaining.min(expected_buffer.len() as u64) as usize;
        expected
            .read_exact(&mut expected_buffer[..read_size])
            .map_err(io_error)?;
        actual
            .read_exact(&mut actual_buffer[..read_size])
            .map_err(io_error)?;
        if expected_buffer[..read_size] != actual_buffer[..read_size] {
            return Err(HardwareError::Io(
                "SPI flash read-back verification failed".into(),
            ));
        }
        remaining -= read_size as u64;
    }
    Ok(())
}

fn verify_erased(path: &Path, size: u64) -> Result<(), HardwareError> {
    let mut input = File::open(path).map_err(io_error)?;
    let mut buffer = [0u8; 64 * 1024];
    let mut remaining = size;
    while remaining > 0 {
        let read_size = remaining.min(buffer.len() as u64) as usize;
        input
            .read_exact(&mut buffer[..read_size])
            .map_err(io_error)?;
        if buffer[..read_size].iter().any(|byte| *byte != 0xff) {
            return Err(HardwareError::Io(
                "SPI flash erase verification failed".into(),
            ));
        }
        remaining -= read_size as u64;
    }
    Ok(())
}

fn path_text(path: &Path) -> Result<&str, HardwareError> {
    path.to_str()
        .ok_or_else(|| HardwareError::Io("SPI flash path is not valid UTF-8".into()))
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
}

fn read_u64(path: impl AsRef<Path>) -> Option<u64> {
    read_trimmed(path).and_then(|value| value.parse().ok())
}

fn rooted_path(root: &Path, value: &str) -> PathBuf {
    root.join(value.trim_start_matches('/'))
}

fn display_path(root: &Path, path: &Path) -> String {
    if root == Path::new("/") {
        return path.display().to_string();
    }
    path.strip_prefix(root)
        .map(|path| format!("/{}", path.display()))
        .unwrap_or_else(|_| path.display().to_string())
}

fn io_error(error: io::Error) -> HardwareError {
    HardwareError::Io(error.to_string())
}

fn humanize_product_id(value: &str) -> String {
    value
        .replace(['-', '_'], " ")
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn demo_status() -> SpiFlashStatus {
    let devices = vec![SpiFlashDevice {
        id: "mtd0".into(),
        path: "/dev/mtd0".into(),
        name: "spi-nor0".into(),
        kind: "nor".into(),
        size_bytes: RK35_IMAGE_SIZE,
        erase_size_bytes: 64 * 1024,
    }];
    let images = vec![SpiBootImage {
        id: "rock-5b-rk3588:rockchip-rk35".into(),
        product_id: "rock-5b-rk3588".into(),
        title: "ROCK 5B RK3588".into(),
        layout: "rockchip-rk35".into(),
        size_bytes: RK35_IMAGE_SIZE,
        components: vec![
            SpiBootComponent {
                file_name: "idbloader-sd_nand.img".into(),
                source_path: "/usr/lib/u-boot/rock-5b-rk3588/idbloader-sd_nand.img".into(),
                offset_bytes: 512 * 64,
                size_bytes: 512 * 1024,
            },
            SpiBootComponent {
                file_name: "u-boot.itb".into(),
                source_path: "/usr/lib/u-boot/rock-5b-rk3588/u-boot.itb".into(),
                offset_bytes: 512 * 16384,
                size_bytes: 2 * 1024 * 1024,
            },
        ],
    }];
    SpiFlashStatus {
        collected_at: Utc::now(),
        synthetic: true,
        supported: true,
        mutable: true,
        revision: status_revision_without_files(&devices, &images),
        devices,
        images,
        unavailable_reason: None,
    }
}

fn status_revision_without_files(devices: &[SpiFlashDevice], images: &[SpiBootImage]) -> String {
    let mut hash = StableHash::new();
    for device in devices {
        hash.update(device.id.as_bytes());
        hash.update(&device.size_bytes.to_le_bytes());
    }
    for image in images {
        hash.update(image.id.as_bytes());
        hash.update(&image.size_bytes.to_le_bytes());
    }
    format!("spi-{:016x}", hash.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root() -> PathBuf {
        std::env::temp_dir().join(format!("rsetup-spi-{}", Uuid::new_v4()))
    }

    fn write_fixture(path: &Path, size: usize, byte: u8) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![byte; size]).unwrap();
    }

    fn add_mtd(root: &Path, id: &str, kind: &str, size: u64) {
        let sysfs = root.join("sys/class/mtd").join(id);
        fs::create_dir_all(&sysfs).unwrap();
        fs::write(sysfs.join("type"), format!("{kind}\n")).unwrap();
        fs::write(sysfs.join("name"), "spi-nor0\n").unwrap();
        fs::write(sysfs.join("size"), format!("{size}\n")).unwrap();
        fs::write(sysfs.join("erasesize"), "65536\n").unwrap();
        write_fixture(&root.join("dev").join(id), size as usize, 0xaa);
    }

    #[test]
    fn discovers_only_nor_mtd_devices() {
        let root = fixture_root();
        add_mtd(&root, "mtd0", "nor", RK35_IMAGE_SIZE);
        add_mtd(&root, "mtd1", "nand", RK35_IMAGE_SIZE);
        let devices = discover_devices(&root).unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "mtd0");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovers_official_rockchip_layouts() {
        let root = fixture_root();
        let uboot = root.join("usr/lib/u-boot");
        write_fixture(&uboot.join("rk3399/idbloader-spi_spl.img"), 32, 0x11);
        write_fixture(&uboot.join("rk3399/u-boot.itb"), 64, 0x12);
        write_fixture(&uboot.join("rk3588/idbloader-sd_nand.img"), 32, 0x21);
        write_fixture(&uboot.join("rk3588/u-boot.itb"), 64, 0x22);
        write_fixture(&uboot.join("rk3328/idbloader-spi.img"), 32, 0x31);
        write_fixture(&uboot.join("rk3328/uboot.img"), 64, 0x32);
        write_fixture(&uboot.join("rk3328/trust.img"), 64, 0x33);
        let images = discover_images(&root).unwrap();
        assert_eq!(images.len(), 3);
        assert!(images.iter().any(|image| image.layout == "rockchip-rk3399"));
        assert!(images.iter().any(|image| image.layout == "rockchip-rk35"));
        assert!(images.iter().any(|image| image.layout == "rockchip-rk33"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ignores_symlinked_boot_components() {
        let root = fixture_root();
        let directory = root.join("usr/lib/u-boot/rk3588");
        let outside = root.join("untrusted-idbloader.img");
        write_fixture(&outside, 32, 0x21);
        fs::create_dir_all(&directory).unwrap();
        std::os::unix::fs::symlink(&outside, directory.join("idbloader-sd_nand.img")).unwrap();
        write_fixture(&directory.join("u-boot.itb"), 64, 0x22);

        assert!(discover_images(&root).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn builds_erased_composite_at_exact_offsets() {
        let root = fixture_root();
        let directory = root.join("usr/lib/u-boot/rk3588");
        write_fixture(&directory.join("idbloader-sd_nand.img"), 32, 0x41);
        write_fixture(&directory.join("u-boot.itb"), 64, 0x42);
        let manager = SpiFlashManager::at_root(root.clone());
        let image = discover_images(&root).unwrap().remove(0);
        let path = manager.build_composite_image(&image).unwrap();
        let bytes = fs::read(path).unwrap();
        assert_eq!(&bytes[512 * 64..512 * 64 + 32], &[0x41; 32]);
        assert_eq!(&bytes[512 * 16384..512 * 16384 + 64], &[0x42; 64]);
        assert_eq!(bytes[0], 0xff);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn plans_bind_target_image_and_revision() {
        let manager = SpiFlashManager::new(true);
        let request = SpiFlashRequest {
            operation: "install".into(),
            target_id: "mtd0".into(),
            image_id: Some("rock-5b-rk3588:rockchip-rk35".into()),
        };
        let plan = manager.plan(&request).unwrap();
        assert!(plan.backup_required);
        assert_ne!(
            plan.plan_token,
            plan_token(
                &plan.revision,
                &SpiFlashRequest {
                    operation: "erase".into(),
                    target_id: "mtd0".into(),
                    image_id: None,
                }
            )
        );
    }

    #[test]
    fn rejects_paths_and_invalid_operation_combinations() {
        assert!(!valid_mtd_id("../mtd0"));
        assert!(!valid_asset_id("../boot"));
        assert!(
            validate_request(&SpiFlashRequest {
                operation: "erase".into(),
                target_id: "mtd0".into(),
                image_id: Some("board:rockchip-rk35".into()),
            })
            .is_err()
        );
    }
}
