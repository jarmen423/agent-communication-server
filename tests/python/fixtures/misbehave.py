#!/usr/bin/env python3
"""Misbehaving CLI for HeadlessCli tests.

  misbehave.py sleep <pidfile>   spawn a grandchild `sleep`, record both pids, sleep
  misbehave.py nonzero           partial stdout + stderr, exit 3
  misbehave.py noisy             1 MiB of stderr, then "ok" on stdout, exit 0
  misbehave.py echo-argv …       print the argv it received as JSON
"""
import json
import subprocess
import sys
import time

mode = sys.argv[1] if len(sys.argv) > 1 else ""

if mode == "sleep":
    child = subprocess.Popen(["sleep", "300"])
    with open(sys.argv[2], "w") as f:
        f.write(f"{child.pid}\n")
    time.sleep(300)
elif mode == "nonzero":
    print("partial output before failing", flush=True)
    sys.stderr.write("boom: something broke\n")
    sys.exit(3)
elif mode == "noisy":
    line = "x" * 1023 + "\n"
    for _ in range(1024):
        sys.stderr.write(line)
    sys.stderr.flush()
    print("ok")
elif mode == "echo-argv":
    print(json.dumps(sys.argv[2:]))
else:
    sys.exit(64)
