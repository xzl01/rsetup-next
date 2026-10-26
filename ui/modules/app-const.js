const i18n = window.RsetupI18n;
const { t } = i18n;

const routes = [
  { id: "overview", icon: "overview" },
  { id: "system", icon: "system" },
  { id: "install", icon: "install" },
  { id: "network", icon: "network" },
  { id: "hardware", icon: "chip" },
  { id: "workflows", icon: "run" },
  { id: "activity", icon: "pulse" },
  { id: "help", icon: "help" },
];

const workflowGroups = [
  { id: "system", actions: ["system.inspect", "system.update", "power.enable-sleep", "power.disable-sleep", "system.reboot"] },
  { id: "network", actions: ["service.ssh-install", "service.ssh-enable", "service.ssh-disable", "service.ssh-regenerate-host-keys", "service.ssh-remove", "network.restart"] },
  { id: "services", actions: ["service.docker-install", "service.docker-enable", "service.docker-disable", "service.docker-remove"] },
  { id: "storage", actions: ["storage.expand-root"] },
];

const quickActionIds = [
  "system.inspect",
  "system.update",
  "system.change-sources",
  "service.ssh-enable",
  "network.restart",
];

const socVendors = [
  { id: "rockchip", name: "Rockchip", partnerLogo: true, patterns: ["rockchip", "rk35"] },
  { id: "allwinner", name: "Allwinner", partnerLogo: true, patterns: ["allwinner", "sunxi", "sun50"] },
  { id: "cix", name: "CIX", partnerLogo: true, patterns: ["cix", "sky1"] },
  { id: "qualcomm", name: "Qualcomm", partnerLogo: true, patterns: ["qualcomm", "qcom", "snapdragon", "qcs"] },
  { id: "amlogic", name: "Amlogic", partnerLogo: true, patterns: ["amlogic", "a311d"] },
  { id: "broadcom", name: "Broadcom", patterns: ["broadcom", "brcm", "bcm"] },
  { id: "mediatek", name: "MediaTek", partnerLogo: true, patterns: ["mediatek", "mtk"] },
  { id: "nvidia", name: "NVIDIA", patterns: ["nvidia", "tegra"] },
  { id: "nxp", name: "NXP", patterns: ["nxp", "fsl,", "imx"] },
  { id: "starfive", name: "StarFive", partnerLogo: true, patterns: ["starfive", "jh71"] },
  { id: "sophgo", name: "Sophgo", patterns: ["sophgo", "cv18"] },
  { id: "intel", name: "Intel", partnerLogo: true, patterns: ["intel"] },
];

const capabilityVisuals = {
  "device-tree": { icon: "device-tree", tone: "signal" },
  gpio: { icon: "gpio", tone: "emerald" },
  video: { icon: "video", tone: "cyan" },
  thermal: { icon: "thermal", tone: "amber" },
  led: { icon: "led", tone: "coral" },
  "spi-flash": { icon: "spi-flash", tone: "signal" },
  storage: { icon: "storage", tone: "cyan" },
};

const hardwareToolIds = new Set(["device-tree", "gpio", "video", "thermal", "led", "spi-flash", "storage"]);

const debugDeviceProfiles = [
  { id: "rockchip-rk3588", label: "ROCK 5B · RK3588", product: "Radxa ROCK 5B Demo", hostname: "debug-rock-5b", socVendor: "Rockchip", soc: "RK3588", architecture: "aarch64", pinoutProfile: "rock5b" },
  { id: "allwinner-a733", label: "Cubie A7A · A733", product: "Radxa Cubie A7A Demo", hostname: "debug-cubie-a7a", socVendor: "Allwinner", soc: "A733", architecture: "aarch64", pinoutProfile: "cubieA7a" },
  { id: "cix-p1", label: "Orion O6 · CIX P1", product: "Radxa Orion O6 Demo", hostname: "debug-orion-o6", socVendor: "CIX", soc: "P1", architecture: "aarch64", pinoutProfile: "orionO6" },
  { id: "qualcomm-qcs6490", label: "Dragon Q6A · QCS6490", product: "Radxa Dragon Q6A Demo", hostname: "debug-dragon-q6a", socVendor: "Qualcomm", soc: "QCS6490", architecture: "aarch64", pinoutProfile: "dragonQ6a" },
  { id: "qualcomm-sc8280xp", label: "Dragon Q8B · SC8280XP", product: "Radxa Dragon Q8B Demo", hostname: "debug-dragon-q8b", socVendor: "Qualcomm", soc: "SC8280XP", architecture: "aarch64", pinoutProfile: "dragonQ8b" },
  { id: "amlogic-a311d", label: "ZERO 2 Pro · A311D", product: "Radxa ZERO 2 Pro Demo", hostname: "debug-zero-2-pro", socVendor: "Amlogic", soc: "A311D", architecture: "aarch64", pinoutProfile: "radxaZero2Pro" },
  { id: "mediatek-genio700", label: "MediaTek · Genio 700", product: "MediaTek Genio 700 Demo", hostname: "debug-genio700", socVendor: "MediaTek", soc: "Genio 700", architecture: "aarch64", pinoutProfile: null },
  { id: "starfive-jh7110", label: "StarFive · JH7110", product: "StarFive JH7110 Demo", hostname: "debug-jh7110", socVendor: "StarFive", soc: "JH7110", architecture: "riscv64", pinoutProfile: null },
];

const defaultDebugCustom = {
  product: "Custom SBC Demo",
  hostname: "debug-sbc",
  socVendor: "Rockchip",
  soc: "RK3588",
  architecture: "aarch64",
};

const helpProfiles = [
  {
    id: "rock-5b",
    name: "ROCK 5B",
    patterns: ["rock 5b", "rock5b"],
    resources: [
      { kind: "guide", path: "/rock5/rock5b" },
      { kind: "faq", path: "/rock5/rock5b/faq" },
      { kind: "download", path: "/rock5/rock5b/download" },
    ],
  },
  {
    id: "orion-o6",
    name: "Orion O6",
    patterns: ["orion o6", "orion-o6"],
    resources: [
      { kind: "guide", path: "/orion/o6" },
      { kind: "faq", path: "/orion/faq" },
      { kind: "download", path: "/orion/download" },
    ],
  },
  {
    id: "dragon-q6a",
    name: "Dragon Q6A",
    patterns: ["dragon q6a", "q6a"],
    resources: [
      { kind: "guide", path: "/dragon/q6a/getting-started" },
      { kind: "download", path: "/dragon/q6a/download" },
    ],
  },
  {
    id: "zero-2-pro",
    name: "ZERO 2 Pro",
    patterns: ["zero 2 pro", "zero2pro", "zero 2pro"],
    resources: [
      { kind: "guide", path: "/zero/zero2pro" },
      { kind: "download", path: "/zero/zero2pro/download" },
    ],
  },
];

const genericHelpResources = [
  { kind: "docs", path: "/welcome" },
  { kind: "forum", url: "https://forum.radxa.com/" },
  { kind: "github", url: "https://github.com/radxa" },
];

const helpFaqs = ["docs", "hardware", "privilege", "support"];

const contactChannels = [
  { id: "forum", mark: "FR", url: "https://forum.radxa.com/" },
  { id: "github", mark: "GH", url: "https://github.com/radxa" },
  { id: "discord", mark: "DC", url: "https://discord.com/invite/mn73YNWdHY" },
  { id: "telegram", mark: "TG", url: "https://t.me/rockpi4" },
];
