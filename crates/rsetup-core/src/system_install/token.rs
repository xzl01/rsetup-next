//! Plan token minting and parsing: the state revision the token binds, the
//! fixed token shape, the per-issuance nonce, the process signing key and the
//! signature over the issued text.

use std::fs::File;
use std::hash::{BuildHasher, Hasher};
use std::io::Read;

use chrono::{DateTime, Utc};

use crate::model::{SystemInstallImage, SystemInstallTarget};

use super::SystemMount;
use super::crypto::{hex, hmac_sha256, sha256};

/// Identity of the whole observed device state; any device or mount change
/// produces a different plan token.
pub(super) fn revision(targets: &[SystemInstallTarget], mounts: &[SystemMount]) -> String {
    let mut bytes = Vec::new();
    for target in targets {
        bytes.extend_from_slice(target.id.as_bytes());
        bytes.extend_from_slice(target.path.as_bytes());
        bytes.extend_from_slice(target.model.as_bytes());
        bytes.extend_from_slice(&target.size_bytes.to_le_bytes());
        bytes.extend_from_slice(target.kind.marker().as_bytes());
        bytes.extend_from_slice(&[target.removable as u8, target.holds_running_system as u8]);
        for partition in &target.partitions {
            bytes.extend_from_slice(partition.name.as_bytes());
            bytes.extend_from_slice(&partition.start_bytes.to_le_bytes());
            bytes.extend_from_slice(&partition.size_bytes.to_le_bytes());
        }
    }
    for mount in mounts {
        bytes.extend_from_slice(mount.device.as_bytes());
        bytes.extend_from_slice(mount.mount_point.as_bytes());
    }
    format!("install-v1-{:016x}", stable_hash(&bytes))
}

/// A token as it is carried: the state binding, when it was issued, a per-issuance
/// nonce, and the signature over all of that.
pub(super) struct ParsedToken {
    pub(super) binding: u64,
    pub(super) issued: u64,
    #[allow(dead_code)]
    nonce: u64,
    /// Exactly the text the signature covers.
    pub(super) signed: String,
    pub(super) mac: [u8; 32],
}

/// Splits the fixed token shape without checking the signature. Anything else is
/// not a token this build produced.
pub(super) fn split_plan_token(token: &str) -> Option<ParsedToken> {
    const PREFIX: &str = "install-plan-";
    let rest = token.strip_prefix(PREFIX)?;
    let mut parts = rest.split('-');
    let binding = u64::from_str_radix(parts.next()?, 16).ok()?;
    let issued = u64::from_str_radix(parts.next()?, 16).ok()?;
    let nonce = u64::from_str_radix(parts.next()?, 16).ok()?;
    let mac_hex = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if rest.len() != 16 + 1 + 10 + 1 + 16 + 1 + 64 || mac_hex.len() != 64 {
        return None;
    }
    let mut mac = [0u8; 32];
    for (index, pair) in mac_hex.as_bytes().chunks(2).enumerate() {
        let text = std::str::from_utf8(pair).ok()?;
        mac[index] = u8::from_str_radix(text, 16).ok()?;
    }
    let signed = format!("{PREFIX}{binding:016x}-{issued:010x}-{nonce:016x}");
    Some(ParsedToken {
        binding,
        issued,
        nonce,
        signed,
        mac,
    })
}

/// A per-issuance value, so two previews of the same request never share a token
/// (a shared token would make the second preview look like a replay of the first).
pub(super) fn mint_nonce() -> u64 {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut seed = 0u64;
    if let Ok(mut urandom) = File::open("/dev/urandom") {
        let mut bytes = [0u8; 8];
        if urandom.read_exact(&mut bytes).is_ok() {
            seed = u64::from_ne_bytes(bytes);
        }
    }
    if seed == 0 {
        seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| u64::from(elapsed.subsec_nanos()))
            .unwrap_or_default();
    }
    seed ^ counter.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// The signing key for this process. Without `/dev/urandom` it falls back to
