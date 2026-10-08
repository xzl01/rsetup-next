use rsetup_protocol::wire::{
    MAX_ACTION_BYTES, MAX_ENVELOPE_BYTES, MAX_METADATA_ENTRIES, MAX_METADATA_KEY_BYTES,
    MAX_METADATA_VAL_BYTES, PING_NONCE_LEN, PacketType, TunnelPacket, WireError,
    validate_business_version, validate_packet,
};
use std::collections::BTreeMap;

#[test]
fn rejects_invalid_business_version() {
    assert_eq!(
        validate_business_version(0),
        Err(WireError::UnsupportedVersion(0))
    );
    assert_eq!(
        validate_business_version(2),
        Err(WireError::UnsupportedVersion(2))
    );
    assert!(validate_business_version(1).is_ok());
}

#[test]
fn rejects_unknown_packet_type_and_empty_action() {
    let empty_action = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: String::new(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(validate_packet(&empty_action), Err(WireError::ActionEmpty));

    let unknown_type = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Unknown,
        action: "device.status.get".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&unknown_type),
        Err(WireError::InvalidPacketType)
    );
}

#[test]
fn validates_business_action_format_and_length() {
    let valid_packet = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: "device.status.get".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![1, 2, 3],
        metadata: BTreeMap::new(),
    };
    assert!(validate_packet(&valid_packet).is_ok());

    let too_long_action = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: "a".repeat(MAX_ACTION_BYTES + 1),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&too_long_action),
        Err(WireError::ActionTooLong {
            len: MAX_ACTION_BYTES + 1,
            max: MAX_ACTION_BYTES,
        })
    );

    let uppercase_action = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: "Device.Status.Get".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&uppercase_action),
        Err(WireError::InvalidAction)
    );

    let invalid_chars_action = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: "device_status_get".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&invalid_chars_action),
        Err(WireError::InvalidAction)
    );

    // 点段边界测试：前缀点、后缀点、连续点、仅点
    let dot_start_action = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: ".device.status".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&dot_start_action),
        Err(WireError::InvalidAction)
    );

    let dot_end_action = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: "device.status.".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&dot_end_action),
        Err(WireError::InvalidAction)
    );

    let consecutive_dots_action = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: "device..status".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&consecutive_dots_action),
        Err(WireError::InvalidAction)
    );

    let only_dot_action = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: ".".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&only_dot_action),
        Err(WireError::InvalidAction)
    );
}

#[test]
fn validates_reserved_tunnel_actions_and_packet_types() {
    // tunnel.kick 和 tunnel.revoke 必须是 System 类型
    let valid_kick = TunnelPacket {
        trace_id: 1,
        kind: PacketType::System,
        action: "tunnel.kick".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert!(validate_packet(&valid_kick).is_ok());

    let invalid_kick_kind = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: "tunnel.kick".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&invalid_kick_kind),
        Err(WireError::InvalidAction)
    );

    let valid_revoke = TunnelPacket {
        trace_id: 2,
        kind: PacketType::System,
        action: "tunnel.revoke".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert!(validate_packet(&valid_revoke).is_ok());

    let invalid_revoke_kind = TunnelPacket {
        trace_id: 2,
        kind: PacketType::Response,
        action: "tunnel.revoke".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&invalid_revoke_kind),
        Err(WireError::InvalidAction)
    );

    // tunnel.ping 不能是 System 或 Event
    let ping_system = TunnelPacket {
        trace_id: 3,
        kind: PacketType::System,
        action: "tunnel.ping".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![0u8; PING_NONCE_LEN],
        metadata: BTreeMap::new(),
    };
    assert_eq!(validate_packet(&ping_system), Err(WireError::InvalidAction));

    // 未知 tunnel.xxx 前缀被拒绝
    let bad_prefix = TunnelPacket {
        trace_id: 4,
        kind: PacketType::Request,
        action: "tunnel.custom_action".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(validate_packet(&bad_prefix), Err(WireError::InvalidAction));
}

