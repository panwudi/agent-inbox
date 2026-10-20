# agent-inbox

[English](README.md) | **简体中文** | [日本語](README.ja.md)

一个 Rust 二进制, 用于在正在运行的 agent 会话之间传递协作消息.
`claude` 是带有鉴权 HTTP inbox 的 Claude Code MCP channel. `send`
把原始文本 POST 到某个 inbox. `steer` 在现有 Codex thread 的当前 turn 中
把文本送进去, thread 空闲时则启动一个 turn.
`codex` 为单个 Codex thread 运行一个鉴权 HTTP inbox.
`hub` 经由 SSH 隧道, 按稳定的逻辑名和经过鉴权的发送方 token 路由跨机器消息.
参见 [hub 配置](docs/HUB-USAGE.md).
`relay` 让每台电脑共用一条隧道, 并路由 `<host>.<agent>` 形式的名字.
自动加入的默认名字用 `<host>.<basename>-claude` 和 `<host>.<basename>-codex` 区分同一目录下的一对会话;
同类型的重复项依次加 `-2`, `-3` 等后缀. 显式的 inbox `--name` 覆盖会保留其指定的那一段.
`who` 发现经过鉴权的 agent 描述及其可用状态. 请从
[host relay 配置与迁移](docs/HOST-RELAY.md) 开始.

Claude 会话的 [自动加入](docs/CLAUDE-AUTO-JOIN.md) 使用一个用户级的 `agent-inbox-join` MCP 条目,
以及一个对所有目录生效的 channel 包装脚本. 每个会话获得一个临时 inbox, 私有凭据和一个目录名;
`inbox_profile` 读取或更新它自己的名字/描述. 无需手工添加会话条目.
新版 Claude 会话优先使用自己的原生收件口, 后台会话即使漏带 channel 启动参数也能收信.
有明确续接记录时, 旧登记退出, 新会话保留原名字; 独立会话仍各自保留.
收件口不可用时不再显示在线. 旧 channel 收件方式仍需匹配的启动参数.
传输检查细节见自动加入说明.

对于 Codex, [按用户运行的 watcher](docs/CODEX-AUTO-JOIN.md) 为每个已加载的精确 thread ID 注册一个 inbox,
名字取自 thread 的 cwd. 在线并不表示有人在看; 发给空闲 Codex 的消息可能会启动自主工作.

`upgrade-codex` 在等待空闲升级窗口期间暂存 Codex inbox 的投递, 校验新的 daemon,
然后放行 FIFO 队列. 关于禁用独立更新程序, 注册每个 inbox, 报告, 恢复以及
定时示例, 参见 [定时 Codex 升级](docs/CODEX-UPGRADES.md).
relay 服务与受监管隧道的配置见 [deploy/README.md](deploy/README.md).

桌面开机启动使用 [一个统一服务](docs/DESKTOP-SERVICE.md):
`agent-inbox daemon --config <toml>` 统一管理 relay 和可选的 Codex watcher.
一个启动项负责整体启动, 停止和重启, 保留现有会话队列和配置.
`--check` 只检查配置, 不启动服务. 原有独立命令仍可使用.

## 构建与使用

需要 Rust 1.95 或更高版本, 以及一个正在运行的 Codex app-server daemon.
该实验性协议已在 Codex 0.162.1 上验证. 其他版本也会接受, 但会给出警告;
不兼容的响应结构会明确报错.

```sh
cargo build --release
./target/release/agent-inbox steer --thread <thread-id> --from claude 'Please check the failing test.'
./target/release/agent-inbox send <inbox-address> --token-file <private-token-file> --from codex 'Hello from Codex.'
./target/release/agent-inbox send --hub <local-hub-address> --to <logical-name> --token-file <my-hub-token> 'Hello through the hub.'
```

安装时请替换文件, 而不是覆盖写入 (`rm ~/.local/bin/agent-inbox && cp target/release/agent-inbox ~/.local/bin/`).
在 macOS 上, 用 `cp` 覆盖已有二进制会保留旧的代码签名缓存, 新二进制一启动就会被杀掉 (exit 137).

接收方的 thread ID 可在其 `CODEX_THREAD_ID` 环境变量中找到.
请使用与接收会话相同的 Codex home 和 daemon.
命令运行期间, 接收方可以继续工作.

`steer` 的可选参数, 放在消息之前:

