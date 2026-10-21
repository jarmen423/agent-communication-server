"""Live account-boundary tests, run by scripts/dev/operator_stack.sh.

Skip in a normal config-mode suite; the dedicated CI step provisions both
accounts and unchanged durable hub-server processes before invoking pytest.
"""
import asyncio
import json
import os
import uuid
from datetime import datetime, timezone
from pathlib import Path

import nats
import pytest

pytestmark = pytest.mark.skipif(
    not os.environ.get("NATS_HUB_OPERATOR_TEST_STACK"),
    reason="requires scripts/dev/operator_stack.sh (make setup/build + install_nsc.sh)",
)


def creds(tenant, user):
    return str(Path(os.environ["OPERATOR_DIR"]) / "creds" / tenant / f"{user}.creds")


async def connect(tenant, user="hub-server", **kwargs):
    return await nats.connect(os.environ["NATS_URL"], user_credentials=creds(tenant, user),
                              inbox_prefix=f"_INBOX.{user}".encode(), **kwargs)


async def api(nc, user, op, params=None):
    response = await nc.request(f"hub.api.{user}.{op}",
                                json.dumps({"op": op, "params": params or {}}).encode(), timeout=5)
    result = json.loads(response.data)
    assert result.get("ok"), result
    return result["data"]


def envelope(user, channel, marker):
    return json.dumps({"meta": {"id": str(uuid.uuid4()), "from": user, "to": None,
                                "channel": channel, "kind": "message", "reply_to": None,
                                "timestamp": datetime.now(timezone.utc).isoformat()},
                       "payload": {"marker": marker}}).encode()


async def absent(sub):
    with pytest.raises(nats.errors.TimeoutError):
        await sub.next_msg(timeout=0.25)


def test_account_subject_spaces():
    async def run():
        clients = [await connect(t) for t in ("T_A", "T_B")]
        try:
            token = uuid.uuid4().hex
            # Even the tenant's most privileged credential cannot cross accounts.
            for subject in (f"hub.probe.{token}", f"channel.task.{token}",
                            f"_INBOX.hub-server.{token}", f"hub.delivery.bob.{token}"):
                subs = [await nc.subscribe(subject) for nc in clients]
                responders = [await nc.subscribe(f"hub.probe.response.{token}") for nc in clients]
                for nc in clients:
                    await nc.flush()
                for index, nc in enumerate(clients):
                    marker = f"tenant-{index}".encode()
                    if subject.startswith("_INBOX"):
                        # Exercise a real dynamic response grant, so a publish
                        # denial cannot make this isolation assertion vacuous.
                        await nc.publish(f"hub.probe.response.{token}", b"request", reply=subject)
                        request = await responders[index].next_msg(timeout=2)
                        await nc.publish(request.reply, marker)
                    else:
                        await nc.publish(subject, marker)
                    await nc.flush()
                    assert (await subs[index].next_msg(timeout=2)).data == marker
                    await absent(subs[1-index])
                for sub in subs + responders:
                    await sub.unsubscribe()
                print(f"PASS tenant account boundary: {subject.split(token)[0]} (both directions)")
        finally:
            for nc in clients:
                await nc.close()
    asyncio.run(run())


def test_two_routers_query_api_and_jetstream_are_isolated():
    async def run():
        clients = [await connect(t, "bob") for t in ("T_A", "T_B")]
        channel = f"task.isolation{uuid.uuid4().hex}"
        try:
            subs = [await nc.subscribe(f"channel.{channel}") for nc in clients]
            for nc in clients:
                await nc.flush()
            for index, nc in enumerate(clients):
                await nc.publish(f"hub.pub.bob.{channel}", envelope("bob", channel, f"tenant-{index}"))
                msg = json.loads((await subs[index].next_msg(timeout=5)).data)
                assert msg["payload"]["marker"] == f"tenant-{index}"
                await absent(subs[1-index])
            for index, nc in enumerate(clients):
                # Both APIs have the same subject; DB contents identify which answered.
                for _ in range(50):
                    result = await api(nc, "bob", "history.query", {"channel": channel})
                    rows = result["envelopes"]
                    if rows:
                        break
                    await asyncio.sleep(0.1)
                assert [row["payload"]["marker"] for row in rows] == [f"tenant-{index}"]
                # Same stream and subject names; DIRECT.GET returns this account's record.
                result = await nc.request("$JS.API.DIRECT.GET.HUB_TASKS",
                                          json.dumps({"last_by_subj": f"channel.{channel}"}).encode(), timeout=5)
                assert json.loads(result.data)["payload"]["marker"] == f"tenant-{index}"
                await api(nc, "bob", "ping")
                await api(nc, "bob", "ping")  # persistent reply subscriptions survive repeated calls
            print("PASS two durable routers: own routes, history API, HUB_TASKS DIRECT.GET only")
        finally:
            for nc in clients:
                await nc.close()
    asyncio.run(run())


def test_scoped_worker_denials_and_system_account():
    async def run():
        errors = []
        async def on_error(error):
            errors.append(str(error).lower())
        nc = await connect("T_B", "carol", error_cb=on_error)
        try:
            subjects = ["hub.pub.bob.task.probe", "channel.inbox.bob", "_INBOX.bob.probe",
                        "hub.delivery.bob.inbox", "$JS.API.STREAM.INFO.HUB_INBOX", "$SYS.REQ.SERVER.PING"]
            for subject in subjects:
                await nc.publish(subject, b"{}")
                await nc.subscribe(subject)
            await nc.flush()
            await asyncio.sleep(0.3)
            for subject in subjects:
                assert any("publish" in e and subject.lower() in e for e in errors), (subject, errors)
                assert any("subscription" in e and subject.lower() in e for e in errors), (subject, errors)
            await api(nc, "carol", "ping")  # own scoped permissions really work
            print("PASS worker publish/subscribe denials: hub, channel, _INBOX, delivery, JS, SYS")
        finally:
            await nc.close()
        # A hub-server has broad tenant permissions, but no system-account access.
        privileged = await connect("T_B", error_cb=on_error)
        try:
            with pytest.raises((nats.errors.TimeoutError, nats.errors.NoRespondersError)):
                await privileged.request("$SYS.REQ.SERVER.PING", b"", timeout=0.5)
        finally:
            await privileged.close()
    asyncio.run(run())
