//! The `.xz` container checks made while planning: the CRC-32 every xz frame
//! carries, the stream header and footer magics, and the backwards walk from
//! the end of the file that adds up the uncompressed size of every stream.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use super::error::SystemInstallError;

/// CRC-32 as xz uses it: reflected, polynomial 0xEDB88320, initial and final xor.
/// The stream header, the index and the stream footer each carry one, so a
/// damaged file is refused while planning instead of being written as an image.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

pub(super) const XZ_MAGIC: [u8; 6] = [0xFD, b'7', b'z', b'X', b'Z', 0x00];
const XZ_FOOTER_MAGIC: [u8; 2] = [0x59, 0x5A];

/// Sums the uncompressed size of **every** stream in an `.xz` file, so a
/// concatenated multi-stream image is measured in full instead of only its last
/// stream (which is what silently truncated one). Streams are walked backwards
/// from the end of the file:
///
/// * a stream footer gives the size of its own index (the stored value is
///   `index size / 4 - 1`, and that size includes the index CRC32),
/// * the index records give every block's unpadded size and its uncompressed
///   size, so the block area plus the two 12-byte frames is the stream's total
///   size,
/// * subtracting that total gives the previous stream, skipping the stream
///   padding (null bytes, a multiple of four) that may sit in between,
/// * the stream header magic must sit exactly where the arithmetic says, so a
///   mis-walk is an error instead of a guessed size.
pub(super) fn read_xz_size(file: &mut File) -> Result<u64, SystemInstallError> {
    let length = file
        .metadata()
        .map_err(|error| SystemInstallError::Io(error.to_string()))?
        .len();
    if length < 32 {
        return Err(SystemInstallError::InvalidInput(
            "the .xz image is too short to hold a stream".into(),
        ));
    }
    let implausible = |detail: &str| SystemInstallError::InvalidInput(detail.to_owned());
    let overflow = || SystemInstallError::InvalidInput("the .xz size overflows".into());
    let mut end = length;
    let mut total = 0u64;
    let mut streams = 0usize;
    while end > 0 {
        let padding = trailing_null_bytes(file, end)?;
        if padding % 4 != 0 {
            return Err(implausible(
                "the .xz stream padding is not a multiple of four",
            ));
        }
        end -= padding;
        if end == 0 {
            break;
        }
        if end < 32 {
            return Err(implausible("the .xz image is too short to hold a stream"));
        }
        let mut footer = [0u8; 12];
        file.seek(SeekFrom::Start(end - 12))
            .and_then(|_| file.read_exact(&mut footer))
            .map_err(|error| SystemInstallError::Io(error.to_string()))?;
        if footer[10..12] != XZ_FOOTER_MAGIC {
            return Err(implausible("the .xz stream footer is malformed"));
        }
        if crc32(&footer[4..10]) != u32::from_le_bytes(footer[0..4].try_into().unwrap_or_default())
        {
            return Err(implausible("the .xz stream footer checksum is wrong"));
        }
        let backward_size = (u64::from(u32::from_le_bytes(
            footer[4..8].try_into().unwrap_or_default(),
        )) + 1)
            * 4;
        if backward_size + 12 > end {
            return Err(implausible("the .xz index size is implausible"));
        }
        // The index sits before the 12-byte stream footer, not at the file end.
        let index_start = end - 12 - backward_size;
        let mut index = vec![0u8; backward_size as usize];
        file.seek(SeekFrom::Start(index_start))
            .and_then(|_| file.read_exact(&mut index))
            .map_err(|error| SystemInstallError::Io(error.to_string()))?;
        if index.first() != Some(&0x00) {
            return Err(implausible("the .xz index is malformed"));
        }
        if index.len() < 4 {
            return Err(implausible("the .xz index is too short"));
        }
        let split = index.len() - 4;
        if crc32(&index[..split])
            != u32::from_le_bytes(index[split..].try_into().unwrap_or_default())
        {
            return Err(implausible("the .xz index checksum is wrong"));
        }
        let mut cursor = 1usize;
        let (records, used) = read_xz_vli(&index[cursor..])?;
        cursor += used;
        let mut blocks_bytes = 0u64;
        let mut stream_bytes = 0u64;
        for _ in 0..records {
            let (unpadded, used) = read_xz_vli(&index[cursor..])?;
            cursor += used;
            let (uncompressed, used) = read_xz_vli(&index[cursor..])?;
            cursor += used;
            // An index record stores the block size *without* the block padding,
            // while the file holds the padding too: each block is rounded up to a
            // multiple of four bytes, and a block header is always a multiple of
            // four, so rounding the unpadded size is enough.
            let padded = unpadded + (4 - unpadded % 4) % 4;
            blocks_bytes = blocks_bytes.checked_add(padded).ok_or_else(overflow)?;
            stream_bytes = stream_bytes
                .checked_add(uncompressed)
                .ok_or_else(overflow)?;
        }
        // Stream header (12) + blocks (their headers and compressed data) + index
        // (which already counts its own CRC32) + stream footer (12).
        let stream_length = 12u64
            .checked_add(blocks_bytes)
            .ok_or_else(overflow)?
            .checked_add(backward_size)
            .ok_or_else(overflow)?
            .checked_add(12)
            .ok_or_else(overflow)?;
        if stream_length > end {
            return Err(implausible("the .xz stream size is implausible"));
        }
        let start = end - stream_length;
        let mut header = [0u8; 12];
        file.seek(SeekFrom::Start(start))
            .and_then(|_| file.read_exact(&mut header))
            .map_err(|error| SystemInstallError::Io(error.to_string()))?;
        if header[..6] != XZ_MAGIC {
            return Err(implausible("the .xz stream header is malformed"));
        }
        // Stream flags: the first byte is reserved and must be zero.
        if header[6] != 0 {
            return Err(implausible("the .xz stream flags are malformed"));
        }
        if crc32(&header[6..8]) != u32::from_le_bytes(header[8..12].try_into().unwrap_or_default())
        {
            return Err(implausible("the .xz stream header checksum is wrong"));
        }
        total = total.checked_add(stream_bytes).ok_or_else(overflow)?;
        streams += 1;
        end = start;
    }
    if streams == 0 {
        return Err(implausible("the image is not an xz stream"));
    }
    if total == 0 {
        return Err(implausible("the .xz image holds no data"));
    }
    Ok(total)
}

