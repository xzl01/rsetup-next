# 真实TCP跨组件装配检查（不是应用双进程验收）

范围：tests/tcp_stack.rs，只有测试代码，无生产实现改动。测试在同一进程用实际TcpListener/TcpStream（临时loopback端口）串接双方Ed25519签名握手/X25519密钥派生→SecureIo→H2KeepaliveIo→h2c→双长流gRPC信封。服务器driver持续poll accept并通过JoinSet处理流，不能inline等body而饿死h2。

3项目标通过：
1 双方握手认证后，同一socket按Control→Data，两流分别传96KiB合法信封，server流窗口32KiB，经真实流控往返且字段/载荷全等、gRPC EOS/trailers正常、两侧Connection/tasks结束。
2 client pin不匹配即UntrustedServer，不交client keys/不开h2，不将server自己的批准冒client信任证明。
3 双方真实TCP认证后注入坏AEAD tag，server AuthFailed/记录层释放，peer实际EOF，不交明文。

cargo test -p rsetup-tunnel --test tcp_stack --locked，CARGO_TARGET_DIR=target/tunnel-tcp-stack，3passed/0failed、exit0；定向rustfmt已执行。首编译因测试误用SecureIo不存在get_ref接口E0599，查实际API后改is_fatal/pair观测并以peerEOF证明关闭；这不是生产RED。此任务是既有实现的集成验证，不宣称TDD产品修复。

TestAdmission只对白名单测试clientpub批准，**不是SQL准入**。同进程真实TCP不等价controller/board两个生产进程，不验证数据库/管理撤销/真实业务handler/设备journal/重连5s排空/故障容量或保活45s；这些须Task7使用最终应用装配另测。所有身份进程内随机，不读secret、不生成可提交凭据、不执行真实重启。
