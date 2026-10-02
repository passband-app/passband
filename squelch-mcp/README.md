# squelch-mcp

The **agent door**: an MCP server exposing squelch's read-only intelligence to any MCP client (Claude Code, Claude Desktop, whatever you run). Agents get ranked summaries — they never hold the Gmail credential, never see raw mailbox access, and structurally cannot see auth mail.

## Tools

Seven tools, read-mostly by design:

- `get_inbox_updates` — ranked updates since a timestamp
- `get_thread` — a sanitized thread by thread id or message id
- `get_deadlines` — bills and deadlines due within N days
- `get_shipments` — tracked packages (en-route by default)
- `search_mail` — hybrid keyword + semantic search
- `set_sender_rule` / `list_sender_rules` — the one write: LOCAL sender disposition (surface / squelch / filtered). It touches squelch's own database only, never Gmail, and writes an audit row.

Sealed messages (2FA codes, password resets, login alerts) are absent from every response — not redacted, absent, indistinguishable from not existing.

## Transports

Transport is chosen in `main` and only there; tool logic is transport-agnostic.

```sh
squelch-mcp                    # stdio (default)
squelch-mcp --http [addr]      # Streamable HTTP, mounted at /mcp
```

`SQUELCH_MCP_HTTP` selects HTTP mode via env. Default HTTP bind is loopback `127.0.0.1:8848`; front it with a reverse proxy to expose it. In normal deployments you don't run this binary at all — `squelchd serve` mounts the same service at `/mcp` alongside the human door.

Point an MCP client at it:

```json
{
  "mcpServers": {
    "squelch": { "type": "http", "url": "http://127.0.0.1:8848/mcp" }
  }
}
```

## Env

- `SQUELCH_DB_PATH` — SQLite path (shared XDG default)
- `SQUELCH_ACCOUNT_EMAIL` — account to serve
- `SQUELCH_MCP_ALLOWED_HOSTS` — allowed Host headers behind a proxy

## Auth (OAuth for hosted tenants)

The HTTP door has three modes, picked once at startup from env (blank counts as unset):

- `SQUELCH_MCP_OAUTH_ISSUER`: the authorization server's issuer (e.g. `https://foo.authkit.app`).
- `SQUELCH_MCP_PUBLIC_URL`: this daemon's canonical `/mcp` URL (e.g. `https://alice.passband.email/mcp`). It is the expected token `aud`, the `resource` in the metadata, and the hostname added to the Host allow-list.
- `SQUELCH_MCP_OAUTH_JWKS_URL`: optional; defaults to `<issuer>/oauth2/jwks`.

| Mode | When | `/mcp` |
|---|---|---|
| Open | neither issuer nor public URL set | no auth, Host allow-list only. Self-host, unchanged. |
| OAuth | both set (https, or http on loopback) | needs an RS256 bearer JWT from the issuer, with our `iss` and `aud`, unexpired, and an `email` claim equal to this daemon's account email. Missing token: `401` with `WWW-Authenticate: Bearer resource_metadata="<origin>/.well-known/oauth-protected-resource/mcp"`. Bad token: the same plus `error="invalid_token"`. JWKS never reachable: `503`. Also serves the RFC 9728 metadata at `/.well-known/oauth-protected-resource[/mcp]`. |
| Misconfigured | exactly one set, or a value that is not a valid URL | `503` for everything, error logged at startup. Never falls back to Open. |

The `email` check is the one that matters: one wildcard resource indicator covers every tenant, so `aud` alone does not stop one Passband user's token from opening another user's door. Device tokens (`sqd_`) and the master token are never accepted here. Refusals are logged as a reason category only, never a token, claim, or address. Code: `src/auth.rs`; construction: `squelch_mcp::agent_door_router`.
