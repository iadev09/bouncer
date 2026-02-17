# Bouncer

Rust workspace for LAN bounce ingestion.

## Crates

- `crates/bouncer-proto`: shared frame format (`BNCE` magic, lengths, `OK\n` ACK)
- `crates/bouncer-client`: sync Postfix pipe client (`stdin` -> TCP -> ACK)
- `crates/bouncer-server`: async ingest daemon (TCP, spool queue, watcher, worker)
- `crates/bouncer-observer`: UDP syslog observer (`127.0.0.1:5140` -> TCP publish, no raw mail content)

## Architecture and data flow

### 1) Network topology (hosts + ports)

```text
                     (each mail host)
+-------------------------------+            TCP 2147             +------------------------------+
| Postfix + rsyslog            |-------------------------------->| bouncer-server               |
|  - maillog                   |                                 |  (delivery host / bouncer)   |
|  - UDP forward to 127.0.0.1  |                                 +------------------------------+
|    :5140                     |                                             |
|                               \                                            | MySQL 3306
| +---------------------------+  \ UDP 5140                                  v
| | bouncer-observer service  |<-- rsyslog omfwd                    +------------------------------+
| | - queue-id <-> message-id |                                     | DB                    |
| | - publish observer_event  |------------------------------------>| mail_messages / bounces      |
| +---------------------------+             TCP 2147                +------------------------------+
+-------------------------------+

```

### 2) Raw mail ingest path (`kind=mail`)

```text
[bouncer-client]
      |
      | TCP frame (header + body)
      v
[server tcp listener]
      |
      | atomic write + fsync
      v
[spool incoming/<uuid>.eml] --ACK--> [client]
      |
      +--> [notify watcher]
      +--> [periodic scan fallback]
              |
              v
        [worker dispatcher]
              |
              v
     incoming -> processing -> done/failed
              |
              v
          [db upsert]
```

### 3) Observer delivery status path (`kind=observer_event`)

```text
[postfix cleanup log] ---> queue_id + message-id(hash)
                                  \
                                   +--> [observer queue-map cache]
                                  /
[postfix smtp log] -----> queue_id + dsn/status/action/diagnostic
                                  |
                                  v
                      [observer delivery event]
                                  |
                                  | TCP frame kind=observer_event
                                  v
                        [server tcp listener]
                                  |
                                  v
                         [db apply_observer_event]
```

### 4) Services running inside `bouncer-server`

```text
+---------------------------------------------------------------+
| bouncer-server process                                        |
|                                                               |
|  [shutdown listener SIGTERM/SIGINT]                          |
|  [tcp listener :2147]                                         |
|  [notify watcher (incoming/)]                                 |
|  [periodic scanner]                                           |
|  [worker dispatcher + fixed workers]                          |
|  [imap fallback loop (optional)]                              |
|                                                               |
|  shared state: [spool paths] + [db pool] + [cancel token]    |
+---------------------------------------------------------------+
```

## Build

```bash
cargo check
cargo build --release
```

## Server config

Server config path resolution order:
- `argv[1]` (optional config path argument)
- `BOUNCER_CONFIG_PATH`
- `${HOME}/bouncer.yaml`
- `${HOME}/bouncer.yaml`
- `./bouncer.yaml`
- `./bouncer.yaml`
- legacy fallback: `./Config.yaml`

```yaml
listen: "0.0.0.0:2147"
spool: "./storage/spool/bouncer"
database_url: "mysql://user:pass@127.0.0.1:3306/test_db"
worker_concurrency: 4
process_queue_per_worker: 1024
incoming_scan_secs: 60
# Optional. Omit the whole `imap` block to disable IMAP polling.
imap:
  host: "mail.example.com"
  port: 993
  user: "noreply@example.com"
  pass: "secret"
  mailbox: "INBOX"
  poll_secs: 60
  connect_timeout_secs: 10
  max_messages_per_poll: 200
  max_history: null # optional window; example: "3d"
  mark_seen_if_not_exist: false
```

`imap.max_history` adds `SINCE` to IMAP search and fetches only newer
messages inside that window.  
`imap.mark_seen_if_not_exist` marks a parsed delivery report as seen when its
hash does not exist in local `mail_messages`.
`imap.connect_timeout_secs` bounds IMAP connect/TLS/login/greeting waits so
network outages fail fast with visible poll warnings.

Spool layout:

```text
incoming/
processing/
done/
failed/
```

ACK is returned only after payload is atomically written to `incoming/<uuid>.eml`.
Process queue capacity is calculated as:
`worker_concurrency * process_queue_per_worker`.

## Observer config

Observer config path resolution order:
- `argv[1]` (optional config path argument)
- `OBSERVER_CONFIG_PATH`
- `${HOME}/observer.yaml`
- `${HOME}/observer.yaml`
- `./observer.yaml`
- `./observer.yaml`

```yaml
listen_udp: "127.0.0.1:5140"
server: "10.0.0.10:2147"
source: "mail-01"
queue_capacity: 4096
connect_timeout_secs: 5
io_timeout_secs: 10
heartbeat_secs: 30
mapping_ttl_secs: 86400
```

Observer `from/to` frame metadata is generated internally.
Transport destination is always `server` over TCP.

## Run

Start server:

```bash
cargo run -p bouncer-server
```

Send test mail:

```bash
printf 'Subject: test\n\nhello' | cargo run -p bouncer-client -- --server 127.0.0.1:2147 --from sender@example.com --to bounces@example.com
```

Start observer:

```bash
cargo run -p bouncer-observer
```

Systemd unit templates:
- `deploy/systemd/bouncer-server.service`
- `deploy/systemd/bouncer-observer.service`

Recommended rsyslog forward rule (`/etc/rsyslog.d/49-postmaster.conf`):

```conf
if ($syslogfacility-text == 'mail') then {
  action(type="omfile" file="/var/log/maillog")
  action(
    type="omfwd"
    Target="127.0.0.1"
    Port="5140"
    Protocol="udp"
    template="RSYSLOG_TraditionalForwardFormat"
    queue.type="LinkedList"
    queue.size="10000"
    action.resumeRetryCount="-1"
  )
  stop
}
```

## Status

- TCP ingest + framing: implemented
- Atomic spool enqueue + ACK semantics: implemented
- Observer ingest (`cleanup + smtp`), reconnect and heartbeat: implemented
- Observer delivery events (`kind=observer_event`) are applied directly to DB (no spool write)
- Notify watcher + periodic fallback scan: implemented
- Worker move flow (`incoming -> processing -> done/failed`): implemented
- Worker DB write (`sqlx`, MySQL): implemented (`success/pending/suspended/failed` mapping)
- IMAP fallback loop: implemented (UNSEEN fetch, parse, DB upsert, mark-seen)
