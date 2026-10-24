//! HTTP/2长生命gRPC流：有界逐包解码、真实流控、取消时重置受影响流。
//! 此层不分配trace、不调度重开；接收错误由session owner重置配对sender。
use super::types::StreamEpoch;
use crate::envelope::{StreamKind, TunnelPacket};
use crate::grpc::{GrpcDecoder, GrpcError, InputCursor, encode_message};
use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, Version};
use std::future::poll_fn;
use thiserror::Error;

pub const CONTROL_PATH: &str = "/custom_tunnel.TunnelService/OpenControl";
pub const DATA_PATH: &str = "/custom_tunnel.TunnelService/OpenData";
const SEND_CHUNK: usize = 16 * 1024;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum StreamError {
    #[error("HTTP/2 transport failed")]
    Transport,
    #[error("HTTP/2 stream reset (reason {0})")]
    Reset(u32),
    #[error("gRPC stream framing violated")]
    Grpc,
    #[error("gRPC response headers or trailers invalid")]
    BadResponse,
    #[error("gRPC stream ended with status {0}")]
    GrpcStatus(u32),
    /// **流级**协议错误：对端在**本流**上送来的应用包不符合信封约束
    /// （`EnvelopeDecode`，含 §6.7.2 规定的非法 `tunnel.ping` 探测请求，如 payload
    /// 不是恰好 16B）。按 §6.7.2 只能终止它所在的那条流，绝不升级成连接级结论。
    #[error("application packet violates envelope constraints (stream scoped)")]
    StreamProtocol,
    /// **连接级**结构违规：§6.3 的流路由规则被打破（TYPE_SYSTEM 出现在非控制流）。
    /// 这条约束是结构性的，不得降级为流级。
    ///
    /// 口径澄清（别把本注释反过来读）：这里覆盖的是**已解码包**的路由归属。未知
    /// System action 在公共 `TunnelPacket::decode` 的 `validate_fields` 阶段就被拒，
    /// 落到 [`Self::StreamProtocol`]（流级），走不到本变体；解码前的任何字段违规都
    /// 不能据本条升级成连接级。
    #[error("packet violates stream routing")]
    Protocol,
    #[error("stream is closed")]
    Closed,
}

impl StreamError {
    /// 该失败是否构成**连接级**结构违规。
    ///
    /// 归属必须由类型给出：调用方不得靠错误 Display 文本或自行维护的 bool 猜测
    /// 安全级别。应用包内容非法（[`Self::StreamProtocol`]）只终结本流；只有结构性
    /// 路由违规（[`Self::Protocol`]）才终结整条连接。
    pub fn connection_fatal(&self) -> bool {
        matches!(self, Self::Protocol)
    }
}
fn h2_error(error: h2::Error) -> StreamError {
    if error.is_reset() {
        StreamError::Reset(u32::from(
            error.reason().unwrap_or(h2::Reason::INTERNAL_ERROR),
        ))
    } else {
        StreamError::Transport
    }
}
fn content_type(headers: &HeaderMap) -> bool {
    headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "application/grpc" || v.starts_with("application/grpc+"))
}

/// gRPC Status=1*DIGIT，十进制无前导零；不接受Rust整数parser额外允许的+。
fn grpc_status(headers: &HeaderMap) -> Result<u32, StreamError> {
    if headers.get_all("grpc-status").iter().count() != 1 {
        return Err(StreamError::BadResponse);
    }
    let value = headers
        .get("grpc-status")
        .and_then(|v| v.to_str().ok())
        .ok_or(StreamError::BadResponse)?;
    if value.is_empty()
        || !value.bytes().all(|b| b.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(StreamError::BadResponse);
    }
    value.parse().map_err(|_| StreamError::BadResponse)
}