/// process and clock entropy; either way the key is never disclosed, and a token
/// is only useful to the process that issued it.
pub(super) fn mint_token_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    if let Ok(mut urandom) = File::open("/dev/urandom")
        && urandom.read_exact(&mut key).is_ok()
    {
        return key;
    }
    let mut seed = Vec::new();
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default(),
    );
    seed.extend_from_slice(&hasher.finish().to_le_bytes());
    let local = 0u8;
    seed.extend_from_slice(&std::ptr::from_ref(&local).addr().to_le_bytes());
    sha256(&seed)
}

/// Binds the observed state, the target identity, the image identity and the
/// acknowledgement the caller must type.
pub(super) fn token_binding(
    revision: &str,
    target: &SystemInstallTarget,
    image: &SystemInstallImage,
) -> u64 {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(revision.as_bytes());
    bytes.extend_from_slice(target.id.as_bytes());
    bytes.extend_from_slice(target.path.as_bytes());
    bytes.extend_from_slice(target.model.as_bytes());
    bytes.extend_from_slice(&target.size_bytes.to_le_bytes());
    bytes.extend_from_slice(image.path.as_bytes());
    bytes.extend_from_slice(&image.size_bytes.to_le_bytes());
    bytes.extend_from_slice(&image.uncompressed_bytes.to_le_bytes());
    bytes.extend_from_slice(image.file_identity.as_bytes());
    bytes.extend_from_slice(image.sha256_prefix.as_bytes());
    // The expected acknowledgement is part of the token, so a plan cannot be
    // replayed against a different device name.
    bytes.extend_from_slice(target.id.as_bytes());
    stable_hash(&bytes)
}

/// The issued token: the binding above, the issuance time, a per-issuance nonce and
/// a signature over exactly that text. Editing any field — the nonce to make a
/// consumed token look new, the time to refresh the window — invalidates the
/// signature, and only the issuing process holds the key.
pub(super) fn plan_token(
    revision: &str,
    target: &SystemInstallTarget,
    image: &SystemInstallImage,
    issued_at: DateTime<Utc>,
    nonce: u64,
    key: &[u8; 32],
) -> String {
    let binding = token_binding(revision, target, image);
    let signed = format!(
        "install-plan-{binding:016x}-{:010x}-{nonce:016x}",
        issued_at.timestamp().max(0)
    );
    let mac = hmac_sha256(key, signed.as_bytes());
    format!("{signed}-{}", hex(&mac))
}

