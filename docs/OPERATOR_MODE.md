# Operator mode with scoped credentials

Config-mode `hub-admin render-config` remains the default self-host setup.
Operator mode gives each team its own NATS account, account quota, subject
space and JetStream streams. Each account runs one unchanged `hub-server`.
No account imports or exports are needed. This guide describes the H-0
single-node proof; TLS, clustered resolver replication, key custody and
credential issuance are deployment responsibilities.

## Reproduce the spike

```bash
make setup
scripts/dev/install_nsc.sh       # nsc v2.15.0, release SHA256 verified
make build
scripts/dev/operator_stack.sh .venv/bin/python -m pytest -q -s \
    tests/python/test_operator_isolation.py tests/python/test_operator_credentials.py
STRICT_INTEGRATION=1 scripts/dogfood_prod_config.sh --operator
scripts/dogfood_durable.sh --operator
```

The lifecycle test takes about 65 seconds: NSC's shortest relative expiry is
`1m`. A normal `make test` skips these operator-only tests cleanly. CI runs
all the commands above in its operator step. The ACL equivalence unit test
also runs in the regular Rust suite.

`operator_stack.sh <command...>` creates an operator, its system account,
accounts `T_A` and `T_B`, five scoped signing keys per account and per-identity
user credentials. It starts a loopback-only server on a random port, using
the full NATS account resolver. Account JWTs are pushed with `nsc push`.
Each tenant runs `hub-server --durable --require-bound-identity` with a
separate temporary DB and identical `HUB_INGRESS`, `HUB_INBOX`, `HUB_TASKS`
stream names. The harness exports:

- `NATS_URL`: private server URL;
- `OPERATOR_DIR`: private temporary operator, resolver, keys, creds and logs;
- `OPERATOR_CREDS_DIR`: tenant A's credential directory;
- `NATS_HUB_OPERATOR_TEST_STACK=1`: opt-in for live tests;
- `NSC_BIN`: checksum-verified NSC executable.

`--nats-only` leaves hub-server startup/restarts to the existing dogfoods.
The production dogfood starts both tenants' hubs and includes an isolation
section. Cleanup kills only recorded PIDs. Successful runs remove temporary
secrets; failures and `KEEP_LOGS=1` retain the private directory printed at
exit. Remove it after inspection. The spike deliberately holds user seeds
in NSC's private temporary key store; it is not a production issuer.

## Provision a persistent deployment

Use a private NSC directory for all commands (`nsc -H /secure/nsc ...`).
Follow the same operations as the readable provisioning block in
[`operator_stack.sh`](../scripts/dev/operator_stack.sh):

1. `nsc add operator --name YOUR_OPERATOR --sys` creates the trust root and
   system account. Protect the operator seed offline after provisioning.
2. `nsc add account --name YOUR_TENANT`, then `nsc edit account` to set
   connection, stream, consumer, disk and memory quotas. The spike uses
   64 connections, 8 streams, 64 consumers, 4 GB disk and 64 MB memory.
   Default hub streams reserve 1 GiB each: allow more than their aggregate
   reservation, or supply smaller `--delivery-config` stream limits.
3. For each role, get JSON from `hub-admin permission-template --role ROLE`.
   Roles are `worker`, `orchestrator`, `admin`, `service`, `hub-server`.
   Create a signing key with `nsc edit signing-key --account YOUR_TENANT
   --sk generate --role ROLE`, passing the JSON's `pub.allow`/`sub.allow`
   entries as repeated `--allow-pub`/`--allow-sub` flags. If `resp` is
   present, add `--allow-pub-response=1 --response-ttl 2s`. The stack script
   shows this without shell interpolation of permission subjects.
4. Create users with `nsc add user --account YOUR_TENANT --name AGENT -K ROLE
   --expiry 1d`, then `nsc generate creds --account YOUR_TENANT --name AGENT
   --output-file /secure/creds/AGENT.creds`. Agent names must pass the same
   rules as `hub-admin add-agent`: `[A-Za-z0-9_-]+`, no reserved component
   names or API namespaces. The component credential is named `hub-server`.
5. Generate `nsc generate config --nats-resolver --config-file resolver.conf`.
   Set its resolver directory to persistent private storage. Include it in
   the NATS server configuration alongside JetStream and TLS, as in
   [`nats-server.operator.conf.example`](../config/nats-server.operator.conf.example).
   Keep include paths relative to the containing configuration file.
