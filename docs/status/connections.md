# Connections: status

Newest first. The person's tool servers (`crates/connections`): kept, signed in to and called by the engine.

## 2026-11-27 · Connected apps in chat sessions

- **Chat sessions** get `connections`, `connection_read` and `connection_act` (`harness_toolkit::connected`) over `connections::service::Service` when the engine has its connections. A read-only tool goes through `connection_read`; any other through `connection_act`, which waits for the person's approval.
- **`operations`** gives each tool's name, description, input schema and `reads_only` (the server's `readOnlyHint`, false by default).
- **Test:** `agents/tests/connected_apps.rs`. A scripted model reads through the stand-in local server and the result reaches its next request; an act waits for approval and nothing is sent before it.

## 2026-11-27 · Connections: local and remote tool servers, browser sign-in

- **What is kept.** The list is `connections.json` in the Capsule directory: each connection's id, name, catalogue key and server, and no secrets. It is written whole to a temporary file and renamed into place before any event goes out (persist, then publish). Secrets (a pasted token, a sign-in's tokens) are kept in the login keychain under `com.isonomic.connections`, the way the identity crate keeps its seed. They are kept in memory on other systems and for a scratch Capsule, so they last one run there. A removed connection's secrets are deleted, and its server is stopped.
- **The protocol.** JSON-RPC 2.0 (`connections::rpc`): `initialize` with protocol revision 2025-06-18 (an older one the server answers with is accepted), then `notifications/initialized`, `tools/list` following `nextCursor` (at most 20 pages), and `tools/call`. A server's own `ping` is answered and its other requests get "method not found". A tool is read-only only when its server says so (`annotations.readOnlyHint`); otherwise it acts.
- **Local servers** (`connections::stdio`): a command and its arguments, one message a line on its standard input and output. The child's environment is cleared and given only what the engine allows: the minimal set (`ChildEnv::minimal`), a search path with the usual command-line tool folders, and the home folder. Its standard error is dropped, since a server may print secrets there. A request waits at most 60 s.
- **Remote servers** (`connections::http`): `https://` only (plain HTTP to 127.0.0.1 or localhost only). Each message is a POST that accepts JSON or an event stream. The session id the server gives on `initialize` and the agreed protocol revision go on every later request. No redirect is followed, so a token never goes to another host. Answers are capped at 8 MB.
- **Sign-in** (`connections::oauth`, `connections::loopback`): the authorization code flow with PKCE (S256).
  - **Discovery:** the protected-resource metadata comes from the address in the server's `WWW-Authenticate` refusal, or from `/.well-known/oauth-protected-resource` beside the server's path, then at its root. It names the authorization server, whose metadata comes from `oauth-authorization-server` or `openid-configuration`. A server with no resource metadata is its own authorization server.
  - **Registration:** the engine registers itself each time (dynamic client registration, `token_endpoint_auth_method: none`). It holds no client id made ahead of time. A server that takes no registrations is refused in plain words, which suggest a pasted token.
  - **The return:** a listener on 127.0.0.1, at a port the system picks, takes one return to `/callback` with the right `state`, for up to ten minutes. A return with another state is ignored.
  - **Tokens:** the code is traded with the verifier and the `resource`. A refused token is refreshed once with the refresh token and kept again. Tokens never reach a log, an error or a `Debug` print.
- **A pasted secret** is sent as a bearer token and never refreshed.
- **States:** `connecting` while it starts, `ready` with its tools, `needs_sign_in` when the server refuses without a token, and `failed` with plain words. Every change is published. On engine start each kept connection reconnects.
- **For the harness** (`connections::service::Service`): `harness_toolkit::services::Connections`. A handle is a connection's id and its operations are its tools. Every app sees every ready connection until app manifests name the connections they need. Results are cut to 16 KB, saying so.
- **Dependencies:** none new. HTTP and TLS come from the workspace's `reqwest` 0.12 with rustls; randomness from `ring`; hashing from `sha2`.
- **Tests:** a stand-in local server script (`tests/stdio.rs`: the environment allow-list, tools with their hints, a call, reconnect after a restart, removal, a command that does not start). A fake remote server and sign-in server on loopback (`tests/http.rs`: a pasted token, the session id, the whole sign-in with registration and PKCE checked, a stray return ignored, a refresh, reconnect with a kept sign-in, a server with no registration). And the SDK's conformance checks.
