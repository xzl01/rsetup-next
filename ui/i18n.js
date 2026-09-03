(() => {
  const dictionaries = {
    en: {
      "document.title": "rsetup · SBC Control Center",
      "document.description": "A calm, local-first control center for your SBC",
      "skip": "Skip to control center",
      "brand.channel": "LOCAL CONTROL",
      "brand.open": "Open overview",
      "board.detecting": "Detecting board",
      "soc.detecting": "Detecting vendor",
      "soc.vendorLabel": "{vendor} system-on-chip",
      "debug.open": "Open device simulator",
      "debug.title": "Device simulator",
      "debug.synthetic": "Synthetic data only",
      "debug.device": "Current device",
      "debug.provider": "Provider device",
      "debug.custom": "Custom device…",
      "debug.product": "Product name",
      "debug.vendor": "SoC vendor",
      "debug.soc": "SoC model",
      "debug.architecture": "Architecture",
      "debug.hostname": "Hostname",
      "debug.apply": "Apply custom device",
      "debug.reset": "Use provider device",
      "debug.changed": "Now simulating {device}.",
      "command.open": "Open command palette",
      "command.label": "Command",
      "command.search": "Search commands",
      "command.placeholder": "Go to a section or run a procedure…",
      "command.section": "SECTION",
      "command.empty": "No matching section or procedure.",
      "theme.toggle": "Toggle color theme",
      "language.toggle": "切换为中文",
      "language.short": "中文",
      "nav.label": "Control center sections",
      "route.overview": "Overview",
      "route.overview.detail": "Live device operating view",
      "route.system": "System",
      "route.system.detail": "Services, storage, and identity",
      "route.network": "Network",
      "route.network.detail": "Interfaces and transfer paths",
      "route.hardware": "Hardware",
      "route.hardware.detail": "Detected SBC capabilities",
      "route.workflows": "Operations",
      "route.workflows.detail": "Maintenance, access, and system changes",
      "route.activity": "Activity",
      "route.activity.detail": "Observed and executed events",
      "overview.title": "Your board at a glance",
      "provider": "Provider",
      "connecting": "Connecting",
      "health.title": "Board health",
      "health.awaiting": "awaiting first signal",
      "local.device": "Local device",
      "board.diagram": "Abstract single-board computer diagram",
      "syncing": "SYNCING",
      "provider.reading": "Reading local provider",
      "metric.cpu": "CPU LOAD",
      "metric.memory": "MEMORY",
      "metric.thermal": "THERMAL",
      "metric.storage": "ROOT MEDIA",
      "metric.sensorPending": "sensor pending",
      "metric.mountPending": "mount pending",
      "metric.uptime": "uptime",
      "metric.kernel": "kernel",
      "metric.architecture": "architecture",
      "operations.title": "Guided operations",
      "operations.openAll": "Open all",
      "operations.loading": "Loading action catalog",
      "operations.empty": "No guided operation is available.",
      "operations.unavailable": "Unavailable",
      "operations.unavailableReason": "Unavailable: {reason}",
      "capabilities.title": "Available capabilities",
      "capabilities.signals": "{count} signals",
      "capabilities.online": "{available}/{total} online",
      "activity.recent": "Recent activity",
      "activity.full": "Full history",
      "activity.waiting": "Waiting for control-plane activity",
      "activity.empty": "No control-plane activity recorded.",
      "activity.simulated": "SIMULATED",
      "system.title": "System overview",
      "sources.title": "Package mirrors",
      "sources.root": "Administrator authorization",
      "sources.distribution": "Distribution",
      "sources.systemCurrent": "System source",
      "sources.radxaCurrent": "Radxa source",
      "sources.managedFiles": "Managed files",
      "sources.choose": "Choose a trusted mirror",
      "sources.loading": "Detecting mirrors…",
      "sources.providerScope": "{location} · system {system} · Radxa {radxa}",
      "sources.supported": "supported",
      "sources.unchanged": "unchanged",
      "sources.preview": "Preview changes",
      "sources.previewing": "Building preview…",
      "sources.changeCount": "{entries} entries · {files} files",
      "sources.noChanges": "No managed source entry needs to change.",
      "sources.replacements": "{count} replacement(s)",
      "sources.warning.radxa_only": "This provider changes Radxa sources only; system sources stay unchanged.",
      "sources.warning.system_only": "This provider changes system sources only; Radxa sources stay unchanged.",
      "sources.warning.already_selected": "Managed sources already use this provider.",
      "sources.warning.no_managed_sources": "No safely managed source entry was detected.",
      "sources.warning.dry_run": "Dry-run mode is active; no system file will be written.",
      "sources.confirm": "Reviewed. Back up, switch, and refresh package indexes.",
      "sources.apply": "Apply mirror",
      "sources.applying": "Applying mirror…",
      "sources.runState": "CONTROL PLANE / RUNNING\nCreating backups and refreshing package metadata.",
      "sources.planned": "DRY RUN / PLAN READY",
      "sources.applied": "MIRROR APPLIED",
      "sources.rolledBack": "REFRESH FAILED / ROLLED BACK",
      "sources.backups": "Backups: {count}",
      "sources.focused": "Package mirror manager is ready.",
      "sources.unavailable": "Package mirror state is unavailable.",
      "provider.official": "Distribution official",
      "provider.mixed": "Mixed providers",
      "provider.unknown": "Not detected",
      "location.global": "Global",
      "location.china": "China",
      "location.hefei": "Hefei, CN",
      "location.beijing": "Beijing, CN",
      "location.chongqing": "Chongqing, CN",
      "location.lanzhou": "Lanzhou, CN",
      "location.wuhan": "Wuhan, CN",
      "location.jinan": "Jinan, CN",
      "location.nanjing": "Nanjing, CN",
      "location.nanyang": "Nanyang, CN",
      "refresh.probe": "Refresh probe",
      "refresh.now": "Refresh now",
      "services.title": "Service signals",
      "storage.title": "Storage map",
      "identity.title": "System identity",
      "network.title": "Network connections",
      "hardware.title": "Hardware capabilities",
      "workflows.title": "System operations",
      "workflows.group.system": "System & power",
      "workflows.group.network": "Network access",
      "workflows.group.services": "Services & containers",
      "workflows.group.storage": "Storage",
      "workflows.group.other": "Other",
      "workflows.group.count": "Operations · {count}",
      "activity.title": "Activity record",
      "risk.safe": "Safe",
      "risk.guarded": "Guarded",
      "risk.high": "High risk",
      "risk.critical": "Critical",
      "status.starting": "Control plane starting",
      "status.connecting": "Connecting to the local provider",
      "status.syncing": "Synchronizing local state",
      "status.reading": "Reading board identity and kernel signals",
      "status.unavailable": "Control plane unavailable",
      "status.demoOnline": "Demo provider online",
      "status.localStable": "Local device link stable",
      "status.demoDetail": "Synthetic values · mutating operations dry-run",
      "status.localDetail": "{product} · {networks} network path(s) · {capabilities} capability signal(s)",
      "toast.probeComplete": "Probe complete",
      "toast.demoLoaded": "Synthetic SBC telemetry loaded.",
      "toast.localCurrent": "Local board state is current.",
      "toast.refreshFailed": "Unable to refresh",
      "core.demo": "Demo connected",
      "core.online": "Device online",
      "temperature.none": "no readable thermal zone",
      "temperature.normal": "inside operating envelope",
      "temperature.hot": "thermal headroom reduced",
      "storage.unavailable": "root mount unavailable",
      "updated": "updated {time}",
      "empty.services": "No service state is available from this provider.",
      "empty.storage": "No mounted block storage was reported.",
      "empty.network": "No non-loopback network interface detected",
      "storage.used": "{percent} used",
      "storage.of": "{used} of {total} · {name}",
      "identity.product": "Product",
      "identity.hostname": "Hostname",
      "identity.soc": "SoC",
      "identity.system": "System",
      "identity.kernel": "Kernel",
      "identity.nodeId": "Node ID",
      "network.address": "ADDRESS",
      "network.type": "TYPE",
      "network.received": "RECEIVED",
      "network.transmitted": "TRANSMITTED",
      "network.notAssigned": "not assigned",
      "steps": "{count} steps",
      "drawer.operation": "Operation",
      "drawer.close": "Close operation",
      "drawer.estimated": "estimated",
      "drawer.privilege": "privilege",
      "drawer.procedure": "Procedure",
      "drawer.confirm": "I understand this operation can change the local board.",
      "drawer.run": "Run procedure",
      "drawer.running": "Procedure running…",
      "drawer.root": "root required",
      "drawer.user": "user session",
      "drawer.runState": "CONTROL PLANE / RUNNING\nWaiting for the local executor.",
      "drawer.dryRun": "DRY RUN",
      "drawer.result": "RESULT",
      "drawer.failed": "PROCEDURE FAILED",
      "run.status.succeeded": "SUCCEEDED",
      "run.status.failed": "FAILED",
      "run.status.planned": "PLANNED",
      "run.status.running": "RUNNING",
      "run.plannedSteps": "Planned steps",
      "toast.dryRun": "Dry run complete",
      "toast.complete": "Procedure complete",
      "toast.failed": "Procedure failed",
      "relative.now": "just now",
      "relative.minutes": "{count}m ago",
      "relative.hours": "{count}h ago",
      "relative.days": "{count}d ago",
      "duration.days": "{days}d {hours}h",
      "duration.hours": "{hours}h {minutes}m",
      "duration.minutes": "{minutes}m",
      "api.unknown_action": "The requested operation no longer exists.",
      "api.confirmation_required": "Confirm the operation before running it.",
      "api.action_unavailable": "This operation is unavailable on the current device.",
      "api.input_required": "Choose a mirror in System → Package mirrors before running this procedure.",
      "api.unknown_mirror": "The selected mirror is not in the trusted catalog.",
      "api.sources_unsupported": "APT source management is unavailable on this system.",
      "api.plan_required": "Preview this mirror change before applying it.",
      "api.stale_plan": "Source files changed after preview. Build and review a fresh plan.",
      "api.root_required": "This operation requires root privileges.",
      "api.authorization_failed": "Administrator authorization was not completed.",
      "api.internal_error": "The local control plane encountered an error.",
      "api.transport_failure": "Unable to reach the local control plane.",
      "api.http_failure": "The local control plane returned HTTP {status}.",
    },
    "zh-CN": {
      "document.title": "rsetup · SBC 控制中心",
      "document.description": "简洁、可靠、本机优先的 SBC 控制中心",
      "skip": "跳到控制中心",
      "brand.channel": "本机控制",
      "brand.open": "打开总览",
      "board.detecting": "正在识别开发板",
      "soc.detecting": "正在识别厂商",
      "soc.vendorLabel": "{vendor} 系统级芯片",
      "debug.open": "打开设备模拟器",
      "debug.title": "设备模拟器",
      "debug.synthetic": "仅用于模拟数据",
      "debug.device": "当前设备",
      "debug.provider": "数据源设备",
      "debug.custom": "自定义设备…",
      "debug.product": "产品名称",
      "debug.vendor": "SoC 厂商",
      "debug.soc": "SoC 型号",
      "debug.architecture": "处理器架构",
      "debug.hostname": "主机名",
      "debug.apply": "应用自定义设备",
      "debug.reset": "恢复数据源设备",
      "debug.changed": "当前模拟：{device}",
      "command.open": "打开快捷指令",
      "command.label": "快捷指令",
      "command.search": "搜索指令",
      "command.placeholder": "前往功能区或运行操作…",
      "command.section": "功能区",
      "command.empty": "没有匹配的功能区或操作。",
      "theme.toggle": "切换明暗主题",
      "language.toggle": "Switch to English",
      "language.short": "EN",
      "nav.label": "控制中心功能区",
      "route.overview": "总览",
      "route.overview.detail": "查看设备实时运行状态",
      "route.system": "系统",
      "route.system.detail": "服务、存储与系统身份",
      "route.network": "网络",
      "route.network.detail": "接口与数据传输路径",
      "route.hardware": "硬件",
      "route.hardware.detail": "已检测的 SBC 能力",
      "route.workflows": "管理",
      "route.workflows.detail": "维护、访问与系统变更",
      "route.activity": "记录",
      "route.activity.detail": "已观察和执行的事件",
      "overview.title": "开发板状态一目了然",
      "provider": "数据源",
      "connecting": "正在连接",
      "health.title": "开发板健康状态",
      "health.awaiting": "等待首次状态信号",
      "local.device": "本机设备",
      "board.diagram": "单板计算机结构示意图",
      "syncing": "同步中",
      "provider.reading": "正在读取本机数据源",
      "metric.cpu": "处理器负载",
      "metric.memory": "内存占用",
      "metric.thermal": "温度",
      "metric.storage": "根存储",
      "metric.sensorPending": "等待温度传感器",
      "metric.mountPending": "等待挂载信息",
      "metric.uptime": "运行时间",
      "metric.kernel": "内核",
      "metric.architecture": "架构",
      "operations.title": "引导式操作",
      "operations.openAll": "查看全部",
      "operations.loading": "正在加载操作目录",
      "operations.empty": "当前没有可用的引导式操作。",
      "operations.unavailable": "当前不可用",
      "operations.unavailableReason": "当前不可用：{reason}",
      "capabilities.title": "可用硬件能力",
      "capabilities.signals": "{count} 项能力",
      "capabilities.online": "{available}/{total} 可用",
      "activity.recent": "最近记录",
      "activity.full": "完整记录",
      "activity.waiting": "等待控制中心记录",
      "activity.empty": "还没有控制中心记录。",
      "activity.simulated": "模拟",
      "system.title": "系统概况",
      "sources.title": "软件源管理",
      "sources.root": "需要管理员授权",
      "sources.distribution": "系统发行版",
      "sources.systemCurrent": "系统软件源",
      "sources.radxaCurrent": "Radxa 软件源",
      "sources.managedFiles": "受管文件",
      "sources.choose": "选择可信镜像",
      "sources.loading": "正在检测镜像…",
      "sources.providerScope": "{location} · 系统源{system} · Radxa 源{radxa}",
      "sources.supported": "支持",
      "sources.unchanged": "不修改",
      "sources.preview": "预览变更",
      "sources.previewing": "正在生成预览…",
      "sources.changeCount": "{entries} 处条目 · {files} 个文件",
      "sources.noChanges": "当前受管软件源无需修改。",
      "sources.replacements": "{count} 处替换",
      "sources.warning.radxa_only": "此镜像只会修改 Radxa 软件源，系统软件源保持不变。",
      "sources.warning.system_only": "此镜像只会修改系统软件源，Radxa 软件源保持不变。",
      "sources.warning.already_selected": "受管软件源已经使用该镜像。",
      "sources.warning.no_managed_sources": "未检测到可安全管理的软件源条目。",
      "sources.warning.dry_run": "当前为预演模式，不会写入系统文件。",
      "sources.confirm": "已检查变更；备份、切换并刷新软件包索引。",
      "sources.apply": "应用镜像",
      "sources.applying": "正在应用镜像…",
      "sources.runState": "控制中心 / 执行中\n正在创建备份并刷新软件包元数据。",
      "sources.planned": "预演 / 计划已生成",
      "sources.applied": "镜像已应用",
      "sources.rolledBack": "刷新失败 / 已回滚",
      "sources.backups": "备份文件：{count} 个",
      "sources.focused": "软件源管理已就绪。",
      "sources.unavailable": "无法读取软件源状态。",
      "provider.official": "发行版官方源",
      "provider.mixed": "混合镜像源",
      "provider.unknown": "未检测到",
      "location.global": "全球",
      "location.china": "中国",
      "location.hefei": "中国·合肥",
      "location.beijing": "中国·北京",
      "location.chongqing": "中国·重庆",
      "location.lanzhou": "中国·兰州",
      "location.wuhan": "中国·武汉",
      "location.jinan": "中国·济南",
      "location.nanjing": "中国·南京",
      "location.nanyang": "中国·南阳",
      "refresh.probe": "刷新探测",
      "refresh.now": "立即刷新",
      "services.title": "服务状态",
      "storage.title": "存储空间",
      "identity.title": "系统身份",
      "network.title": "网络连接",
      "hardware.title": "硬件能力",
      "workflows.title": "系统管理",
      "workflows.group.system": "系统与电源",
      "workflows.group.network": "网络访问",
      "workflows.group.services": "服务与容器",
      "workflows.group.storage": "存储",
      "workflows.group.other": "其他",
      "workflows.group.count": "操作 · {count}",
      "activity.title": "活动记录",
      "risk.safe": "安全",
      "risk.guarded": "需确认",
      "risk.high": "高风险",
      "risk.critical": "严重风险",
      "status.starting": "控制中心正在启动",
      "status.connecting": "正在连接本机数据源",
      "status.syncing": "正在同步本机状态",
      "status.reading": "正在读取开发板身份与内核信号",
      "status.unavailable": "控制中心不可用",
      "status.demoOnline": "演示数据源在线",
      "status.localStable": "本机设备连接稳定",
      "status.demoDetail": "模拟数据 · 变更操作只会预演",
      "status.localDetail": "{product} · {networks} 条网络路径 · {capabilities} 项硬件能力",
      "toast.probeComplete": "探测完成",
      "toast.demoLoaded": "已加载模拟 SBC 遥测数据。",
      "toast.localCurrent": "本机开发板状态已更新。",
      "toast.refreshFailed": "刷新失败",
      "core.demo": "演示设备已连接",
      "core.online": "设备在线",
      "temperature.none": "没有可读取的温度区域",
      "temperature.normal": "处于正常工作范围",
      "temperature.hot": "温度余量降低",
      "storage.unavailable": "根分区不可用",
      "updated": "更新于{time}",
      "empty.services": "当前数据源没有提供服务状态。",
      "empty.storage": "未发现已挂载的块存储。",
      "empty.network": "未检测到非回环网络接口",
      "storage.used": "已使用 {percent}",
      "storage.of": "已用 {used}，共 {total} · {name}",
      "identity.product": "产品",
      "identity.hostname": "主机名",
      "identity.soc": "SoC",
      "identity.system": "系统",
      "identity.kernel": "内核",
      "identity.nodeId": "节点 ID",
      "network.address": "地址",
      "network.type": "类型",
      "network.received": "已接收",
      "network.transmitted": "已发送",
      "network.notAssigned": "未分配",
      "steps": "{count} 步",
      "drawer.operation": "操作",
      "drawer.close": "关闭操作",
      "drawer.estimated": "预计用时",
      "drawer.privilege": "所需权限",
      "drawer.procedure": "执行步骤",
      "drawer.confirm": "我已了解此操作可能会更改本机开发板。",
      "drawer.run": "运行操作",
      "drawer.running": "正在运行…",
      "drawer.root": "需要管理员权限",
      "drawer.user": "当前用户会话",
      "drawer.runState": "控制中心 / 执行中\n正在等待本机执行器。",
      "drawer.dryRun": "预演",
      "drawer.result": "结果",
      "drawer.failed": "操作失败",
      "run.status.succeeded": "成功",
      "run.status.failed": "失败",
      "run.status.planned": "已计划",
      "run.status.running": "执行中",
      "run.plannedSteps": "计划步骤",
      "toast.dryRun": "预演完成",
      "toast.complete": "操作完成",
      "toast.failed": "操作失败",
      "relative.now": "刚刚",
      "relative.minutes": "{count} 分钟前",
      "relative.hours": "{count} 小时前",
      "relative.days": "{count} 天前",
      "duration.days": "{days}天 {hours}小时",
      "duration.hours": "{hours}小时 {minutes}分钟",
      "duration.minutes": "{minutes}分钟",
      "api.unknown_action": "请求的操作已不存在。",
      "api.confirmation_required": "请确认后再运行此操作。",
      "api.action_unavailable": "当前设备不支持此操作。",
      "api.input_required": "请先在“系统 → 软件源管理”中选择镜像。",
      "api.unknown_mirror": "所选镜像不在可信目录中。",
      "api.sources_unsupported": "当前系统不支持 APT 软件源管理。",
      "api.plan_required": "请先预览这次镜像变更，再进行应用。",
      "api.stale_plan": "软件源文件已在预览后变化，请重新生成并检查计划。",
      "api.root_required": "此操作需要管理员权限。",
      "api.authorization_failed": "未完成管理员授权。",
      "api.internal_error": "本机控制中心发生错误。",
      "api.transport_failure": "无法连接本机控制中心。",
      "api.http_failure": "本机控制中心返回 HTTP {status}。",
    },
  };

  const actionCopy = {
    "system.inspect": ["运行系统检查", "重新读取开发板身份、健康状态、服务与已检测硬件。", "观察", ["读取操作系统与设备树身份", "检查存储、网络、温度与服务", "重新计算提醒"]],
    "system.update": ["更新操作系统", "刷新软件包索引并升级已安装的软件包。", "维护", ["刷新软件包元数据", "升级已安装的软件包", "列出被保留的软件包"]],
    "system.change-sources": ["切换软件源", "选择可信的 Debian、Ubuntu 与 Radxa 镜像；执行前预览并备份，刷新失败时自动回滚。", "维护", ["检测受管 APT 软件源条目", "只预览已识别的 Debian、Ubuntu 与 Radxa 地址变更", "备份并原子替换受影响文件", "刷新软件包元数据，失败时恢复原文件"]],
    "service.ssh-install": ["安装远程终端", "安装 OpenSSH 服务端软件包，不改变当前启用状态。", "连接", ["安装 OpenSSH 服务端软件包", "刷新检测到的 SSH 服务状态"]],
    "service.ssh-enable": ["启用远程终端", "启用并启动 SSH；请先确认远程登录账户使用安全凭据。", "连接", ["检查 SSH 服务与账户安全", "设置开机启用", "启动 SSH 服务"]],
    "service.ssh-disable": ["停用远程终端", "停止 SSH 服务并禁止其自动启动。", "连接", ["停止 SSH 服务", "禁止自动启动", "检查服务状态"]],
    "service.ssh-regenerate-host-keys": ["重新生成 SSH 主机密钥", "替换本机 SSH 服务身份并生成一组新密钥。", "连接", ["移除现有 SSH 主机密钥", "生成一组新主机密钥", "刷新检测到的 SSH 服务状态"]],
    "service.ssh-remove": ["移除远程终端", "从本机移除 OpenSSH 服务端软件包。", "连接", ["移除 OpenSSH 服务端软件包", "刷新检测到的 SSH 服务状态"]],
    "network.restart": ["重启网络管理器", "重启 NetworkManager 并重新检查本机网络接口。", "连接", ["记录活动接口", "重启 NetworkManager", "等待网络接口恢复"]],
    "service.docker-install": ["安装容器运行时", "安装发行版提供的 Docker 软件包，不自动启用服务。", "服务", ["安装 Docker 软件包", "刷新检测到的 Docker 服务状态"]],
    "service.docker-enable": ["启用容器运行时", "启用并启动 Docker 服务。", "服务", ["设置 Docker 开机启用", "启动 Docker 服务", "检查服务状态"]],
    "service.docker-disable": ["停用容器运行时", "停止 Docker 服务并禁止其自动启动。", "服务", ["停止 Docker 服务及容器", "禁止自动启动", "检查服务状态"]],
    "service.docker-remove": ["移除容器运行时", "移除 Docker 软件包，保留现有容器数据。", "服务", ["移除 Docker 软件包", "保留 /var/lib/docker 中的现有数据"]],
    "storage.expand-root": ["扩展根文件系统", "将支持的根文件系统扩展至可用存储空间。", "存储", ["确定根块设备", "验证 ext4 或 btrfs", "扩展文件系统", "检查扩展后容量"]],
    "power.enable-sleep": ["启用睡眠与休眠", "恢复 systemd 睡眠与休眠目标。", "电源", ["取消屏蔽睡眠目标", "重新加载 systemd", "检查目标状态"]],
    "power.disable-sleep": ["禁用睡眠与休眠", "屏蔽系统睡眠与休眠目标，让 SBC 持续在线。", "电源", ["屏蔽睡眠目标", "重新加载 systemd", "检查目标状态"]],
    "system.reboot": ["重启设备", "停止服务并立即重启本机开发板。", "电源", ["写回待处理数据", "停止服务", "请求系统重启"]],
  };

  const capabilityCopy = {
    "device-tree": ["设备树叠加层", "已检测到叠加层存储"],
    gpio: ["GPIO", "GPIO 字符设备"],
    video: ["视频采集", "Video4Linux 设备"],
    thermal: ["温控能力", "内核温控子系统"],
    "spi-flash": ["SPI 启动闪存", "MTD 闪存设备"],
  };

  const serviceCopy = {
    "ssh.service": "远程终端",
    "NetworkManager.service": "网络管理器",
    "docker.service": "容器运行时",
  };

  function normalize(value) {
    return String(value || "").trim().toLowerCase().startsWith("zh") ? "zh-CN" : "en";
  }

  function storedLocale() {
    try { return localStorage.getItem("rsetup-locale-v1"); } catch { return null; }
  }

  let locale = normalize(storedLocale() || navigator.languages?.[0] || navigator.language || "en");

  function t(key, values = {}) {
    const template = dictionaries[locale][key] ?? dictionaries.en[key] ?? key;
    return template.replace(/\{(\w+)\}/g, (_, name) => values[name] ?? `{${name}}`);
  }

  function setLocale(next, { persist = true, announce = true } = {}) {
    locale = normalize(next);
    document.documentElement.lang = locale;
    document.documentElement.dataset.locale = locale;
    if (persist) {
      try { localStorage.setItem("rsetup-locale-v1", locale); } catch { /* local storage is optional */ }
    }
    if (announce) window.dispatchEvent(new CustomEvent("rsetup:locale", { detail: { locale } }));
    return locale;
  }

  function translatedUnavailableReason(reason) {
    if (locale !== "zh-CN" || !reason) return reason;
    if (reason === "OpenSSH server is already installed.") return "OpenSSH 服务端已安装。";
    if (reason === "Docker is already installed.") return "Docker 已安装。";
    if (reason === "SSH is already enabled and running.") return "SSH 已启用并正在运行。";
    if (reason === "SSH is already disabled and stopped.") return "SSH 已停用并停止运行。";
    if (reason === "Docker is already enabled and running.") return "Docker 已启用并正在运行。";
    if (reason === "Docker is already disabled and stopped.") return "Docker 已停用并停止运行。";
    if (reason === "Sleep and hibernate targets are already enabled.") return "睡眠与休眠目标已启用。";
    if (reason === "Sleep and hibernate targets are already disabled.") return "睡眠与休眠目标已停用。";
    if (reason === "Neither resize2fs nor btrfs is installed.") return "未安装 resize2fs 或 btrfs。";
    const packageMatch = reason.match(/^Package (.+) is not installed\.$/);
    if (packageMatch) return `未安装软件包 ${packageMatch[1]}。`;
    const unitMatch = reason.match(/^Systemd unit (.+) is not installed\.$/);
    if (unitMatch) return `未安装 systemd 单元 ${unitMatch[1]}。`;
    const commandMatch = reason.match(/^Missing required command\(s\): (.+)\.$/);
    if (commandMatch) return `缺少必要命令：${commandMatch[1]}。`;
    return reason;
  }

  function action(value) {
    const base = { ...value, steps: [...(value.steps || [])], unavailableReason: translatedUnavailableReason(value.unavailableReason) };
    if (locale !== "zh-CN" || !actionCopy[value.id]) return base;
    const [title, description, category, steps] = actionCopy[value.id];
    return { ...base, title, description, category, steps: [...steps] };
  }

  function capability(value) {
    if (locale !== "zh-CN") return value;
    const copy = capabilityCopy[value.id];
    if (!copy) return value;
    let detail = value.detail;
    if (!value.available) detail = "此设备未检测到";
    else if (value.id === "device-tree" && /^\d+ overlays available$/.test(detail)) detail = detail.replace(" overlays available", " 个叠加层可用");
    else if (value.id === "gpio" && /^\d+ gpiochips · \d+ lines$/.test(detail)) detail = detail.replace(" gpiochips", " 个 GPIO 芯片").replace(" lines", " 条线路");
    else if (value.id === "video" && /^\d+ Video4Linux devices$/.test(detail)) detail = detail.replace(" Video4Linux devices", " 个 Video4Linux 设备");
    else if (value.id === "thermal" && /^\d+ zones/.test(detail)) detail = detail.replace(" zones", " 个温区");
    else if (value.id === "spi-flash" && /MTD device$/.test(detail)) detail = detail.replace("MTD device", "MTD 设备");
    else if (["Overlay storage detected", "GPIO character device", "Video4Linux device", "Kernel thermal subsystem", "MTD flash device"].includes(detail)) detail = copy[1];
    return { ...value, label: copy[0], detail };
  }

  function service(value) {
    if (locale !== "zh-CN") return value;
    let detail = value.detail;
    if (detail === "Installed · stopped") detail = "已安装 · 已停止";
    else if (detail === "systemd state unavailable") detail = "systemd 状态不可用";
    else if (/^Listening on /.test(detail)) detail = detail.replace("Listening on ", "监听端口 ");
    else if (/ interfaces managed$/.test(detail)) detail = detail.replace(" interfaces managed", " 个接口已托管");
    return { ...value, label: serviceCopy[value.id] || value.label, detail };
  }

  function enumLabel(group, value) {
    const key = `${group}.${String(value).toLowerCase()}`;
    const maps = {
      "state.active": ["ACTIVE", "运行中"], "state.inactive": ["INACTIVE", "未运行"],
      "state.failed": ["FAILED", "失败"], "state.unknown": ["UNKNOWN", "未知"],
      "network.online": ["online", "在线"], "network.up": ["up", "已连接"],
      "network.standby": ["standby", "待机"], "network.down": ["down", "离线"],
      "network.unknown": ["unknown", "未知"], "kind.ethernet": ["ethernet", "有线网络"],
      "kind.wireless": ["wireless", "无线网络"],
    };
    return maps[key]?.[locale === "zh-CN" ? 1 : 0] || value;
  }

  function activity(value) {
    if (locale !== "zh-CN") return value;
    const titleByEnglish = {
      "Demo control plane ready": "演示控制中心已就绪",
      "Local control plane ready": "本机控制中心已就绪",
      ...Object.fromEntries(Object.entries(actionCopy).map(([id, copy]) => {
        const english = {
          "system.inspect": "Run system inspection", "system.update": "Update operating system",
          "system.change-sources": "Change package mirrors",
          "service.ssh-install": "Install remote shell", "service.ssh-enable": "Enable remote shell",
          "service.ssh-disable": "Disable remote shell", "service.ssh-regenerate-host-keys": "Regenerate SSH host keys",
          "service.ssh-remove": "Remove remote shell", "network.restart": "Restart network manager",
          "service.docker-install": "Install container runtime", "service.docker-enable": "Enable container runtime",
          "service.docker-disable": "Disable container runtime", "service.docker-remove": "Remove container runtime",
          "storage.expand-root": "Expand root filesystem", "power.enable-sleep": "Enable sleep and hibernate",
          "power.disable-sleep": "Disable sleep and hibernate",
          "system.reboot": "Reboot device",
        }[id];
        return [english, copy[0]];
      })),
    };
    const detailByEnglish = {
      "Inspection is active. Mutating operations will produce dry-run results.": "设备检查已启用，变更操作将只返回预演结果。",
      "Live execution is enabled for the fixed action catalog.": "固定操作目录已启用实时执行。",
      "Dry run completed; no system state was changed.": "预演完成；未修改任何系统状态。",
      "Dry run completed; no APT source file was changed.": "预演完成；未修改任何 APT 软件源文件。",
      "APT sources already use the selected mirror.": "APT 软件源已经使用所选镜像。",
      "APT sources changed and package metadata refreshed.": "APT 软件源已切换，软件包元数据已刷新。",
      "Package metadata refresh failed; the previous APT sources were restored.": "软件包元数据刷新失败；已恢复之前的 APT 软件源。",
      "Operation completed successfully.": "操作已成功完成。",
      "Inspection completed.": "检查已完成。",
    };
    return { ...value, title: titleByEnglish[value.title] || value.title, detail: detailByEnglish[value.detail] || value.detail };
  }

  function runSummary(run) {
    if (locale !== "zh-CN") return run.summary;
    if (run.actionId === "system.change-sources" && run.synthetic) return "预演完成；未修改任何 APT 软件源文件。";
    if (run.synthetic) return "预演完成；未修改任何系统状态。";
    return run.status === "succeeded" ? "操作已成功完成。" : run.status === "failed" ? "操作执行失败。" : run.summary;
  }

  function apiError(code, fallback) {
    const key = `api.${code}`;
    return dictionaries[locale][key] || fallback || t("api.internal_error");
  }

  setLocale(locale, { persist: false, announce: false });
  window.RsetupI18n = { t, setLocale, getLocale: () => locale, action, capability, service, enumLabel, activity, runSummary, apiError };
})();
