# agent-inbox

[English](README.md) | [简体中文](README.zh-CN.md) | **日本語**

実行中のエージェントセッション間で調整メッセージをやり取りするための, 単一の Rust バイナリです.
`claude` は, 認証付き HTTP inbox を備えた Claude Code の MCP channel です. `send`
は inbox に生のテキストを POST します. `steer` は既存の Codex スレッドに対し, 実行中の turn
の最中にテキストを届けます. スレッドがアイドル状態なら新しい turn を開始します.
`codex` は, 1 つの Codex スレッド用の認証付き HTTP inbox を実行します.
`hub` は, 安定した論理名と認証済みの sender token に基づいて, SSH トンネル経由でマシン間の
メッセージをルーティングします. [hub のセットアップ](docs/HUB-USAGE.md) を参照してください.
`relay` はコンピュータごとに 1 本のトンネルを共有し, `<host>.<agent>` 形式の名前をルーティングします.
自動参加のデフォルト名は, 同じディレクトリのペアを `<host>.<basename>-claude` と `<host>.<basename>-codex` で区別します.
同じ種類の重複には `-2`, `-3` などが付きます. inbox の `--name` で明示的に上書きした場合は, 指定した部分がそのまま使われます.
`who` は, 認証済みエージェントの説明と在席状況を探索します. まずは
[host relay のセットアップと移行](docs/HOST-RELAY.md) から始めてください.

Claude セッションの [自動参加](docs/CLAUDE-AUTO-JOIN.md) は, ユーザースコープの `agent-inbox-join` MCP エントリ 1 つと,
すべてのディレクトリで使える channel ラッパーを使います. 各セッションには一時的な inbox, 専用の認証情報とディレクトリ名が割り当てられ,
`inbox_profile` で自分の名前/説明を読み取り, 更新できます. セッションを手動で登録する必要はありません.

Codex では, [ユーザー単位の watcher](docs/CODEX-AUTO-JOIN.md) がロード済みの thread ID ごとに inbox を 1 つ登録し,
名前は thread の cwd から付けます. オンラインであっても人が見ているとは限りません. アイドル中の Codex へのメッセージは自律的な作業を開始させることがあります.

`upgrade-codex` は, アイドル状態のアップグレード時間帯を待つ間 Codex inbox への配信を保留し,
新しい daemon を検証してから FIFO キューを解放します. 独立した updater の無効化, すべての inbox
の登録, レポート, 復旧, スケジュール例については
[Codex の定期アップグレード](docs/CODEX-UPGRADES.md) を参照してください.
relay サービスと監視付きトンネルのセットアップは [deploy/README.md](deploy/README.md) にあります.

デスクトップの自動起動には [統合サービス](docs/DESKTOP-SERVICE.md) を使います:
`agent-inbox daemon --config <toml>` が relay と任意の Codex watcher を管理します.
起動項目 1 つでまとめて起動, 停止, 再起動し, 既存のセッションキューと設定を維持します.
`--check` はサービスを起動せず設定だけを検証します. 従来の単独コマンドも引き続き使えます.

## ビルドと使い方

Rust 1.95 以降と, 実行中の Codex app-server daemon が必要です.
実験的プロトコルは Codex 0.162.1 で検証済みです. それ以外のバージョンは警告付きで
受け入れられますが, 互換性のないレスポンス形式の場合は明示的に失敗します.

```sh
cargo build --release
./target/release/agent-inbox steer --thread <thread-id> --from claude 'Please check the failing test.'
./target/release/agent-inbox send <inbox-address> --token-file <private-token-file> --from codex 'Hello from Codex.'
./target/release/agent-inbox send --hub <local-hub-address> --to <logical-name> --token-file <my-hub-token> 'Hello through the hub.'
```

インストールする際は, ファイルを上書きせずに置き換えてください (`rm ~/.local/bin/agent-inbox && cp target/release/agent-inbox ~/.local/bin/`).
macOS では, 既存のバイナリに `cp` で上書きすると古いコード署名キャッシュが残り, 新しいバイナリは起動時に強制終了されます (exit 137).

