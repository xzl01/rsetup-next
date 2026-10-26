async function init() {
  loadDebugState();
  applyStaticTranslations();
  let storedTheme = null;
  try { storedTheme = localStorage.getItem("rsetup-theme-v2"); } catch { /* local storage is optional */ }
  setTheme(storedTheme || "light");
  bindEvents();
  startClock();
  const initialRoute = location.hash.slice(1);
  if (routes.some((route) => route.id === initialRoute)) navigate(initialRoute);
  await refreshAll({ quiet: true });
  window.setInterval(() => refreshAll({ quiet: true }), 10_000);
}

init();
