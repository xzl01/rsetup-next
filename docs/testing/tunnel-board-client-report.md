# 六方法业务BoardClient适配的阶段记录

主代理实现transport/board_client.rs+tests/transport_board_client.rs及transport/mod.rs登记。DeviceRpc是连接管理器未来实现的唯一会话RPC注入接口，不建立socket、不声称已接UnwiredBoardClient；生产main尚未使用本模块，必须最终装配才可交付。

- 使用真实BusinessCodec encode/decode与owner witness，不另造JSON生产载荷；六方法固定Control动作。
- DTO错误/非法schema/坏响应typedkind失败关闭；prepare响应绑定subtask+boot，execute预发送核对ticket/request的subtask、boot、token并校验DTO，返回record也绑定subtask；task_record仅1006映射None，未知status不变成成功；超时/断线透传结果未知，一次调用不自动重试。
- RpcReply Debug隐藏payload；不从peer error原文提取不受信reason。

TDD/验证：先写6tests、空模块编译后接口E0432 exit101（bash-3697已收齐）。实现后cargo test -p rsetup-controller --test transport_board_client --locked -> exit0，6passed。后续补4tests覆盖status+clock成功、prepare/execute/task成功与wire绑定、prepare错subtask、unknownstatus；再次命令exit101，因其它在途h2_keepalive.rs1491调用尚未实现notify_peer_goaway，不是本模块运行失败，**新增四项尚未验证**，整体验证与review待模块稳定后完成。归属rustfmt已执行，无真实凭据/DB参与。

限制：当前测试Rpc为内存fake并记录真实wire；不能作为真实网络、SQL批准、双进程证据。DeviceRpc实现必须按已认证公钥/当前代际路由，不能自动重放变更，session必须先验证trace/action/stream。未来manager装配、HTTP实时view、snapshot轮询均尚未由本模块提供。


## 在途阻塞解除后的复验

GOAWAY模块缺方法的在途编译错误解除后，主代理实跑 cargo test -p rsetup-controller --test transport_board_client --locked：10 passed/0failed（原6+新4）。测试覆盖六方法typed成功路径、精确action/wire、ticket/subtask绑定、错误码和结果未知不重试。仍是mock RPC真实codec单元测试，不等价真实网络manager装配。


## 独立主审与测试Low收尾

GLMf611b0cf Spec/Quality PASS，真实复跑10目标通过且无逻辑阻断。主代理按6项测试证据缺口补5个组合回归：execute/task错误响应subtask、prepare错boot、Rejected/边界status、execute错token/boot零发送、prepare/taskquery wire字段精确解码。当前cargo test -p rsetup-controller --test transport_board_client --locked ->15passed/0failed；无生产逻辑修改。caller仍负责session公钥/trace/action/代际证明、5s与scheduler TTL/撤权，mock不冒真实网络。
