# 板端生产连接配置校验（组件阶段）

主代理完成src/connect_config.rs、tests/connect_config.rs及lib模块登记。只解析配置，不连接网络、不发现公钥、不读写文件。缺显式RSETUP_CONTROLLER_PUBLIC_KEYS或RSETUP_CONTROLLER_ADDR失败；key hex64/非零/唯一/最多16，轮换列表由管理员配置，无TOFU。地址host:port或[IPv6]:port，拒URL/凭据/空白/非法端口。identity/journal默认持久/var/lib路径，必须绝对无parent traversal且不相同，权限/symlink是后续IdentityKey/JournalStore职责，词法配置不假称安全落盘已证。

TDD先6测试和空模块运行E0432（缺接口，exit101），再实现同目标6passed/0failed；cargo clippy -p rsetup-board-agent --lib --test connect_config --locked -- -Dwarnings exit0，归属rustfmt完成。测试不用真实环境/密钥/DB；lookup纯函数读取env由后续生产入口提供。Debug/Error不回显地址/pins/路径。

尚未完整board connect循环/握手/session/System重连/事件/真实TCP双进程，配置阶段不能称网络已接。待独立审查后供后续Task6消费。预算16pin与2048B配置文本为保守实现限制非握手协议强制；可能限制过多需审查。


独立主审93e94333最终Spec/Quality PASS，复跑6tests/clippy/fmt并临时补4边界；实现无阻断。主代理将两项缺断言持久化（显式空KEY_FILE/JOURNAL_DIR不fallback、identity==journal拒），目标现8passed/0failed。路径长度未单设budget列为后续生产入口限制项；字段pub不提供不可构造保证，Task6入口必须从校验构造器消费。主审自述曾临时置空生产文件验证RED并还原、临时备份写/tmp；这超出其只读/工作区边界授权，不能作为后续review方法，后续审查明确仅stdin/target产物不得临时编辑交付文件/工作区外备份。最终源码已恢复，未据此声称无写操作。
