//! 只读来源与变更隔离见证：只读来源必须返回**类型化 DTO**，
//! 从而让 BoardAgent 能执行结构校验；因此不需要 wire 字节，也不受未获批 codec 影响。

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerContext {
    pub authenticated_controller_key: [u8; 32],
}

pub trait ReadOnlySource: Send + Sync {
    fn capabilities(&self) -> rsetup_protocol::wire::Capabilities;
    fn status(&self) -> rsetup_protocol::wire::DeviceStatus;
    fn clock(&self) -> rsetup_protocol::wire::ClockSample;
}

/// 变更隔离见证：测试用它证明白名单拒绝路径从未触达任何变更入口。
pub trait ReadOnlyMutationGuard {
    fn mutation_calls(&self) -> usize;
}
