<div align="center">
<img src="./assets/avatar.png" style="border-radius: 10%; width: 100px; height: 100px;">
<h1>Resonix Node</h1>
<p>Low-latency audio node written in Rust. It exposes a simple HTTP API to create/manage audio players and a WebSocket that streams raw PCM for clients (e.g., Discord bots). An example Discord.js client is included in `examples/discord-js-bot`.</p>
</div>

Features
- HTTP API for creating/controlling players
- WebSocket PCM stream (48 kHz, stereo, 16-bit, 20 ms frames)
- Optional resolving/downloading for YouTube/Spotify links via yt-dlp
- Allow/block URL patterns via regex
- Lightweight EQ and volume filters
- Minimal authentication via static password header

### Status
Early preview. APIs may evolve. See License (BSD-3-Clause).

---

## Quick start

1) Build
- Install Rust (stable). Then build:
	- Windows PowerShell
		- `cargo build --release`

2) Configure (optional)
- Copy `Resonix.toml` to your working directory and tweak values (lowercase `resonix.toml` is also supported). See Configuration below.

3) Run
- Start the server binary from the repository root. Default bind: `0.0.0.0:2333`.

4) Try it
- Create a player for a URL, then connect a WS client to stream PCM.

---

## API

Base URL: `http://<host>:<port>` (default `http://127.0.0.1:2333`). If a password is set in config, include header: `Authorization: <password>`.

Endpoints
- POST `/players` → Create player
	- Request JSON: `{ "id": string, "uri": string }`
	- Behavior: Validates against allow/block patterns. If resolver is enabled, attempts to resolve page URLs (YouTube/Spotify) to a direct audio file before playback.
	- Responses: `201 { "id": string }`, `403` (blocked), `409` (exists), `400` (bad input)

- POST `/players/{id}/play` → Resume playback
	- Response: `204` or `404`

- POST `/players/{id}/pause` → Pause playback
	- Response: `204` or `404`

- PATCH `/players/{id}/filters` → Update filters
	- Request JSON: `{ "volume"?: number(0.0..5.0), "eq"?: [{ "band": 0..4, "gain_db": number }] }`
	- Response: `204` or `404`

- DELETE `/players/{id}` → Stop and remove player
	- Response: `204` or `404`

- GET `/resolve?url=<encoded>` → Resolve to a direct audio file (if resolver enabled)
	- Responses: `200 <path-or-url>`, `400` on errors, `400` if resolver disabled

WebSocket stream
- URL: `ws://<host>:<port>/players/{id}/ws`
- Frames: binary, interleaved little-endian i16 PCM
	- Sample rate: 48,000 Hz
	- Channels: 2 (stereo)
	- Frame size: 960 samples/channel (20 ms), 3,840 bytes per packet
	- A single silent priming frame is sent first

---

## Configuration

The server loads configuration from `resonix.toml` (lowercase) or `Resonix.toml` in the current working directory. Environment variables can override some resolver options.

TOML sections and keys
- `[server]`
	- `host` (string) → default `"0.0.0.0"`
	- `port` (u16) → default `2333`
	- `password` (string, optional) → if set, all requests must include header `Authorization: <password>`

- `[logging]`
	- `clean_log_on_start` (bool) → truncate `.logs/latest.log` on startup; default `true`

- `[resolver]` (requires external `yt-dlp` in PATH or configured)
	- `enabled` (bool) → default `false`
	- `ytdlp_path` (string) → default `"yt-dlp"`
	- `timeout_ms` (u64) → default `20000`
	- `preferred_format` (string) → default `"140"` (m4a)
	- `allow_spotify_title_search` (bool) → default `true` (resolves Spotify URLs via title search)

- `[sources]`
	- `allowed` (array of regex strings) → if empty, all allowed unless blocked
	- `blocked` (array of regex strings) → takes priority over allowed

Environment overrides
- `RESONIX_RESOLVE=1|true` → enable resolver
- `YTDLP_PATH=...` → path to `yt-dlp`
- `RESOLVE_TIMEOUT_MS=...` → override timeout

Notes
- The resolver downloads temporary audio files using `yt-dlp`. Ensure sufficient disk space and legal use in your jurisdiction.

---

## Example client

See `examples/discord-js-bot` for a minimal Discord.js bot that connects to the node over WebSocket and plays a URL.

---

## Development

Prereqs
- Rust toolchain, `cargo`
- Optional: `yt-dlp` in PATH for resolver

Common tasks
- Format: `cargo fmt` (configured via `rustfmt.toml`)
- Build debug: `cargo build`
- Build release: `cargo build --release`

Project layout
- `src/` → application code
- `src/api/handlers.rs` → HTTP/WS handlers
- `src/middleware/auth.rs` → simple auth middleware (Authorization header equals password)
- `src/config/` → config loading and effective config
- `src/audio/` → decoding, DSP, player
- `Resonix.toml` → example configuration

---

## Security

Report vulnerabilities via GitHub Security Advisories (see SECURITY.md). Do not open public issues for sensitive reports.

---

## License

BSD 3-Clause. See `LICENSE`.
