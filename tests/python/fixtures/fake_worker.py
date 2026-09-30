#!/usr/bin/env python3
"""Fake worker entrypoint for supervisor tests (no NATS).

  --mode ok      print the runtime's inbox-subscribed marker, spawn a
                 grandchild `sleep` (pid → --pidfile), then sleep
  --mode crash   exit 1 while --counter shows fewer than --crash-times starts,
                 then behave like ok
  --mode silent  never print the marker (readiness must time out)
Unknown args (--identity/--repo/--nats-url/--model) are accepted.
"""
import argparse
import subprocess
import sys
import time

p = argparse.ArgumentParser()
p.add_argument("--mode", default="ok")
p.add_argument("--identity", default="fake")
p.add_argument("--pidfile", default=None)
p.add_argument("--counter", default=None)
p.add_argument("--crash-times", type=int, default=0)
args, _ = p.parse_known_args()

print(f"[fake-worker] starting {args.identity}")
if args.mode == "crash" and args.counter:
    try:
        n = int(open(args.counter).read().strip() or 0)
    except FileNotFoundError:
        n = 0
    with open(args.counter, "w") as f:
        f.write(str(n + 1))
    if n < args.crash_times:
        print(f"[fake-worker] crashing (start #{n + 1})")
        sys.exit(1)

if args.mode != "silent":
    print(f"[fake-worker] subscribed to channel.inbox.{args.identity} (oneshot + sessions)")
if args.pidfile:
    child = subprocess.Popen(["sleep", "300"])
    with open(args.pidfile, "w") as f:
        f.write(str(child.pid))
time.sleep(300)
