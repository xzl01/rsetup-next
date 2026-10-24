use rsetup_controller::tasks::board_client::{BoardClient, BoardClientError, BoardFuture};
use rsetup_controller::transport::board_client::{DeviceRpc, RpcReply, TransportBoardClient};
use rsetup_protocol::BusinessCodec;
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessPayload, BusinessPayloadKind, Capabilities, RebootExecute,
    RebootTicket, approved_codec_witness, decode_business, encode_business,
};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use uuid::Uuid;
struct Rpc {
    responses: Mutex<VecDeque<Result<RpcReply, BoardClientError>>>,
    calls: Mutex<Vec<(&'static str, Vec<u8>)>>,
}
impl DeviceRpc for Rpc {
    fn request<'a>(
        &'a self,
        _device: &'a [u8; 32],
        action: &'static str,
        payload: Vec<u8>,
    ) -> BoardFuture<'a, RpcReply> {
        self.calls.lock().unwrap().push((action, payload));
        let result = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("queued reply");
        Box::pin(async move { result })
    }
}
fn fixture(replies: Vec<Result<RpcReply, BoardClientError>>) -> (Arc<Rpc>, TransportBoardClient) {
    let rpc = Arc::new(Rpc {
        responses: Mutex::new(replies.into()),
        calls: Mutex::new(vec![]),
    });
    let client = TransportBoardClient::new(rpc.clone());
    (rpc, client)
}
fn wire(payload: BusinessPayload) -> RpcReply {
    let w = approved_codec_witness();
    RpcReply {
        status_code: 0,
        payload: encode_business(&BusinessCodec, Some(&w), &payload).unwrap(),
    }
}
fn caps() -> Capabilities {
    Capabilities {
        schema_version: BUSINESS_SCHEMA_VERSION,
        boot_id: Uuid::new_v4().to_string(),
        agent_version: "test".into(),
        supported_actions: vec!["device.status.get".into()],
        durable_task_journal: true,
        max_status_bytes: 65536,
        journal_epoch: Uuid::new_v4().to_string(),
    }
}
#[tokio::test]
async fn capabilities_uses_real_business_codec() {
    let expected = caps();
    let (rpc, client) = fixture(vec![Ok(wire(BusinessPayload::Capabilities(
        expected.clone(),
    )))]);
    assert_eq!(client.capabilities(&[1; 32]).await.unwrap(), expected);
    let calls = rpc.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "device.capabilities.get");
    let w = approved_codec_witness();
    assert!(matches!(
        decode_business(
            &BusinessCodec,
            Some(&w),
            BusinessPayloadKind::Query,
            &calls[0].1
        )
        .unwrap(),
        BusinessPayload::Query(_)
    ));
}
#[tokio::test]
async fn task_not_found_is_none_only_for_task_query() {
    let (_, client) = fixture(vec![Ok(RpcReply {
        status_code: 1006,
        payload: vec![],
    })]);
    assert_eq!(
        client.task_record(&[1; 32], Uuid::new_v4()).await.unwrap(),
        None
    );
    let (_, client) = fixture(vec![Ok(RpcReply {
        status_code: 1006,
        payload: vec![],
    })]);
    assert!(matches!(
        client.capabilities(&[1; 32]).await,
        Err(BoardClientError::Rejected { status: 1006, .. })
    ));
}
#[tokio::test]
async fn execute_ticket_mismatch_sends_nothing() {
    let (rpc, client) = fixture(vec![]);
    let boot = Uuid::new_v4().to_string();
    let ticket = RebootTicket {
        schema_version: 1,
        sub_task_id: Uuid::new_v4().to_string(),
        boot_id: boot.clone(),
        execution_token: [1; 32],
        valid_for_ms: 30000,
    };
    let request = RebootExecute {
        schema_version: 1,
        sub_task_id: Uuid::new_v4().to_string(),
        expected_boot_id: boot,
        execution_token: [1; 32],
    };
    assert!(matches!(
        client.execute(&[1; 32], &ticket, &request).await,
        Err(BoardClientError::ProtocolMismatch)
    ));
    assert!(rpc.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn timeout_is_unknown_and_never_retried() {
    let (rpc, client) = fixture(vec![Err(BoardClientError::Timeout)]);
    assert!(matches!(
        client.clock(&[1; 32]).await,
        Err(BoardClientError::Timeout)
    ));
    assert_eq!(rpc.calls.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn malformed_or_wrong_schema_reply_fails_closed() {
    let (_, client) = fixture(vec![Ok(RpcReply {
        status_code: 0,
        payload: vec![0xff],
    })]);
    assert!(matches!(
        client.capabilities(&[1; 32]).await,
        Err(BoardClientError::ProtocolMismatch)
    ));
}
#[tokio::test]
async fn stable_remote_statuses_map_to_typed_errors() {
    for (code, kind) in [(1004, 0), (1005, 1), (1008, 2)] {
        let (_, client) = fixture(vec![Ok(RpcReply {
            status_code: code,
            payload: vec![],
        })]);
        let e = client.status(&[1; 32]).await.unwrap_err();
        assert!(matches!(
            (kind, e),
            (0, BoardClientError::CapabilityUnavailable)
                | (1, BoardClientError::DeviceBusy)
                | (2, BoardClientError::ResourceExhausted)
        ));
    }
}

#[tokio::test]
async fn status_and_clock_replies_preserve_typed_values() {
    use rsetup_protocol::wire::{ClockSample, DeviceStatus};
    let boot = Uuid::new_v4().to_string();
    let epoch = Uuid::new_v4().to_string();
    let status = DeviceStatus {
        schema_version: 1,
        boot_id: boot.clone(),
        agent_epoch: Uuid::new_v4().to_string(),
        sample_seq: 1,
        sampled_unix_ms: 1,
        sampled_monotonic_ns: 2,
        clock_epoch: epoch.clone(),
        metrics: vec![],
        clock_quality: "observed".into(),
    };
    let clock = ClockSample {
        schema_version: 1,
        boot_id: boot,
        clock_epoch: epoch,
        receive_unix_ms: 1,
        send_unix_ms: 2,
        receive_monotonic_ns: 3,
        send_monotonic_ns: 4,
        quality: "observed".into(),
    };
    let (rpc, client) = fixture(vec![
        Ok(wire(BusinessPayload::DeviceStatus(status.clone()))),
        Ok(wire(BusinessPayload::ClockSample(clock.clone()))),
    ]);
    assert_eq!(client.status(&[1; 32]).await.unwrap(), status);
    assert_eq!(client.clock(&[1; 32]).await.unwrap(), clock);
    let calls = rpc.calls.lock().unwrap();
    assert_eq!(calls[0].0, "device.status.get");
    assert_eq!(calls[1].0, "device.clock.get");
}
#[tokio::test]
async fn prepare_execute_and_task_record_bind_the_requested_subtask() {
    use rsetup_protocol::wire::TaskRecord;
    let id = Uuid::new_v4();
    let boot = Uuid::new_v4().to_string();
    let ticket = RebootTicket {
        schema_version: 1,
        sub_task_id: id.to_string(),
        boot_id: boot.clone(),
        execution_token: [9; 32],
        valid_for_ms: 30000,
    };
    let record = TaskRecord {
        schema_version: 1,
        sub_task_id: id.to_string(),
        journal_epoch: Uuid::new_v4().to_string(),
        record_version: 1,
        state: "accepted".into(),
        pre_boot_id: boot.clone(),
        observed_boot_id: None,
        reason_code: None,
        updated_unix_ms: None,
        evidence_kind: None,
    };
    let (rpc, client) = fixture(vec![
        Ok(wire(BusinessPayload::RebootTicket(ticket.clone()))),
        Ok(wire(BusinessPayload::TaskRecord(record.clone()))),
        Ok(wire(BusinessPayload::TaskRecord(record.clone()))),
    ]);
    assert_eq!(client.prepare(&[1; 32], id, &boot).await.unwrap(), ticket);
    let request = RebootExecute {
        schema_version: 1,
        sub_task_id: id.to_string(),
        expected_boot_id: boot,
        execution_token: [9; 32],
    };
    assert_eq!(
        client.execute(&[1; 32], &ticket, &request).await.unwrap(),
        record
    );
    assert_eq!(
        client.task_record(&[1; 32], id).await.unwrap(),
        Some(record)
    );
    let calls = rpc.calls.lock().unwrap();
    assert_eq!(
        calls.iter().map(|c| c.0).collect::<Vec<_>>(),
        vec![
            "device.reboot.prepare",
            "device.reboot.execute",
            "device.task.get"
        ]
    );
    let w = approved_codec_witness();
    let decoded = decode_business(
        &BusinessCodec,
        Some(&w),
        BusinessPayloadKind::RebootExecute,
        &calls[1].1,
    )
    .unwrap();
    assert_eq!(decoded, BusinessPayload::RebootExecute(request));
}
#[tokio::test]
async fn mismatched_prepare_response_is_not_accepted() {
    let id = Uuid::new_v4();
    let boot = Uuid::new_v4().to_string();
    let wrong = RebootTicket {
        schema_version: 1,
        sub_task_id: Uuid::new_v4().to_string(),
        boot_id: boot.clone(),
        execution_token: [1; 32],
        valid_for_ms: 30000,
    };
    let (_, client) = fixture(vec![Ok(wire(BusinessPayload::RebootTicket(wrong)))]);
    assert!(matches!(
        client.prepare(&[1; 32], id, &boot).await,
        Err(BoardClientError::ProtocolMismatch)
    ));
}
#[tokio::test]
async fn unsupported_error_code_is_not_reinterpreted_as_success() {
    let (_, client) = fixture(vec![Ok(RpcReply {
        status_code: 65536,
        payload: vec![],
    })]);
    assert!(matches!(
        client.capabilities(&[1; 32]).await,
        Err(BoardClientError::ProtocolMismatch)
    ));
}

fn execution_fixture() -> (Uuid, RebootTicket, RebootExecute) {
    let id = Uuid::new_v4();
    let boot = Uuid::new_v4().to_string();
    let ticket = RebootTicket {
        schema_version: 1,
        sub_task_id: id.to_string(),
        boot_id: boot.clone(),
        execution_token: [8; 32],
        valid_for_ms: 30000,
    };
    let request = RebootExecute {
        schema_version: 1,
        sub_task_id: id.to_string(),
        expected_boot_id: boot,
        execution_token: [8; 32],
    };
    (id, ticket, request)
}
fn record_fixture(id: Uuid, boot: &str) -> rsetup_protocol::wire::TaskRecord {
    rsetup_protocol::wire::TaskRecord {
        schema_version: 1,
        sub_task_id: id.to_string(),
        journal_epoch: Uuid::new_v4().to_string(),
        record_version: 1,
        state: "accepted".into(),
        pre_boot_id: boot.into(),
        observed_boot_id: None,
        reason_code: None,
        updated_unix_ms: None,
        evidence_kind: None,
    }
}
#[tokio::test]
async fn execute_and_task_query_reject_wrong_response_subtask() {
    let (id, ticket, request) = execution_fixture();
    let wrong = record_fixture(Uuid::new_v4(), &ticket.boot_id);
    let (_, client) = fixture(vec![
        Ok(wire(BusinessPayload::TaskRecord(wrong.clone()))),
        Ok(wire(BusinessPayload::TaskRecord(wrong))),
    ]);
    assert!(matches!(
        client.execute(&[1; 32], &ticket, &request).await,
        Err(BoardClientError::ProtocolMismatch)
    ));
    assert!(matches!(
        client.task_record(&[1; 32], id).await,
        Err(BoardClientError::ProtocolMismatch)
    ));
}
#[tokio::test]
async fn prepare_rejects_wrong_response_boot_id() {
    let (id, mut ticket, request) = execution_fixture();
    ticket.boot_id = Uuid::new_v4().to_string();
    let (_, client) = fixture(vec![Ok(wire(BusinessPayload::RebootTicket(ticket)))]);
    assert!(matches!(
        client
            .prepare(&[1; 32], id, &request.expected_boot_id)
            .await,
        Err(BoardClientError::ProtocolMismatch)
    ));
}
#[tokio::test]
async fn rejected_bucket_and_status_boundaries_are_explicit() {
    for status in [1001, 1003, 1007, 1010] {
        let (_, client) = fixture(vec![Ok(RpcReply {
            status_code: status,
            payload: vec![],
        })]);
        assert!(
            matches!(client.clock(&[1;32]).await,Err(BoardClientError::Rejected{status:s,reason_code:None}) if s==status)
        );
    }
    for status in [-1, 1000, 1011] {
        let (_, client) = fixture(vec![Ok(RpcReply {
            status_code: status,
            payload: vec![],
        })]);
        assert!(matches!(
            client.clock(&[1; 32]).await,
            Err(BoardClientError::ProtocolMismatch)
        ));
    }
}
#[tokio::test]
async fn execute_wrong_token_or_boot_is_rejected_before_sending() {
    for wrong_boot in [true, false] {
        let (_, ticket, mut request) = execution_fixture();
        if wrong_boot {
            request.expected_boot_id = Uuid::new_v4().to_string();
        } else {
            request.execution_token = [7; 32];
        }
        let (rpc, client) = fixture(vec![]);
        assert!(matches!(
            client.execute(&[1; 32], &ticket, &request).await,
            Err(BoardClientError::ProtocolMismatch)
        ));
        assert!(rpc.calls.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn prepare_and_task_query_wire_bind_expected_identifiers() {
    use rsetup_protocol::wire::{RebootPrepare, TaskQuery};
    let (id, ticket, request) = execution_fixture();
    let (rpc, client) = fixture(vec![
        Ok(wire(BusinessPayload::RebootTicket(ticket))),
        Ok(RpcReply {
            status_code: 1006,
            payload: vec![],
        }),
    ]);
    client
        .prepare(&[1; 32], id, &request.expected_boot_id)
        .await
        .unwrap();
    client.task_record(&[1; 32], id).await.unwrap();
    let calls = rpc.calls.lock().unwrap();
    let w = approved_codec_witness();
    assert_eq!(
        decode_business(
            &BusinessCodec,
            Some(&w),
            BusinessPayloadKind::RebootPrepare,
            &calls[0].1
        )
        .unwrap(),
        BusinessPayload::RebootPrepare(RebootPrepare {
            schema_version: 1,
            sub_task_id: id.to_string(),
            expected_boot_id: request.expected_boot_id
        })
    );
    assert_eq!(
        decode_business(
            &BusinessCodec,
            Some(&w),
            BusinessPayloadKind::TaskQuery,
            &calls[1].1
        )
        .unwrap(),
        BusinessPayload::TaskQuery(TaskQuery {
            schema_version: 1,
            sub_task_id: id.to_string()
        })
    );
}