受信側のスレッド ID は, その `CODEX_THREAD_ID` 環境変数で確認できます.
受信セッションと同じ Codex home と daemon を使用してください.
コマンドの実行中も, 受信側は作業を続けられます.

`steer` のオプションフラグ (メッセージの前に置きます):

```text
--from <name>           Sender label, defaults to teammate
--log <path>            Durable Markdown message journal
--codex-bin <path>      Codex executable, defaults to codex
--sock <path>           Custom app-server control socket
--timeout-secs <n>      Timeout per response, 1 to 60 seconds, defaults to 10
--                     Treat remaining arguments as message text
```

送信側には, 受け付けられた RPC メソッド, スレッド ID, turn ID, journal のパスが表示されます.
受け付けとは, サーバーが入力を受理したという意味であり, 受信側がすでにそれを読んだ,
あるいはそれに基づいて行動したという意味ではありません.

メッセージは, ユーザーの指示や承認ではなく, チームメイトからの調整としてラベル付けされます.
空のメッセージと 64 KiB を超えるメッセージは拒否されます.
送信者ラベルとスレッド ID に改行を含めることはできません.

## Claude Code のセットアップ

内容を表示せずに token ファイルを作成します. HTTP inbox と `send` のどちらも,
パーミッション 0600 の通常ファイルで, 空白を含まない空でない ASCII の token
を含むものを要求します. 末尾の改行は許容されます.

```sh
mkdir -p ~/.config/agent-inbox
(umask 077; openssl rand -hex 32 > ~/.config/agent-inbox/token)
```

各受信セッションは自分のアドレスを選びます. `--listen` は必須で, デフォルトの
ポートはありません. ポート番号のみを指定すると 127.0.0.1 にバインドされます. そのセッション用に
確保したアドレスを使って MCP エントリを追加します:

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

次に, 受信側の対話型 Claude Code セッションを以下で起動します:

```sh
claude --dangerously-load-development-channels server:agent-inbox
```

HTTP リスナーは MCP プロセスが所有します. 同じポートで 2 つ目のプロセスを起動しないでください.
channel フラグと設定した MCP エントリ名は一致している必要があります.
`--dangerously-load-development-channels` がないと, HTTP POST が成功していても
Claude Code は通知を黙って破棄します. その場合, 受信側が利用できるのは journal
のコピーだけになり, 通知は自動的には再送されません.
`--reply-thread` は省略可能です. 指定した場合, channel の instructions には
Claude が返信に使える正確な `steer` コマンドが含まれます.