#[test]
fn validates_metadata_boundaries() {
    let mut meta = BTreeMap::new();
    for i in 0..MAX_METADATA_ENTRIES {
        meta.insert(format!("key{i}"), format!("val{i}"));
    }
    let valid_meta_packet = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: "device.status.get".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: meta.clone(),
    };
    assert!(validate_packet(&valid_meta_packet).is_ok());

    // 超过 entries 数量
    let mut too_many_meta = meta.clone();
    too_many_meta.insert("overflow_key".to_string(), "val".to_string());
    let overflow_count_packet = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: "device.status.get".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: too_many_meta,
    };
    assert_eq!(
        validate_packet(&overflow_count_packet),
        Err(WireError::MetadataTooMany {
            len: MAX_METADATA_ENTRIES + 1,
            max: MAX_METADATA_ENTRIES,
        })
    );

    // key 过长
    let mut long_key_meta = BTreeMap::new();
    long_key_meta.insert("k".repeat(MAX_METADATA_KEY_BYTES + 1), "v".to_string());
    let long_key_packet = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: "device.status.get".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: long_key_meta,
    };
    assert_eq!(
        validate_packet(&long_key_packet),
        Err(WireError::MetadataKeyTooLong {
            len: MAX_METADATA_KEY_BYTES + 1,
            max: MAX_METADATA_KEY_BYTES,
        })
    );

    // val 过长
    let mut long_val_meta = BTreeMap::new();
    long_val_meta.insert(
        "valid_key".to_string(),
        "v".repeat(MAX_METADATA_VAL_BYTES + 1),
    );
    let long_val_packet = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: "device.status.get".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: long_val_meta,
    };
    assert_eq!(
        validate_packet(&long_val_packet),
        Err(WireError::MetadataValTooLong {
            len: MAX_METADATA_VAL_BYTES + 1,
            max: MAX_METADATA_VAL_BYTES,
        })
    );
}

#[test]
fn rejects_payload_exactly_512kib_when_envelope_overhead_exceeds_limit() {
    let full_payload = TunnelPacket {
        trace_id: 42,
        kind: PacketType::Request,
        action: "device.status.get".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![0u8; MAX_ENVELOPE_BYTES],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&full_payload),
        Err(WireError::EnvelopeTooLarge {
            len: MAX_ENVELOPE_BYTES,
            max: MAX_ENVELOPE_BYTES,
        })
    );
}

#[test]
fn validates_ping_requests_and_responses() {
    let valid_ping_req = TunnelPacket {
        trace_id: 10,
        kind: PacketType::Request,
        action: "tunnel.ping".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![0x42u8; PING_NONCE_LEN],
        metadata: BTreeMap::new(),
    };
    assert!(validate_packet(&valid_ping_req).is_ok());

    let valid_ping_resp = TunnelPacket {
        trace_id: 10,
        kind: PacketType::Response,
        action: "tunnel.ping".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![0x42u8; PING_NONCE_LEN],
        metadata: BTreeMap::new(),
    };
    assert!(validate_packet(&valid_ping_resp).is_ok());

    // Nonce 长度不足或超长
    let short_nonce = TunnelPacket {
        trace_id: 10,
        kind: PacketType::Request,
        action: "tunnel.ping".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![0u8; 15],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&short_nonce),
        Err(WireError::InvalidPingNonceLength(15))
    );

    let long_nonce = TunnelPacket {
        trace_id: 10,
        kind: PacketType::Request,
        action: "tunnel.ping".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![0u8; 17],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&long_nonce),
        Err(WireError::InvalidPingNonceLength(17))
    );

    // Ping request 不能有 metadata
    let mut req_meta = BTreeMap::new();
    req_meta.insert("foo".to_string(), "bar".to_string());
    let ping_req_with_meta = TunnelPacket {
        trace_id: 10,
        kind: PacketType::Request,
        action: "tunnel.ping".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![0u8; PING_NONCE_LEN],
        metadata: req_meta,
    };
    assert_eq!(
        validate_packet(&ping_req_with_meta),
        Err(WireError::PingRequestMetadataNotEmpty)
    );

    // Ping response status_code 必须为 0
    let ping_resp_bad_status = TunnelPacket {
        trace_id: 10,
        kind: PacketType::Response,
        action: "tunnel.ping".to_string(),
        status_code: 1,
        error_message: String::new(),
        payload: vec![0u8; PING_NONCE_LEN],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&ping_resp_bad_status),
        Err(WireError::InvalidPingStatusCode(1))
    );

    // Ping response error_message 必须为空
    let ping_resp_with_err = TunnelPacket {
        trace_id: 10,
        kind: PacketType::Response,
        action: "tunnel.ping".to_string(),
        status_code: 0,
        error_message: "some error".to_string(),
        payload: vec![0u8; PING_NONCE_LEN],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&ping_resp_with_err),
        Err(WireError::InvalidPingResponseFields)
    );

    // Ping response metadata 必须为空
    let mut resp_meta = BTreeMap::new();
    resp_meta.insert("k".to_string(), "v".to_string());
    let ping_resp_with_meta = TunnelPacket {
        trace_id: 10,
        kind: PacketType::Response,
        action: "tunnel.ping".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![0u8; PING_NONCE_LEN],
        metadata: resp_meta,
    };
    assert_eq!(
        validate_packet(&ping_resp_with_meta),
        Err(WireError::InvalidPingResponseFields)
    );
}

