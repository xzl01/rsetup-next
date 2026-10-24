//! 封闭入站动作白名单：仅 6 项入站请求动作。
//! device.task.result 是仅板端→中控的 TYPE_EVENT，绝不纳入入站白名单。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    ReadOnly,
    JournalRead,
    RebootPrepare,
    RebootExecute,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionSpec {
    pub name: &'static str,
    pub kind: ActionKind,
    pub reserved_prefix: bool,
}

/// 封闭入站动作白名单：仅 6 项入站请求动作。
pub const ACTION_ALLOWLIST: &[ActionSpec] = &[
    ActionSpec {
        name: "device.capabilities.get",
        kind: ActionKind::ReadOnly,
        reserved_prefix: false,
    },
    ActionSpec {
        name: "device.status.get",
        kind: ActionKind::ReadOnly,
        reserved_prefix: false,
    },
    ActionSpec {
        name: "device.clock.get",
        kind: ActionKind::ReadOnly,
        reserved_prefix: false,
    },
    ActionSpec {
        name: "device.task.get",
        kind: ActionKind::JournalRead,
        reserved_prefix: false,
    },
    ActionSpec {
        name: "device.reboot.prepare",
        kind: ActionKind::RebootPrepare,
        reserved_prefix: false,
    },
    ActionSpec {
        name: "device.reboot.execute",
        kind: ActionKind::RebootExecute,
        reserved_prefix: false,
    },
];

/// 板端主动事件动作名称常量（仅供出站事件使用，不参与入站查表）
pub const TASK_RESULT_EVENT_ACTION: &str = "device.task.result";

/// `device.reboot.execute` 动作名常量：出站 `device.task.result` 事件的唯一触发条件。
pub const REBOOT_EXECUTE_ACTION: &str = "device.reboot.execute";

/// 白名单之外的动作（含空串、超长、`tunnel.` 保留前缀、板端出站事件名及任意变更动作）一律 None。
/// 严格 `==` 比较，不做大小写/前缀/模糊匹配。
pub fn lookup_action(action: &str) -> Option<&'static ActionSpec> {
    ACTION_ALLOWLIST.iter().find(|spec| spec.name == action)
}

/// `action.starts_with("tunnel.")`
pub fn is_reserved(action: &str) -> bool {
    action.starts_with("tunnel.")
}

pub fn allowed_action_names() -> Vec<&'static str> {
    ACTION_ALLOWLIST.iter().map(|spec| spec.name).collect()
}