6. Start NATS, then `nsc push --all --account-jwt-server-url tls://HOST:PORT`
   using the system account. No tenant gets system credentials.
7. Start a hub for each account, with a distinct DB/volume:

   ```bash
   NATS_USER=hub-server NATS_CREDENTIALS_FILE=/secure/TENANT/hub-server.creds \
       hub-server --nats-url tls://HOST:PORT --db-path /data/TENANT/db \
       --durable --require-bound-identity --api-admin owner-1
   ```

`NATS_USER=hub-server` is an inbox-prefix hint here; credential-file JWT
authentication takes precedence. It keeps all internal hub clients on
`_INBOX.hub-server.>`. Setting only `NATS_HUB_IDENTITY=hub-server` is
insufficient: the internal wave orchestrator has its own client identity.
Admin JWT permissions alone do not grant API administration: also configure
`--api-admin <identity>` as in config mode.

A worker or MCP process uses its own creds and identity normally:

```bash
NATS_CREDENTIALS_FILE=/secure/worker-1.creds \
    .venv/bin/python echo_worker.py --identity worker-1 --nats-url tls://HOST:PORT
# MCP: NATS_URL=tls://HOST:PORT, NATS_CREDENTIALS_FILE=..., NATS_HUB_IDENTITY=orch-1
```

## Templates and extra channels

`hub-admin permission-template --role worker` emits JSON with `{{name()}}`
in identity positions, including `inbox-{{name()}}`. It and config-mode
`user_entry` use one permission function. `--inbox-stream`, `--tasks-stream`
and repeated `--channel chat` work for templates too. The stream names must
match the hub's delivery config.

Hosted v1 uses **tenant-wide extra broadcast channels** in each applicable
role template. A tenant is one team; task/session/wave channels are already
team-public. A per-agent exception would require an explicitly reviewed
unscoped signer and permissions; the hosted issuer should not silently
fall back to one. Self-host config-mode per-agent `--channel` remains useful.

The issuer must control both role and JWT `name`: scoped keys constrain
permissions to the supplied name, not which identity the signer can mint.
Validate uniqueness/reserved names in the control plane. Never hand a
signing seed to an agent.

## Refresh and revoke

Refresh a user's JWT using the **same user nkey**, sign with the same role,
and atomically rename a complete `0600` creds file over the old one before
expiry. Rust's connection helper now rereads JWT and seed together for every
authentication. nats-py already rereads credentials on reconnect; our shared
helper additionally treats exactly `User Authentication Expired` as a
reconnect event (the library otherwise closes permanently). MCP uses that
same vendored helper. Existing subscriptions survive this reconnect. A
rewrite does not reauthenticate an already connected client immediately;
its original JWT expiry still forces a reconnect.

For a managed NSC user:

```bash
nsc -H /secure/nsc edit user -a YOUR_TENANT -n AGENT -K worker --expiry 1d
nsc -H /secure/nsc generate creds -a YOUR_TENANT -n AGENT -o /secure/creds/AGENT.new
# Set private permissions, then rename AGENT.new over AGENT.creds on the client.
nsc -H /secure/nsc revocations add-user -a YOUR_TENANT -n AGENT --at 0
nsc -H /secure/nsc push -a YOUR_TENANT --account-jwt-server-url tls://HOST:PORT
```

`--at 0` revokes credentials issued at or before now. Also stop refreshing
that user; later issuance with the same key can pass a timestamp revocation.
Pushing the account JWT disconnects the connected revoked user and rejects
reconnects with those credentials. Expiry recovery does not bypass revocation.

For production, use the maintained NATS JWT/NKey libraries in the private
control plane, not NSC subprocesses: keep tenant signing keys in KMS, accept
only client-generated public user keys, return the signed JWT to the device,
and push account updates via the resolver's system API. The device keeps
its seed and performs the atomic rewrite. Issuer outages leave unexpired
credentials usable; refresh still needs the issuer. Auth callout was not
benchmarked in H-0; the observed offline JWT verification and resolver
updates support keeping operator/JWT for v1.

See the upstream [decentralized authentication guide](https://docs.nats.io/learn/security/decentralized-auth)
and [scoped template introduction](https://nats.io/blog/nats-server-29-release/).
