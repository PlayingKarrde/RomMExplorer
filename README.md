# RomM Explorer

A fast desktop client for your [RomM](https://github.com/rommapp/romm) server. You can browse your whole collection with cover art, install games into a local folder of your choice, and uninstall them later. Installed games stay organised by platform.

Written in Rust with [egui](https://github.com/emilk/egui). It ships as a single native executable with no runtime to install.

## Features

- **Browse**: a cover-art grid or a compact list, with instant search (Ctrl+F) across titles, file names and platforms.
- **Filter and sort**: by platform, installed or not installed, name, platform, size, release date, or recently installed.
- **Details panel**: large cover, summary, genres, regions and languages, release date, developer, rating and file info.
- **Install / uninstall**: one click, from the hover button on a card, the details panel, the right-click menu, or a double-click.
  - Two downloads run in parallel. The tray shows live progress, speed and ETA, and lets you cancel or retry.
  - Downloads are written to a `.part` file and only moved into place once complete, so the library never holds half-written files.
  - Multi-file games (multi-disc, folders) are extracted into their own folder. You can turn this off to keep the `.zip`.
  - **Install all** on any platform view queues everything that isn't installed yet.
- **An organised library**:
  ```
  <library folder>/
    .romm-explorer.json          ← manifest of what's installed
    snes/Super Mario World (USA).sfc
    gba/Metroid Fusion (USA).gba
    psx/Final Fantasy VII (USA)/  ← multi-disc game, extracted
  ```
  - Platform folders use RomM's folder names (`snes`, `gba`, `psx`…), the platform slug, or the full platform name.
  - **Reorganize folders** moves existing installs when you change the naming scheme.
  - **Detect existing files** finds games already in the folder and marks them installed.
  - **Verify installed** forgets installs whose files were deleted outside the app.
  - The manifest lives inside the library folder, so you can move the library to another drive or machine.
- Cover art is cached on disk, so later launches load quickly.

## Getting started

1. Install Rust: <https://rustup.rs>
2. Build and run:
   ```bash
   cargo run --release
   ```
   The binary ends up at `target/release/romm-explorer(.exe)`.
3. Enter your RomM server address, your username and password (or a client API token `rmm_…`), and pick a library folder.

### Authentication

You can log in two ways:

- **Username and password**: sent as HTTP Basic auth.
- **API token**: a RomM client token (`rmm_…`), sent as a Bearer token. It takes priority over the password when set.

Turn on *Allow self-signed HTTPS certificates* if your server uses a self-signed certificate.

> Settings, including credentials, are stored in plain text in your user config directory:
> `%APPDATA%\RomMExplorer\RomMExplorer\config\config.json` on Windows,
> `~/.config/rommexplorer/config.json` on Linux.
> Prefer an API token over your password if that concerns you.

## Development

```bash
cargo build
cargo test
# Install/uninstall round-trip against a real or mock server:
ROMM_TEST_URL=http://localhost:8080 ROMM_TEST_USER=me ROMM_TEST_PASS=secret cargo test -- --ignored
```

Server details can come from a `.env` file in the project root or next to the executable. They pre-fill the login screen when nothing is configured yet, and the real-server smoke test uses them. `.env` is git-ignored.

```ini
server=https://romm.example.com
user=me
password=secret
# token=rmm_...   (optional, used instead of user/password)
```

```bash
cargo test real_server -- --ignored --nocapture
```

Set `ROMM_EXPLORER_CONFIG=/path/to/config.json` to use a separate config file, for example while testing.

### Layout

| File | Purpose |
|---|---|
| `src/api.rs` | RomM REST client (platforms, paginated roms, covers, streaming downloads) |
| `src/library.rs` | Local library: install, extract, uninstall, reorganize, detect, verify |
| `src/covers.rs` | Background cover loader with disk cache |
| `src/app.rs` | UI and app state |
| `src/widgets.rs`, `src/theme.rs` | Custom widgets and the dark theme |

Tested against RomM's current API (`/api/roms` with `items`/`total` pagination, `/api/roms/{id}/content/{file}`). It also accepts the older list-style responses.
