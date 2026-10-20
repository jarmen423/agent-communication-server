#!/usr/bin/env python3
"""Switch every license declaration in the repo to ONE license, in one step.

nats-hub has not chosen a license yet (see LICENSE.md). Today the declarations
disagree: Cargo.toml says BSL-1.1, the plugin manifests say MIT, and there is
no LICENSE file. Once Josh decides, run for example:

    python3 packaging/license/set_license.py MIT --dry-run      # show the plan
    python3 packaging/license/set_license.py MIT                # apply it

    python3 packaging/license/set_license.py Apache-2.0

    python3 packaging/license/set_license.py BSL-1.1 \\
        --change-date 2030-01-01 --change-license Apache-2.0 \\
        --additional-use-grant "You may make production use of the Licensed Work, \\
    provided you do not offer it to third parties as a hosted messaging service."

What it does:
  * writes LICENSE with the full license text. MIT is generated here. Apache-2.0
    and BUSL-1.1 are downloaded from SPDX license-list-data (pinned tag, SHA-256
    checked), and BUSL-1.1 gets its Parameters block;
  * sets the SPDX id in Cargo.toml, both plugin.json manifests, the Hermes
    plugin.yaml (BSL-1.1 becomes SPDX `BUSL-1.1`, because
    `BSL-1.0` is the unrelated Boost license);
  * rewrites the README section between <!-- license:start/end --> markers;
  * deletes LICENSE.md (this decision note), which is then obsolete.

It is stdlib-only and idempotent. Review with `git diff` and commit the result.
Set NATS_HUB_REPO_ROOT to run it against a different tree (the tests do).
"""

from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import os
import re
import sys
import urllib.request
from pathlib import Path

ROOT = Path(os.environ.get("NATS_HUB_REPO_ROOT") or Path(__file__).resolve().parents[2])

HOLDER_DEFAULT = "Agent Memory Labs"  # Cargo.toml `authors`
PROJECT = "nats-hub"

# SPDX ids per choice. BSL-1.1 on the command line = SPDX BUSL-1.1.
SPDX = {"MIT": "MIT", "Apache-2.0": "Apache-2.0", "BSL-1.1": "BUSL-1.1"}

SPDX_TAG = "v3.29.0"
SPDX_TEXT = {  # id -> sha256 of text/<id>.txt at SPDX_TAG
    "Apache-2.0": "074e6e32c86a4c0ef8b3ed25b721ca23aca83df277cd88106ef7177c354615ff",
    "BUSL-1.1": "d90560217049db1d6020ec7cf482da4198bfa372dffd22270ba08fd8088cd93b",
}

MIT_TEXT = """MIT License

Copyright (c) {year} {holder}

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"""

BUSL_PARAMS = """Business Source License 1.1

Parameters

Licensor:             {holder}
Licensed Work:        {project} (the version released with this file)
                      The Licensed Work is (c) {year} {holder}
Additional Use Grant: {grant}
Change Date:          {change_date}
Change License:       {change_license}

For information about alternative licensing arrangements for the Licensed
Work, please contact the Licensor.

Notice

"""

README_TEXT = {
    "MIT": "MIT. See [`LICENSE`](LICENSE).",
    "Apache-2.0": "Apache License 2.0. See [`LICENSE`](LICENSE).",
    "BUSL-1.1": (
        "Business Source License 1.1 (`BUSL-1.1`); converts to {change_license} "
        "on {change_date}. See [`LICENSE`](LICENSE) for the Additional Use Grant."
    ),
}

# (path, regex, replacement template). `{id}` is the SPDX id.
DECLARATIONS = [
    ("Cargo.toml", r'(?m)^license\s*=\s*"[^"]*"', 'license = "{id}"'),
    ("claude-code-plugin/.claude-plugin/plugin.json", r'"license"\s*:\s*"[^"]*"', '"license": "{id}"'),
    ("codex-plugin/.codex-plugin/plugin.json", r'"license"\s*:\s*"[^"]*"', '"license": "{id}"'),
]


def fetch_spdx(spdx_id: str) -> str:
    url = f"https://raw.githubusercontent.com/spdx/license-list-data/{SPDX_TAG}/text/{spdx_id}.txt"
    with urllib.request.urlopen(url, timeout=30) as resp:  # noqa: S310 (fixed https URL)
        data = resp.read()
    digest = hashlib.sha256(data).hexdigest()
    if digest != SPDX_TEXT[spdx_id]:
        sys.exit(f"sha256 mismatch for {url}: got {digest}, expected {SPDX_TEXT[spdx_id]}")
    return data.decode("utf-8")