#[test]
fn rejects_zero_trace_id() {
    let zero_trace = TunnelPacket {
        trace_id: 0,
        kind: PacketType::Request,
        action: "device.status.get".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(validate_packet(&zero_trace), Err(WireError::InvalidTraceId));

    let zero_trace_ping = TunnelPacket {
        trace_id: 0,
        kind: PacketType::Request,
        action: "tunnel.ping".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![0u8; PING_NONCE_LEN],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&zero_trace_ping),
        Err(WireError::InvalidTraceId)
    );
}

#[test]
fn rejects_non_response_packet_with_response_fields() {
    // Request with status_code != 0
    let req_with_status = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: "device.status.get".to_string(),
        status_code: 500,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&req_with_status),
        Err(WireError::UnexpectedResponseFields { status_code: 500 })
    );

    // Request with non-empty error_message
    let req_with_err_msg = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: "device.status.get".to_string(),
        status_code: 0,
        error_message: "boom".to_string(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&req_with_err_msg),
        Err(WireError::UnexpectedResponseFields { status_code: 0 })
    );

    // Event with status_code
    let event_with_status = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Event,
        action: "telemetry.metric".to_string(),
        status_code: 1,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&event_with_status),
        Err(WireError::UnexpectedResponseFields { status_code: 1 })
    );

    // System with error_message
    let system_with_err = TunnelPacket {
        trace_id: 1,
        kind: PacketType::System,
        action: "tunnel.kick".to_string(),
        status_code: 0,
        error_message: "kicked".to_string(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&system_with_err),
        Err(WireError::UnexpectedResponseFields { status_code: 0 })
    );

    // Ping request with status_code != 0
    let ping_req_with_status = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: "tunnel.ping".to_string(),
        status_code: 400,
        error_message: String::new(),
        payload: vec![0u8; PING_NONCE_LEN],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&ping_req_with_status),
        Err(WireError::UnexpectedResponseFields { status_code: 400 })
    );

    // Ping request with non-empty error_message
    let ping_req_with_err = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: "tunnel.ping".to_string(),
        status_code: 0,
        error_message: "fail".to_string(),
        payload: vec![0u8; PING_NONCE_LEN],
        metadata: BTreeMap::new(),
    };
    assert_eq!(
        validate_packet(&ping_req_with_err),
        Err(WireError::UnexpectedResponseFields { status_code: 0 })
    );

    // Valid Response with status_code and error_message should be OK
    let valid_response = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Response,
        action: "device.status.get".to_string(),
        status_code: 500,
        error_message: "server error".to_string(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert!(validate_packet(&valid_response).is_ok());
}
