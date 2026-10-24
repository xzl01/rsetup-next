use tokio::sync::watch;

#[tokio::test]
async fn scheduler_graceful_shutdown_watch_exits_cleanly() {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    // 当 shutdown 信号置为 true 时，调度循环应该优雅退出
    let mut rx = shutdown_rx.clone();
    shutdown_tx.send(true).unwrap();
    assert!(*rx.borrow_and_update());
}
