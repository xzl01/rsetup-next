# Task 4.2 Review Package
Base: 7f694544e06e700dd38f0391661ddb502d0b1561
Head: 4e6110332ffdd816a7cc3ff0feadb23e98e1ce1f

## Git Log
4e61103 feat(app): wire MMC status into TUI app state

## Git Diff --stat
 crates/rsetup-app/src/tui.rs | 27 +++++++++++++++++++++++++++
 1 file changed, 27 insertions(+)

## Git Diff
diff --git a/crates/rsetup-app/src/tui.rs b/crates/rsetup-app/src/tui.rs
index 9a422e4..65f99cd 100644
--- a/crates/rsetup-app/src/tui.rs
+++ b/crates/rsetup-app/src/tui.rs
@@ -75,20 +75,21 @@ struct App {
     actions: Vec<ActionSpec>,
     selected: usize,
     confirm_pending: bool,
     last_run: Option<ActionRun>,
     source_status: SourceStatus,
     source_plan: Option<SourcePlan>,
     source_picker: bool,
     source_selected: usize,
     notice: Option<String>,
     pub(crate) nvme_status: rsetup_core::NvmeStatus,
+    pub(crate) mmc_status: rsetup_core::MmcStatus,
     benchmark_rx: Option<
         std::sync::mpsc::Receiver<Result<rsetup_core::MirrorBenchmark, rsetup_core::SourceError>>,
     >,
     benchmarks: std::collections::BTreeMap<String, rsetup_core::MirrorBenchmark>,
 }
 
 impl App {
     fn new(controller: Controller, locale: Locale) -> Result<Self> {
         let snapshot = controller.snapshot()?;
         let actions = controller.actions();
@@ -100,34 +101,40 @@ impl App {
             .iter()
             .position(|provider| {
                 Some(provider.id.as_str()) == source_status.current_system_provider.as_deref()
             })
             .unwrap_or(0);
         let nvme_status = controller.nvme_status().unwrap_or_else(|_| rsetup_core::NvmeStatus {
             initialized: false,
             devices: vec![],
             message: Some("Failed to query NVMe status".into()),
         });
+        let mmc_status = controller.mmc_status().unwrap_or_else(|_| rsetup_core::MmcStatus {
+            initialized: false,
+            devices: vec![],
+            message: Some("Failed to query MMC status".into()),
+        });
         Ok(Self {
             controller,
             locale,
             snapshot,
             actions,
             selected: 0,
             confirm_pending: false,
             last_run: None,
             source_status,
             source_plan: None,
             source_picker: false,
             source_selected,
             notice: None,
             nvme_status,
+            mmc_status,
             benchmark_rx: None,
             benchmarks: Default::default(),
         })
     }
 
     fn next(&mut self) {
         if self.source_picker {
             self.source_selected = (self.source_selected + 1)
                 .min(self.source_status.providers.len().saturating_sub(1));
             self.confirm_pending = false;
@@ -196,20 +203,28 @@ impl App {
             .source_status()
             .map_err(|error| anyhow!(self.locale.source_error(&error)))?;
         self.nvme_status = self
             .controller
             .nvme_status()
             .unwrap_or_else(|_| rsetup_core::NvmeStatus {
                 initialized: false,
                 devices: vec![],
                 message: Some("Failed to query NVMe status".into()),
             });
+        self.mmc_status = self
+            .controller
+            .mmc_status()
+            .unwrap_or_else(|_| rsetup_core::MmcStatus {
+                initialized: false,
+                devices: vec![],
+                message: Some("Failed to query MMC status".into()),
+            });
         if self.source_picker {
             self.update_source_plan();
         }
         Ok(())
     }
 
     fn request_run(&mut self) {
         let Some(action) = self.actions.get(self.selected) else {
             return;
         };
@@ -848,20 +863,32 @@ mod tests {
     #[test]
     fn test_tui_app_loads_and_refreshes_nvme_status() {
         let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
         let mut app = App::new(controller, Locale::En).expect("init app");
         assert!(app.nvme_status.initialized);
         assert_eq!(app.nvme_status.devices.len(), 1);
         app.refresh().expect("refresh app");
         assert!(app.nvme_status.initialized);
     }
 
+    #[test]
+    fn test_tui_app_loads_and_refreshes_mmc_status() {
+        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
+        let mut app = App::new(controller, Locale::En).expect("init app");
+        // Demo mode returns 2 MMC devices (eMMC + SD).
+        assert!(app.mmc_status.initialized);
+        assert_eq!(app.mmc_status.devices.len(), 2);
+        app.refresh().expect("refresh app");
+        assert!(app.mmc_status.initialized);
+        assert_eq!(app.mmc_status.devices.len(), 2);
+    }
+
     #[test]
     fn test_render_nvme_summary_demo() {
         let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
         let app = App::new(controller, Locale::ZhCn).expect("init app");
         let backend = ratatui::backend::TestBackend::new(80, 25);
         let mut terminal = Terminal::new(backend).expect("init test terminal");
         terminal
             .draw(|frame| {
                 let area = Rect::new(0, 0, 80, 6);
                 render_nvme_summary(frame, &app, area);
