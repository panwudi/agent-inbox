# Forward an existing Feishu bot's events

Run one `agent-inbox feishu-watch` beside the existing Feishu bot. It reads that
bot's append-only `events.jsonl`, with records shaped as
`{kind, received_ms, event: {header, event: {sender, message}}}`. It does not use
Feishu credentials, subscribe to Feishu, or change the bot's event handling.

## Configuration

1. Add a fixed `[[agents]]` entry to the local relay with `name`, `description`
   and `sender_token_file`. Omit both `inbox` and `token_file`. The sender token
   must be an independent mode-600 file, just like other relay credentials.
   The fixed authenticated name is `<host>.feishubot` when the name is
   `feishubot`. Sender-only entries do not appear as available inboxes; attempts
   to address one return HTTP 405 after journaling.
2. Copy [feishu-watch.toml.example](../deploy/feishu-watch.toml.example), filling
   the event path, local relay, sender token, private state file and each chat's
   qualified `to` name. Paths are relative to the config file unless absolute.
   Each chat has exactly one destination. Names come from `agent-inbox who`;
   mapping a chat to an agent is an operator decision, never event text.
3. Give the watcher read access to the bot's event file and its own sender
   token, and write access to its private state directory. It needs neither
   the bot's app secret nor the relay's host token. Do not weaken the relay's
   private control socket to bridge OS users.
4. Adapt [the systemd unit](../deploy/agent-inbox-feishu-watch.service) to the
   service account, binary/config/state paths and local relay unit name. Keep
   only one watcher for a state file; a kernel lock rejects a second instance.

```sh
agent-inbox feishu-watch --config /absolute/path/watch.toml --once
agent-inbox feishu-watch --config /absolute/path/watch.toml
```

On first use, `start_at_end = true` skips existing history. Subsequent runs use
the saved cursor. Set it to false only when deliberately importing a fresh
journal with a fresh state file. Restart the watcher to load route changes.

Only `im.message.receive_v1` events whose sender type is `user` and whose chat
is configured are forwarded. Bot messages and other events are ignored. Text
is preserved; other content is forwarded as JSON. Each delivery cites the
original `feishu-message: om_<id>`, its source chat and type. The receiver must
fetch that original and apply its own owner verification rules. Each message
includes the complete verification rule, so older sessions can read their own
global instructions if the pinned owner ID is not yet in their context. No
owner ID is supplied by the bridge. The bridge does not turn other participants'
messages or card clicks into owner approval.
Replies belong in the source Feishu chat when authorized; the sender-only
service has no reply inbox.

## State and recovery

The mode-600 JSON state contains the source file identity, byte cursor, the
last 4096 message IDs and up to 256 pending deliveries. It contains message
content; protect it like the inbox journals. Partial final lines wait for the
writer to finish. File replacement/truncation restarts reading at the beginning
with recent message-ID deduplication. Malformed JSON, invalid routed content
or messages exceeding the inbox's 64 KiB limit stop visibly without advancing
past the bad record. Inspect the source before correcting it and restarting.

Directory availability is checked before sending. Messages addressed to an
offline or absent agent stay `queued`; one unavailable agent does not stop
other routes. Pending capacity stops ingestion without advancing the cursor.
Watch the service journal and state file for queued or unconfirmed work.

Before each HTTP delivery, its phase is durably marked `unconfirmed`. HTTP
200/202 records acceptance; an error, disconnect or process crash preserves
the uncertain state and never triggers an automatic resend. Inspect the
receiver's inbox journal or queue first. If it was not accepted, stop the
watcher and explicitly retry the delivery ID:

```sh
agent-inbox feishu-watch --config /absolute/path/watch.toml --retry <delivery-id> --once
```

Then restart the service. HTTP 202 confirms durable acceptance by the inbox,
not that an agent has completed the work. Delivery is not exactly-once across
all failure modes: uncertainty requires reconciliation, and replaying more
than the recent 4096 IDs after source rotation can repeat older messages.
