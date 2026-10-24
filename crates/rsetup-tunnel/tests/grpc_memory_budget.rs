//! 内存预算定向回归（gRPC 编帧 + 信封，独立测试二进制）。
//!
//! 为什么单独一个二进制：`#[global_allocator]` 对整个测试二进制生效，且并发分配会互相污染，
//! 因此这里**必须**以 `--test-threads=1` 运行，并用 `MEASURE_LOCK` 串行化度量段。断言只针对
//! "是否出现 ≥ 某阈值的分配"与"分配总量"，不使用对并发敏感的小数精确值。
//!
//! 本文件唯一的 `unsafe` 是实现 `GlobalAlloc`（unsafe trait）以观测分配请求大小；
//! 生产代码 `envelope.rs` / `grpc.rs` 不含任何 `unsafe`。

use rsetup_tunnel::envelope::{MAX_ENVELOPE_SIZE, PacketType, TunnelPacket};
use rsetup_tunnel::grpc::{
    GRPC_HEADER_SIZE, GrpcDecoder, GrpcError, MAX_PACKETS_PER_PUSH, encode_message,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

/// 视为"大分配"的阈值：1 MiB。
const BIG_ALLOC_THRESHOLD: usize = 1024 * 1024;

static MAX_REQUEST: AtomicUsize = AtomicUsize::new(0);
static TOTAL_BYTES: AtomicUsize = AtomicUsize::new(0);
static BIG_REQUESTS: AtomicUsize = AtomicUsize::new(0);
static MEASURING: AtomicBool = AtomicBool::new(false);
static MEASURE_LOCK: Mutex<()> = Mutex::new(());

struct CountingAlloc;

fn record(size: usize) {
    if MEASURING.load(Ordering::Relaxed) {
        MAX_REQUEST.fetch_max(size, Ordering::Relaxed);
        TOTAL_BYTES.fetch_add(size, Ordering::Relaxed);
        if size >= BIG_ALLOC_THRESHOLD {
            BIG_REQUESTS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

// SAFETY: 仅做计数后原样委托给 `System`，不改变对齐/大小语义，也不持有跨线程状态
// （计数用原子量，度量段由 `MEASURE_LOCK` 串行化）。
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

#[derive(Debug, Clone, Copy)]
struct Metrics {
    max_request: usize,
    total_bytes: usize,
    big_requests: usize,
}

/// 串行化并度量一次闭包内的分配行为。
fn measure<T>(f: impl FnOnce() -> T) -> (T, Metrics) {
    let _guard: MutexGuard<'_, ()> = MEASURE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    MAX_REQUEST.store(0, Ordering::Relaxed);
    TOTAL_BYTES.store(0, Ordering::Relaxed);
    BIG_REQUESTS.store(0, Ordering::Relaxed);
    MEASURING.store(true, Ordering::Relaxed);
    let value = f();
    MEASURING.store(false, Ordering::Relaxed);
    (
        value,
        Metrics {
            max_request: MAX_REQUEST.load(Ordering::Relaxed),
            total_bytes: TOTAL_BYTES.load(Ordering::Relaxed),
            big_requests: BIG_REQUESTS.load(Ordering::Relaxed),
        },
    )
}

fn legal_packet(trace_id: u64) -> TunnelPacket {
    TunnelPacket {
        trace_id,
        packet_type: PacketType::Request,
        action: "device.status".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![1, 2, 3],
        metadata: BTreeMap::new(),
    }
}

const B62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

fn b62_key(v: u32, len: usize) -> Vec<u8> {
    let mut out = vec![b'0'; len];
    let mut i = len;
    let mut v = v;
    while i > 0 {
        i -= 1;
        out[i] = B62[(v % 62) as usize];
        v /= 62;
    }
    out
}

fn varint(mut val: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        if val < 0x80 {
            out.push(val as u8);
            return out;
        }
        out.push(((val as u8) & 0x7f) | 0x80);
        val >>= 7;
    }
}

fn field_varint(field: u32, val: u64) -> Vec<u8> {
    let mut out = varint((field as u64) << 3);
    out.extend(varint(val));
    out
}

fn field_bytes(field: u32, val: &[u8]) -> Vec<u8> {
    let mut out = varint(((field as u64) << 3) | 2);
    out.extend(varint(val.len() as u64));
    out.extend_from_slice(val);
    out
}

/// 恰好 200,007 字节、20,000 项 metadata 的 wire（构造成本计入 fixture，不计入度量段）。
fn metadata_heavy_wire(entries: usize) -> Vec<u8> {
    let mut raw = Vec::new();
    raw.extend(field_varint(1, 1));
    raw.extend(field_varint(2, 1));
    raw.extend(field_bytes(3, b"device.status"));
    let long = (entries - 12) as u32;
    for i in 0..entries as u32 {
        let key = if i < long {
            b62_key(i, 3)
        } else {
            b62_key(i - long, 2)
        };
        let mut entry = Vec::new();
        entry.extend(field_bytes(1, &key));
        entry.extend(field_bytes(2, b"v"));
        raw.extend(field_bytes(7, &entry));
    }
    assert_eq!(raw.len(), 200_007, "fixture 必须恰好 200,007 字节");
    raw
}

// ---------------------------------------------------------------------------

#[test]
fn encode_8mib_payload_does_not_allocate_it() {
    // 8MiB payload 的信元：必须在预算判定处失败，不产生与输入等大的第二次分配
    let mut packet = legal_packet(1);
    packet.payload = vec![0u8; 8 * 1024 * 1024];

    let (res, m) = measure(|| packet.encode());
    assert!(res.is_err(), "8MiB 信封必须被拒绝");
    assert_eq!(m.big_requests, 0, "不得出现 ≥1MiB 的分配: {m:?}");
    assert!(
        m.max_request < 4096,
        "超限 encode 的最大分配为 {} 字节: {m:?}",
        m.max_request
    );
}

#[test]
fn push_8mib_chunk_does_not_allocate_it() {
    // 8MiB chunk（长度前缀声明 8MiB）：只看 5 字节头即失败
    let mut chunk = vec![0u8; 5 + 8 * 1024 * 1024];
    chunk[1..5].copy_from_slice(&(8u32 * 1024 * 1024).to_be_bytes());
    let mut decoder = GrpcDecoder::new();

    let (res, m) = measure(|| decoder.push(&chunk));
    assert!(matches!(res, Err(GrpcError::MessageTooLarge(_))), "{res:?}");
    assert_eq!(m.big_requests, 0, "不得先缓存整个 chunk: {m:?}");
    assert!(
        m.total_bytes < 64 * 1024,
        "拒收 8MiB chunk 的分配总量 {m:?} 过大"
    );
}

#[test]
fn sticky_10000_packets_stay_within_batch_and_memory_budget() {
    // 10,000 个小包粘成一个 chunk：每次输出 ≤ MAX_PACKETS_PER_PUSH，且分配总量受限
    let count = 10_000usize;
    let mut chunk = Vec::new();
    for i in 0..count as u64 {
        chunk.extend(encode_message(&legal_packet(i + 1)).unwrap());
    }
    assert!(chunk.len() < 1024 * 1024, "fixture 本身应保持很小");

    let mut decoder = GrpcDecoder::new();
    let (delivered, m) = measure(|| {
        let mut input = chunk.as_slice();
        let mut total = 0usize;
        let mut max_per_call = 0usize;
        while !input.is_empty() {
            let out = decoder.push(input).expect("合法粘包不得报错");
            max_per_call = max_per_call.max(out.packets().len());
            total += out.packets().len();
            input = out.rest();
        }
        (total, max_per_call)
    });

    assert_eq!(delivered.0, count, "10,000 包必须全部交付");
    assert!(
        delivered.1 <= MAX_PACKETS_PER_PUSH,
        "单次输出 {} 个包，突破批量预算",
        delivered.1
    );
    assert_eq!(m.big_requests, 0, "不得出现 ≥1MiB 分配: {m:?}");
    // 旧实现一次 push 直接吐出 10,000 个包（数十 MiB 级 Vec）；预算内应远小于该量级
    assert!(
        m.total_bytes < 4 * 1024 * 1024,
        "解码 10,000 个信包的分配总量 {m:?} 过大"
    );
}

#[test]
fn metadata_heavy_decode_fails_in_small_constant_budget() {
    // 200,007 字节 / 20,000 项 metadata：必须在小常量预算内失败
    let raw = metadata_heavy_wire(20_000);

    let (res, m) = measure(|| TunnelPacket::decode(&raw));
    assert!(res.is_err(), "超预算 metadata 必须拒绝");
    assert_eq!(m.big_requests, 0, "{m:?}");
    assert!(
        m.max_request < 64 * 1024,
        "解码失败前最大分配 {} 字节: {m:?}",
        m.max_request
    );
    assert!(
        m.total_bytes < 64 * 1024,
        "解码失败累计分配 {:?} 超过小常量预算",
        m
    );
}

#[test]
fn partial_large_frame_buffer_stays_under_512kib_plus_5() {
    // 头声明 512KiB，实际一次到达 8MiB：允许填满当前帧，但绝不超过 5 + 512KiB
    let mut chunk = vec![0u8; GRPC_HEADER_SIZE];
    chunk[1..5].copy_from_slice(&(MAX_ENVELOPE_SIZE as u32).to_be_bytes());
    chunk.resize(GRPC_HEADER_SIZE + MAX_ENVELOPE_SIZE + 8 * 1024 * 1024, 0u8);

    let mut decoder = GrpcDecoder::new();
    let (_res, m) = measure(|| decoder.push(&chunk));
    assert!(
        m.big_requests <= 2,
        "填充当前帧至多 1~2 次 ~512KiB 级分配（含容量翻倍），实际 {m:?}"
    );
    assert!(
        m.max_request < 4 * 1024 * 1024,
        "最大分配 {} 字节，说明把整个 chunk 复制进了缓冲",
        m.max_request
    );
    // 512KiB 帧在此 fixture 中内容非法，出错后缓冲必须清空
    assert_eq!(decoder.buffered_len(), 0, "致命错误后不得保留缓冲");
}

#[test]
fn baseline_allocations_are_actually_observed() {
    // 自检：计数器确实在工作，避免前面的预算断言因"什么都没测到"而虚过
    let (_, m) = measure(|| {
        let v: Vec<u8> = vec![0u8; 2 * 1024 * 1024];
        std::hint::black_box(&v);
    });
    assert!(
        m.big_requests >= 1 && m.max_request >= 2 * 1024 * 1024,
        "分配计数器失效: {m:?}"
    );
}