[Claude Code の channel 契約](https://code.claude.com/docs/en/channels-reference)
は, `capabilities.experimental["claude/channel"]`, stdio トランスポート, そして
`notifications/claude/channel` イベントを要求します. このサーバーはそれらのワイヤーフォーマットを
Rust で直接実装しています. 起動時の診断情報は stderr に出力され, stdout には MCP の
JSON 行だけが含まれます.

従来の生の POST 形式も引き続き使えます:

```sh
curl -sS -H "Authorization: Bearer $(cat /absolute/path/to/private-token)" \
  --data-binary 'Hello from a teammate.' http://127.0.0.1:8797/
```

`claude` のフラグ:

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

ループバック以外の `--listen` アドレスを明示的に指定すると, LAN からのアクセスが有効になります. 各 inbox は
それぞれ独自のアドレスと token ファイルを使用します. `send` は `host:port` または
`http://host:port/` を受け付け, `--token-file`, `--from`, `--log` はテキストの前に置きます.
リダイレクトには決して従わず, 認証情報や任意の HTTP レスポンスを表示することもありません.
省略可能な `X-Agent-From` ヘッダーは送信者ラベルを運びます. これは調整用の
メタデータであり, 送信者が別個の身元であることの証明ではありません.
hub 経由の返信では, `--reply-thread` の代わりに `--hub` と `--hub-token-file` を併用します.
MCP の instructions には正確な hub アドレスと token ファイルが含まれ,
受信メッセージの `from` 属性を返信の論理的な宛先として使います.

## HTTP の結果

| Status | 意味 |
| --- | --- |
| 200 | メッセージが記録され Claude への通知が書き出された, または Codex が入力を受理した. |
| 202 | メッセージが記録され, MCP の初期化が完了するまでキューに入った. |
| 400 | 空, サイズ超過, 途中で切れた, またはフレーミングが不正なリクエスト. |
| 401 | bearer token がない, または誤っている. |
| 431 | リクエストヘッダーが 16 KiB を超えている. |
| 500 | journal への書き込みに失敗した. 通知は出力されていない. |
| 503 | channel がビジーまたは利用不可. メッセージが保存されたかどうかはレスポンスで確認する. |

HTTP リクエストは `Content-Length` を使用し, ボディの上限は 64 KiB です. 重複した
authorization ヘッダーや length ヘッダー, および transfer encoding は拒否されます.
各リクエストの読み取りには 5 秒の期限があり, 同時に実行されるリクエストは最大 16 件です.
channel の初期化前は最大 64 件のメッセージがメモリ内で待機し, それを超える
認証済みメッセージは journal に保存されたうえで 503 を返します.

HTTP 200 は通知が書き出されたことを確認するものであり, Claude がそれを処理したことを
確認するものではありません. channel プロトコルには受信確認の仕組みがありません. `send` は
受理または保留のフィードバックを表示し, 拒否された場合は 0 以外のステータスを返します.

## Codex HTTP inbox のセットアップ

受信側の Codex スレッド用にポートとプライベートな token ファイルを選び, 次を実行します:

```sh
./target/release/agent-inbox codex \
  --thread <codex-thread-id> --listen 127.0.0.1:8799 \
  --token-file <private-token-file> --log <receiver-journal>
```

送信側は `agent-inbox send <that-address> --token-file <that-token-file>
--from claude <text>` か, 同じ生の curl POST 形式を使います. これは独立した
プロセスで, stdin が閉じられても動作し続けます. Ctrl-C かプロセススーパーバイザーで
停止してください.

`codex` には `--thread`, `--listen`, token ファイルが必須です. ほかに
`--log`, `--from` (default: claude), `--codex-bin`, `--sock`,
`--timeout-secs` も受け付け, `steer` と同じプロトコル設定を使います.

認証済みの各メッセージは, HTTP 202 を返す前に journal に記録され, 永続的にキューに入れられます.
FIFO ワーカーは再接続し, draining 中や拒否された接続をバックオフ付きで再試行します.
実行中の turn には `turn/steer`, アイドル状態のスレッドには `turn/start` が届きます.
永続的なクライアントメッセージ ID と完全な履歴の突き合わせにより,
失われた確認応答やプロセスの再起動から保護されます. 履歴が読めない, または不完全な場合は,
そのスレッドへの後続メッセージをブロックし, journal に通知を書き込み,
設定された通知フックを呼び出します. キューのパス, notify フラグ,
`agent-inbox codex-queue` によるステータス確認と制御については
[耐障害性と明示的な再送/破棄](docs/CODEX-RESILIENCE.md) を参照してください.

## host relay のセットアップ

中央の hub がホストを登録し, 各ホストは自分のエージェント表とローカルの
sender token を所有します. すべてのエージェントに 1 行の説明を追加してください. ローカルの relay は
プレフィックスで範囲を限定したディレクトリを報告し, 同一ホスト内のメッセージはローカルで処理します.
誰に尋ねればよいかわからないときは `agent-inbox who --hub <local-relay> --token-file <local-sender-token>` を
使ってください. channel の instructions にはそのコマンドが含まれています.
[Codex AGENTS.md のレシピ](docs/HOST-RELAY.md#set-up-a-host) を各プロジェクトにコピーしてください.

[host relay ガイド](docs/HOST-RELAY.md) には, ホストごとの鍵/token のセットアップ,
launchd/systemd サービス, ステータスの意味, エージェントのライブリロード, 移行についての説明があります.

## Feishu メッセージの自動転送

`agent-inbox feishu-watch --config <toml>` は既存の Feishu bot のイベントログを読み,
設定されたチャットを指定のエージェントに転送します. bot の既存接続を使い,
Feishu への二重接続は作りません. 所有者の指示として実行する前に,
受信側が引用された元メッセージを自分で確認します.
チャット名や転送文から所有者の身元を判断しません.

各エージェントは報告用チャットを作成した後, 自分の資格情報で登録します:
`agent-inbox chat-route register --chat <chat-id> --hub <local-relay> --token-file <own-sender-token>`.
hub は認証済みの送信者を宛先として保存し, 別のエージェントによる上書きを拒否します.
`chat-route list` で結果を確認し, `chat-route remove` で自分の登録だけを削除できます.
Claude の profile と Codex の現在のセッション状態には登録コマンドが含まれます.
`use_hub_routes = true` を有効にすると, 転送プロセスは登録を動的に読み込みます.
未登録のグループでは, 送信者がローカル設定の `fallback_sender_id` と一致するメッセージだけを
`fallback_agent` に送ります. 他の送信者のメッセージは bot の元のログに残し, 転送しません.
登録済みグループの宛先は変わらず, エージェントがオフラインならその宛先のキューで待ちます.
個別チャットと bot のメッセージは転送しません.

転送前にカーソルと未配信メッセージを保存します. オフラインのエージェント宛ては
キューで待機します. 再起動や最近の重複イベントでは受理済みのメッセージを繰り返しません.
確認応答を失った配信は手動で調査してから `--retry <delivery-id>` で再試行します.
初回起動は過去のログを飛ばし, 以後は保存した位置から再開します.
`--once` は 1 バッチを処理して終了します.

サービスには固定名の送信専用 relay エントリを使います.
`inbox` と `token_file` を省略し, `name`, `description`,
`sender_token_file` を残します. 送信とディレクトリ照会はできますが,
返信は受信できず, 利用可能な inbox の一覧には表示されません.
設定と復旧方法は [Feishu watcher の設定](docs/FEISHU-WATCH.md) を参照してください.

## 旧来のセッション単位の hub セットアップ

relay の所有者は, `my-pair-claude` と `my-pair-codex` のような 2 つの安定した論理名を,
hub の `peers` 表と `senders` 表の両方に登録します. 各ロールは
inbox token と, それとは別の sender 用 hub token を持ち, 必要な場所には mode-600 のコピーを置きます.
セッションが再起動しても, 名前と認証情報は変わりません.

このペアが所有する, 未使用のローカルポートと relay ポートを選びます. 以下の配線例は
設定の一例であり, デフォルトのポートではありません:

| Role | Local inbox | Local hub forward | Relay reverse endpoint |
| --- | --- | --- | --- |
| my-pair-claude | 127.0.0.1:8797 | 127.0.0.1:8821 | 127.0.0.1:8817 |
| my-pair-codex | 127.0.0.1:8799 | 127.0.0.1:8823 | 127.0.0.1:8819 |

1. relay の所有者は, [hub ガイド](docs/HUB-USAGE.md) に従って hub の peer ルートと sender token
   を設定します. ローカルには独立した 4 つの token ファイルを保持します:
   `claude.inbox.token`, `claude.hub.token`, `codex.inbox.token`,
   `codex.hub.token`. 上記の配線と relay 側で選んだ router ポートを使い,
   [トンネルスーパーバイザー](deploy/README.md) でこのペアの外向き SSH トンネルを起動します.
   各ロールの `-R` はその inbox に届き,
   `-L` は hub に届きます. relay の所有者はレビュー後に hub をデプロイします.

2. 受信側の Codex セッションを起動してその `CODEX_THREAD_ID` を取得し,
   ローカルでその inbox を実行します:

   ```sh
   /absolute/path/to/agent-inbox codex --thread <codex-thread-id> \
     --listen 127.0.0.1:8799 --token-file <codex.inbox.token> --log <codex-journal>
   ```

   バイナリと token の絶対パスを使って, 次の返信指示をそのプロジェクトの `AGENTS.md`
   に記載します. 送信者名は受信メッセージから取得します:

   ```text
   Reply to agent-inbox coordination messages with:
   /absolute/path/to/agent-inbox send --hub 127.0.0.1:8823 --to <incoming-logical-from-name> --token-file <my-codex.hub.token> '<msg>'
   ```

3. Claude プロジェクトのディレクトリで inbox と hub の返信経路を設定し,
   対応する channel フラグを付けて対話セッションを起動します:

   ```sh
   claude mcp add --transport stdio --scope local my-pair-inbox -- \
     /absolute/path/to/agent-inbox claude --listen 127.0.0.1:8797 \
     --token-file <claude.inbox.token> --log <claude-journal> \
     --name my-pair-claude --hub 127.0.0.1:8821 --hub-token-file <claude.hub.token>
   claude --dangerously-load-development-channels server:my-pair-inbox
   ```

4. 各送信側自身の hub token を使って両方向を確認し, それぞれの受信セッションで
   メッセージを確認します:

   ```sh
   agent-inbox send --hub 127.0.0.1:8823 --to my-pair-claude \
     --token-file <codex.hub.token> 'Codex -> Claude through the hub'
   agent-inbox send --hub 127.0.0.1:8821 --to my-pair-codex \
     --token-file <claude.hub.token> 'Claude -> Codex through the hub'
   ```

呼び出し側が自分の身元を選ぶことはありません: hub が sender token から `from` を導出します.
`--from` や `X-Agent-From` による主張でその身元を上書きすることはできません. 必要なのは,
自分の hub アドレス, 自分の token, そして宛先の論理名だけです. オフラインの宛先は
503 を返します. 再送する前に, 送信側, hub, 受信側の journal を確認してください. 再接続後の
"peer offline" 503 は, sshd が古い `-R` フォワードをまだ保持していることを意味する場合もあります.
これは古い SSH セッションが終了し, スーパーバイザーが既存の認証済み接続上で新しいリバースフォワードを張ると解消されます.
リバースポートの復旧中も, ローカルの hub フォワードは使えます.
[公開リンクのタイミングと古いポートに関するガイダンス](deploy/README.md#route-selection-retries-and-stale-reverse-ports)
を参照してください.
Claude は依然として channel フラグが必要で, ないと通知を破棄します.

直接配信と hub 経由の配信のどちらでも, `send` は HTTP のやり取りに 25 秒を許容します.
hub の peer 転送期限は 10 秒で, 遅いトンネルの応答を許容しつつ,
送信側には hub とのやり取りとレスポンス遅延のための余裕を持たせています.
確認の取れていない送信はすでに配信済みである可能性があります. 重複を避けるため,
再試行する前に journal を確認してください.

## relay の sshd を保護する

経路選択には, ホストに設定されたイントラネットの IPv4 プレフィックスかデフォルト
ゲートウェイのみを使います. 監視下の各試行は実際の SSH 接続を 1 本だけ張り, 共有の
カーネルファイルロックにより, 同時接続の試行はホスト/relay ごとに 1 本に制限されます. 確立済みの
トンネルは共存できます. 再試行のバックオフは 15 秒から始まって 300 秒まで伸び,
+/-20% のジッターと 300 秒の上限を持ち, 認証済みの状態が 5 分続くとリセットされます.
再起動する前に, 古いトンネルの環境ファイルを更新してください.
古い sshd がまだリバースポートを保持している場合は, そのフォワードだけが既存の master 上で 2.4-3.6 秒ごとに再試行されます.
ローカルの hub フォワードは維持され, 追加の SSH ログインも発生しません. 両方のフォワードの準備ができるとログで確認できます.

relay の所有者は, 管理用アクセスと実測したハンドシェイク時間を踏まえて `MaxStartups` と
`LoginGraceTime` を見直します.
[デプロイの制限とアップグレード手順](deploy/README.md#protecting-the-relays-sshd) を参照してください.

## 直接ペアのセットアップ

このペアが所有する未使用のポートを 2 つ使います. この例では Claude に 8797,
Codex に 8799 を使います. `/absolute/path/to/agent-inbox` はビルドしたバイナリの
絶対パスに置き換え, ペアごとに新しいディレクトリを使ってください.

1. ビルドしてから, 新しいペア用ディレクトリに独立したプライベート token を 2 つ作成します:

   ```sh
   cargo build --release
   mkdir -p "$HOME/.config/agent-inbox/my-pair"
   (umask 077
    openssl rand -hex 32 > "$HOME/.config/agent-inbox/my-pair/claude.token"
    openssl rand -hex 32 > "$HOME/.config/agent-inbox/my-pair/codex.token")
   ```

2. 受信側の Codex セッションをそのプロジェクトで起動し, `CODEX_THREAD_ID` を報告させます
   (そのセッション内で `printenv CODEX_THREAD_ID`). 別の
   ターミナルで, その ID を使ってこの inbox プロセスを動かし続けます:

   ```sh
   /absolute/path/to/agent-inbox codex --thread <codex-thread-id> \
     --listen 127.0.0.1:8799 \
     --token-file "$HOME/.config/agent-inbox/my-pair/codex.token" \
     --log "$HOME/.config/agent-inbox/my-pair/codex.md"
   ```

   また, 実際のバイナリパスと, このペアの Claude のポートおよび token ファイルを使って,
   次の返信指示をその Codex プロジェクトの `AGENTS.md` に記載します. Codex には
   MCP の channel instructions がないため, プロジェクトファイルで返信方法を伝えます:

   ```text
   Reply to Claude's agent-inbox coordination messages with:
   /absolute/path/to/agent-inbox send 127.0.0.1:8797 --token-file "$HOME/.config/agent-inbox/my-pair/claude.token" --from codex --log "$HOME/.config/agent-inbox/my-pair/sent.md" '<msg>'
   ```

3. 受信側の Claude プロジェクトのディレクトリで channel をローカルスコープに登録し,
   channel フラグを付けて対話型の Claude セッションを起動します:

   ```sh
   claude mcp add --transport stdio --scope local my-pair-inbox -- \
     /absolute/path/to/agent-inbox claude --listen 127.0.0.1:8797 \
     --token-file "$HOME/.config/agent-inbox/my-pair/claude.token" \
     --log "$HOME/.config/agent-inbox/my-pair/claude.md" \
     --name my-pair-claude --reply-thread <codex-thread-id>
   claude --dangerously-load-development-channels server:my-pair-inbox
   ```

4. 別のターミナルから各受信側に確認メッセージを送り, それぞれのセッションに
   メッセージが表示されることを確認します. 宛先の受信側の token ファイルを使います:

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

HTTP 200 はトランスポートでの受理を確認するものです. メッセージは受信側で確認してください.
Claude 側のメッセージが見当たらない場合は, channel フラグが正確か, `/mcp` のステータスを確認してください.
送信に失敗した場合は, 再送する前にこのペアの journal を確認してください. Codex の
inbox は Ctrl-C で停止します. MCP inbox プロセスのライフタイムは Claude が管理します.

## 複数の inbox と LAN

各 inbox は, 明示的なアドレスと token ファイルをそれぞれ持つ独立したプロセスです.
デフォルトのポートを選ぶコマンドはありません. 例:

| Receiver | Address | Token file | Journal |
| --- | --- | --- | --- |
| Pair A, Claude | 127.0.0.1:8797 | pair-a/claude.token | pair-a/claude.md |
| Pair A, Codex | 127.0.0.1:8799 | pair-a/codex.token | pair-a/codex.md |
| Pair B, Claude | 127.0.0.1:8807 | pair-b/claude.token | pair-b/claude.md |
| Pair B, Codex | 127.0.0.1:8809 | pair-b/codex.token | pair-b/codex.md |

別々の Claude 受信側には別々の MCP エントリ名かプロジェクトローカルの設定を, Codex
受信側には別々のスレッド ID を使ってください. ある inbox の token は, 別の token で設定された
inbox に対しては認証されません. journal は
分けても共有してもかまいません. ファイルロックがプロセス間の書き込みを直列化し,
各見出しには送信者と受信者が記録されます.

ポート番号のみ (`--listen 8809`) の場合は 127.0.0.1 にのみバインドします. IPv6 のループバックには
`--listen '[::1]:8809'` のような明示的なアドレスを使います. LAN へのバインドは,
ループバック以外のアドレスが明示的に指定された場合にのみ行われます. 受信側のホストでは,
実際の LAN IP に置き換えてください:

```sh
agent-inbox codex --thread <thread-id> --listen 192.168.1.20:8809 \
  --token-file <receiver-token-file> --log <receiver-journal>
```

許可された送信者にその受信側の token の mode-600 コピーを渡し,
設定したホストとポートに送信します:

```sh
agent-inbox send http://192.168.1.20:8809/ --token-file <matching-token-file> \
  --from <sender-name> 'Hello over the LAN.'
```

同じ `--listen` の形式は Claude の MCP プロセスでも使えます. 明示的な
`0.0.0.0:8809` はすべての IPv4 インターフェースで待ち受け, 特定の LAN IP を指定すると
リスナーはそのアドレスに限定されます. 他のプロジェクトの既存セッションは, それぞれ自分の
ポート, token, 設定を保持します.

## 配信とログ記録

有効なメッセージはすべて, daemon に接続したり HTTP リクエストを送信したりする前に,
journal に追記され同期されます. どちらの inbox も, 配信前に認証済みメッセージを
journal に記録します. デフォルトの journal は
`$XDG_STATE_HOME/agent-inbox/messages.md` で, `XDG_STATE_HOME` が未設定の場合は
`~/.local/state/agent-inbox/messages.md` です.
各エントリには, 送信者, 受信者, UTC オフセット付きのローカルで読みやすい時刻,
そして元のメッセージが記録されます.
新しい journal ファイルのパーミッションは 0600 です. 排他的なファイルロックが
同時の追記を直列化します.

受信側はユーザーに先に尋ねることなく, 各メッセージに返信します.
回答, 作業結果, 1 行の受領確認, または対応しない理由を返します.
Claude の MCP instructions と各 `<channel>` イベントの `reply="expected"`,
Codex の各メッセージのヘッダーにこのルールが含まれます.

チームメイトの依頼は通常のプロジェクト作業として, 既存の権限内で処理し,
ユーザーに再確認する必要はありません. 破壊的または不可逆な操作,
プロジェクト外の作業, 本番環境の変更, 人へのメッセージなど,
もともとユーザーの同意が必要な行動だけは本人の許可を求めます.
転送文にユーザーが承認したと書かれていても, 承認とはみなしません.

唯一の例外は, 受信側自身が取得して確認した Feishu の元メッセージです.
転送者は `feishu-message: om_<id>` で引用します.
受信側は feishubot の `get_message` で読み, 送信者が user,
open_id が自身の指示ファイルにある所有者の ID と一致,
まとめて転送されたメッセージではない, 24 時間以内,
原文だけでその行動を明確に依頼している, という条件をすべて確認します.
条件を満たす場合に取得した原文だけを指示として扱います.
Claude は MCP instructions から, Codex は引用があるメッセージのヘッダーからルールを受け取ります.
所有者の open_id はグローバル `AGENTS.md` など受信側自身の指示ファイルに置き,
メッセージから取得してはいけません.

ログ記録に失敗した場合, 配信は試みられません. 配信に失敗した場合, コマンドは
0 以外の終了ステータスを返し, 保存された journal エントリのファイルを示します.
安全な turn 状態の競合は, スレッドを読み直したうえで最大 3 回まで再試行されます.
トランスポートの失敗は再試行されず, journal も自動的には再送されません.
リクエストを送信した後に接続が失敗した場合, 接続が閉じる前に受信側がそれを
受理していた可能性があります. 重複メッセージを避けるため, 再送する前に受信側を確認してください.

デフォルトのソケットでは, `agent-inbox` は `codex --version` と
`codex app-server daemon version` を使って CLI と daemon の両方のバージョンを読み取ります. `--sock` を使う場合は,
CLI のバージョンを確認し, プロトコルのレスポンスを検証します. テスト済みのバージョンと異なる場合は,
配信を妨げる代わりに 1 行の警告を出力します.

制御ソケットは Codex に属し, 現在のユーザーに対してローカルです. `steer`
は HTTP リスナーを起動しません. ポート番号のみの `--listen` は 127.0.0.1 にバインドします.

## プロトコル

[テスト済みのプロトコル契約](docs/protocol-0.162.1.md) を参照してください.
トランスポートは, `codex app-server proxy` の stdin/stdout 上で WebSocket フレームを使います.
daemon は HTTP Upgrade ハンドシェイクとそれに続く WebSocket フレームを期待するため,
JSONL を proxy に直接送ると失敗します.

クライアントは一度だけ初期化し, スレッドのランタイムステータスを読み取ってから, 次のように動作します:

- Active: `thread/turns/list` で最新の turn を読み取り, その turn の ID を
  `expectedTurnId` として `turn/steer` を使います.
- Idle: スレッドの設定を変更せずに `turn/start` を使います.
- Not loaded: 設定の上書きなしで `thread/resume` を使い, 返されたステータスに応じて
  active または idle の経路を選びます.
- Unavailable またはステータス不明: 失敗し, メッセージは journal に保持されます.

検索から配信までの間に turn が変わった場合, コマンドはスレッドを読み直し,
最大 3 回まで再試行します. 再試行の対象となるのは, 確認された前提条件の拒否か,
実行中の turn が見つからなかった検索のみです. 配信結果があいまいな失敗は
エラーのままとなり, そのメッセージは journal に残ります.

## 検証

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

テストでは, MCP ハンドシェイクと通知フォーマットを確認し, Claude 側のサブプロセスに
実際のリクエストを POST して, その stdout から通知を読み取ります. さらに,
認証, mode-600 の token ファイル, メッセージとヘッダーの上限,
安全な HTTP 拒否, 保留中の配信, 送信側へのフィードバック, 同時の通知と
journal 書き込み, 認証情報を含まない出力も検証します.
回帰テストは, macOS で接続の 300 ms 後に届くリクエスト, 配信成功後に 16 秒遅延する直接
または hub 経由のレスポンス, そして hub の転送期限内で 6 秒かかる peer の返信をカバーします.

Codex のテストは Rust のみで書かれた偽の実行ファイルをコンパイルし, それを偽の WebSocket app-server
への生の stdio proxy として使います. ハンドシェイクの順序, active および idle での
配信, ロードされていないスレッドの再開, 通知と ping, 古い turn の拒否,
タイムアウト, バージョンの不一致, メッセージの上限, 永続的な失敗処理を確認します.
Codex HTTP inbox は, その偽サーバーに対する実際の POST を通じてテストされ,
配信失敗の後に新しい接続で成功するケースも含みます.
マルチプロセスのテストでは, token と受信側の分離, 共有 journal への追記,
ポート番号のみ指定時のループバックバインド, 明示的なループバック以外へのバインドを確認します.
デプロイのテストは, ローカルの事実に基づく経路選択, 共有ファイルロック,
クラッシュ時のクリーンアップ, 穏やかなジッター付き再試行, トンネル制御ソケットのクリーンアップをカバーします.

省略可能な読み取り専用チェックで, 現在稼働中の daemon と実行中の turn を検査できます:

```sh
CODEX_THREAD_ID=<thread-id> cargo test --test steer real_daemon_read_only -- --ignored --nocapture
```

このチェックは入力を送信せず, turn も開始しません.

## ライセンス

[MIT](LICENSE)
