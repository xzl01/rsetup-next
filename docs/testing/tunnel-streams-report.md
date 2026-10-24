# h2长流读写桥的阶段报告

范围：session/streams.rs、tests/session_streams.rs和session/mod.rs模块登记；主代理实现，未提交/推送。不是完整session driver，尚未接真实握手/SecureIo/控制器/板端进程。

RED：先编写8个真实h2判别测试并运行，接口缺失E0432；其中ready()消耗SendRequest造成测试Drop类型E0509已修为poll_fn(poll_ready)，不把测试自身笔误算RED语义。
GREEN：CARGO_TARGET_DIR=target/tunnel-session-state timeout30s cargo test -p rsetup-tunnel --test session_streams --locked -- --test-threads=1 -> exit0，8 passed/0failed。

覆盖：Control/Data双向REQUEST/RESPONSE；512KiB级信封通过32KiB stream flow window持续发送；未知path回grpc-status12；非法method/header reset只影响该流；发送future取消时guard reset该流，后续发送Closed不拼接半条gRPC；data REFUSED_STREAM不关闭control；grpc-status仅流EOS而非逻辑RPC；服务路径常量。

实现使用h2 SendStream reserve_capacity/poll_capacity/send_data，最多16KiB分片；接收每个h2 DATA release_capacity后按InputCursor逐包消费，不用64包便利批量API；保留当前chunk尾部。header认证为POST/application-grpc/TE trailers；server接受后即时response headers，结束时grpc-status trailer。状态码及错误信息通过信封，不回显底层错误文本。

静态：归属rustfmt通过。clippy --lib --test session_state --test session_streams -Dwarnings exit101，仅7项落在并行writer secure_io.rs，无本任务span；不能称整个crate lint通过，最终整合必须重跑。

待独立review重点：send/open future取消是否完整释放或reset、recv cancellation保留partial与尾部、EOS trailers缺失/错误处理的sticky性与配对sender reset责任、h2 flow-control释放与有界内存假设、路由与peer设置。未做单次driver/流代际注册/重开/GOAWAY/应用装配，不能以8tests宣称生产完成。


## Fix round1（主代理TDD，待独立复审）

备审91b758a3确认3P2+1P3：finish后reset被半关闭标志挡住；Grpc/Protocol/Reset/GrpcStatus后再次recv变EOF；Trailers-Only初始status丢失；+0非法status被当0。主代理先用真h2 RED分别复现，再单项实现：reset始终委托h2；ReceiveTerminal明确Open/Eof/Failed且所有错误统一存储；响应真实EOS且无待收DATA/trailers时解析初始status；统一status helper要求单字段、非空ASCII数字、无前导零且u32不溢出，适用于初始和EOS trailers。依据https://grpc.github.io/grpc/core/md_doc__p_r_o_t_o_c_o_l-_h_t_t_p2.html （主代理实际fetch）Response允许Trailers-Only、Status=1*DIGIT且无leading zeros。

新增6用例至14：半关闭RST对端确收PROTOCOL_ERROR、重复失败不变EOF、TrailersOnly0/12、非法+0/00/01/空/负数/溢出stickyBadResponse、普通nonEOS初始status0不能截断后续packet+最终13、Reset/缺status重复同错。原8目标保留。CARGO_TARGET_DIR=target/tunnel-session-state cargo test -p rsetup-tunnel --test session_streams --locked ->14passed/0failed；同target clippy --lib --test session_streams -Dwarnings exit0；两归属文件rustfmt --check exit0。

仍需owner在首次recv错误显式reset配对sender、回收两半；只drop sender保留receiver是合法半关闭，不强加Drop重置。open取消会同时drop局部两h2引用，备审真h2已确认不泄漏。owner负责15s开流/probe定时。内存预算含h2窗口/maxframe/Bytes backing allocation/decoder/队列，不把发送16KiB分片等同接收chunk16KiB。未跑真实socket/双进程/RSS；完整sessiondriver在独立实现，不冒称本组件已应用接线。