/// Counts the null bytes that end `[0, end)`. Stream padding sits between streams
/// and may also trail the file; a valid stream never ends with a null byte (its
/// footer ends with `YZ`), so everything skipped here is padding.
fn trailing_null_bytes(file: &mut File, end: u64) -> Result<u64, SystemInstallError> {
    /// Padding only ever aligns a stream, so anything beyond this is a malformed
    /// file rather than padding worth scanning.
    const MAX_PADDING: u64 = 4 * 1024 * 1024;
    const STEP: u64 = 64;
    let mut count = 0u64;
    let mut cursor = end;
    while cursor > 0 && count < MAX_PADDING {
        let size = STEP.min(cursor);
        let mut buffer = vec![0u8; size as usize];
        file.seek(SeekFrom::Start(cursor - size))
            .and_then(|_| file.read_exact(&mut buffer))
            .map_err(|error| SystemInstallError::Io(error.to_string()))?;
        let trailing = buffer.iter().rev().take_while(|byte| **byte == 0).count() as u64;
        count += trailing;
        if trailing < size {
            break;
        }
        cursor -= size;
    }
    if count >= MAX_PADDING {
        return Err(SystemInstallError::InvalidInput(
            "the .xz stream padding is implausibly long".into(),
        ));
    }
    Ok(count)
}

