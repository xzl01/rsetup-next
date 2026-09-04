use serde::Deserialize;
use std::{fs, path::Path, sync::OnceLock};

const CATALOG_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../data/pinouts.json"
));

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PinoutProfile {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub layout: String,
    pub patterns: Vec<String>,
    pub connectors: Vec<PinoutConnector>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PinoutConnector {
    pub id: String,
    pub name: String,
    pub pins: Vec<PinoutPin>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PinoutPin {
    pub number: u8,
    pub name: String,
    pub default_function: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub gpio: Option<String>,
    pub voltage: Option<String>,
    #[serde(default)]
    pub functions: Vec<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FunctionEvidence {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedFunction {
    pub name: Option<String>,
    pub kind: String,
    pub source_detail: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PinoutCatalog {
    profiles: Vec<PinoutProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionSelector {
    family: String,
    controller: String,
    mode: Option<String>,
}

fn catalog() -> &'static PinoutCatalog {
    static CATALOG: OnceLock<PinoutCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(CATALOG_JSON).expect("embedded pinout catalog must be valid")
    })
}

pub(crate) fn profile_by_id(id: &str) -> Option<PinoutProfile> {
    catalog()
        .profiles
        .iter()
        .find(|profile| profile.id == id)
        .cloned()
}

pub(crate) fn profile_for_root(root: &Path) -> Option<PinoutProfile> {
    let observations = [
        root.join("proc/device-tree/model"),
        root.join("proc/device-tree/compatible"),
        root.join("sys/firmware/devicetree/base/model"),
        root.join("sys/firmware/devicetree/base/compatible"),
    ]
    .into_iter()
    .filter_map(|path| fs::read(path).ok())
    .flat_map(|bytes| {
        bytes
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect::<Vec<_>>()
    })
    .collect::<Vec<_>>();
    match_profile(&observations)
}

fn match_profile(observations: &[String]) -> Option<PinoutProfile> {
    let observations = observations
        .iter()
        .map(|value| normalize(value))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    catalog()
        .profiles
        .iter()
        .filter_map(|profile| {
            let score = profile
                .patterns
                .iter()
                .map(|pattern| normalize(pattern))
                .filter(|pattern| pattern.len() >= 5)
                .filter(|pattern| observations.iter().any(|value| value.contains(pattern)))
                .map(|pattern| pattern.len())
                .max()?;
            Some((score, profile))
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, profile)| profile.clone())
}

pub(crate) fn resolve_function_evidence(
    pin: &PinoutPin,
    evidence: &[FunctionEvidence],
) -> ResolvedFunction {
    let mut matches = Vec::<(String, String, String)>::new();
    for item in evidence {
        let Some(evidence_selector) = parse_selector(&item.text) else {
            continue;
        };
        for function in &pin.functions {
            let Some(function_selector) = parse_selector(function) else {
                continue;
            };
            if selectors_match(&evidence_selector, &function_selector) {
                matches.push((
                    function.clone(),
                    function_selector.family.to_ascii_lowercase(),
                    item.id.clone(),
                ));
            }
        }
    }
    matches.sort();
    matches.dedup();
    if matches.is_empty() {
        return ResolvedFunction {
            name: None,
            kind: "unconfirmed".into(),
            source_detail: None,
        };
    }
    let functions = matches
        .iter()
        .map(|(function, _, _)| function.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if functions.len() > 1 {
        let sources = matches
            .iter()
            .map(|(_, _, source)| source.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        return ResolvedFunction {
            name: None,
            kind: "conflict".into(),
            source_detail: Some(sources),
        };
    }
    let (name, kind, _) = &matches[0];
    let sources = matches
        .iter()
        .map(|(_, _, source)| source.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    ResolvedFunction {
        name: Some(name.clone()),
        kind: kind.clone(),
        source_detail: Some(sources),
    }
}

fn selectors_match(evidence: &FunctionSelector, function: &FunctionSelector) -> bool {
    evidence.family == function.family
        && evidence.controller == function.controller
        && evidence
            .mode
            .as_ref()
            .is_none_or(|mode| function.mode.as_ref() == Some(mode))
}

fn parse_selector(value: &str) -> Option<FunctionSelector> {
    let upper = value.to_ascii_uppercase();
    let mut candidates = Vec::new();
    for family in [
        "UART", "I2C", "TWI", "SPI", "PWM", "I2S", "CAN", "PDM", "SPDIF",
    ] {
        for (start, _) in upper.match_indices(family) {
            let suffix = &upper[start + family.len()..];
            let controller = suffix
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            if controller.is_empty() {
                continue;
            }
            let mode = parse_mode(&suffix[controller.len()..]);
            let prefix = upper[..start].trim_end();
            let context_score = if prefix.ends_with("GROUP") {
                2
            } else if prefix.ends_with("FUNCTION") {
                1
            } else {
                0
            };
            let score = u8::from(mode.is_some()) * 4 + context_score;
            candidates.push((
                score,
                start,
                FunctionSelector {
                    family: if family == "TWI" { "I2C" } else { family }.into(),
                    controller,
                    mode,
                },
            ));
        }
    }
    candidates
        .into_iter()
        .max_by_key(|(score, position, _)| (*score, *position))
        .map(|(_, _, selector)| selector)
}

fn parse_mode(suffix: &str) -> Option<String> {
    let compact = suffix
        .strip_prefix('M')
        .map(|value| {
            value
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .filter(|value| !value.is_empty());
    compact.or_else(|| {
        suffix
            .split(|character: char| !character.is_ascii_alphanumeric())
            .find_map(|token| {
                let value = token.strip_prefix('M')?;
                (!value.is_empty() && value.chars().all(|character| character.is_ascii_digit()))
                    .then(|| value.to_owned())
            })
    })
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(id: &str) -> FunctionEvidence {
        FunctionEvidence {
            id: id.into(),
            text: id.into(),
        }
    }

    #[test]
    fn catalog_contains_the_imported_sbc_profiles() {
        assert_eq!(catalog().profiles.len(), 20);
        let rock5b = profile_by_id("rock5b").unwrap();
        let pin3 = rock5b.connectors[0]
            .pins
            .iter()
            .find(|pin| pin.number == 3)
            .unwrap();
        assert_eq!(pin3.default_function, "GPIO4_B3");
        assert!(profile_by_id("arduino").is_none());
    }

    #[test]
    fn board_matching_prefers_the_specific_model() {
        let profile = match_profile(&["Radxa ROCK 5B".into(), "radxa,rock-5b".into()]).unwrap();
        assert_eq!(profile.id, "rock5b");
    }

    #[test]
    fn function_evidence_respects_mux_mode() {
        let profile = profile_by_id("rock5b").unwrap();
        let pins = &profile.connectors[0].pins;
        let pin8 = pins.iter().find(|pin| pin.number == 8).unwrap();
        let pin36 = pins.iter().find(|pin| pin.number == 36).unwrap();
        let active = [evidence("pin 13: function uart2m0 group uart2m0-xfer")];
        assert_eq!(
            resolve_function_evidence(pin8, &active).name.as_deref(),
            Some("UART2_TX_M0")
        );
        assert_eq!(resolve_function_evidence(pin36, &active).name, None);
    }

    #[test]
    fn multiple_active_muxes_are_reported_as_a_conflict() {
        let profile = profile_by_id("rock5b").unwrap();
        let pin = profile.connectors[0]
            .pins
            .iter()
            .find(|pin| pin.number == 8)
            .unwrap();
        let active = [
            evidence("rk3588-uart2-m0.dtbo"),
            evidence("rk3588-i2c1-m0.dtbo"),
        ];
        let resolved = resolve_function_evidence(pin, &active);
        assert_eq!(resolved.kind, "conflict");
        assert_eq!(resolved.name, None);
    }

    #[test]
    fn runtime_selector_skips_driver_owner_and_uses_the_mux_group() {
        let selector = parse_selector(
            "pin 119 (gpio3-23): fea90000.i2c (GPIO UNCLAIMED) function i2c3 group i2c3m1-xfer",
        )
        .unwrap();
        assert_eq!(selector.family, "I2C");
        assert_eq!(selector.controller, "3");
        assert_eq!(selector.mode.as_deref(), Some("1"));
    }
}