pub struct StreamSender {
    inner: h2::SendStream<Bytes>,
    kind: StreamKind,
    epoch: StreamEpoch,
    server: bool,
    closed: bool,
}
impl StreamSender {
    pub fn kind(&self) -> StreamKind {
        self.kind
    }
    pub fn epoch(&self) -> StreamEpoch {
        self.epoch
    }
    pub fn stream_id(&self) -> u32 {
        self.inner.stream_id().as_u32()
    }
    pub fn is_closed(&self) -> bool {
        self.closed
    }
    pub fn reset(&mut self, reason: h2::Reason) {
        // closed表示发送半边已EOS；它不能阻止对仍活跃接收半边的整流RST。
        self.inner.send_reset(reason);
        self.closed = true;
    }
    /// 整条gRPC消息有且仅有一个sender持有写所有权。取消future则reset此流，
    /// 防止已排队的部分gRPC帧与后续消息拼接；不能自动重放结果未知的变更。
    pub async fn send_packet(&mut self, packet: &TunnelPacket) -> Result<(), StreamError> {
        if self.closed {
            return Err(StreamError::Closed);
        }
        packet
            .validate(self.kind)
            .map_err(|_| StreamError::Protocol)?;
        let data = Bytes::from(encode_message(packet).map_err(|_| StreamError::Grpc)?);
        let mut guard = SendGuard {
            sender: self,
            finished: false,
        };
        let mut offset = 0;
        while offset < data.len() {
            if guard.sender.inner.capacity() == 0 {
                guard
                    .sender
                    .inner
                    .reserve_capacity((data.len() - offset).min(SEND_CHUNK));
                match poll_fn(|cx| guard.sender.inner.poll_capacity(cx)).await {
                    Some(Ok(0)) => {
                        tokio::task::yield_now().await;
                        continue;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(h2_error(error)),
                    None => return Err(StreamError::Closed),
                }
            }
            let n = guard
                .sender
                .inner
                .capacity()
                .min(data.len() - offset)
                .min(SEND_CHUNK);
            guard
                .sender
                .inner
                .send_data(data.slice(offset..offset + n), false)
                .map_err(h2_error)?;
            offset += n;
        }
        guard.finished = true;
        Ok(())
    }
    /// Server的grpc-status仅随HTTP/2流EOS发送，不随每个逻辑RPC发送。
    pub fn finish(&mut self) -> Result<(), StreamError> {
        if self.closed {
            return Ok(());
        }
        if self.server {
            let mut trailers = HeaderMap::new();
            trailers.insert("grpc-status", HeaderValue::from_static("0"));
            self.inner.send_trailers(trailers).map_err(h2_error)?;
        } else {
            self.inner.send_data(Bytes::new(), true).map_err(h2_error)?;
        }
        self.closed = true;
        Ok(())
    }
}
struct SendGuard<'a> {
    sender: &'a mut StreamSender,
    finished: bool,
}
impl Drop for SendGuard<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.sender.reset(h2::Reason::CANCEL);
        }
    }
}

#[derive(Clone, Copy)]
enum ReceiveTerminal {
    Open,
    Eof,
    Failed(StreamError),
}

