use serde::Deserialize;
use std::{fs, path::Path, sync::OnceLock};

const CATALOG_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../data/pinouts.json"
));
const Q8B_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../data/pinouts/dragon-q8b.json"
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
    pub exclusive: Vec<String>,
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
    high_speed_uart: bool,
}

fn catalog() -> &'static PinoutCatalog {
    static CATALOG: OnceLock<PinoutCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut catalog: PinoutCatalog =
            serde_json::from_str(CATALOG_JSON).expect("embedded pinout catalog must be valid");
        // Keep documentation-derived profiles separate from the generated pin-out snapshot.
        catalog
            .profiles
            .push(serde_json::from_str(Q8B_JSON).expect("embedded Q8B pinout must be valid"));
        catalog
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
            if selectors_match(&evidence_selector, &function_selector)
                && evidence_covers_pin(item, pin, function, &function_selector.family)
            {
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

fn evidence_covers_pin(
    item: &FunctionEvidence,
    pin: &PinoutPin,
    function: &str,
    family: &str,
) -> bool {
    let pads = item
        .exclusive
        .iter()
        .map(|resource| normalize(resource))
        .filter(|resource| is_pad_resource(resource))
        .collect::<Vec<_>>();
    if !pads.is_empty() {
        return [&pin.default_function, &pin.name]
            .into_iter()
            .chain(pin.gpio.iter())
            .any(|name| pads.contains(&normalize(name)));
    }
    // Without pin metadata a UART overlay only establishes its data pair.
    // CTS/RTS need explicit pad evidence, not just the same controller/mux.
    family != "UART"
        || function
            .to_ascii_uppercase()
            .split('_')
            .any(|signal| matches!(signal, "TX" | "RX"))
}

fn is_pad_resource(resource: &str) -> bool {
    let bytes = resource.as_bytes();
    (resource.starts_with("gpio") && bytes.get(4).is_some_and(u8::is_ascii_digit))
        || (bytes.first() == Some(&b'p')
            && bytes.get(1).is_some_and(u8::is_ascii_alphabetic)
            && bytes.get(2).is_some_and(u8::is_ascii_digit))
}

fn selectors_match(evidence: &FunctionSelector, function: &FunctionSelector) -> bool {
    evidence.family == function.family
        && evidence.controller == function.controller
        && evidence.high_speed_uart == function.high_speed_uart
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
                    high_speed_uart: family == "UART"
                        && (prefix.ends_with("HS-") || prefix.ends_with("HS_")),
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
            exclusive: vec![],
        }
    }

    #[test]
    fn catalog_contains_the_imported_sbc_profiles() {
        assert_eq!(catalog().profiles.len(), 21);
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
    fn q8b_matches_the_board_not_just_the_soc() {
        for model in ["Radxa Dragon Q8B", "radxa,dragon-q8b"] {
            assert_eq!(match_profile(&[model.into()]).unwrap().id, "dragonQ8b");
        }
        assert!(match_profile(&["qcom,sc8280xp".into()]).is_none());
        assert_eq!(
            match_profile(&["Radxa Dragon Q6A".into()]).unwrap().id,
            "dragonQ6a"
        );
    }

    #[test]
    fn q8b_preserves_all_forty_function0_defaults() {
        let profile = profile_by_id("dragonQ8b").unwrap();
        assert_eq!(profile.connectors.len(), 1);
        let pins = &profile.connectors[0].pins;
        let expected = "3.3V 5V GPIO_41 5V GPIO_42 GND GPIO_175 GPIO_63 GND GPIO_64 \
            GPIO_111 GPIO_174 GPIO_66 GND GPIO_67 GPIO_68 3.3V GPIO_110 GPIO_88 GND \
            GPIO_87 GPIO_92 GPIO_89 GPIO_90 GND GPIO_91 GPIO_43 GPIO_44 GPIO_157 GND \
            GPIO_156 GPIO_114 GPIO_115 GND GPIO_171 GPIO_112 GPIO_69 GPIO_172 GND GPIO_173";
        assert_eq!(pins.len(), 40);
        for (index, (pin, expected)) in pins.iter().zip(expected.split_whitespace()).enumerate() {
            assert_eq!(pin.number as usize, index + 1);
            assert_eq!(pin.default_function, expected);
            assert_eq!(pin.name, expected);
            if pin.kind == "GPIO" {
                assert_eq!(pin.gpio.as_deref(), expected.strip_prefix("GPIO_"));
                assert!(
                    pin.voltage.is_none(),
                    "the source does not specify GPIO voltage"
                );
            } else {
                assert!(pin.functions.is_empty());
                assert!(pin.gpio.is_none());
            }
        }
        // Preserve the published labels, including unusual spellings, without guessing wiring.
        assert_eq!(pins[31].functions, ["CCI_I2C_SCL0", "GCC_GP2_CLK_MIRA"]);
        assert_eq!(pins[32].functions, ["CCI_I2C_SDA1", "GCC_GP3_CLK_MRIA"]);
        assert_eq!(pins[39].functions, ["UART4_TX", "SPI4_SCKL"]);
    }

    #[test]
    fn q8b_uart_and_high_speed_uart_are_distinct_muxes() {
        let profile = profile_by_id("dragonQ8b").unwrap();
        let pins = &profile.connectors[0].pins;
        let mut uart = evidence("sc8280xp-uart18.dtbo");
        uart.exclusive = ["spi18", "uart18", "i2c18", "gpio68", "gpio69"]
            .map(String::from)
            .to_vec();
        for (number, expected) in [
            (13, None),
            (15, None),
            (16, Some("UART18_TX")),
            (37, Some("UART18_RX")),
        ] {
            let resolved = resolve_function_evidence(&pins[number - 1], &[uart.clone()]);
            assert_eq!(resolved.name.as_deref(), expected, "pin {number}");
            assert_ne!(resolved.kind, "conflict");
        }
        let hs_uart = evidence("function HS-UART18");
        assert_eq!(
            resolve_function_evidence(&pins[15], &[hs_uart])
                .name
                .as_deref(),
            Some("HS-UART18_TX")
        );
    }

    #[test]
    fn q8b_spi_and_i2c_use_declared_overlay_pads() {
        let profile = profile_by_id("dragonQ8b").unwrap();
        let pins = &profile.connectors[0].pins;
        // Resource lists from radxa-overlays sc8280xp-spi4-spidev and sc8280xp-i2c8.
        let mut spi = evidence("sc8280xp-spi4-spidev.dtbo");
        spi.exclusive = [
            "spi4", "uart4", "i2c4", "gpio171", "gpio172", "gpio173", "gpio174",
        ]
        .map(String::from)
        .to_vec();
        let mut i2c = evidence("sc8280xp-i2c8.dtbo");
        i2c.exclusive = ["spi8", "uart8", "i2c8", "gpio43", "gpio44"]
            .map(String::from)
            .to_vec();
        for (number, expected) in [
            (7, None),
            (11, None),
            (36, None),
            (12, Some("SPI4_CS_0")),
            (35, Some("SPI4_MISO")),
            (38, Some("SPI4_MOSI")),
            (40, Some("SPI4_SCKL")),
            (27, Some("I2C8_SDA")),
            (28, Some("I2C8_SCL")),
        ] {
            assert_eq!(
                resolve_function_evidence(&pins[number - 1], &[spi.clone(), i2c.clone()])
                    .name
                    .as_deref(),
                expected,
                "pin {number}"
            );
        }
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
    fn two_wire_uart_does_not_claim_flow_control_pins() {
        let profile = profile_by_id("rock5b").unwrap();
        let pins = &profile.connectors[0].pins;
        let mut uart = evidence("rk3588-uart7-m1.dtbo");
        for resources in [vec![], vec!["uart7"], vec!["GPIO3_C1", "GPIO3_C0"]] {
            uart.exclusive = resources.into_iter().map(String::from).collect();
            for (number, expected) in [
                (7, None),
                (32, None),
                (11, Some("UART7_RX_M1")),
                (15, Some("UART7_TX_M1")),
            ] {
                let pin = pins.iter().find(|pin| pin.number == number).unwrap();
                assert_eq!(
                    resolve_function_evidence(pin, &[uart.clone()])
                        .name
                        .as_deref(),
                    expected,
                    "pin {number}"
                );
            }
        }
        uart.exclusive
            .extend(["GPIO3_C3".into(), "GPIO3_C2".into()]);
        for (number, expected) in [(7, "UART7_CTSN_M1"), (32, "UART7_RTSN_M1")] {
            let pin = pins.iter().find(|pin| pin.number == number).unwrap();
            assert_eq!(
                resolve_function_evidence(pin, &[uart.clone()])
                    .name
                    .as_deref(),
                Some(expected)
            );
        }
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
