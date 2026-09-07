//! Capture-node discovery shared by hardware tools and overview capabilities.
use crate::hardware::VideoDevice;
use std::{fs, io, path::Path};

const CAPTURE: u32 = 0x0000_0001;
const CAPTURE_MPLANE: u32 = 0x0000_1000;
const M2M: u32 = 0x0000_8000;
const M2M_MPLANE: u32 = 0x0000_4000;
const DEVICE_CAPS: u32 = 0x8000_0000;

// Linux UAPI struct v4l2_capability; contains no pointers or architecture-sized fields.
// https://docs.kernel.org/userspace-api/media/v4l/vidioc-querycap.html
#[repr(C)]
#[derive(Default)]
struct V4l2Capability {
    driver: [u8; 16],
    card: [u8; 32],
    bus_info: [u8; 32],
    version: u32,
    capabilities: u32,
    device_caps: u32,
    reserved: [u32; 3],
}

impl V4l2Capability {
    fn is_capture(&self) -> bool {
        let caps = if self.capabilities & DEVICE_CAPS != 0 {
            self.device_caps
        } else {
            self.capabilities
        };
        caps & (CAPTURE | CAPTURE_MPLANE) != 0 && caps & (M2M | M2M_MPLANE) == 0
    }
}

pub(crate) fn capture_devices(root: &Path) -> (Vec<VideoDevice>, bool) {
    capture_devices_with(root, query_capabilities)
}

fn capture_devices_with(
    root: &Path,
    query: impl Fn(&Path) -> io::Result<V4l2Capability>,
) -> (Vec<VideoDevice>, bool) {
    let mut devices = Vec::new();
    let mut probe_failed = false;
    let entries = match fs::read_dir(root.join("sys/class/video4linux")) {
        Ok(entries) => entries,
        Err(error) => return (devices, error.kind() != io::ErrorKind::NotFound),
    };
    for entry in entries {
        let Ok(entry) = entry else {
            probe_failed = true;
            continue;
        };
        let id = entry.file_name().to_string_lossy().into_owned();
        if !id.strip_prefix("video").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            continue;
        }
        let caps = match query(&root.join("dev").join(&id)) {
            Ok(caps) => caps,
            Err(_) => {
                // An inaccessible or uninitialised node is not a proven camera.
                probe_failed = true;
                continue;
            }
        };
        if !caps.is_capture() {
            continue;
        }
        let name = nul_string(&caps.card);
        let driver = nul_string(&caps.driver);
        devices.push(VideoDevice {
            path: format!("/dev/{id}"),
            name: if name.is_empty() { id.clone() } else { name },
            driver: (!driver.is_empty()).then_some(driver),
            id,
        });
    }
    devices.sort_by(|left, right| left.id.cmp(&right.id));
    (devices, probe_failed)
}

fn nul_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes.split(|byte| *byte == 0).next().unwrap_or_default()).into_owned()
}

#[cfg(target_os = "linux")]
fn query_capabilities(path: &Path) -> io::Result<V4l2Capability> {
    use std::os::{
        fd::AsRawFd,
        unix::fs::{FileTypeExt, OpenOptionsExt},
    };
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.file_type().is_char_device() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a character device",
        ));
    }
    let mut caps = V4l2Capability::default();
    // SAFETY: the fd is live and the writable, C-layout buffer matches the UAPI.
    // QUERYCAP is read-only: no format, buffer or streaming ioctl is issued.
    let result = unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            libc::_IOR::<V4l2Capability>(b'V' as u32, 0),
            &mut caps,
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(caps)
    }
}

#[cfg(not(target_os = "linux"))]
fn query_capabilities(_path: &Path) -> io::Result<V4l2Capability> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "V4L2 requires Linux",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn capability_abi_and_per_node_flags_are_respected() {
        assert_eq!(std::mem::size_of::<V4l2Capability>(), 104);
        for flags in [CAPTURE, CAPTURE_MPLANE] {
            assert!(
                V4l2Capability {
                    capabilities: flags,
                    ..Default::default()
                }
                .is_capture()
            );
            assert!(
                V4l2Capability {
                    capabilities: DEVICE_CAPS,
                    device_caps: flags,
                    ..Default::default()
                }
                .is_capture()
            );
        }
        for flags in [
            0,
            2,
            0x0080_0000,
            M2M,
            M2M_MPLANE,
            CAPTURE | M2M,
            CAPTURE_MPLANE | M2M_MPLANE,
        ] {
            assert!(
                !V4l2Capability {
                    capabilities: flags,
                    ..Default::default()
                }
                .is_capture()
            );
            // A capture capability on a sibling node must not admit a metadata/codec node.
            assert!(
                !V4l2Capability {
                    capabilities: DEVICE_CAPS | CAPTURE,
                    device_caps: flags,
                    ..Default::default()
                }
                .is_capture()
            );
        }
    }

    #[test]
    fn discovers_cameras_at_any_index_and_excludes_codecs_and_unverified_nodes() {
        let root = std::env::temp_dir().join(format!("rsetup-video-{}", Uuid::new_v4()));
        for id in [
            "video0",
            "video1",
            "video2",
            "video3",
            "video7",
            "v4l-subdev0",
        ] {
            fs::create_dir_all(root.join("sys/class/video4linux").join(id)).unwrap();
        }
        let (devices, failed) = capture_devices_with(&root, |path| {
            let device_caps = match path.file_name().unwrap().to_str().unwrap() {
                "video0" => M2M_MPLANE,
                "video1" => M2M,
                "video2" => 0x0080_0000, // metadata sibling of a camera
                "video3" => return Err(io::ErrorKind::PermissionDenied.into()),
                "video7" => CAPTURE,
                _ => panic!("non-video node must not be queried"),
            };
            Ok(V4l2Capability {
                capabilities: DEVICE_CAPS | CAPTURE,
                device_caps,
                ..Default::default()
            })
        });
        assert!(failed);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "video7");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_video_subsystem_is_not_a_probe_error() {
        let root = std::env::temp_dir().join(format!("rsetup-no-video-{}", Uuid::new_v4()));
        let (devices, failed) = capture_devices(&root);
        assert!(devices.is_empty());
        assert!(!failed);
    }
}
