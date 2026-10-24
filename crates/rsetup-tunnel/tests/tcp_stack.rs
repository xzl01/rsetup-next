//! 真实loopback TCP跨组件装配；TestAdmission不替代controller SQL或双进程验收。
use bytes::Bytes;
use rsetup_protocol::handshake_msg::DeviceDescriptor;
use rsetup_tunnel::envelope::{PacketType, StreamKind, TunnelPacket};
use rsetup_tunnel::h2_keepalive::{H2KeepaliveIo, H2Role};
use rsetup_tunnel::handshake::{
    AdmissionDecision, AdmissionFuture, AdmissionPolicy, ClientHandshakeConfig, HandshakeError,
    HandshakeResult, ServerHandshakeConfig, VerifiedPeer, client_handshake, server_handshake,
};
use rsetup_tunnel::identity::IdentityKey;
use rsetup_tunnel::secure_io::{SecureIo, SecureIoFailure, secure_io_failure};
use rsetup_tunnel::session::streams::{accept_server_stream, open_client_stream};
use rsetup_tunnel::session::types::StreamEpoch;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn identity() -> Arc<IdentityKey> {
    let doc =
        ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
    Arc::new(IdentityKey::from_pkcs8(doc.as_ref()).unwrap())
}
struct TestAdmission([u8; 32]);
impl AdmissionPolicy for TestAdmission {
    fn evaluate<'a>(&'a self, peer: &'a VerifiedPeer) -> AdmissionFuture<'a> {
        Box::pin(async move {
            if peer.public_key != self.0 {
                return Err(HandshakeError::Policy);
            }
            Ok(AdmissionDecision::Approved)
        })
    }
}
async fn handshake_pair(
    trust_correct: bool,
) -> (
    Result<HandshakeResult<TcpStream>, HandshakeError>,
    Result<HandshakeResult<TcpStream>, HandshakeError>,
) {
    let client_key = identity();
    let server_key = identity();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (server, _) = listener.accept().await.unwrap();
    client.set_nodelay(true).unwrap();
    server.set_nodelay(true).unwrap();
    let expected_client = client_key.public_key();
    let expected_server = server_key.public_key();
    let cconfig = ClientHandshakeConfig {
        identity: client_key,
        trusted_servers: vec![if trust_correct {
            expected_server
        } else {
            identity().public_key()
        }],
        descriptor: DeviceDescriptor::new("integration", "tcp-loopback", "test", "test"),
    };
    let sconfig = ServerHandshakeConfig {
        identity: server_key,
    };
    let policy = TestAdmission(expected_client);
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(
            client_handshake(client, &cconfig),
            server_handshake(server, &sconfig, &policy)
        )
    })
    .await
    .unwrap();
    if let Ok(server) = &result.1 {
        assert_eq!(server.peer_public_key, expected_client);
    }
    if let Ok(client) = &result.0 {
        assert_eq!(client.peer_public_key, expected_server);
    }
    result
}
fn packet(id: u64, payload: Vec<u8>) -> TunnelPacket {
    TunnelPacket {
        trace_id: id,
        packet_type: PacketType::Request,
        action: "device.status.get".into(),
        status_code: 0,
        error_message: String::new(),
        payload,
        metadata: BTreeMap::new(),
    }
}
#[tokio::test]
async fn real_tcp_signed_encrypted_stack_carries_both_grpc_streams() {
    tokio::time::timeout(Duration::from_secs(5),async{
        let(c,s)=handshake_pair(true).await;let c=c.unwrap();let s=s.unwrap();
        let client_io=H2KeepaliveIo::new(SecureIo::new(c.io,c.session_keys.into_client_record_layers()),H2Role::Client);
        let server_io=H2KeepaliveIo::new(SecureIo::new(s.io,s.session_keys.into_server_record_layers()),H2Role::Server);
        let server_task=tokio::spawn(async move{
            let mut conn=h2::server::Builder::new().initial_window_size(32768).initial_connection_window_size(1024*1024).handshake::<_,Bytes>(server_io).await.unwrap();
            let mut handlers=tokio::task::JoinSet::new();let mut opened=Vec::new();
            loop{
                tokio::select!{
                    incoming=conn.accept()=>match incoming{
                        Some(Ok((request,response)))=>{
                            let(kind,mut send,mut recv)=accept_server_stream(request,response,StreamEpoch(1)).unwrap().unwrap();
                            opened.push(kind);
                            handlers.spawn(async move{
                                let mut request=recv.recv_packet().await.unwrap().unwrap();
                                request.packet_type=PacketType::Response;
                                send.send_packet(&request).await.unwrap();send.finish().unwrap();
                                assert!(recv.recv_packet().await.unwrap().is_none());
                            });
                        }
                        Some(Err(e))=>panic!("server transport failed: {e}"),None=>break,
                    },
                    finished=handlers.join_next(),if !handlers.is_empty()=>finished.unwrap().unwrap(),
                }
            }
            while let Some(done)=handlers.join_next().await{done.unwrap();}
            assert_eq!(opened,vec![StreamKind::Control,StreamKind::Data]);
        });
        let(mut client,conn)=h2::client::Builder::new().initial_window_size(1024*1024).initial_connection_window_size(1024*1024).handshake(client_io).await.unwrap();
        let client_task=tokio::spawn(conn);
        for(kind,id)in[(StreamKind::Control,1),(StreamKind::Data,2)]{
            let(mut send,mut recv)=open_client_stream(&mut client,kind,StreamEpoch(1)).await.unwrap();
            let input=packet(id,vec![id as u8;96*1024]);
            let mut expected=input.clone();expected.packet_type=PacketType::Response;
            let(sent,received)=tokio::join!(send.send_packet(&input),recv.recv_packet());sent.unwrap();assert_eq!(received.unwrap(),Some(expected));
            send.finish().unwrap();assert!(recv.recv_packet().await.unwrap().is_none());
        }
        drop(client);client_task.await.unwrap().unwrap();server_task.await.unwrap();
    }).await.expect("full TCP stack failed to complete within its test budget");
}
#[tokio::test]
async fn real_tcp_unknown_server_pin_never_yields_client_session_keys() {
    let (c, s) = handshake_pair(false).await;
    assert!(matches!(c, Err(HandshakeError::UntrustedServer)));
    // Server's own approval is not a client trust proof; no h2/business is opened.
    drop(s);
}
#[tokio::test]
async fn real_tcp_authenticated_session_bad_tag_closes_before_any_plaintext() {
    let (c, s) = handshake_pair(true).await;
    let mut c = c.unwrap();
    let s = s.unwrap();
    let mut tx = c.session_keys.into_client_record_layers();
    let mut record = tx.tx.seal(b"never deliver").unwrap();
    let last = record.len() - 1;
    record[last] ^= 0x80;
    c.io.write_all(&record).await.unwrap();
    let mut server = SecureIo::new(s.io, s.session_keys.into_server_record_layers());
    let mut out = [0u8; 64];
    let error = tokio::time::timeout(Duration::from_secs(2), server.read(&mut out))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(secure_io_failure(&error), Some(SecureIoFailure::AuthFailed));
    assert!(server.is_fatal());
    assert!(server.pair().is_none());
    let n = tokio::time::timeout(Duration::from_secs(2), c.io.read(&mut out))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(n, 0);
}
