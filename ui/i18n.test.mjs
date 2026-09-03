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
});

test("switches language, persists the choice, and keeps unknown provider copy", () => {
  const { i18n, storage } = loadI18n();
  i18n.setLocale("zh");
  assert.equal(storage.get("rsetup-locale-v1"), "zh-CN");
  assert.equal(i18n.action({ id: "vendor.action", title: "Vendor action", description: "Vendor detail", category: "Vendor", steps: ["One"] }).title, "Vendor action");
  i18n.setLocale("en");
  assert.equal(i18n.t("overview.title"), "Your board at a glance");
});

test("localizes API error codes without changing the API payload", () => {
  const { i18n } = loadI18n("zh_CN");
  assert.equal(i18n.apiError("root_required", "fallback"), "此操作需要管理员权限。");
  assert.equal(i18n.apiError("vendor_error", "provider detail"), "provider detail");
  assert.equal(i18n.t("api.transport_failure"), "无法连接本机控制中心。");
  assert.equal(i18n.t("api.http_failure", { status: 503 }), "本机控制中心返回 HTTP 503。");
});
