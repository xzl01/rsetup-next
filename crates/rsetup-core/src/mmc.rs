use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MmcError {
    #[error("Invalid buffer length: expected {expected}, got {actual}")]
    InvalidBufferLength { expected: usize, actual: usize },
    #[error("Device not supported: {0}")]
    NotSupported(String),
    #[error("I/O error: {0}")]
    Io(String),
}

fn parse_hex_or_dec_u8(s: &str) -> Option<u8> {
    let s = s.trim();
    if let Some(hex_str) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u8::from_str_radix(hex_str, 16).ok()
    } else {
        s.parse::<u8>().ok()
    }
}

fn map_life_time_val(val: u8) -> Option<u8> {
    match val {
        0x00 => None,
        0x01..=0x0A => Some(val * 10),
        0x0B => Some(101),
        _ => None,
    }
}

pub fn parse_life_time_str(s: &str) -> (Option<u8>, Option<u8>) {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens.len() != 2 {
        return (None, None);
    }

    let val_a = parse_hex_or_dec_u8(tokens[0]).and_then(map_life_time_val);
    let val_b = parse_hex_or_dec_u8(tokens[1]).and_then(map_life_time_val);

    (val_a, val_b)
}

pub fn parse_pre_eol_info_str(s: &str) -> u8 {
    let val = parse_hex_or_dec_u8(s);
    match val {
        Some(1) => 1,
        Some(2) => 2,
        Some(3) => 3,
        _ => 0,
    }
}

pub fn parse_manfid_to_name(manfid: u32) -> &'static str {
    match manfid {
        0x15 => "Samsung",
        0x90 => "SK Hynix",
        0x13 => "Micron",
        0x45 => "SanDisk",
        0x70 => "Kingston",
        0x11 => "Toshiba",
        0xfe => "Micron",
        _ => "Unknown",
    }
}

pub fn format_manufacturer(manfid_str: &str) -> String {
    let s = manfid_str.trim();
    let parsed_val = if let Some(hex_str) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex_str, 16).ok()
    } else {
        s.parse::<u32>().ok()
    };

    if let Some(val) = parsed_val {
        let name = parse_manfid_to_name(val);
        if name != "Unknown" {
            format!("{} ({})", name, s)
        } else {
            s.to_string()
        }
    } else {
        s.to_string()
    }
}

pub fn generate_warning_flags(
    pre_eol: u8,
    life_a: Option<u8>,
    life_b: Option<u8>,
) -> Vec<String> {
    let mut flags = Vec::new();
    if pre_eol == 2 {
        flags.push("pre_eol_warning".to_string());
    } else if pre_eol == 3 {
        flags.push("pre_eol_urgent".to_string());
    }

    if life_a.map(|v| v >= 100).unwrap_or(false) {
        flags.push("life_time_typ_a_exceeded".to_string());
    }
    if life_b.map(|v| v >= 100).unwrap_or(false) {
        flags.push("life_time_typ_b_exceeded".to_string());
    }

    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_life_time() {
        // Valid pairs
        assert_eq!(parse_life_time_str("0x01 0x02"), (Some(10), Some(20)));
        assert_eq!(parse_life_time_str("0x01\n0x02"), (Some(10), Some(20)));
        assert_eq!(parse_life_time_str("1 2"), (Some(10), Some(20)));
        assert_eq!(parse_life_time_str("0x0A 0x05"), (Some(100), Some(50)));

        // Exceeded
        assert_eq!(parse_life_time_str("0x0B 0x0B"), (Some(101), Some(101)));

        // Invalid values / 0x00 (Not defined) / 0x0C (Reserved)
        assert_eq!(parse_life_time_str("0x00 0x0C"), (None, None));
        assert_eq!(parse_life_time_str("0x01 0x00"), (Some(10), None));
        assert_eq!(parse_life_time_str("0x00 0x02"), (None, Some(20)));

        // Empty or invalid format
        assert_eq!(parse_life_time_str(""), (None, None));
        assert_eq!(parse_life_time_str("not_a_number"), (None, None));
        assert_eq!(parse_life_time_str("0x01"), (None, None));
    }

    #[test]
    fn test_parse_pre_eol_info() {
        assert_eq!(parse_pre_eol_info_str("0x01"), 1);
        assert_eq!(parse_pre_eol_info_str("1"), 1);
        assert_eq!(parse_pre_eol_info_str("0x02"), 2);
        assert_eq!(parse_pre_eol_info_str("2"), 2);
        assert_eq!(parse_pre_eol_info_str("0x03"), 3);
        assert_eq!(parse_pre_eol_info_str("3"), 3);

        // Invalid / unknown
        assert_eq!(parse_pre_eol_info_str("0x00"), 0);
        assert_eq!(parse_pre_eol_info_str("0x04"), 0);
        assert_eq!(parse_pre_eol_info_str(""), 0);
        assert_eq!(parse_pre_eol_info_str("foo"), 0);
    }

    #[test]
    fn test_format_manufacturer() {
        assert_eq!(format_manufacturer("0x000015"), "Samsung (0x000015)");
        assert_eq!(format_manufacturer("0x15"), "Samsung (0x15)");
        assert_eq!(format_manufacturer("0x000090"), "SK Hynix (0x000090)");
        assert_eq!(format_manufacturer("0x000013"), "Micron (0x000013)");
        assert_eq!(format_manufacturer("0x000045"), "SanDisk (0x000045)");
        assert_eq!(format_manufacturer("0x000070"), "Kingston (0x000070)");
        assert_eq!(format_manufacturer("0x000011"), "Toshiba (0x000011)");
        assert_eq!(format_manufacturer("0x0000fe"), "Micron (0x0000fe)");
        assert_eq!(format_manufacturer("0x000099"), "0x000099");
        assert_eq!(format_manufacturer("invalid"), "invalid");
    }

    #[test]
    fn test_generate_warning_flags() {
        // Normal
        assert_eq!(
            generate_warning_flags(1, Some(50), Some(60)),
            Vec::<String>::new()
        );

        // Pre EOL warning
        assert_eq!(
            generate_warning_flags(2, Some(50), Some(60)),
            vec!["pre_eol_warning".to_string()]
        );

        // Pre EOL urgent
        assert_eq!(
            generate_warning_flags(3, Some(50), Some(60)),
            vec!["pre_eol_urgent".to_string()]
        );

        // Typ A exceeded
        assert_eq!(
            generate_warning_flags(1, Some(100), Some(60)),
            vec!["life_time_typ_a_exceeded".to_string()]
        );
        assert_eq!(
            generate_warning_flags(1, Some(101), Some(60)),
            vec!["life_time_typ_a_exceeded".to_string()]
        );

        // Typ B exceeded
        assert_eq!(
            generate_warning_flags(1, Some(50), Some(100)),
            vec!["life_time_typ_b_exceeded".to_string()]
        );

        // All flags
        assert_eq!(
            generate_warning_flags(3, Some(100), Some(101)),
            vec![
                "pre_eol_urgent".to_string(),
                "life_time_typ_a_exceeded".to_string(),
                "life_time_typ_b_exceeded".to_string()
            ]
        );
    }
}
