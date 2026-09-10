import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";

function loadI18n(language = "en-US") {
  const attributes = {};
  const storage = new Map();
  const context = {
    CustomEvent: class CustomEvent {
      constructor(type, options) { this.type = type; this.detail = options?.detail; }
    },
    document: {
      documentElement: {
        dataset: {},
        set lang(value) { attributes.lang = value; },
        get lang() { return attributes.lang; },
      },
    },
    localStorage: {
      getItem: (key) => storage.get(key) ?? null,
      setItem: (key, value) => storage.set(key, value),
    },
    navigator: { language, languages: [language] },
  };
  context.window = { dispatchEvent() {}, RsetupI18n: null };
  vm.runInNewContext(fs.readFileSync(new URL("./i18n.js", import.meta.url), "utf8"), context);
  return { i18n: context.window.RsetupI18n, attributes, storage };
}

test("auto-detects Chinese and localizes known actions by stable id", () => {
  const { i18n, attributes } = loadI18n("zh-CN");
  assert.equal(i18n.getLocale(), "zh-CN");
  assert.equal(attributes.lang, "zh-CN");
  assert.equal(i18n.action({ id: "system.reboot", title: "Reboot device", description: "", category: "Power", steps: [] }).title, "重启设备");
  assert.equal(i18n.action({ id: "service.docker-enable", title: "Enable container runtime", description: "", category: "Services", steps: [] }).title, "启用容器运行时");
});

test("mirror benchmark states are bilingual and distinguish simulated sampling", () => {
  const { i18n } = loadI18n("zh-CN");
  assert.equal(i18n.t("sources.simulatedTest"), "模拟测速");
  assert.match(i18n.t("sources.benchmarkNote"), /不修改配置/);
  assert.equal(i18n.t("sources.testing", { done: 2, total: 10 }), "正在测速 2/10…");
  for (const locale of ["zh-CN", "en"]) {
    i18n.setLocale(locale);
    for (const status of ["ok", "timeout", "http_error", "network_error", "invalid_index", "sample_too_large", "curl_missing"]) {
      const key = `sources.testStatus.${status}`;
      assert.notEqual(i18n.t(key), key);
    }
  }
  assert.equal(i18n.t("sources.simulatedTest"), "Simulated test");
  assert.match(i18n.t("sources.benchmarkNote"), /not peak bandwidth/);
});

test("keeps action availability and localizes its reason", () => {
  const { i18n } = loadI18n("zh-CN");
  const action = i18n.action({
    id: "service.ssh-enable",
    title: "Enable remote shell",
    description: "",
    category: "Connect",
    steps: [],
    available: false,
    unavailableReason: "Package openssh-server is not installed.",
  });
  assert.equal(action.available, false);
  assert.equal(action.unavailableReason, "未安装软件包 openssh-server。");
});

test("switches language, persists the choice, and keeps unknown provider copy", () => {
  const { i18n, storage } = loadI18n();
  assert.equal(i18n.t("route.help"), "FAQ");
  assert.equal(i18n.t("help.libraryTitle", { board: "ROCK 5B" }), "ROCK 5B help");
  i18n.setLocale("zh");
  assert.equal(storage.get("rsetup-locale-v1"), "zh-CN");
  assert.equal(i18n.t("help.libraryTitle", { board: "ROCK 5B" }), "ROCK 5B 帮助");
  assert.equal(i18n.t("contact.wechatQr"), "Radxa 官方微信群二维码");
  assert.equal(i18n.action({ id: "vendor.action", title: "Vendor action", description: "Vendor detail", category: "Vendor", steps: ["One"] }).title, "Vendor action");
  i18n.setLocale("en");
  assert.equal(i18n.t("overview.title"), "Your SBC at a glance");
});