def license_text(spdx_id: str, a: argparse.Namespace) -> str:
    if spdx_id == "MIT":
        return MIT_TEXT.format(year=a.year, holder=a.holder)
    body = fetch_spdx(spdx_id)
    if spdx_id == "BUSL-1.1":
        # SPDX's text starts with the title; the Parameters block goes in front.
        body = re.sub(r"^Business Source License 1\.1\s*\n", "", body, count=1)
        return BUSL_PARAMS.format(
            holder=a.holder, project=PROJECT, year=a.year, grant=a.additional_use_grant,
            change_date=a.change_date, change_license=a.change_license,
        ) + body
    return body


class Plan:
    def __init__(self, dry_run: bool) -> None:
        self.dry_run = dry_run
        self.changes: list[str] = []

    def write(self, rel: str, new: str) -> None:
        path = ROOT / rel
        old = path.read_text() if path.exists() else None
        if old == new:
            return
        self.changes.append(f"{'create' if old is None else 'update'} {rel}")
        if not self.dry_run:
            path.write_text(new)

    def delete(self, rel: str) -> None:
        if (ROOT / rel).exists():
            self.changes.append(f"delete {rel}")
            if not self.dry_run:
                (ROOT / rel).unlink()


def sub_once(rel: str, text: str, pattern: str, repl: str) -> str:
    new, n = re.subn(pattern, repl, text, count=1)
    if n != 1:
        sys.exit(f"{rel}: license declaration not found (pattern {pattern!r}); update this script")
    return new


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("license", choices=sorted(SPDX))
    ap.add_argument("--holder", default=HOLDER_DEFAULT, help=f"copyright holder / Licensor (default: {HOLDER_DEFAULT})")
    ap.add_argument("--year", default=str(_dt.date.today().year))
    ap.add_argument("--change-date", help="BSL-1.1: date the Change License takes effect (YYYY-MM-DD)")
    ap.add_argument("--change-license", default="Apache-2.0", help="BSL-1.1: license after the Change Date")
    ap.add_argument("--additional-use-grant", help="BSL-1.1: production use you allow (or 'None')")
    ap.add_argument("--dry-run", action="store_true", help="print the plan, write nothing")
    a = ap.parse_args()

    spdx_id = SPDX[a.license]
    if spdx_id == "BUSL-1.1":
        if not a.change_date or not a.additional_use_grant:
            ap.error("BSL-1.1 needs --change-date and --additional-use-grant")
        if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", a.change_date):
            ap.error("--change-date must be YYYY-MM-DD")

    plan = Plan(a.dry_run)

    for rel, pattern, repl in DECLARATIONS:
        text = (ROOT / rel).read_text()
        plan.write(rel, sub_once(rel, text, pattern, repl.format(id=spdx_id)))

    # Hermes manifest has no license key today: replace or insert after `version:`.
    rel = "hermes-plugin/plugin.yaml"
    text = (ROOT / rel).read_text()
    if re.search(r"(?m)^license:", text):
        text = sub_once(rel, text, r"(?m)^license:.*$", f"license: {spdx_id}")
    else:
        text = sub_once(rel, text, r"(?m)^(version:.*)$", rf"\1\nlicense: {spdx_id}")
    plan.write(rel, text)

    rel = "README.md"
    text = (ROOT / rel).read_text()
    blurb = README_TEXT[spdx_id].format(change_license=a.change_license, change_date=a.change_date)
    text = sub_once(
        rel, text, r"(?s)<!-- license:start -->.*?<!-- license:end -->",
        f"<!-- license:start -->\n{blurb}\n<!-- license:end -->",
    )
    plan.write(rel, text)

    if a.dry_run and spdx_id != "MIT":
        # Don't hit the network for a dry run; the text is fetched on apply.
        plan.changes.append(f"create/update LICENSE (SPDX {SPDX_TAG} text/{spdx_id}.txt, sha256-pinned)")
    else:
        plan.write("LICENSE", license_text(spdx_id, a))
    plan.delete("LICENSE.md")

    verb = "would" if a.dry_run else "did"
    print(f"{spdx_id}: {verb} make {len(plan.changes)} change(s) under {ROOT}")
    for c in plan.changes:
        print(f"  {c}")
    if not a.dry_run:
        print("Next: review `git diff`, then commit.")


if __name__ == "__main__":
    main()
