# systemd Units — nats-hub

Two units run a hub host: `nats-hub-nats.service` (the NATS bus) and
`nats-hub-server.service` (the hub-server router). The router depends on the
bus, so bring them up in this order.

For the full hub install runbook — including TLS cert generation, auth setup,
and firewall rules — see [`../../docs/OPERATOR_HUB.md`](../../docs/OPERATOR_HUB.md).
This README covers the systemd-specific steps only.

## Files

| File | Runs |
|---|---|
| `nats-hub-nats.service`   | `nats-server -c /etc/nats/nats-server.conf` as user `nats` |
| `nats-hub-server.service` | `/usr/local/bin/hub-server --db-path /var/lib/nats-hub/nats_hub.db` as user `nats-hub` |

## Prerequisites

Install the two binaries before enabling the units:

```bash
# NATS server binary — your distro package, or:
curl -L https://github.com/nats-io/nats-server/releases/latest/download/nats-server-v2.10.0-linux-amd64.tar.gz \
    | tar -xz -C /tmp
sudo install -m 0755 /tmp/nats-server-v2.10.0-linux-amd64/nats-server /usr/local/bin/nats-server

# hub-server binary — built from this repo (see README at repo root):
#   cargo build --release --bin hub-server
sudo install -m 0755 target/release/hub-server /usr/local/bin/hub-server
```

## Install steps

Run as root on the hub host:

```bash
# 1. System users — one per service, both unprivileged (no shell, no login).
sudo useradd --system --no-create-home --shell /usr/sbin/nologin nats
sudo useradd --system --no-create-home --shell /usr/sbin/nologin \
    --home-dir /var/lib/nats-hub nats-hub

# 2. State directories with correct ownership.
sudo install -d -m 0750 -o nats -g nats     /run/nats-hub
sudo install -d -m 0750 -o nats -g nats     /etc/nats /etc/nats/tls
sudo install -d -m 0750 -o nats -g nats     /var/log/nats-hub
sudo install -d -m 0750 -o nats-hub -g nats-hub /var/lib/nats-hub

# 3. Config — copy the example and edit secrets in place, OR via envsubst
#    from a secrets manager (recommended for production).
sudo install -m 0640 -o nats -g nats \
    config/nats-server.prod.conf.example /etc/nats/nats-server.conf
# Edit /etc/nats/nats-server.conf: pick token OR users[], fill REPLACE_* values.

# 4. TLS cert + key — see docs/OPERATOR_HUB.md "TLS certificate".
#    Both must end at /etc/nats/tls/nats-server.{crt,key}, owned nats:nats,
#    key 0600, cert 0644.

# 5. Systemd units.
sudo install -m 0644 deploy/systemd/nats-hub-nats.service   /etc/systemd/system/
sudo install -m 0644 deploy/systemd/nats-hub-server.service /etc/systemd/system/
sudo systemctl daemon-reload

# 6. Validate config and units BEFORE starting.
nats-server -t -c /etc/nats/nats-server.conf          # must say "valid"
systemd-analyze verify /etc/systemd/system/nats-hub-*.service

# 7. Enable and start.
sudo systemctl enable --now nats-hub-nats.service
sleep 2  # give the bus a beat
sudo systemctl enable --now nats-hub-server.service

# 8. Verify.
systemctl status nats-hub-nats.service   --no-pager
systemctl status nats-hub-server.service --no-pager
journalctl -u nats-hub-server.service -n 50 --no-pager
```

## Where secrets live

| Secret | Where it lives | Notes |
|---|---|---|
| NATS token / user passwords | `/etc/nats/nats-server.conf` (0600 `nats:nats`) | Don't commit. Inject via your secrets manager (`envsubst`, Vault, sops, etc.). |
| TLS private key | `/etc/nats/tls/nats-server.key` (0600 `nats:nats`) | Self-signed OK for testing; use a real CA / ACME for production. |
| TLS certificate | `/etc/nats/tls/nats-server.crt` (0644) | Public; copy to clients as the CA file for self-signed setups. |

Lock the config down:

```bash
sudo chmod 0600 /etc/nats/nats-server.conf
sudo chown nats:nats /etc/nats/nats-server.conf
```

## Log rotation

`nats-server` writes to `/var/log/nats-hub/nats-server.log`. Add a logrotate
drop-in:

```bash
sudo tee /etc/logrotate.d/nats-hub >/dev/null <<'EOF'
/var/log/nats-hub/*.log {
    daily
    rotate 14
    compress
    delaycompress
    missingok
    notifempty
    copytruncate
}
EOF
```

## Common operations

```bash
sudo systemctl restart nats-hub-nats.service       # restart the bus
sudo systemctl restart nats-hub-server.service     # restart the router
sudo journalctl -u nats-hub-nats.service -f        # follow logs
sudo journalctl -u nats-hub-server.service -f
```

## Backups

The only persistent state is the RocksDB file at `/var/lib/nats-hub/nats_hub.db`.
Snapshot it while hub-server is stopped (or use `hub-history`/`hub-stats` for
consistent logical exports — see `docs/OPERATOR_HUB.md` "Backup").

```bash
sudo systemctl stop nats-hub-server.service
sudo tar -czf "/var/backups/nats-hub-$(date +%F).tar.gz" /var/lib/nats-hub
sudo systemctl start nats-hub-server.service
```