test("EFI authorization and review distinguish saved configuration from active hardware", () => {
  const { i18n } = loadI18n("zh-CN");
  assert.match(i18n.t("overlay.readRequired"), /不会修改系统/);
  assert.match(i18n.t("overlay.kernelScope", { kernel: "7.0.11-5-qcom" }), /7\.0\.11-5-qcom/);
  assert.match(i18n.t("overlay.kernelScope", { kernel: "test" }), /不会更改默认启动项/);
  assert.match(i18n.t("overlay.cached", { time: "12:00" }), /上次授权读取/);
  assert.equal(i18n.hardwareReason("EFI boot files require administrator authorization to read."), "读取 EFI 配置需要管理员授权。");
  i18n.setLocale("en");
  assert.match(i18n.t("overlay.savedView"), /not the running pin state/);
  assert.match(i18n.t("overlay.kernelScope", { kernel: "test" }), /default boot entry is not changed/);
});

test("uses SBC terminology and explicit risk acknowledgement copy", () => {
  const source = fs.readFileSync(new URL("./i18n.js", import.meta.url), "utf8");
  const deprecatedTerm = String.fromCodePoint(0x5f00, 0x53d1, 0x677f);
  const { i18n } = loadI18n("zh-CN");
  assert.equal(source.includes(deprecatedTerm), false);
  assert.equal(
    i18n.t("drawer.confirm"),
    "我已了解此操作会更改 SBC 系统设置，并接受由此带来的潜在网络安全风险。",
  );
  i18n.setLocale("en");
  assert.equal(
    i18n.t("drawer.confirm"),
    "I understand this operation changes SBC system settings, and I accept the potential network security risks.",
  );
});

test("localizes API error codes without changing the API payload", () => {
  const { i18n } = loadI18n("zh_CN");
  assert.equal(i18n.apiError("root_required", "fallback"), "此操作需要管理员权限。");
  assert.equal(i18n.apiError("authorization_failed", "fallback"), "未完成管理员授权。");
  assert.equal(i18n.apiError("vendor_error", "provider detail"), "provider detail");
  assert.equal(i18n.t("api.transport_failure"), "无法连接本机控制中心。");
  assert.equal(i18n.t("api.http_failure", { status: 503 }), "本机控制中心返回 HTTP 503。");
});

test("localizes LED capability and controls without changing identifiers", () => {
  const { i18n } = loadI18n("zh-CN");
  const capability = i18n.capability({
    id: "led",
    label: "LED control",
    detail: "2 status LEDs · 1 RGB group",
    available: true,
  });
  assert.equal(capability.label, "LED 控制");
  assert.equal(capability.detail, "2 个状态灯 · 1 组 RGB 灯");
  assert.equal(i18n.t("led.mode.breath"), "呼吸");
  i18n.setLocale("en");
  assert.equal(i18n.t("led.applyRgb"), "Apply RGB pattern");
});

test("localizes SPI flash planning and destructive confirmations", () => {
  const { i18n } = loadI18n("zh-CN");
  const capability = i18n.capability({
    id: "spi-flash",
    label: "SPI boot flash",
    detail: "16 MiB MTD device",
    available: true,
  });
  assert.equal(capability.label, "SPI 启动闪存");
  assert.equal(capability.detail, "16 MiB MTD 设备");
  assert.match(i18n.t("spiFlash.confirmInstall", { target: "/dev/mtd0", image: "ROCK 5B" }), /SBC 无法启动/);
  assert.equal(i18n.t("spiFlash.apply.erase"), "备份并擦除闪存");
  assert.match(i18n.apiError("stale_plan", "fallback"), /系统状态/);
  i18n.setLocale("en");
  assert.equal(i18n.t("spiFlash.preview"), "Review SPI operation");
  assert.match(i18n.apiError("stale_plan", "fallback"), /System state/);
});

test("localizes fan curve controls and thermal safety copy", () => {
  const { i18n } = loadI18n("zh-CN");
  assert.equal(i18n.t("fanCurve.tab"), "风扇曲线");
  assert.match(i18n.t("fanCurve.pointsHint"), /90 °C/);
  assert.match(i18n.t("fanCurve.confirmEnable"), /SBC 温控设置/);
  assert.match(i18n.t("fanCurve.warning.sensor_failure_forces_full_speed"), /满速/);
  i18n.setLocale("en");
  assert.equal(i18n.t("fanCurve.preview"), "Review fan curve");
  assert.match(i18n.t("fanCurve.confirmEnable"), /insufficient cooling/);
});

