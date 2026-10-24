use rsetup_controller::tasks::scheduler::SchedulerStore;
use rsetup_controller::tasks::scheduler_store::SqlxSchedulerStore;

#[test]
fn sqlx_scheduler_store_type_implements_scheduler_store() {
    // 单元编译与类型约束测试：证明 SqlxSchedulerStore 确实实现了 SchedulerStore
    fn assert_impl<T: SchedulerStore>() {}
    assert_impl::<SqlxSchedulerStore>();
}
