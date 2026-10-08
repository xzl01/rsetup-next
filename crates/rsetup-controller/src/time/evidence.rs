use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeQuality {
    NtpValid,
    SystemFallback,
    Stale,
}

pub mod opt_u64_str {
    use super::*;

    pub fn serialize<S>(val: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match val {
            Some(v) => serializer.serialize_str(&v.to_string()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt_str = Option::<String>::deserialize(deserializer)?;
        match opt_str {
            None => Ok(None),
            Some(s) => {
                if s.is_empty() {
                    return Err(serde::de::Error::custom("invalid sample_age_ms"));
                }
                if !s.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(serde::de::Error::custom("invalid sample_age_ms"));
                }
                if s.len() > 1 && s.starts_with('0') {
                    return Err(serde::de::Error::custom("invalid sample_age_ms"));
                }
                let val = s
                    .parse::<u64>()
                    .map_err(|_| serde::de::Error::custom("invalid sample_age_ms"))?;
                Ok(Some(val))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimeEvidence {
    pub system_wall_utc: DateTime<Utc>,
    pub reference_utc: DateTime<Utc>,
    pub quality: TimeQuality,
    pub clock_epoch: Uuid,
    pub source: Option<String>,
    #[serde(with = "opt_u64_str", default)]
    pub sample_age_ms: Option<u64>,
    pub offset_ms: Option<i64>,
    pub uncertainty_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn time_evidence_serializes_sample_age_as_decimal_string_or_null() {
        let now = Utc::now();
        let mut evidence = TimeEvidence {
            system_wall_utc: now,
            reference_utc: now,
            quality: TimeQuality::SystemFallback,
            clock_epoch: Uuid::new_v4(),
            source: None,
            sample_age_ms: Some(150),
            offset_ms: Some(-5),
            uncertainty_ms: Some(15),
        };
        let wire = serde_json::to_value(&evidence).unwrap();
        assert_eq!(wire["sample_age_ms"], json!("150")); // 初次 RED：桩输出 150
        assert_eq!(wire["offset_ms"], json!(-5));
        assert_eq!(wire["uncertainty_ms"], json!(15));
        evidence.sample_age_ms = None;
        let null_wire = serde_json::to_value(&evidence).unwrap();
        assert!(null_wire["sample_age_ms"].is_null());
        for invalid in [json!(150), json!("01"), json!(-1)] {
            let mut bad = null_wire.clone();
            bad["sample_age_ms"] = invalid;
            assert!(serde_json::from_value::<TimeEvidence>(bad).is_err());
        }
        // Additional invalid strings: empty, non-digit, overflow
        for invalid_str in ["", "abc", " 10 ", "18446744073709551616"] {
            let mut bad = null_wire.clone();
            bad["sample_age_ms"] = json!(invalid_str);
            assert!(serde_json::from_value::<TimeEvidence>(bad).is_err());
        }
        // Valid edge cases: "0"
        let mut zero_case = null_wire.clone();
        zero_case["sample_age_ms"] = json!("0");
        let parsed_zero = serde_json::from_value::<TimeEvidence>(zero_case).unwrap();
        assert_eq!(parsed_zero.sample_age_ms, Some(0));

        let roundtrip = serde_json::from_value::<TimeEvidence>(wire).unwrap();
        assert_eq!(roundtrip.sample_age_ms, Some(150));
    }
}
