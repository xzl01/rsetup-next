# rsetup-board-agent（板端本地测试入口）

本 crate 是板端代理库：白名单动作分发、持久 task journal、一次性重启票据与无参 OS 重启边界。

> **边界声明**：当前**没有生产加密隧道**，也没有真实 host reboot。板端业务 Protobuf codec
> （`rsetup_protocol::codec::BusinessCodec`）已按**项目所有者 2026-10-22 的授权**启用，
> `BoardAgent::handle_wire` 走真实 encode → decode → 白名单分发；该授权**不是外部专家决议**，
> 也不构成对 03 规格「待审阅」状态的批准，跨库固定测试向量、两进程互操作与跨库一致性**仍未闭环**。
> 传入 `NoCodec` 等未获批实现时，wire 路径仍按 `CodecUnapproved` → `1009 INTERNAL_ERROR` 失败关闭；
> 未知 `schema_version` 为 1001，超 512 KiB 为 1008，请求自身编码缺陷为 1003。
> `rsetup-board-agent` 可执行文件是本地 amd64 测试夹具，不是板端生产守护进程；其
> `--loopback-test-server` 的逐行 JSON 协议仅用于本机验收，不能当作生产 wire 协议。

## 构建与运行（amd64 本机）

```bash
cargo build -p rsetup-board-agent --release --bin rsetup-board-agent
./target/release/rsetup-board-agent --once --journal-dir /tmp/rsetup-board-agent-journal
```

`--once` 通过真实 `BoardAgent::handle_typed_action` 执行三条只读动作并输出 JSON：

- `device.capabilities.get`
- `device.status.get`（`/proc/loadavg`、`/proc/meminfo`、`/proc/uptime`）
- `device.clock.get`

周期采样（长期运行，适合本机观察）：

```bash
./target/release/rsetup-board-agent --watch --interval-secs 5 \
  --journal-dir /tmp/rsetup-board-agent-journal
```

## 本地 JSON-lines 测试控制台

```bash
./target/release/rsetup-board-agent --loopback-test-server \
  --listen 127.0.0.1:39876 --journal-dir /tmp/rsetup-board-agent-journal
```

每行一个请求，响应一行 JSON：

```text
{"action":"device.capabilities.get"}
{"action":"device.status.get"}
{"action":"device.clock.get"}
{"action":"device.task.get","sub_task_id":"<canonical-uuid-v4>"}
```

- 三个只读动作与 `device.task.get` 走既有白名单/DTO 校验/journal 查询；
- `device.reboot.prepare`/`device.reboot.execute` 恒返回 `1004 capability unavailable`，
  本地入口绝不触发真实重启；
- 未知动作返回 `1002`，畸形请求返回 `INVALID_REQUEST`；请求行上限 4 KiB。

可用 `printf`/`nc` 或任意 TCP 客户端测试；服务只绑定回环地址，不提供 TLS/认证隧道。

## 测试

```bash
cargo test -p rsetup-board-agent --bin rsetup-board-agent
cargo test -p rsetup-board-agent --test board_agent_bin
cargo test -p rsetup-board-agent
```

真实主机重启、生产隧道与 board 部署单元仍属未闭环项，见
[`docs/testing/controller-v1-known-gaps.md`](../../docs/testing/controller-v1-known-gaps.md) 的 b 节。
