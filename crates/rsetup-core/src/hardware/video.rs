//! Video capture domain of the hardware module tree.
use super::{HardwareError, HardwareManager, shared::command_exists};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoStatus {
    pub collected_at: DateTime<Utc>,
    pub synthetic: bool,
    pub supported: bool,
    pub capture_available: bool,
    pub devices: Vec<VideoDevice>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VideoDevice {
    pub id: String,
    pub path: String,
    pub name: String,
    pub driver: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoFrame {
    pub device_id: String,
    pub captured_at: DateTime<Utc>,
    pub mime_type: String,
    pub base64: String,
    pub synthetic: bool,
}

impl HardwareManager {
    pub(crate) fn video_status(&self) -> Result<VideoStatus, HardwareError> {
        if self.synthetic {
            return Ok(demo_video_status());
        }
        let (devices, probe_failed) = crate::video::capture_devices(&self.root);
        let supported = !devices.is_empty();
        let capture_available = supported && command_exists("ffmpeg") && command_exists("timeout");
        Ok(VideoStatus {
            collected_at: Utc::now(),
            synthetic: false,
            supported,
            capture_available,
            devices,
            unavailable_reason: if !supported && probe_failed {
                Some("Unable to verify video capture devices. Check device permissions and driver readiness.".into())
            } else if !supported {
                Some("No video capture device was detected.".into())
            } else if !capture_available {
                Some("Install ffmpeg to capture a webcam test frame.".into())
            } else {
                None
            },
        })
    }

    pub(crate) fn capture_video_frame(&self, device_id: &str) -> Result<VideoFrame, HardwareError> {
        if self.synthetic {
            if !matches!(device_id, "video0" | "video1") {
                return Err(HardwareError::InvalidInput(format!(
                    "unknown video device {device_id}"
                )));
            }
            return Ok(demo_video_frame(device_id));
        }
        if !valid_video_id(device_id) {
            return Err(HardwareError::InvalidInput(
                "invalid video device id".into(),
            ));
        }
        let status = self.video_status()?;
        if !status.devices.iter().any(|device| device.id == device_id) {
            return Err(HardwareError::InvalidInput(format!(
                "unknown video device {device_id}"
            )));
        }
        if !status.capture_available {
            return Err(HardwareError::Unsupported(
                status
                    .unavailable_reason
                    .unwrap_or_else(|| "video capture is unavailable".into()),
            ));
        }
        let device_path = format!("/dev/{device_id}");
        let output = Command::new("timeout")
            .args([
                "--signal=KILL",
                "8",
                "ffmpeg",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "video4linux2",
                "-i",
                &device_path,
                "-frames:v",
                "1",
                "-f",
                "image2pipe",
                "-vcodec",
                "mjpeg",
                "pipe:1",
            ])
            .output()
            .map_err(|error| HardwareError::Io(format!("unable to start capture: {error}")))?;
        if !output.status.success() {
            return Err(HardwareError::Io(format!(
                "camera capture failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        if output.stdout.is_empty() || output.stdout.len() > 8 * 1024 * 1024 {
            return Err(HardwareError::Io(
                "camera returned an empty or oversized frame".into(),
            ));
        }
        Ok(VideoFrame {
            device_id: device_id.into(),
            captured_at: Utc::now(),
            mime_type: "image/jpeg".into(),
            base64: encode_base64(&output.stdout),
            synthetic: false,
        })
    }
}

fn valid_video_id(id: &str) -> bool {
    id.strip_prefix("video").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        output.push(TABLE[((value >> 18) & 0x3f) as usize] as char);
        output.push(TABLE[((value >> 12) & 0x3f) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[((value >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(value & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    output
}

fn demo_video_status() -> VideoStatus {
    VideoStatus {
        collected_at: Utc::now(),
        synthetic: true,
        supported: true,
        capture_available: true,
        devices: vec![
            VideoDevice {
                id: "video0".into(),
                path: "/dev/video0".into(),
                name: "USB 1080p Camera".into(),
                driver: Some("uvcvideo".into()),
            },
            VideoDevice {
                id: "video1".into(),
                path: "/dev/video1".into(),
                name: "RKISP Main Path".into(),
                driver: Some("rkisp".into()),
            },
        ],
        unavailable_reason: None,
    }
}

fn demo_video_frame(device_id: &str) -> VideoFrame {
    let label = if device_id == "video0" {
        "USB 1080p Camera"
    } else {
        "RKISP Main Path"
    };
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="960" height="540" viewBox="0 0 960 540"><defs><linearGradient id="g" x1="0" x2="1" y1="0" y2="1"><stop stop-color="#dfe5f4"/><stop offset="1" stop-color="#f5ead7"/></linearGradient></defs><rect width="960" height="540" fill="url(#g)"/><circle cx="480" cy="240" r="120" fill="#fff" opacity=".72"/><path d="M390 210h120l72-45v150l-72-45H390z" fill="#5b68b5"/><text x="480" y="420" text-anchor="middle" font-family="sans-serif" font-size="28" fill="#293246">{label}</text><text x="480" y="458" text-anchor="middle" font-family="monospace" font-size="16" fill="#68748b">SYNTHETIC TEST FRAME</text></svg>"##
    );
    VideoFrame {
        device_id: device_id.into(),
        captured_at: Utc::now(),
        mime_type: "image/svg+xml".into(),
        base64: encode_base64(svg.as_bytes()),
        synthetic: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_encoder_handles_padding() {
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
    }
}