test("describes Overlay assignments and defaults without assuming source column names", () => {
  const { i18n } = loadI18n("zh-CN");
  const capability = i18n.capability({
    id: "gpio",
    label: "GPIO",
    detail: "Overlay-aware 40-pin map",
    available: true,
  });
  assert.equal(capability.detail, "Overlay 联动的 40Pin 图");
  assert.match(i18n.t("gpio.currentOnly"), /Overlay 配置/);
  assert.equal(i18n.t("gpio.source.overlay"), "Overlay 配置");
  assert.equal(i18n.t("gpio.source.default"), "默认功能");
  assert.doesNotMatch(i18n.t("gpio.profileDescription"), /Function[01]/);
  assert.match(i18n.t("gpio.profileDescription"), /默认功能/);
  assert.equal(i18n.t("gpio.configurationUnknown"), "配置未读取");
  assert.match(i18n.t("gpio.baselineOnly"), /尚未读取 Overlay/);
  assert.equal(i18n.t("gpio.source.unassigned"), "未分配");
  assert.equal(i18n.t("gpio.overlays", { count: 2 }), "2 个已配置 Overlay");
  i18n.setLocale("en");
  assert.doesNotMatch(i18n.t("gpio.profileDescription"), /Function[01]/);
  assert.match(i18n.t("gpio.profileDescription"), /default function/);
  assert.equal(i18n.t("gpio.configurationUnknown"), "Configuration not read");
  assert.match(i18n.t("gpio.baselineOnly"), /not read yet/);
  assert.equal(i18n.t("gpio.unassigned"), "Unassigned");
  assert.equal(i18n.t("gpio.header.main"), "40-pin expansion header");
});

test("keeps UEFI + DT and capture-probe errors distinct from missing hardware", () => {
  const { i18n } = loadI18n("zh-CN");
  const reason = "UEFI + DT detected. Overlay configuration is not read or managed yet.";
  const capability = { id: "device-tree", available: false, detail: reason };
  assert.equal(i18n.capability(capability).detail, reason);
  assert.equal(capability.detail, reason);
  assert.match(i18n.hardwareReason("Unable to verify video capture devices. Check device permissions and driver readiness."), /权限和驱动/);
  assert.equal(i18n.hardwareReason("No video capture device was detected."), "未检测到视频采集设备。");
  assert.match(i18n.hardwareReason("Install ffmpeg to capture a webcam test frame."), /安装 ffmpeg/);
  assert.equal(i18n.capability({ id: "gpio", available: true, detail: "40-pin defaults · overlay configuration unread" }).detail, "40Pin 默认功能 · Overlay 配置未读取");
  assert.equal(i18n.hardwareReason("Unknown provider detail"), "Unknown provider detail");
  i18n.setLocale("en");
  assert.equal(i18n.hardwareReason(reason), reason);
  assert.equal(i18n.capability(capability).detail, reason);
});

test("dictionaries keep identical keys and placeholders", () => {
  const source = fs.readFileSync(new URL("./i18n.js", import.meta.url), "utf8");
  // Export only a test copy of the dictionaries, without changing the public runtime API.
  const match = source.match(/const dictionaries = ([\s\S]*?);\n/);
  assert.ok(match);
  const dictionaries = vm.runInNewContext("(" + match[1] + ")");
  assert.deepEqual(Object.keys(dictionaries.en).sort(), Object.keys(dictionaries["zh-CN"]).sort());
  const slots = (text) => [...text.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();
  for (const key of Object.keys(dictionaries.en)) {
    assert.deepEqual(slots(dictionaries.en[key]), slots(dictionaries["zh-CN"][key]), key);
  }
});

test("unknown dynamic keys and missing parameters do not leak template syntax", () => {
  const { i18n } = loadI18n("zh-CN");
  assert.equal(i18n.t("sources.warning.future"), "暂无详细信息");
  assert.doesNotMatch(i18n.t("gpio.overlays"), /\{count\}/);
});
