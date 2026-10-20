# How to Test / Use `hub-tui`
Need `nats-server` and `hub-server` (query API + router) running
```bash
export CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats

# 1. NATS server (message bus)
nats-server -p 4222 --jetstream &

# 2. hub-server (router + query API, needs a DB path)
/data/cargo-targets/jfrie/nats/debug/hub-server --db-path /tmp/my_hub.db &

# 3. Launch the dashboard
/data/cargo-targets/jfrie/nats/debug/hub-tui
```

If just testing, can generate activity in 2nd Terminal...
```bash
B=/data/cargo-targets/jfrie/nats/debug
$B/hub-register --identity worker-1 --capabilities code,review
$B/hub-publish --channel agents.tasks --from you --message "hello tui"
$B/hub-publish --channel agents.tasks --from woker-1 --message "ack"
```

