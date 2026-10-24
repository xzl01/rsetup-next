# Task 4b：会话未决表与探测纯状态组件

主代理实施，未提交/未推送。范围为session/{mod,types,pending,probe}.rs、tests/session_state.rs和tunnel/lib.rs单行模块登记。不是h2网络driver，尚未接controller/board。

## 接口

- RequestKey绑定trace/stream/StreamEpoch/action；PendingTable<T>惰性分配，连接总上限1024，业务保留2个probe槽，各流至多一probe。匹配仅Response+同流/epoch/action/未过期，错probe不取出/不刷新；expire/close_stream/close_all结清一次、无无界墓碑。
- next_trace_id使用进程static AtomicU64，从1分配，MAX为耗尽哨兵不回绕；末值单测用私有allocator，普通生产无重置入口。
- ProbeState按传入Tokio Instant驱动，open15s/firstprobe15s，打开立即send首probe、运行三失败重开单流、三次open/首健康失败升连接；仅正确本端pong重置，旧epoch/nonce/trace不credit；drain不发probe/不重开。
- Reply Debug隐藏error/payload；ProbeRequest Debug隐藏nonce。真实driver未来必须先validate(stream)再匹配，处理InputCursor逐包与全局内存背压；这些不由纯状态组件冒充。

## RED/GREEN

- RED：先写session_state.rs再运行，接口不存在导致4处unresolved import/type错误，exit101；测试本身不存在非ASCII bytes/私有字段访问等错误。
- GREEN：CARGO_TARGET_DIR=target/tunnel-session-state cargo test -p rsetup-tunnel --test session_state --locked -> 12 passed/0failed。
- CARGO_TARGET_DIR=target/tunnel-session-state cargo test -p rsetup-tunnel --lib session::pending --locked -> 1passed/其余过滤；明确不是整个lib测试证据。
- rustfmt --edition2024五个归属文件通过。
- clippy --lib --test session_state -Dwarnings：exit101，全输出10项落在并行handshake.rs/secure_io.rs，无本任务span；未压制lint，也不声称全crate通过，最终集成必须重跑。

## 尚需审查/限制

- 本端probe健康不等同能观测对端自身probe结果。
- RequestError/Reply不是生产handler/SessionHandle，网络driver/flow-control/GOAWAY/事件背压仍未实现。
- 无真实TCP/双进程/fuzz；本任务仅纯状态断言与编译约束，必须独立review后才能下游使用。


## 独立审查与Low收尾

GLM340566a6独立主审Spec/Quality PASS，复跑12+1与20个一次性边界反例均通过。主代理随后TDD修is_healthy不排drain：新增测试先RED后GREEN，状态目标现13项；删除无产生路径EntropyUnavailable，模块级提升“先PendingTable匹配再accept_pong、先结清旧epoch再重开”契约。target状态13+stream8目标实际exit0，不宣称h2在途组件全crate lint绿。
