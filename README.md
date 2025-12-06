<div align="center">
<img src="./assets/social/avatar-rounded.png" style="width: 100px; height: 100px;">
<h1>Resonix Node</h1>
<p>Low-latency relay-based audio node written in Rust. It exposes a HTTP API to create/manage audio players and a WebSocket that streams raw PCM for clients (e.g., Discord bots).</p>
</div>

Features
- HTTP API for creating/controlling players
- WebSocket PCM stream (48 kHz, stereo, 16-bit, 20 ms frames)
- Optional resolver that turns YouTube/SoundCloud/Spotify links into direct stream URLs via the [Riva](https://github.com/resonix-dev/riva) crate
- Allow/block URL patterns via regex
- Lightweight EQ and volume filters
- Minimal authentication via static password header
- Robust decoding via the system `ffmpeg` binary (piped PCM, no intermediate files)
- Automatic cleanup of downloaded/transcoded temp audio files when not looping; best‑effort cleanup on shutdown

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
	- Behavior: Validates against allow/block patterns. If resolver is enabled, attempts to resolve page URLs (YouTube/Spotify/SoundCloud) to a direct audio file before playback.
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

- `[resolver]`
	- `enabled` (bool) → default `false`
	- `ffmpeg_path` (string, optional) → default `"ffmpeg"`. If the binary is missing Resonix downloads the newest BtbN build into `~/.resonix/bin` (or `%USERPROFILE%\.resonix\bin`) and rewrites this path automatically. You can still override it via `FFMPEG_PATH`.
	- `timeout_ms` (u64) → default `20000`
	- `allow_spotify_title_search` (bool) → default `true` (permits YouTube search fallback for Spotify URLs)

- `[spotify]`
	- `client_id` (string, optional) → Either the literal client id OR the NAME of an env var containing it.
	- `client_secret` (string, optional) → Either the literal client secret OR the NAME of an env var containing it.
	- Behavior: If set to a string that matches an existing environment variable, that env var’s value is used. Otherwise the string is treated as the literal credential. If not provided, defaults to reading `SPOTIFY_CLIENT_ID` and `SPOTIFY_CLIENT_SECRET` from the environment.

- `[sources]`
	- `allowed` (array of regex strings) → if empty, all allowed unless blocked
	- `blocked` (array of regex strings) → takes priority over allowed

Environment overrides
- `RESONIX_RESOLVE=1|true` → enable resolver
- `FFMPEG_PATH=...` → explicit path or command name for `ffmpeg` (overrides the bundled auto-downloaded binary)
- `RESOLVE_TIMEOUT_MS=...` → override timeout
- `SPOTIFY_CLIENT_ID` / `SPOTIFY_CLIENT_SECRET` → fallback env vars if `[spotify]` section is omitted.
	- You can also set custom env var names and reference them from the config, e.g.: `client_id = "MY_APP_SPOTIFY_ID"` and then set `MY_APP_SPOTIFY_ID` in your `.env` or environment.
Notes
- The resolver obtains direct stream URLs via Riva. Ensure you comply with the target platform's terms of service.
- Cleanup: If loop mode is not enabled (neither `track` nor `queue`), Resonix deletes any temporary audio file it created for the finished track. On process shutdown, Resonix also best‑effort removes leftover temp files with the `resonix_` prefix from the OS temp directory.

### ffmpeg requirement

The node shells out to `ffmpeg` for every track and streams raw PCM from its stdout. Provide a path via `resolver.ffmpeg_path` in the config or the `FFMPEG_PATH` env variable. On startup Resonix runs `ffmpeg -version`; if the command is missing it automatically downloads the latest build from <https://github.com/BtbN/FFmpeg-Builds/releases> into `~/.resonix/bin/ffmpeg` (or `%USERPROFILE%\.resonix\bin\ffmpeg.exe`) and switches to it. If the download fails or your platform is unsupported, Resonix exits with an error so you can install `ffmpeg` manually.

---

## Development

Prereqs
- Rust toolchain, `cargo`
- `ffmpeg` accessible on the PATH (Resonix auto-downloads a Linux/Windows build if none is found)

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

### CLI

Basic flags:
- `--version`, `-V`, `-version` → print version and exit
- `--init-config` → create `Resonix.toml` in the current directory (fails if it already exists) and exit

If no flag is passed, the server starts normally.

---

## Security

Report vulnerabilities via GitHub Security Advisories (see [SECURITY.md](./SECURITY.md)). Do not open public issues for sensitive reports.

---

## License

BSD 3-Clause. See [LICENSE](./LICENSE).