```text
--from <name>           Sender label, defaults to teammate
--log <path>            Durable Markdown message journal
--codex-bin <path>      Codex executable, defaults to codex
--sock <path>           Custom app-server control socket
--timeout-secs <n>      Timeout per response, 1 to 60 seconds, defaults to 10
--                     Treat remaining arguments as message text
```

发送方会看到被接受的 RPC 方法, thread ID, turn ID 以及 journal 路径.
"接受"表示服务器接受了输入; 并不表示接收方已经读到或已据此行动.

消息会被标注为队友之间的协作, 而不是用户的指令或批准.
空消息和超过 64 KiB 的消息会被拒绝.
发送方标签和 thread ID 不能包含换行符.

## Claude Code 配置

创建 token 文件, 不要打印其内容. HTTP inbox 和 `send` 都要求它是权限为 0600
的普通文件, 内容为不含空白字符的非空 ASCII token. 允许末尾有一个换行符.

```sh
mkdir -p ~/.config/agent-inbox
(umask 077; openssl rand -hex 32 > ~/.config/agent-inbox/token)
```

每个接收会话自行选择地址. `--listen` 是必填项; 没有默认端口.
只给端口时绑定 127.0.0.1. 用为该会话预留的地址添加一个 MCP 条目:

```json
{
  "mcpServers": {
    "agent-inbox": {
      "command": "/absolute/path/to/agent-inbox",
      "args": [
        "claude",
        "--listen", "127.0.0.1:8797",
        "--token-file", "/absolute/path/to/private-token",
        "--log", "/absolute/path/to/messages.md",
        "--reply-thread", "<codex-thread-id>"
      ]
    }
  }
}
```

然后用以下命令启动接收消息的交互式 Claude Code 会话:

```sh
claude --dangerously-load-development-channels server:agent-inbox
```

HTTP 监听器由 MCP 进程持有. 不要在同一端口上再启动第二个进程.
channel 参数与所配置的 MCP 条目名必须一致.
如果没有 `--dangerously-load-development-channels`, 即使 HTTP POST 成功,
Claude Code 也会静默丢弃这些 notification. 此时 journal 是接收方唯一可用的副本;
notification 不会自动重放.
`--reply-thread` 是可选的; 设置后, channel instructions 中会包含
Claude 可用于回复的确切 `steer` 命令.

