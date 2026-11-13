# Live sessions (real-time collaboration)

Status: new in this branch. Revision history at the end.

Several people edit one presentation at the same time: in the DeckCraft desktop app (macOS,
Windows, Linux) and in the browser editor (desktop browsers, iPad, Android tablets and phones).
Changes appear on everyone's screen within a fraction of a second, and the server keeps the
shared presentation, so a reload, a dropped connection or a server restart loses nothing.

## Quick start (host on a Mac, others in the browser)

1. On the Mac, double-click `server/start.command` (first start builds the server and the
   browser editor; it needs [Rust](https://rustup.rs) and `cargo install trunk --locked`).
   It prints the address others use, e.g. `http://192.168.1.20:8080/`, and opens the host
   dashboard `http://localhost:8080/host`.
2. Open your presentation in the DeckCraft app, then **Review › Live Session › Start live
   session** (server `http://localhost:8080`). The dialog shows the **access code** and the
   invitation link, with Copy buttons. The status bar shows `● Live · N people`.
   (Alternatively start the session on the dashboard, with an uploaded .pptx/.deckcraft file.)
3. Colleagues open `http://<your Mac's address>:8080/` on their iPad, Android device or
   computer, type their name and the code, and land in the editor with the shared presentation.
   Colleagues with the DeckCraft app use **Review › Live Session › Join** instead.
4. Everyone edits. The host's dashboard lists people, lets the host change roles (editor/viewer),
   remove someone, issue a new access code, end the session and download the presentation as
   .pptx or .deckcraft.

Stop the server with `server/stop.command` (Windows: `stop.bat`): it saves every session first.
Your copy in the app stays an ordinary presentation you can save locally at any time.

## What is synchronised

Everything in the presentation: slides (add, delete, duplicate, move, layout, background,
transitions, animations, notes, comments), shapes of every kind (pictures, tables, charts,
groups, text boxes, connectors, media), all their properties, text and its formatting, masters,
layouts, themes, slide size and sections. Embedded pictures, audio and video travel with the
shape that uses them. Not synchronised, on purpose: each person's selection, view, zoom and
current slide (the dashboard and the Live Session dialog show who is on which slide).

## How it works

- The engine edits each person's copy as in single-user mode. After every frame the app
  compares the presentation with the last synchronised state, by `Arc` identity: one moved
  shape costs one slide's comparison, not the deck's.
- A change is a set of keyed values (`meta`, `order`, `slide/<id>`, `shape/<slide>/<shape>`,
  `media/<id>`). Only changed keys are sent (`crates/collab/src/patch.rs`).
- The server orders all operations (sequence numbers) and applies each key as last-writer-wins.
  A client applies other people's changes at once, except keys it changed itself and the
  server hasn't confirmed: the server orders its own write later, so its value is already the
  final one. Different shapes, even on the same slide, never overwrite each other; two people
  changing the same shape at the same moment end with the later change. A slide or shape whose
  position in a list lost a race is appended, never lost.
- New ids come from a block reserved for each connection, so two people inserting at the same
  moment never create the same id.
- Undo and Redo take back only your own changes: other people's changes are applied to your
  undo history too (`Session::apply_remote`).
- After a dropped connection the app reconnects (1 s, then up to 10 s between tries), receives
  the current presentation and re-sends what the server hadn't confirmed plus what was edited
  offline. The server ignores an operation it already applied.

Protocol details: `crates/collab/src/proto.rs`. Tests: `crates/collab/src/tests.rs`
(four clients editing concurrently, reconnect with lost and offline changes),
`crates/engine/src/remote_tests.rs` (undo isolation, selection and caret),
`apps/deckcraft-server/src/tests.rs` (the real server over HTTP and WebSocket).

## Security

- **Access codes** are 10 characters from a 30-character alphabet (≈49 bits), generated on the
  server and shown only to the host (dashboard, the host's app). There is no endpoint that
  reveals a code. Wrong codes are rate limited: 10 failures per address per 10 minutes.
- **Tokens** are HS256 JWTs signed with a secret that never leaves the server
  (`DECKCRAFT_JWT_SECRET`, or a random key created in the data folder with mode 0600). A token
  names a session and a participant; the role is looked up on the server for every operation,
  so changing someone to viewer or removing them takes effect at once. Tokens expire after 14
  days. The join page hands the token to the editor in the URL fragment, which browsers never
  send to a server; the editor removes it from the address bar.
- **The host dashboard** opens without a password only on the server's own computer, reached as
  `localhost` (a request through any other host name is refused, which defeats DNS
  rebinding). From elsewhere it needs `DECKCRAFT_ADMIN_PASSWORD`. Every dashboard form carries
  a CSRF value; requests from other web origins are refused (`DECKCRAFT_ALLOWED_ORIGINS` allows
  more).
- **Input**: every operation is decoded against the document model before it's applied; a
  malformed one is refused whole. Messages are limited to 96 MiB, embedded files to 64 MiB,
  uploads to 512 MiB, operations to 250 000 keys, sessions to 500 participants.
- **Files**: every save writes a temporary file, flushes it and renames it over the old one;
  the last 20 versions are kept as backups (every 10 minutes while the presentation changes)
  in `<data>/sessions/<id>/backups/`. A deleted session is moved to `<data>/deleted/`.

### Development and production

`DECKCRAFT_MODE=development` (the default) is for a trusted local network: plain HTTP, the
dashboard open on the server's own computer, the start banner says so.

`DECKCRAFT_MODE=production` refuses to start unless `DECKCRAFT_ADMIN_PASSWORD` (12+ characters)
and an `https://` `DECKCRAFT_PUBLIC_URL` are set; it doesn't trust localhost and marks the admin
cookie `Secure`. Run it behind a TLS proxy (Caddy, nginx, Traefik) that forwards WebSocket
upgrades; the apps then connect with `wss://`.

| Variable | Default | Meaning |
|---|---|---|
| `DECKCRAFT_MODE` | `development` | `development` or `production` |
| `DECKCRAFT_BIND` | `0.0.0.0:8080` | Listening address |
| `DECKCRAFT_DATA` | `./deckcraft-data` (scripts: the user's app data) | Sessions, backups, secret key |
| `DECKCRAFT_WEB_DIR` | `./dist/web` | The browser editor build |
| `DECKCRAFT_ADMIN_PASSWORD` | – | Dashboard password from other computers |
| `DECKCRAFT_PUBLIC_URL` | – | Address in invitation links (required in production) |
| `DECKCRAFT_JWT_SECRET` | random key in the data folder | Token signing secret (32+ characters) |
| `DECKCRAFT_ALLOWED_ORIGINS` | – | Other web origins allowed to call the API |
| `DECKCRAFT_TRUST_LOOPBACK` | `1` in development | Open dashboard on `localhost` |

The scripts read these from `server/deckcraft-server.env` (copy the `.example`; never commit it).

## Running the server

- **macOS / Linux:** `server/start.command`, `server/stop.command`.
- **Windows:** `server\start.bat`, `server\stop.bat` (PowerShell). Windows has no Ctrl+C for a
  hidden process, so `stop.bat` waits for the two-second save cycle before ending the server.
- **By hand:** `cargo run --release -p deckcraft-server` (after `trunk build --release` in
  `apps/deckcraft-web`). `deckcraft-server --help` lists the settings.
- **Container:** `docker compose -f server/docker-compose.yml up -d --build` (set
  `DECKCRAFT_ADMIN_PASSWORD` in `server/deckcraft-server.env`: in a container `localhost` is not
  the host's browser, so the dashboard always asks for it).

One port serves everything: the join page `/`, the dashboard `/host`, the editor `/app/`, the
API `/api/…` and the socket `/ws`. The earlier experimental setup with a separate web host on
port 8081 and a Python helper is not needed.

## HTTP API

| Request | Who | Answer |
|---|---|---|
| `POST /api/sessions?name=&host=` body: .pptx/.deckcraft or empty | host (localhost, or `Authorization: Bearer <admin password>`) | `{sessionId, code, token, inviteUrl, dashboardUrl, wsPath}` |
| `POST /api/join` `{name, code}` | anyone (rate limited) | `{token, sessionId, title, role, wsPath}`; 403 wrong code, 429 too many tries |
| `GET /ws` then `{"t":"hello","clientId","token"}` | token holder | see `proto.rs` |

The app commands `live.start`, `live.join`, `live.leave`, `live.status` and `live.dialog` drive
the same from the control channel and MCP.

## Known limits

- Text is merged per shape: two people typing in the *same* text box at the same instant keep
  the later version of that box. Different boxes are independent.
- A viewer's local edits are undone at once rather than prevented: the editing controls stay
  visible.
- Presence shows who is on which slide, not live cursors.
- Very large embedded media are sent in one message (up to 64 MiB each).
- The desktop app's live-session code is the same on every platform, but in this branch it
  was run end to end on Linux only; macOS and Windows builds are checked by CI.

## Revision history

- 2026-11: first version (server, desktop and web clients, dashboard, scripts, container).
