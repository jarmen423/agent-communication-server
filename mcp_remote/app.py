"""Starlette app of the remote MCP endpoint (resource server).

- ``POST /mcp`` (or the path of ``--resource-url``): MCP over streamable
  HTTP, **stateless** (no ``Mcp-Session-Id``; each request stands alone), so
  any replica behind a load balancer can serve any request.
- Unauthenticated → ``401`` with ``WWW-Authenticate: Bearer
  resource_metadata="…/.well-known/oauth-protected-resource/mcp"``.
- ``GET /.well-known/oauth-protected-resource[/mcp]``: RFC 9728 metadata
  naming the control plane as the authorization server.
- ``GET /healthz``.

Every tool call runs inside ``hub_connection.bound(identity, …)`` with the
identity from the verified access token and that identity's own NATS
connection from the pool.
"""

from __future__ import annotations

import contextlib
from typing import Any, AsyncIterator
from urllib.parse import urlparse

from mcp.server import Server
from mcp.server.auth.middleware.auth_context import AuthContextMiddleware
from mcp.server.auth.middleware.bearer_auth import (
    AuthenticatedUser,
    BearerAuthBackend,
    RequireAuthMiddleware,
)
from mcp.server.auth.routes import build_resource_metadata_url, create_protected_resource_routes
from mcp.server.streamable_http_manager import StreamableHTTPSessionManager
from mcp.server.transport_security import TransportSecuritySettings
from pydantic import AnyHttpUrl
from starlette.applications import Starlette
from starlette.middleware import Middleware
from starlette.middleware.authentication import AuthenticationMiddleware
from starlette.requests import Request
from starlette.responses import JSONResponse, Response
from starlette.routing import Route

import hub_connection as conn
from hub_app import build_server

from .auth import HubAccessToken, JwksTokenVerifier
from .handlers import HANDLERS, current_token
from .pool import NatsPool
from .tools import TOOLS

SCOPE = "acs:hub"
INSTRUCTIONS = (
    "ACS hub connector: delegate work to your agents and read their results. "
    "delegate returns a task_id at once; call get_task_result(task_id, worker) "
    "to get the result, now or in a later turn. read_inbox shows direct messages "
    "sent to you. Results are kept for 24 h and inbox messages for 7 days.")


def bind_from(pool: NatsPool):
    @contextlib.asynccontextmanager
    async def bind(server: Server) -> AsyncIterator[HubAccessToken]:
        request = server.request_context.request
        user = request.scope.get("user") if request is not None else None
        if not isinstance(user, AuthenticatedUser) or not isinstance(
                user.access_token, HubAccessToken):
            raise PermissionError("unauthenticated tool call")
        tok = user.access_token

        async def get_nc() -> Any:
            return await pool.get(tok.tenant, tok.identity, tok.token)

        with conn.bound(tok.identity, get_nc):
            reset = current_token.set(tok)
            try:
                yield tok
            finally:
                current_token.reset(reset)

    return bind


class _AsgiEndpoint:
    def __init__(self, manager: StreamableHTTPSessionManager) -> None:
        self.manager = manager

    async def __call__(self, scope, receive, send) -> None:
        await self.manager.handle_request(scope, receive, send)


def create_app(*, resource_url: str, issuer: str, nats_url: str, cp_url: str,
               service_key: str, jwks_url: str | None = None) -> Starlette:
    pool = NatsPool(nats_url=nats_url, cp_url=cp_url, service_key=service_key)
    verifier = JwksTokenVerifier(issuer=issuer, resource=resource_url, jwks_url=jwks_url)
    server = build_server("acs-hub", TOOLS, HANDLERS, bind=bind_from(pool), indent=None,
                          ensure_ascii=False, instructions=INSTRUCTIONS)
    manager = StreamableHTTPSessionManager(
        app=server, stateless=True, json_response=False,
        # Bearer tokens are never ambient browser credentials, so DNS
        # rebinding can't borrow them; Host/Origin checks would only break
        # tunnels and load balancers.
        security_settings=TransportSecuritySettings(enable_dns_rebinding_protection=False))

    resource = AnyHttpUrl(resource_url)
    metadata_url = build_resource_metadata_url(resource)
    mcp_path = urlparse(resource_url).path or "/"
    mcp_endpoint = RequireAuthMiddleware(_AsgiEndpoint(manager), [SCOPE], metadata_url)

    async def healthz(request: Request) -> Response:
        return JSONResponse({"ok": True})

    prm = create_protected_resource_routes(
        resource_url=resource, authorization_servers=[AnyHttpUrl(issuer)],
        scopes_supported=[SCOPE], resource_name="ACS hub")
    routes = [Route(mcp_path, endpoint=mcp_endpoint, methods=["GET", "POST", "DELETE"]),
              *prm, Route("/healthz", healthz, methods=["GET"])]
    # Some clients look for the metadata at the root as well.
    root_prm = "/.well-known/oauth-protected-resource"
    if all(getattr(r, "path", None) != root_prm for r in prm):
        routes.append(Route(root_prm, endpoint=prm[0].endpoint, methods=["GET", "OPTIONS"]))

    @contextlib.asynccontextmanager
    async def lifespan(app: Starlette) -> AsyncIterator[None]:
        async with manager.run():
            try:
                yield
            finally:
                await pool.close()

    app = Starlette(
        routes=routes,
        middleware=[
            Middleware(AuthenticationMiddleware,
                       backend=BearerAuthBackend(verifier, resource_server_url=resource)),
            Middleware(AuthContextMiddleware),
        ],
        lifespan=lifespan,
    )
    app.state.pool = pool
    app.state.verifier = verifier
    return app
