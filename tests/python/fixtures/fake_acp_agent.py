#!/usr/bin/env python3
"""Fake ACP agent (JSON-RPC 2.0 over stdio) for acp_stdio tests.

The prompt text selects behaviour:
  "crash"       exit(3) mid-turn with a stderr message (EOF for the client)
  "permission"  ask session/request_permission (reject/once/always options)
  "fs"          send an fs/read_text_file request (client must refuse it)
The reply text is JSON describing what the client sent us.
Every initialize writes ~200 KiB to stderr (deadlocks an undrained pipe).
"""
import json
import os
import sys

state = {"caps": None, "n": 0}


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def read_msg():
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    return json.loads(line)


def ask(method, params, req_id):
    send({"jsonrpc": "2.0", "id": req_id, "method": method, "params": params})
    while True:
        msg = read_msg()
        if msg.get("id") == req_id and "method" not in msg:
            return msg


def prompt(params):
    text = params["prompt"][0]["text"]
    sid = params["sessionId"]
    info = {"caps": state["caps"], "pid": os.getpid(), "session": sid}
    if "crash" in text:
        sys.stderr.write("fatal: agent crashed on purpose\n")
        sys.stderr.flush()
        os._exit(3)
    if "permission" in text:
        resp = ask("session/request_permission", {"sessionId": sid, "toolCall": {"title": "rm"},
                   "options": [{"optionId": "opt-reject", "name": "No", "kind": "reject_once"},
                               {"optionId": "opt-once", "name": "Once", "kind": "allow_once"},
                               {"optionId": "opt-always", "name": "Always", "kind": "allow_always"}]},
                   "perm-1")
        info["permission"] = resp.get("result")
    if "fs" in text:
        resp = ask("fs/read_text_file", {"sessionId": sid, "path": "/etc/hostname"}, "fs-1")
        info["fs"] = resp.get("error") or resp.get("result")
    for chunk in ("hello ", json.dumps(info)):
        send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid, "update": {
            "sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": chunk}}}})
    return {"stopReason": "end_turn"}


while True:
    msg = read_msg()
    method, params, req_id = msg.get("method"), msg.get("params") or {}, msg.get("id")
    if method is None:
        continue
    if method == "initialize":
        state["caps"] = params.get("clientCapabilities")
        sys.stderr.write(("stderr noise " * 80 + "\n") * 200)
        sys.stderr.flush()
        result = {"protocolVersion": 1, "authMethods": [{"id": "cached_token"}]}
    elif method == "session/new":
        state["n"] += 1
        result = {"sessionId": f"s-{os.getpid()}-{state['n']}"}
    elif method == "session/prompt":
        result = prompt(params)
    else:  # authenticate, session/set_model, session/command, session/set_mode
        result = {}
    send({"jsonrpc": "2.0", "id": req_id, "result": result})
