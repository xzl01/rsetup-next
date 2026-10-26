const state = {
  providerSnapshot: null,
  snapshot: null,
  actions: [],
  activity: [],
  sources: null,
  sourcePlan: null,
  sourcePreviewVersion: 0,
  sourceBenchmark: { version: 0, running: false, stop: false, results: [], completed: 0, total: 0 },
  selectedAction: null,
  selectedHardware: null,
  hardwareData: null,
  hardwareLoadVersion: 0,
  gpioSelectedPin: null,
  overlayPlan: null,
  overlaySelection: [],
  videoFrame: null,
  thermalPolicy: null,
  thermalPanel: "policy",
  fanCurveDraft: null,
  fanCurvePlan: null,
  fanCurvePreviewVersion: 0,
  ledPanel: "status",
  ledSelection: null,
  rgbLedConfig: null,
  spiFlashOperation: "install",
  spiFlashTarget: null,
  spiFlashImage: null,
  spiFlashPlan: null,
  spiFlashPreviewVersion: 0,
  storageRefreshPromise: null,
  storageRefreshing: false,
  storageRefreshError: null,
  storageStale: false,
  storageRefreshedAt: null,
  // System image install page. `installOptions` mirrors the server payload
  // `{ liveAllowed, targets, images }`; the acknowledgement is always the raw
  // string a user typed and is never collapsed into a boolean.
  // `installTargetId`/`installImageId` are the *plan* selection only: the id an
  // apply submits always comes back from the plan response (`plan.image.id`), so
  // there is exactly one source of the applied image identity.
  installOptions: null,
  installLiveAllowed: false,
  installLoading: false,
  installLoadingPromise: null,
  installLoadError: null,
  installTargetId: "",
  installImageId: "",
  // Display metadata for a file the desktop shell picked (name + size; a path
  // never reaches the WebView). Never used as an apply identity: that is always
  // the `image.id` the plan response echoes.
  installPickedImage: null,
  installPicking: false,
  installPickNotice: null,
  installPlan: null,
  installPlanning: false,
  installPreviewVersion: 0,
  installAcknowledgement: "",
  installRunning: false,
  installError: null,
  installResult: null,
  lastInvoker: null,
  contactInvoker: null,
  route: "overview",
  refreshing: false,
  debugProfile: "provider",
  debugCustom: { ...defaultDebugCustom },
};

const $ = (selector, scope = document) => scope.querySelector(selector);
const $$ = (selector, scope = document) => Array.from(scope.querySelectorAll(selector));
const tauriInvoke = window.__TAURI__?.core?.invoke;