[Claude Code channel 约定](https://code.claude.com/docs/en/channels-reference)
要求 `capabilities.experimental["claude/channel"]`, stdio 传输以及
`notifications/claude/channel` 事件. 本服务器直接用 Rust 实现了这些线上格式.
启动诊断输出到 stderr; stdout 只包含 MCP JSON 行.

旧的原始 POST 形式仍然可用:

```sh
curl -sS -H "Authorization: Bearer $(cat /absolute/path/to/private-token)" \
  --data-binary 'Hello from a teammate.' http://127.0.0.1:8797/
```

`claude` 参数:

```text
--listen <addr>         Required; a bare port uses 127.0.0.1; port 0 chooses a free port
--token-file <path>     Required, or use AGENT_INBOX_TOKEN_FILE
--log <path>            Durable message journal
--name <receiver>       Journal receiver label, defaults to claude
--from <sender>         Default sender for curl, defaults to codex
--reply-thread <id>     Codex thread used by the configured reply command
--hub <addr>            Hub address through this session's local SSH forward
--hub-token-file <path> This sender's hub token, for configured hub replies
```

显式指定非 loopback 的 `--listen` 地址即可开启局域网访问. 每个 inbox 使用
各自的地址和 token 文件. `send` 接受 `host:port` 或
`http://host:port/`, 并在文本之前给出 `--token-file`, `--from` 和 `--log`.
它从不跟随重定向, 也从不打印凭据或任意 HTTP 响应.
可选的 `X-Agent-From` 头携带发送方标签; 它是协作元数据,
不能证明发送方是一个独立的身份.
hub 回复同时使用 `--hub` 和 `--hub-token-file`, 而不是
`--reply-thread`. MCP instructions 会包含确切的 hub 地址和 token 文件,
并把传入消息的 `from` 属性用作回复的逻辑目的地.

## HTTP 结果

| 状态码 | 含义 |
| --- | --- |
| 200 | 消息已记录且 Claude notification 已写出, 或输入已被 Codex 接受. |
| 202 | 消息已记录并等待初始化, 或已写入 Claude 原生收件口. |
| 400 | 请求为空, 超长, 被截断或分帧错误. |
| 401 | bearer token 缺失或错误. |
| 431 | 请求头超过 16 KiB. |
| 500 | journal 写入失败; 未发出 notification. |
| 503 | channel 忙或不可用; 查看响应以确认消息是否已保存. |

HTTP 请求使用 `Content-Length`, body 上限为 64 KiB. 重复的
authorization 或 length 头以及 transfer encoding 都会被拒绝. 读取
每个请求有 5 秒的期限, 最多同时处理 16 个请求.
channel 初始化之前, 最多 64 条消息在内存中等待; 超出的
经鉴权消息会保存到 journal 并返回 503.

HTTP 200/202 确认传输层已接受, 而不是 Claude 已处理消息.
需要实际收信确认时请让对方回复. `send` 会显示已接受或待处理的反馈,
被拒绝时返回非零状态.

## Codex HTTP inbox 配置

为接收方 Codex thread 选择一个端口和私有 token 文件, 然后运行:

```sh
./target/release/agent-inbox codex \
  --thread <codex-thread-id> --listen 127.0.0.1:8799 \
  --token-file <private-token-file> --log <receiver-journal>
```

发送方使用 `agent-inbox send <that-address> --token-file <that-token-file>
--from claude <text>` 或同样的原始 curl POST 形式. 这是一个独立进程,
即使 stdin 关闭也会继续运行. 用 Ctrl-C 或你的进程管理器停止它.

`codex` 需要 `--thread`, `--listen` 和一个 token 文件. 它还接受
`--log`, `--from` (默认: claude), `--codex-bin`, `--sock` 和
`--timeout-secs`, 协议设置与 `steer` 相同.

每条经鉴权的消息在返回 HTTP 202 之前都会写入 journal 并持久化入队.
FIFO worker 会重连, 并对 draining/refused 连接进行带退避的重试.
活跃的 turn 收到 `turn/steer`; 空闲的 thread 收到 `turn/start`.
持久化的客户端消息 ID 和完整历史比对可以防范
确认丢失和进程重启. 历史无法读取或不完整时,
会阻塞发往该 thread 的后续消息, 写入一条 journal 通知并调用
所配置的通知 hook. 队列路径, notify 参数以及 `agent-inbox codex-queue`
状态/控制, 参见 [韧性与显式 resend/drop](docs/CODEX-RESILIENCE.md).

## 配置 host relay

中央 hub 负责注册主机, 而每台主机拥有自己的 agent 表和本地
发送方 token. 为每个 agent 添加一行描述. 本地 relay
上报其按前缀限定范围的目录, 并在本地处理同一主机内的消息.
不确定该问谁时, 使用 `agent-inbox who --hub <local-relay> --token-file <local-sender-token>`.
channel instructions 中包含该命令; 请把
[Codex AGENTS.md 配方](docs/HOST-RELAY.md#set-up-a-host) 复制到每个项目中.

[host relay 指南](docs/HOST-RELAY.md) 包含每台主机的 key/token 配置,
launchd/systemd 服务, 状态含义, agent 实时重载以及迁移.

## 自动转发飞书消息

`agent-inbox feishu-watch --config <toml>` 读取已有飞书机器人的事件日志,
按照配置把每个群的消息交给指定的 agent. 它复用机器人现有的连接,
不会再建立第二条飞书监听连接. 接收方执行业主指令前, 必须自行回查原消息;
不能根据群名或转发文字认定谁是业主.

每个 agent 建好汇报群后, 用自己的身份登记:
`agent-inbox chat-route register --chat <chat-id> --hub <local-relay> --token-file <own-sender-token>`.
hub 根据已认证的发送者绑定收件人并保存, 拒绝别的 agent 覆盖已有登记.
`chat-route list` 用于核对结果, `chat-route remove` 只能移除自己的登记.
Claude 的 profile 和 Codex 当前会话状态都包含登记命令.
启用 `use_hub_routes = true` 后, 转发程序会实时读取这些登记.
没有登记的群, 只有发送人匹配本地配置 `fallback_sender_id` 的消息才交给 `fallback_agent`.
其他发送人的消息保留在机器人原始日志中, 不转发. 已登记群按原有归属转发;
对应的 agent 离线时仍为它保留消息. 私聊和机器人消息不转发.

转发前会保存读取位置和待发消息. agent 离线时, 消息留在队列里等待.
重启和近期的重复事件不会重发已被接收的消息. 如果投递结果不确定,
先人工核对, 再用 `--retry <delivery-id>` 重试. 第一次启动跳过旧记录,
后续启动接着保存的位置继续. `--once` 处理一批后退出.

为服务配置固定的只发送 relay 条目: 省略 `inbox` 和 `token_file`,
保留 `name`, `description` 和 `sender_token_file`.
这类条目可以发消息和查询目录, 但不能收回复, 也不会列为可用收件箱.
路由, 状态和恢复方法见 [飞书转发配置](docs/FEISHU-WATCH.md).

## 旧版按会话的 hub 配置

relay 所有者注册两个稳定的逻辑名, 例如 `my-pair-claude`
和 `my-pair-codex`, 同时写入 hub 的 `peers` 和 `senders` 表. 每个角色都有
一个 inbox token 和一个独立的发送方 hub token, 并在需要的地方保存 mode-600 副本.
会话重启时, 这些名字和凭据保持不变.

选择未被占用, 归这一对所有的本地端口和 relay 端口. 下面的示例接线只是
配置示例, 并非一组默认端口:

| 角色 | 本地 inbox | 本地 hub 转发 | relay 反向端点 |
| --- | --- | --- | --- |
| my-pair-claude | 127.0.0.1:8797 | 127.0.0.1:8821 | 127.0.0.1:8817 |
| my-pair-codex | 127.0.0.1:8799 | 127.0.0.1:8823 | 127.0.0.1:8819 |

1. relay 所有者按照 [hub 指南](docs/HUB-USAGE.md) 配置 hub 的 peer 路由和发送方 token.
   在本地保留四个相互独立的 token 文件:
   `claude.inbox.token`, `claude.hub.token`, `codex.inbox.token` 和
   `codex.hub.token`. 按上面的接线和 relay 选定的 router 端口, 使用
   [隧道监管程序](deploy/README.md) 启动这一对的出站 SSH 隧道.
   每个角色的 `-R` 通向其 inbox;
   其 `-L` 通向 hub. relay 所有者在评审后部署 hub.

2. 启动接收消息的 Codex 会话并获取其 `CODEX_THREAD_ID`, 然后在本地运行
   它的 inbox:

   ```sh
   /absolute/path/to/agent-inbox codex --thread <codex-thread-id> \
     --listen 127.0.0.1:8799 --token-file <codex.inbox.token> --log <codex-journal>
   ```

   把下面这条回复说明放进该项目的 `AGENTS.md`, 使用二进制和 token 的绝对路径.
   发送方名字取自传入的消息:

   ```text
   Reply to agent-inbox coordination messages with:
   /absolute/path/to/agent-inbox send --hub 127.0.0.1:8823 --to <incoming-logical-from-name> --token-file <my-codex.hub.token> '<msg>'
   ```

3. 在 Claude 项目目录中, 配置它的 inbox 和 hub 回复路径,
   然后用匹配的 channel 参数启动交互式会话:

   ```sh
   claude mcp add --transport stdio --scope local my-pair-inbox -- \
     /absolute/path/to/agent-inbox claude --listen 127.0.0.1:8797 \
     --token-file <claude.inbox.token> --log <claude-journal> \
     --name my-pair-claude --hub 127.0.0.1:8821 --hub-token-file <claude.hub.token>
   claude --dangerously-load-development-channels server:my-pair-inbox
   ```

4. 用各发送方自己的 hub token 检查两个方向, 然后在各接收会话中
   确认消息:

   ```sh
   agent-inbox send --hub 127.0.0.1:8823 --to my-pair-claude \
     --token-file <codex.hub.token> 'Codex -> Claude through the hub'
   agent-inbox send --hub 127.0.0.1:8821 --to my-pair-codex \
     --token-file <claude.hub.token> 'Claude -> Codex through the hub'
   ```

调用方不能自选身份: hub 根据发送方 token 推导出 `from`.
`--from` 和 `X-Agent-From` 声明无法覆盖这一身份. 你只需要
自己的 hub 地址, 自己的 token 以及目的地的逻辑名. 目的地离线时
返回 503; 重发之前请检查发送方, hub 和接收方的 journal.
重连之后, "peer offline" 503 也可能表示 sshd 仍持有一个过期的 `-R` 转发.
旧的 SSH 会话结束, 且监管程序在已鉴权的现有连接上挂上新的反向转发后, 它就会消失.
在反向端口恢复期间, 本地 hub 转发保持可用. 参见
[公网链路时序与过期端口指南](deploy/README.md#route-selection-retries-and-stale-reverse-ports).
Claude 仍然需要 channel 参数, 否则会丢弃 notification.

无论直连还是经 hub 投递, `send` 都为 HTTP 交换留出 25 秒.
hub 的 peer 转发期限是 10 秒, 既容许较慢的隧道回复,
又让发送方为 hub 交换和响应延迟留有余量.
未确认的发送可能已经送达; 重试之前请检查 journal,
以免产生重复消息.

## 保护 relay 的 sshd

路由选择只使用主机所配置的内网 IPv4 前缀或默认网关.
每次受监管的尝试只建立一条真实的 SSH 连接, 并用共享的内核文件锁
把每个 host/relay 的并发拨号限制为一个. 已建立的隧道可以共存.
重试退避从 15 秒开始, 增长到 300 秒, 使用 +/-20% 抖动并以 300 秒为硬上限,
在鉴权成功持续五分钟后重置. 重启之前请更新旧的隧道环境文件.
如果旧的 sshd 仍占着反向端口, 只有这个转发会在现有 master 上每 2.4-3.6 秒重试一次;
本地 hub 转发保持存活, 也不会产生额外的 SSH 登录. 两个转发都就绪时日志会给出确认.

relay 所有者需要根据管理访问和实测的握手时间审视 `MaxStartups` 和 `LoginGraceTime`.
参见 [部署限制与升级说明](deploy/README.md#protecting-the-relays-sshd).

## 配置直连对

使用两个未被占用, 归这一对所有的端口. 示例中 Claude 用 8797,
Codex 用 8799. 把 `/absolute/path/to/agent-inbox` 替换为构建出的二进制的
绝对路径, 并为每一对使用一个新目录.

1. 构建, 然后在一个全新的 pair 目录中创建两个相互独立的私有 token:

   ```sh
   cargo build --release
   mkdir -p "$HOME/.config/agent-inbox/my-pair"
   (umask 077
    openssl rand -hex 32 > "$HOME/.config/agent-inbox/my-pair/claude.token"
    openssl rand -hex 32 > "$HOME/.config/agent-inbox/my-pair/codex.token")
   ```

2. 在其项目中启动接收消息的 Codex 会话, 并让它报告
   `CODEX_THREAD_ID` (在该会话内执行 `printenv CODEX_THREAD_ID`). 在另一个
   终端中, 用该 ID 保持这个 inbox 进程运行:

   ```sh
   /absolute/path/to/agent-inbox codex --thread <codex-thread-id> \
     --listen 127.0.0.1:8799 \
     --token-file "$HOME/.config/agent-inbox/my-pair/codex.token" \
     --log "$HOME/.config/agent-inbox/my-pair/codex.md"
   ```

   同时把下面这条回复说明放进该 Codex 项目的 `AGENTS.md`, 使用
   实际的二进制路径以及这一对的 Claude 端口和 token 文件. Codex 没有
   MCP channel instructions, 所以由项目文件告诉它如何回复:

   ```text
   Reply to Claude's agent-inbox coordination messages with:
   /absolute/path/to/agent-inbox send 127.0.0.1:8797 --token-file "$HOME/.config/agent-inbox/my-pair/claude.token" --from codex --log "$HOME/.config/agent-inbox/my-pair/sent.md" '<msg>'
   ```

3. 在接收消息的 Claude 项目目录中, 以 local scope 注册 channel,
   然后用 channel 参数启动交互式 Claude 会话:

   ```sh
   claude mcp add --transport stdio --scope local my-pair-inbox -- \
     /absolute/path/to/agent-inbox claude --listen 127.0.0.1:8797 \
     --token-file "$HOME/.config/agent-inbox/my-pair/claude.token" \
     --log "$HOME/.config/agent-inbox/my-pair/claude.md" \
     --name my-pair-claude --reply-thread <codex-thread-id>
   claude --dangerously-load-development-channels server:my-pair-inbox
   ```

4. 在另一个终端中, 向每个接收方发送一条检查消息, 并确认每个会话
   都显示了它的消息. 使用目标接收方的 token 文件:

   ```sh
   /absolute/path/to/agent-inbox send 127.0.0.1:8797 \
     --token-file "$HOME/.config/agent-inbox/my-pair/claude.token" \
     --from codex --log "$HOME/.config/agent-inbox/my-pair/sent.md" \
     'Pair setup check: Codex -> Claude'
   /absolute/path/to/agent-inbox send 127.0.0.1:8799 \
     --token-file "$HOME/.config/agent-inbox/my-pair/codex.token" \
     --from claude --log "$HOME/.config/agent-inbox/my-pair/sent.md" \
     'Pair setup check: Claude -> Codex'
   ```

HTTP 200 确认传输层已接受; 请在接收方中确认消息.
如果 Claude 的消息没出现, 检查 channel 参数是否完全正确以及 `/mcp` 状态.
如果发送失败, 重发之前请检查这一对的 journal. 用 Ctrl-C 停止 Codex
inbox; Claude 的 MCP inbox 进程由 Claude 管理其生命周期.

## 多个 inbox 与局域网

每个 inbox 是一个独立进程, 有自己显式指定的地址和 token 文件.
没有任何命令会选择默认端口. 例如:

| 接收方 | 地址 | Token 文件 | Journal |
| --- | --- | --- | --- |
| Pair A, Claude | 127.0.0.1:8797 | pair-a/claude.token | pair-a/claude.md |
| Pair A, Codex | 127.0.0.1:8799 | pair-a/codex.token | pair-a/codex.md |
| Pair B, Claude | 127.0.0.1:8807 | pair-b/claude.token | pair-b/claude.md |
| Pair B, Codex | 127.0.0.1:8809 | pair-b/codex.token | pair-b/codex.md |

为不同的 Claude 接收方使用不同的 MCP 条目名或项目本地配置,
为 Codex 接收方使用不同的 thread ID. 一个 inbox 的 token
无法通过配置了另一个 token 的 inbox 的鉴权. journal
可以分开也可以共用; 文件锁会在进程之间串行化写入,
每个标题都记录发送方和接收方.

只给端口 (`--listen 8809`) 时只绑定 127.0.0.1. IPv6 loopback 需使用
显式地址, 例如 `--listen '[::1]:8809'`. 只有在提供了显式的非 loopback 地址时,
才会绑定到局域网. 在接收主机上, 替换为
它实际的局域网 IP:

```sh
agent-inbox codex --thread <thread-id> --listen 192.168.1.20:8809 \
  --token-file <receiver-token-file> --log <receiver-journal>
```

给获得授权的发送方一份该接收方 token 的 mode-600 副本, 然后发送到
所配置的主机和端口:

```sh
agent-inbox send http://192.168.1.20:8809/ --token-file <matching-token-file> \
  --from <sender-name> 'Hello over the LAN.'
```

同样的 `--listen` 形式也适用于 Claude MCP 进程. 显式的
`0.0.0.0:8809` 监听所有 IPv4 接口; 指定某个局域网 IP 则把
监听限制在该地址. 其他项目中已有的会话保留各自的
端口, token 和配置.

## 投递与日志

每条有效消息都会在联系 daemon 或发送 HTTP 请求之前追加并同步到 journal.
两种 inbox 也都会在投递之前把经鉴权的消息写入 journal. 默认 journal 是
`$XDG_STATE_HOME/agent-inbox/messages.md`, 未设置 `XDG_STATE_HOME` 时为
`~/.local/state/agent-inbox/messages.md`.
每条记录包含发送方, 接收方, 带 UTC 偏移的本地可读时间,
以及原始消息.
新的 journal 文件权限为 0600. 排他文件锁会串行化
并发追加.

接收方应直接回复每条消息, 不必先问用户: 给出答案, 处理结果,
一行确认, 或说明为什么不处理. Claude 的 MCP instructions 和每条
`<channel>` 事件的 `reply="expected"` 会提示这条规则;
Codex 的每条消息也会在开头说明.

队友的请求按项目内的正常工作处理, 在原有权限内直接做,
无需让用户重复确认. 只有本来就需要用户同意的动作, 例如破坏性或不可逆操作,
项目之外的事情, 生产变更和给真人发消息, 才需要用户本人的许可.
即使转发文字声称用户已经同意, 也不算用户授权.

唯一例外是接收方亲自取回并核对过的飞书消息.
转发者用 `feishu-message: om_<id>` 引用原消息;
接收方用 feishubot 的 `get_message` 读取它. 只有发送者是 user,
open_id 与接收方自身指令中的业主身份一致, 不是合并转发,
消息在 24 小时以内, 且原文单独就清楚要求该动作时,
才按业主指令处理, 并且只执行取回的原文.
Claude 从 MCP instructions 获得此规则; Codex 在引用飞书消息时会在消息开头收到它.
业主 open_id 应放在接收方自己的指令文件中, 例如全局 `AGENTS.md`,
不能从消息里取.

如果写日志失败, 不会尝试投递. 如果投递失败, 命令会
返回非零退出状态, 并指出已保存的 journal 条目所在的文件.
安全的 turn 状态竞争会在重新读取 thread 后重试最多三次.
传输失败不会重试, journal 也不会自动重放.
如果请求发出后
连接失败, 可能意味着接收方在连接关闭之前已接受了它;
重发之前请检查接收方, 以免产生重复消息.

对于默认 socket, `agent-inbox` 通过
`codex --version` 和 `codex app-server daemon version` 读取 CLI 和 daemon 两者的版本.
使用 `--sock` 时, 它检查 CLI 版本并校验协议响应. 与
测试版本不同的版本只会产生一行警告, 而不会阻止投递.

控制 socket 属于 Codex, 且仅限当前用户本地使用. `steer`
不会启动 HTTP 监听器. 只给 `--listen` 端口时绑定到 127.0.0.1.

## 协议

参见 [经过测试的协议约定](docs/protocol-0.162.1.md).
传输层通过 `codex app-server proxy` 的 stdin/stdout 使用 WebSocket 帧.
直接向 proxy 发送 JSONL 会失败, 因为 daemon 期望先进行 HTTP
Upgrade 握手, 然后才是 WebSocket 帧.

客户端初始化一次, 读取 thread 的运行时状态, 然后:

- 活跃: 用 `thread/turns/list` 读取最新的 turn, 然后使用 `turn/steer`,
  并以该 turn 的 ID 作为 `expectedTurnId`.
- 空闲: 使用 `turn/start`, 不改变 thread 的配置.
- 未加载: 使用不带配置覆盖的 `thread/resume`, 然后
  根据返回的状态选择活跃或空闲路径.
- 不可用或未知状态: 报错失败, 消息保留在 journal 中.

如果 turn 在查询与投递之间发生变化, 命令会重新读取 thread
并重试最多三次. 只有已确认的前置条件拒绝, 或查询时没有活跃 turn,
才符合重试条件. 含糊的投递失败
仍作为错误处理, 其消息保留在 journal 中.

## 验证

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

测试检查 MCP 握手和 notification 格式, 并向一个 Claude 侧子进程 POST 真实请求,
然后从其 stdout 读取 notification. 测试还
覆盖鉴权, mode-600 token 文件, 消息和请求头上限,
安全的 HTTP 拒绝, 待处理投递, 发送方反馈, 并发 notification
与 journal 写入, 以及不含凭据的输出.
回归测试覆盖 macOS 上连接后 300 ms 才到达的请求, 投递成功后
延迟 16 秒的直连或 hub 响应, 以及在 hub 转发期限内耗时六秒的 peer 回复.

Codex 测试会编译一个纯 Rust 的伪可执行文件, 把它用作通往伪 WebSocket app-server
的原始 stdio proxy. 它们检查握手顺序, 活跃和空闲
投递, 未加载 thread 的 resume, notification 与 ping, 过期 turn 拒绝,
超时, 版本不匹配, 消息上限以及持久化的失败处理.
Codex HTTP inbox 通过针对该伪服务器的真实 POST 进行测试,
包括一次失败投递之后再用全新连接成功投递的情形.
多进程测试检查 token 与接收方隔离, 共享 journal 追加,
只给端口时的 loopback 绑定, 以及显式的非 loopback 绑定.
部署测试覆盖基于本地事实的路由选择, 共享文件锁,
崩溃清理, 温和的带抖动重试以及隧道控制 socket 的清理.

一项可选的只读检查会检视当前在线的 daemon 和活跃 turn:

```sh
CODEX_THREAD_ID=<thread-id> cargo test --test steer real_daemon_read_only -- --ignored --nocapture
```

该检查不会发送输入, 也不会启动 turn.

## 许可证

[MIT](LICENSE)