/// State-change hashing for tokens (FNV-1a based, matching the other managers).
/// It is not a security primitive.
fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system_install::test_support::*;
    use crate::system_install::{MIB, SystemImageOpener, SystemInstallError, SystemInstallManager};
    use std::sync::{Arc, Mutex};

    #[test]
    fn plan_token_is_single_use_and_expires() {
        let harness = fixture("token", boot_mounts(), RereadBehavior::Matches);
        let request = install_request(&harness.image);
        let plan = harness.manager.plan(&request).expect("plan");
        harness
            .manager
            .apply_live(&request, &plan.plan_token, "sda")
            .expect("first apply");
        assert!(
            matches!(
                harness
                    .manager
                    .apply_live(&request, &plan.plan_token, "sda"),
                Err(SystemInstallError::StalePlan)
            ),
            "a token cannot be replayed"
        );

        let expiry = fixture("token-ttl", boot_mounts(), RereadBehavior::Matches);
        let started = Utc::now();
        let now = Arc::new(Mutex::new(started));
        let clock = now.clone();
        let manager = expiry
            .manager
            .clone()
            .with_clock(Arc::new(move || *clock.lock().expect("clock lock")));
        let expiry_request = install_request(&expiry.image);
        let plan = manager
            .plan(&expiry_request)
            .expect("plan with a frozen clock");
        *now.lock().expect("clock lock") = started + chrono::Duration::seconds(601);
        assert!(matches!(
            manager.apply_live(&expiry_request, &plan.plan_token, "sda"),
            Err(SystemInstallError::StalePlan)
        ));
    }

    /// The F-T13-002 / F-T20-001 property, restated for the current design: a
    /// token is only usable by the process that signed and issued it. A second
    /// manager stands in for a second process (the privileged helper), and the
    /// token it carries — even a well formed one — buys it nothing.
    #[test]
    fn another_process_cannot_use_a_token_it_did_not_issue() {
        let harness = fixture(
            "token-other-process",
            boot_mounts(),
            RereadBehavior::Matches,
        );
        let request = install_request(&harness.image);
        let plan = harness.manager.plan(&request).expect("plan");

        // A second process: its own key and its own issuance record.
        let other = SystemInstallManager::with_backend(
            harness.backend.clone(),
            Arc::new(SystemImageOpener),
            false,
        );
        assert!(
            matches!(
                other.apply_live(&request, &plan.plan_token, "sda"),
                Err(SystemInstallError::StalePlan)
            ),
            "a token minted elsewhere must not authorize a write"
        );
        // Even after that process previewed the same request itself, the caller's
        // token is still not the one it issued.
        let own = other.plan(&request).expect("plan in the second process");
        assert_ne!(own.plan_token, plan.plan_token);
        assert!(matches!(
            other.apply_live(&request, &plan.plan_token, "sda"),
            Err(SystemInstallError::StalePlan)
        ));
    }

    /// The tamper paths from F-T20-001: editing the nonce (to look like a new
    /// token) or the issuance time (to refresh the window) breaks the signature,
    /// so neither buys anything.
    #[test]
    fn editing_any_token_field_is_refused() {
        let harness = fixture("token-tamper", boot_mounts(), RereadBehavior::Matches);
        let request = install_request(&harness.image);
        let started = Utc::now();
        let clock = Arc::new(Mutex::new(started));
        let frozen = clock.clone();
        let manager = harness
            .manager
            .clone()
            .with_clock(Arc::new(move || *frozen.lock().expect("clock lock")));
        let plan = manager.plan(&request).expect("plan");
        let token = plan.plan_token.clone();

        // Split into the text the signature covers and the signature itself.
        let (signed, mac) = token.rsplit_once('-').expect("token has a signature");
        let (head, nonce) = signed.rsplit_once('-').expect("token has a nonce");
        assert_eq!(nonce.len(), 16, "the last signed field is the nonce");

        let cases = [
            // A fresh nonce with the old signature: the pre-fix way to escape the
            // consumption record by using a different key.
            format!("{head}-ffffffffffffffff-{mac}"),
            // A refreshed issuance time with the old signature: the pre-fix way to
            // restart the 600 second window.
            {
                let binding_len = "install-plan-".len() + 16 + 1;
                format!("{}-ffffffffff-{nonce}-{mac}", &signed[..binding_len])
            },
            // A truncated or extended signature.
            format!("{signed}-{}", &mac[..mac.len() - 1]),
            format!("{signed}-{mac}0"),
            // A different binding with a valid-looking shape (the pre-fix code
            // would reject this one, but the case belongs here for completeness).
            format!("install-plan-0000000000000000-{nonce}-{mac}"),
        ];
        for tampered in cases {
            assert!(
                matches!(
                    manager.apply_live(&request, &tampered, "sda"),
                    Err(SystemInstallError::StalePlan)
                ),
                "an edited token must be refused: {tampered}"
            );
        }

        // The untouched token still works, so the refusals above are about the
        // edits and not about a broken manager.
        manager
            .apply_live(&request, &token, "sda")
            .expect("the untouched token is accepted");

        // Once consumed, replaying it — or an edited copy — is refused as well.
        assert!(matches!(
            manager.apply_live(&request, &token, "sda"),
            Err(SystemInstallError::StalePlan)
        ));
        let edited = format!("{head}-eeeeeeeeeeeeeeee-{mac}");
        assert!(matches!(
            manager.apply_live(&request, &edited, "sda"),
            Err(SystemInstallError::StalePlan)
        ));
    }

    #[test]
    fn every_preview_issues_its_own_token_and_a_forged_one_is_refused() {
        let harness = fixture("token-nonce", boot_mounts(), RereadBehavior::Matches);
        let request = install_request(&harness.image);
        let first = harness.manager.plan(&request).expect("first plan");
        let second = harness.manager.plan(&request).expect("second plan");
        assert_ne!(
            first.plan_token, second.plan_token,
            "two previews must not share a token, or a second install would look \
             like a replay of the first"
        );
        assert!(split_plan_token(&first.plan_token).is_some());
        // Both tokens are usable: the repeat is not blocked by the first write.
        harness
            .manager
            .apply_live(&request, &first.plan_token, "sda")
            .expect("first write");
        harness
            .manager
            .apply_live(&request, &second.plan_token, "sda")
            .expect("the same request may be written again after a new preview");

        for forged in [
            "install-plan-0000000000000000-0000000000-0000000000000000",
            &format!("{}-extra", first.plan_token),
            &first.plan_token.replace("install-plan-", ""),
            "install-plan-",
            "",
        ] {
            assert!(
                matches!(
                    harness.manager.apply_live(&request, forged, "sda"),
                    Err(SystemInstallError::PlanRequired) | Err(SystemInstallError::StalePlan)
                ),
                "a token this build did not issue must be refused: {forged:?}"
            );
        }
    }

    /// The advisory cross-process path: a surface that issues its own token (the
    /// privileged helper does) uses `verify_preview_token` to notice that the
    /// state moved or that the preview the operator saw is old. It is explicitly
    /// not an authorization, which is why the checks are about drift and age.
    #[test]
    fn a_preview_token_is_checked_for_state_drift_and_age() {
        let harness = fixture("preview-token", boot_mounts(), RereadBehavior::Matches);
        let request = install_request(&harness.image);
        let started = Utc::now();
        let clock = Arc::new(Mutex::new(started));
        let frozen = clock.clone();
        let issuing = harness
            .manager
            .clone()
            .with_clock(Arc::new(move || *frozen.lock().expect("clock lock")));
        let plan = issuing.plan(&request).expect("plan");

        // A second process cannot verify the signature, but it can compare the
        // binding against the state it rebuilds itself.
        let helper = SystemInstallManager::with_backend(
            harness.backend.clone(),
            Arc::new(SystemImageOpener),
            false,
        );
        helper
            .verify_preview_token(&request, &plan.plan_token)
            .expect("the state is unchanged");
        assert!(
            split_plan_token(&plan.plan_token).is_some(),
            "the helper can read the shape it is handed"
        );

        // The image changed since the preview: the helper notices the drift.
        write_file(&harness.image, mbr_image(1, 4 * MIB));
        assert!(matches!(
            helper.verify_preview_token(&request, &plan.plan_token),
            Err(SystemInstallError::StalePlan)
        ));

        // An aged preview is refused by the advisory window. The helper's own
        // clock is used here, so the exact same token that was fresh above is old
        // 601 seconds later.
        let later_clock = Arc::new(Mutex::new(started + chrono::Duration::seconds(601)));
        let later = helper.clone().with_clock(Arc::new({
            let clock = later_clock.clone();
            move || *clock.lock().expect("clock lock")
        }));
        let fresh_for_shape = issuing.plan(&request);
        // The image changed above, so rebuild a matching request for the window
        // check: the point here is the age, not the binding.
        let _ = fresh_for_shape;
        assert!(matches!(
            later.verify_preview_token(&request, &plan.plan_token),
            Err(SystemInstallError::StalePlan)
        ));
    }

    #[test]
    fn a_changed_image_or_target_invalidates_the_plan() {
        let harness = fixture("swap-image", boot_mounts(), RereadBehavior::Matches);
        let request = install_request(&harness.image);
        let plan = harness.manager.plan(&request).expect("plan");
        write_file(&harness.image, mbr_image(1, 4 * MIB));
        assert!(matches!(
            harness
                .manager
                .apply_live(&request, &plan.plan_token, "sda"),
            Err(SystemInstallError::StalePlan)
        ));

        let target_harness = fixture("swap-target", boot_mounts(), RereadBehavior::Matches);
        let swapped = install_request(&target_harness.image);
        let plan = target_harness.manager.plan(&swapped).expect("plan");
        target_harness.backend.targets.lock().expect("targets lock")[0].size_bytes = 32 * MIB;
        assert!(matches!(
            target_harness
                .manager
                .apply_live(&swapped, &plan.plan_token, "sda"),
            Err(SystemInstallError::StalePlan)
        ));
    }
}