/// xz stores variable-length integers as little-endian base-128.
fn read_xz_vli(bytes: &[u8]) -> Result<(u64, usize), SystemInstallError> {
    let mut value = 0u64;
    let mut shift = 0u32;
    for (index, byte) in bytes.iter().enumerate() {
        if shift > 63 {
            return Err(SystemInstallError::InvalidInput(
                "the .xz variable length integer is too large".into(),
            ));
        }
        value |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Ok((value, index + 1));
        }
        shift += 7;
    }
    Err(SystemInstallError::InvalidInput(
        "the .xz variable length integer is truncated".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InstallImageFormat, SystemInstallImage};
    use crate::system_install::test_support::*;
    use crate::system_install::{ImageOpener, SystemInstallManager};
    use std::fs;
    use std::sync::Arc;

    /// The frames xz itself checks — stream header, index, stream footer — are
    /// checked while planning, so a damaged file never reaches the write path.
    /// The offsets below are the frame bytes of the embedded fixtures (the header
    /// CRC lives at 8, the index CRC at length-16, the footer CRC at length-12).
    #[test]
    fn xz_frame_checksums_are_checked_while_planning() {
        let harness = fixture("xz-crc", boot_mounts(), RereadBehavior::Matches);
        let cases: Vec<(&str, usize)> = vec![
            ("header crc", 8),
            ("header flags", 7),
            ("index crc", XZ_FIXTURE.len() - 16),
            ("footer crc", XZ_FIXTURE.len() - 12),
        ];
        for (label, offset) in cases {
            let path = harness.root.join(format!("{label}.img.xz"));
            write_file(&path, flip(XZ_FIXTURE, offset));
            match harness.manager.plan(&install_request(&path)) {
                Err(SystemInstallError::InvalidInput(detail)) => {
                    assert!(
                        detail.contains("checksum") || detail.contains("flags"),
                        "{label}: {detail}"
                    );
                }
                other => panic!("{label} must be refused while planning: {other:?}"),
            }
        }
        // The same file with intact frames still plans, so the refusals above are
        // about the damaged bytes.
        let intact = harness.root.join("intact.img.xz");
        write_file(&intact, XZ_FIXTURE);
        harness
            .manager
            .plan(&install_request(&intact))
            .expect("the intact fixture plans");
    }

    /// The CRC-32 that xz frames are checked with.
    #[test]
    fn crc32_matches_the_reference_vectors() {
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    /// Every fixture's uncompressed size is compared against the reference
    /// implementation, not against this module's own arithmetic:
    ///
    /// ```text
    /// fixture                                     streams        xz -dc | wc -c
    /// XZ_FIXTURE                                  1                     512
    /// XZ_FIXTURE + XZ_FIXTURE                     2                     1024
    /// XZ_FIXTURE + 0x00 * 4 + XZ_FIXTURE          2 (stream padding)    1024
    /// XZ_FIXTURE + 0x00 * 4 + XZ_FIXTURE * 2      3 (stream padding)    1536
    /// XZ_FIXTURE + 0x00 * 4                       1 (trailing padding)  512
    /// XZ_MULTI_BLOCK_FIXTURE                      1 (4 blocks)        1024
    /// ```
    #[test]
    fn xz_uncompressed_size_adds_up_every_stream() {
        let harness = fixture("xz-streams", boot_mounts(), RereadBehavior::Matches);
        let padding = [0u8; 4];
        let cases: Vec<(&str, Vec<u8>, u64)> = vec![
            ("single.img.xz", XZ_FIXTURE.to_vec(), XZ_FIXTURE_BYTES),
            (
                "two.img.xz",
                [XZ_FIXTURE, XZ_FIXTURE].concat(),
                XZ_FIXTURE_BYTES * 2,
            ),
            (
                "padded.img.xz",
                [XZ_FIXTURE, &padding, XZ_FIXTURE].concat(),
                XZ_FIXTURE_BYTES * 2,
            ),
            (
                "three.img.xz",
                [XZ_FIXTURE, &padding, XZ_FIXTURE, XZ_FIXTURE].concat(),
                XZ_FIXTURE_BYTES * 3,
            ),
            (
                "trailing.img.xz",
                [XZ_FIXTURE, &padding].concat(),
                XZ_FIXTURE_BYTES,
            ),
            ("multi-block.img.xz", XZ_MULTI_BLOCK_FIXTURE.to_vec(), 1024),
        ];
        for (name, bytes, expected) in cases {
            let path = harness.root.join(name);
            write_file(&path, &bytes);
            let plan = harness
                .manager
                .plan(&install_request(&path))
                .unwrap_or_else(|error| panic!("{name} must be planned: {error}"));
            assert_eq!(plan.image.format, InstallImageFormat::Xz, "{name}");
            assert_eq!(plan.image.size_bytes, bytes.len() as u64, "{name}");
            assert_eq!(
                plan.image.uncompressed_bytes, expected,
                "{name}: the uncompressed size must add up every stream"
            );
        }
    }

    /// Stream padding is a multiple of four; the reference implementation refuses
    /// the same file (`xz -dc` exits 1 on it), so refusing it here agrees with it
    /// instead of inventing a size.
    #[test]
    fn xz_padding_that_is_not_a_multiple_of_four_is_rejected() {
        let harness = fixture("xz-padding", boot_mounts(), RereadBehavior::Matches);
        let path = harness.root.join("bad-padding.img.xz");
        write_file(&path, [XZ_FIXTURE, XZ_FIXTURE, &[0u8; 2]].concat());
        match harness.manager.plan(&install_request(&path)) {
            Err(SystemInstallError::InvalidInput(detail)) => {
                assert!(detail.contains("padding"), "{detail}");
            }
            other => panic!("two padding bytes must be refused: {other:?}"),
        }
    }

    /// The reproduction from the finding, end to end: two concatenated streams
    /// plan as the sum of both and are written in full. A last-stream-only size
    /// would have written half the image and reported success.
    #[test]
    fn a_concatenated_image_is_planned_and_written_in_full() {
        struct TotalOpener(u64);
        impl ImageOpener for TotalOpener {
            fn open(
                &self,
                _image: &SystemInstallImage,
            ) -> Result<Box<dyn Read + Send>, SystemInstallError> {
                Ok(Box::new(std::io::Cursor::new(mbr_bytes(self.0 as usize))))
            }
        }
        let total = XZ_FIXTURE_BYTES * 2;
        let harness = fixture(
            "xz-two-streams-write",
            boot_mounts(),
            RereadBehavior::Unsupported,
        );
        let xz = harness.root.join("two-stream.img.xz");
        write_file(&xz, [XZ_FIXTURE, XZ_FIXTURE].concat());
        let manager = SystemInstallManager::with_backend(
            harness.backend.clone(),
            Arc::new(TotalOpener(total)),
            false,
        );
        let request = install_request(&xz);
        let plan = manager.plan(&request).expect("plan");
        assert_eq!(
            plan.image.uncompressed_bytes, total,
            "both streams are counted, so the plan describes the whole image"
        );
        let result = manager
            .apply_live(&request, &plan.plan_token, "sda")
            .expect("write the concatenated image");
        assert_eq!(result.bytes_written, total);
        let device = fs::read(&harness.device).expect("read the fixture device");
        assert_eq!(
            &device[510..512],
            &[0x55, 0xAA],
            "both streams reached the device"
        );
    }
}