pub struct StreamReceiver {
    inner: h2::RecvStream,
    decoder: GrpcDecoder,
    chunk: Option<Bytes>,
    offset: usize,
    kind: StreamKind,
    epoch: StreamEpoch,
    response: bool,
    terminal: ReceiveTerminal,
}
impl StreamReceiver {
    pub fn kind(&self) -> StreamKind {
        self.kind
    }
    pub fn epoch(&self) -> StreamEpoch {
        self.epoch
    }
    pub fn stream_id(&self) -> u32 {
        self.inner.stream_id().as_u32()
    }
    /// 返回一包即交调用方处理/释放，未消费尾部留在当前有界h2 DATA chunk，
    /// 不使用64包便捷批量API；所有packet在handler前校验stream归属。
    pub async fn recv_packet(&mut self) -> Result<Option<TunnelPacket>, StreamError> {
        match self.terminal {
            ReceiveTerminal::Eof => return Ok(None),
            ReceiveTerminal::Failed(error) => return Err(error),
            ReceiveTerminal::Open => {}
        }
        let result = self.recv_open().await;
        match &result {
            Ok(None) => self.terminal = ReceiveTerminal::Eof,
            Err(error) => {
                self.terminal = ReceiveTerminal::Failed(*error);
                self.chunk = None;
                self.offset = 0;
                self.decoder = GrpcDecoder::new();
            }
            Ok(Some(_)) => {}
        }
        result
    }
    async fn recv_open(&mut self) -> Result<Option<TunnelPacket>, StreamError> {
        loop {
            if let Some(chunk) = self.chunk.as_ref() {
                let mut cursor = InputCursor::new(&chunk[self.offset..]);
                let parsed = self.decoder.next_packet(&mut cursor);
                self.offset += cursor.consumed();
                if self.offset == chunk.len() {
                    self.chunk = None;
                    self.offset = 0;
                }
                let packet = match parsed {
                    Ok(p) => p,
                    // 解码失败 = 对端送来的**应用包**本身不合法（字段约束不符，含
                    // 非 16B 的 tunnel.ping）。§6.7.2：流级协议错误，只终结本流。
                    // 注意公共 decode 的强校验一个都没放宽，这里只是把归属写进类型。
                    Err(GrpcError::EnvelopeDecode(_)) => {
                        return Err(StreamError::StreamProtocol);
                    }
                    // 帧框架层失败（压缩位/超长/截断/中毒）：保持既有 Grpc 归类，
                    // 同样是流级，不是连接级。
                    Err(_) => {
                        return Err(StreamError::Grpc);
                    }
                };
                if let Some(packet) = packet {
                    if packet.validate(self.kind).is_err() {
                        // 能解码但 validate 失败，只可能是 §6.3 路由规则：连接级。
                        return Err(StreamError::Protocol);
                    }
                    return Ok(Some(packet));
                }
            }
            match self.inner.data().await {
                Some(Ok(data)) => {
                    self.inner
                        .flow_control()
                        .release_capacity(data.len())
                        .map_err(h2_error)?;
                    if data.is_empty() {
                        continue;
                    }
                    self.chunk = Some(data);
                    self.offset = 0;
                }
                Some(Err(error)) => {
                    return Err(h2_error(error));
                }
                None => {
                    self.decoder.finish().map_err(|_| StreamError::Grpc)?;
                    if self.response {
                        let trailers = self
                            .inner
                            .trailers()
                            .await
                            .map_err(h2_error)?
                            .ok_or(StreamError::BadResponse)?;
                        let status = grpc_status(&trailers)?;
                        if status != 0 {
                            return Err(StreamError::GrpcStatus(status));
                        }
                    }
                    return Ok(None);
                }
            }
        }
    }
}
fn stream_parts(
    send: h2::SendStream<Bytes>,
    recv: h2::RecvStream,
    kind: StreamKind,
    epoch: StreamEpoch,
    server: bool,
) -> (StreamSender, StreamReceiver) {
    (
        StreamSender {
            inner: send,
            kind,
            epoch,
            server,
            closed: false,
        },
        StreamReceiver {
            inner: recv,
            kind,
            epoch,
            response: !server,
            decoder: GrpcDecoder::new(),
            chunk: None,
            offset: 0,
            terminal: ReceiveTerminal::Open,
        },
    )
}
/// 调用方必须持续驱动client Connection；初建按Control然后Data调用。
pub async fn open_client_stream(
    client: &mut h2::client::SendRequest<Bytes>,
    kind: StreamKind,
    epoch: StreamEpoch,
) -> Result<(StreamSender, StreamReceiver), StreamError> {
    poll_fn(|cx| client.poll_ready(cx))
        .await
        .map_err(h2_error)?;
    let path = match kind {
        StreamKind::Control => CONTROL_PATH,
        StreamKind::Data => DATA_PATH,
    };
    let request = Request::builder()
        .method(Method::POST)
        .version(Version::HTTP_2)
        .uri(format!("http://rsetup{path}"))
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(())
        .map_err(|_| StreamError::Protocol)?;
    let (response, mut send) = client.send_request(request, false).map_err(h2_error)?;
    let response = match response.await {
        Ok(v) => v,
        Err(error) => {
            send.send_reset(h2::Reason::CANCEL);
            return Err(h2_error(error));
        }
    };
    if response.status() != StatusCode::OK || !content_type(response.headers()) {
        send.send_reset(h2::Reason::PROTOCOL_ERROR);
        return Err(StreamError::BadResponse);
    }
    // ResponseFuture消费首HEADERS后，只有真实EOS且无待收DATA/trailers时才是
    // Trailers-Only；不能因普通首headers携带status就提前结束长流。
    let terminal = if response.body().is_end_stream() {
        match grpc_status(response.headers()) {
            Ok(0) => Some(ReceiveTerminal::Eof),
            Ok(status) => Some(ReceiveTerminal::Failed(StreamError::GrpcStatus(status))),
            Err(error) => {
                send.send_reset(h2::Reason::PROTOCOL_ERROR);
                return Err(error);
            }
        }
    } else {
        None
    };
    let (send, mut recv) = stream_parts(send, response.into_body(), kind, epoch, false);
    if let Some(terminal) = terminal {
        recv.terminal = terminal;
    }
    Ok((send, recv))
}
/// 返回None表示已对非法/未知RPC作流级拒绝，连接本身仍可处理其它流。
/// 同类流顶替及代际推进由session driver执行，不在此处暗自注册新代际。
pub fn accept_server_stream(
    request: Request<h2::RecvStream>,
    mut response: h2::server::SendResponse<Bytes>,
    epoch: StreamEpoch,
) -> Result<Option<(StreamKind, StreamSender, StreamReceiver)>, StreamError> {
    let te = request
        .headers()
        .get("te")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("trailers"));
    if request.method() != Method::POST || !content_type(request.headers()) || !te {
        response.send_reset(h2::Reason::PROTOCOL_ERROR);
        return Ok(None);
    }
    let kind = match request.uri().path() {
        CONTROL_PATH => StreamKind::Control,
        DATA_PATH => StreamKind::Data,
        _ => {
            let headers = Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/grpc")
                .body(())
                .map_err(|_| StreamError::Protocol)?;
            let mut send = response.send_response(headers, false).map_err(h2_error)?;
            let mut trailers = HeaderMap::new();
            trailers.insert("grpc-status", HeaderValue::from_static("12"));
            send.send_trailers(trailers).map_err(h2_error)?;
            return Ok(None);
        }
    };
    let headers = Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/grpc")
        .body(())
        .map_err(|_| StreamError::Protocol)?;
    let send = response.send_response(headers, false).map_err(h2_error)?;
    let (send, recv) = stream_parts(send, request.into_body(), kind, epoch, true);
    Ok(Some((kind, send, recv)))
}
