"""Thin async client for the Resend Inboxes API (only what the plugin uses).

Paced to stay under Resend's per-team request rate, and retries 429/5xx.
The API key never appears in errors or logs.
"""
from __future__ import annotations

import asyncio
import time

import httpx

MIN_INTERVAL = 0.5  # seconds between requests (~2 req/s)
RETRIES = 4


class ResendError(RuntimeError):
    def __init__(self, status: int, message: str):
        super().__init__(f"Resend API {status}: {message}")
        self.status = status


class ResendInboxes:
    def __init__(self, api_key: str, base_url: str = "https://api.resend.com",
                 transport: httpx.AsyncBaseTransport | None = None,
                 min_interval: float = MIN_INTERVAL):
        self._http = httpx.AsyncClient(
            base_url=base_url.rstrip("/"), transport=transport, timeout=30.0,
            headers={"Authorization": f"Bearer {api_key}",
                     "User-Agent": "acs-email-plugin/0.1"})
        self._lock = asyncio.Lock()
        self._last = 0.0
        self._min_interval = min_interval
        self._labels: dict[tuple[str, str], str] = {}

    async def close(self) -> None:
        await self._http.aclose()

    async def _call(self, method: str, path: str, *, json: dict | None = None,
                    params: dict | None = None, idempotency_key: str | None = None) -> dict:
        headers = {"Idempotency-Key": idempotency_key} if idempotency_key else None
        for attempt in range(RETRIES + 1):
            async with self._lock:
                wait = self._min_interval - (time.monotonic() - self._last)
                if wait > 0:
                    await asyncio.sleep(wait)
                self._last = time.monotonic()
                try:
                    resp = await self._http.request(method, path, json=json,
                                                    params=params, headers=headers)
                except httpx.TransportError as e:
                    if attempt == RETRIES:
                        raise ResendError(0, f"{method} {path}: {type(e).__name__}") from None
                    await asyncio.sleep(2 ** attempt)
                    continue
            if resp.status_code == 429 or resp.status_code >= 500:
                if attempt == RETRIES:
                    break
                retry_after = resp.headers.get("retry-after")
                await asyncio.sleep(float(retry_after) if retry_after and retry_after.replace(".", "").isdigit()
                                    else 2 ** attempt)
                continue
            break
        if resp.status_code >= 400:
            try:
                msg = resp.json().get("message") or resp.text
            except ValueError:
                msg = resp.text
            raise ResendError(resp.status_code, f"{method} {path}: {msg[:300]}")
        return resp.json() if resp.content else {}

    # ── inboxes ────────────────────────────────────────────────────────
    async def create_inbox(self, address: str, name: str, from_name: str | None) -> str:
        body = {"email_address": address, "name": name}
        if from_name:
            body["from_name"] = from_name
        return (await self._call("POST", "/inboxes", json=body))["id"]

    async def list_inboxes(self) -> list[dict]:
        return (await self._call("GET", "/inboxes", params={"limit": 100})).get("data", [])

    # ── threads and emails ─────────────────────────────────────────────
    async def list_threads(self, inbox: str, folders: str = "inbox", limit: int = 50) -> list[dict]:
        resp = await self._call("GET", f"/inboxes/{inbox}/threads",
                                params={"folders": folders, "limit": limit})
        return resp.get("data", [])

    async def list_thread_emails(self, inbox: str, thread: str) -> list[dict]:
        out: list[dict] = []
        params: dict = {"limit": 100}
        while True:
            resp = await self._call("GET", f"/inboxes/{inbox}/threads/{thread}/emails",
                                    params=params)
            out += resp.get("data", [])
            if not resp.get("has_more") or not out:
                return out
            params = {"limit": 100, "after": out[-1]["id"]}

    async def received_email(self, email_id: str) -> dict:
        """The received-email record: ``authentication`` and ``headers``."""
        return await self._call("GET", f"/emails/receiving/{email_id}")

    # ── sending ────────────────────────────────────────────────────────
    async def reply(self, inbox: str, thread: str, email_id: str, text: str,
                    headers: dict | None = None) -> str:
        body: dict = {"text": text}
        if headers:
            body["headers"] = headers
        resp = await self._call("POST", f"/inboxes/{inbox}/threads/{thread}/emails/{email_id}/reply",
                                json=body, idempotency_key=f"acs-reply-{email_id}")
        return resp["id"]

    async def draft_reply(self, inbox: str, thread: str, email_id: str, text: str) -> str:
        resp = await self._call("POST", f"/inboxes/{inbox}/drafts", json={
            "thread_id": thread, "reply_to_email_id": email_id, "text": text})
        return resp["id"]

    async def send_new(self, inbox: str, to: list[str], subject: str, text: str,
                       headers: dict | None = None, idempotency_key: str | None = None) -> str:
        draft = await self._call("POST", f"/inboxes/{inbox}/drafts",
                                 json={"to": to, "subject": subject, "text": text})
        resp = await self._call("POST", f"/inboxes/{inbox}/drafts/{draft['id']}/send",
                                json={"headers": headers} if headers else None,
                                idempotency_key=idempotency_key)
        return resp["id"]

    # ── labels ─────────────────────────────────────────────────────────
    async def label_id(self, inbox: str, name: str, color: str) -> str:
        key = (inbox, name.lower())
        if key not in self._labels:
            for lab in (await self._call("GET", f"/inboxes/{inbox}/labels")).get("data", []):
                self._labels[(inbox, lab["name"].lower())] = lab["id"]
        if key not in self._labels:
            created = await self._call("POST", f"/inboxes/{inbox}/labels",
                                       json={"name": name, "color": color})
            self._labels[key] = created["id"]
        return self._labels[key]

    async def add_labels(self, inbox: str, thread: str, label_ids: list[str]) -> None:
        await self._call("POST", f"/inboxes/{inbox}/threads/{thread}/labels",
                         json={"labels": label_ids})
