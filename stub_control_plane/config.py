"""Configuration and the redirect-URI allowlist (DEV ONLY stub)."""

from __future__ import annotations

import ipaddress
import re
from dataclasses import dataclass, field
from urllib.parse import urlparse

from .nats_issuer import TenantKeys

SCOPE = "acs:hub"

# Web chat apps' OAuth callbacks (.planning/hosted/H-0b-web-mcp.md).
CLAUDE_CALLBACKS = (
    "https://claude.ai/api/mcp/auth_callback",
    "https://claude.com/api/mcp/auth_callback",
)
CHATGPT_STABLE_CALLBACK = "https://chatgpt.com/connector_platform_oauth_redirect"
# chatgpt.com/connector/oauth/{callback_id}: one opaque path segment.
_CHATGPT_CALLBACK = re.compile(r"^https://chatgpt\.com/connector/oauth/[A-Za-z0-9_-]{1,128}$")
# RFC 8252 §7.3 loopback redirects (native apps, MCP Inspector, tests).
_LOOPBACK = re.compile(r"^http://(127\.0\.0\.1|localhost|\[::1\])(:\d{1,5})?(/[^?#\s]*)?$")

# Connector identity name prefix by redirect host.
_PREFIXES = {"claude.ai": "claude-web", "claude.com": "claude-web",
             "chatgpt.com": "chatgpt-web", "grok.com": "grok-web"}


@dataclass
class StubConfig:
    issuer: str  # public base URL of this control plane, no trailing slash
    resources: list[str]  # allowed RFC 8707 resources (MCP endpoint URLs)
    tenant: str  # the fixed dev user's tenant
    tenants: dict[str, TenantKeys]
    dev_user: str = "dev-user"
    role: str = "orchestrator"  # NATS role of connector identities
    extra_redirect_uris: list[str] = field(default_factory=list)
    allow_loopback_redirects: bool = False
    allow_http_cimd: bool = False  # tests only: http://127.0.0.1 CIMD URLs
    access_ttl: int = 900
    refresh_ttl: int = 30 * 86400
    code_ttl: int = 300
    nats_ttl: int = 3600

    def __post_init__(self) -> None:
        self.issuer = self.issuer.rstrip("/")
        if self.tenant not in self.tenants:
            raise ValueError(f"no key file for the dev tenant {self.tenant!r}")
        if not self.resources:
            raise ValueError("at least one --resource (MCP endpoint URL) is required")

    def redirect_allowed(self, uri: str) -> bool:
        uri = str(uri)
        if uri in CLAUDE_CALLBACKS or uri == CHATGPT_STABLE_CALLBACK:
            return True
        if _CHATGPT_CALLBACK.match(uri) or uri in self.extra_redirect_uris:
            return True
        return self.allow_loopback_redirects and bool(_LOOPBACK.match(uri))

    def resource_allowed(self, resource: str) -> str | None:
        """The configured resource equal to ``resource`` (trailing slash
        aside), or None."""
        want = resource.rstrip("/")
        for r in self.resources:
            if r.rstrip("/") == want:
                return r
        return None

    def cimd_url_allowed(self, url: str) -> bool:
        p = urlparse(url)
        if p.scheme == "https" and p.hostname and p.path not in ("", "/"):
            return True
        return (self.allow_http_cimd and p.scheme == "http"
                and p.hostname in ("127.0.0.1", "localhost"))


def identity_prefix(redirect_uri: str) -> str:
    host = (urlparse(str(redirect_uri)).hostname or "").lower()
    return _PREFIXES.get(host, "mcp-web")


def is_loopback_host(host: str) -> bool:
    if host in ("localhost",):
        return True
    try:
        return ipaddress.ip_address(host.strip("[]")).is_loopback
    except ValueError:
        return False
