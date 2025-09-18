# FreeDiscord

A desktop app built with Tauri 2 (Rust) and React to help you find, test, and use proxy server for Discord.

## Features

- Discover free proxies from multiple sources
- Custom proxy support
- Test proxies for availability and latency
- Optional geolocation lookup for working proxies
- Filter by country, type, and working status
- Start a local forwarder and route Discord through a selected proxy
- Quickly launch Discord via the chosen proxy

## How it works

1. Scrapes/loads public proxy lists from various sources
2. Tests connectivity and measures basic latency
3. Optionally fetches geolocation info for working proxies
4. Lets you select a proxy to use
5. Starts a local proxy forwarder (Hyper) that routes traffic through your selection
6. Provides a one-click way to start Discord through the proxy

## Getting started (Windows)

### Prerequisites

- Rust toolchain (stable) via rustup: https://www.rust-lang.org/tools/install
- Node.js (LTS recommended): https://nodejs.org/
- Yarn (project uses Yarn scripts): https://yarnpkg.com/
- Tauri prerequisites for Windows (WebView2 runtime, MSVC Build Tools): https://tauri.app/v2/guides/getting-started/prerequisites

### Setup

1. Clone the repository
2. Install JavaScript dependencies with Yarn:
   ```powershell
   yarn install
   ```
3. Run the desktop app in development:
   ```powershell
   yarn tauri dev
   ```
   - This runs Vite (frontend) and the Tauri backend together. The dev URL is http://localhost:1420.
4. Build a production bundle (installer/binary):
   ```powershell
   yarn tauri build
   ```

Notes
- You can run the frontend only with `yarn dev`, but most features require the Tauri backend.
- The app and build steps are configured in `src-tauri/tauri.conf.json` (`beforeDevCommand`, `beforeBuildCommand`, `devUrl`).

## Tech stack

- Frontend: React 19 + Vite 7
- Desktop shell: Tauri 2
- Backend (Rust): Tokio async runtime, Reqwest (HTTP), Hyper (local forwarder), Scraper, Regex
- Extras: WebSocket (tokio-tungstenite), ping, logging

Logs
- Runtime logs are written under `src-tauri/logs/` (e.g., dated log files).

## Project structure

- `index.html`, `src/` – React UI and Vite entry
- `src-tauri/` – Tauri app (Rust): config, icons, Rust sources
  - `src-tauri/src/main.rs`, `logger.rs` – backend entry and logging
  - `src-tauri/tauri.conf.json` – Tauri app configuration

## Troubleshooting

- Windows build errors about MSVC or linker: install "Desktop development with C++" (MSVC) via Visual Studio Build Tools.
- WebView2 not found: install the Evergreen WebView2 runtime (link in Tauri prerequisites).
- Yarn not found: install Yarn, or switch the project to npm and update `beforeDevCommand`/`beforeBuildCommand` in `tauri.conf.json` accordingly.

## Known issues

### Discord CDN assets are not loading

- Avatars, images, icons, or attachments fail to load; parts of the app look blank or keep spinning.
- Logs show messages like "CDN probe failed" or "HTTP proxy could not verify CDN connectivity".

### QR login WebSocket connection issues

- QR code generation fails or the QR code does not load.
- Logs show messages like:
   - `Setting up WebSocket tunnel to: wss://remote-auth-gateway.discord.gg/?v=2`
   - `Attempting WebSocket handshake ...`
   - `Timeout connecting to target WebSocket: wss://remote-auth-gateway.discord.gg/?v=2`

## Use your own proxy (ProxyBox)

If you prefer a proxy you control, you can run ProxyBox locally. It provides two containerized proxies via Docker Compose:

- Squid: HTTP/HTTPS proxy on port 3128
- Dante: SOCKS5 proxy on port 1080

Then point FreeDiscord at either localhost:3128 (HTTP) or localhost:1080 (SOCKS5).

### Prerequisites

- Docker Desktop for Windows (with WSL 2 backend)
- Git

### Start ProxyBox

1. Clone and start the stack:
   ```powershell
   git clone https://github.com/mq1n/ProxyBox
   cd ProxyBox
   docker compose up --build -d
   ```
2. Default exposed ports:
   - Squid (HTTP/HTTPS): 3128
   - Dante (SOCKS5): 1080
3. Optional: bind to localhost only for safety (edit `docker-compose.yml` before starting):
   ```yaml
   ports:
     - "127.0.0.1:3128:3128" # Squid
     - "127.0.0.1:1080:1080" # Dante
   ```
4. Verify the ports are up (PowerShell):
   ```powershell
   Test-NetConnection -ComputerName 127.0.0.1 -Port 3128
   Test-NetConnection -ComputerName 127.0.0.1 -Port 1080
   ```

### Point FreeDiscord to ProxyBox

- Using HTTP via Squid: host 127.0.0.1, port 3128, type HTTP
- Using SOCKS5 via Dante: host 127.0.0.1, port 1080, type SOCKS5

If you enable authentication in ProxyBox, provide username/password in the app (or use the URL form `http://user:pass@127.0.0.1:3128` if supported). By default, the example config may allow local access without auth; to enable auth/rules:

- Squid: edit `squid/squid.conf` and `squid/passwd`
- Dante: edit `dante/danted.conf`

Restart to apply changes:
```powershell
docker compose down
docker compose up -d
```

To stop and remove the containers entirely:
```powershell
docker compose down
```

## Disclaimer

- This application is for educational purposes only.
- Use proxies responsibly and be aware of the legal and ethical implications.
- The author is not responsible for any misuse of this application.

## License

MIT License. See `LICENSE` for details.
