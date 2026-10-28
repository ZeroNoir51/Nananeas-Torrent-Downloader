# Nananeas Torrent Downloader 0.1

[Español](README.es.md)

A Rust desktop application for keeping a local catalog of magnet links and torrent files, searching their indexed file names and downloading selected files individually.

## Features

SQLite catalog and settings; JSON source import/export; individual source deletion, metadata retry and full magnet copying; persistent file indexes; order-independent literal word search (not regular expressions); selective downloads with confirmation; individual transfer progress and aggregate speed graph; mutually exclusive seeding and move-to-root options; editable English/Spanish XML translations; confirmation before controlled shutdown.

## Repository layout

```text
Cargo.toml
Cargo.lock
build.rs
nananeas.sh
src/
assets/
translations/
vendor/librqbit-9.0.1/
docs/ENGINE_PATCHES.md
README.md
README.es.md
.gitignore
.gitattributes
```

Commit Cargo.lock, assets, translations and the complete modified vendor engine, including its upstream license files. Do not commit target directories, runtime catalogs, diagnostic logs or source archives containing personal links. The application crate/executable name remains torrent-finder for compatibility.

## Linux: build and run

Install a current stable Rust toolchain, a C/C++ compiler, pkg-config, OpenSSL development files, X11/XKB development libraries and graphics drivers. A desktop portal backend is needed for native file dialogs. Reuse an existing working Rust installation.

Arch system dependencies:

```bash
sudo pacman -S --needed base-devel git pkgconf openssl libxkbcommon libxcb mesa xdg-desktop-portal
```

Install the portal backend appropriate for your desktop (for example xdg-desktop-portal-gtk on GTK desktops or xdg-desktop-portal-kde on KDE).

From the repository root:

```bash
rustc --version
cargo --version
cargo test --manifest-path vendor/librqbit-9.0.1/Cargo.toml --lib
cargo test --locked && cargo build --release --locked
chmod +x nananeas.sh
./nananeas.sh
```

If Cargo.lock is absent when preparing the first commit, create it with cargo generate-lockfile first and commit it. A dependency lockfile is not a pinned compiler version. No minimum Rust version has been established for this project; record rustc -Vv with build results.

The launcher works from another directory and changes only the soft open-file limit inherited by the app. Its default target is 32768, chosen after a 1024-descriptor session failed with large multi-file torrents. It is not a measured universal requirement, and concurrent downloads may need more. It never lowers an existing higher limit. Override it with:

```bash
NANANEAS_NOFILE=65536 ./nananeas.sh
```

If the hard limit is lower than the requested target, it reports an error. It does not change permanent system limits.

Optional diagnostics (can produce large logs):

```bash
NANANEAS_DIAGNOSTIC=1 ./nananeas.sh 2>&1 | tee diagnostic.log
```

## Windows: native MSVC build

Windows builds have not yet been verified. Install Rust for x86_64-pc-windows-msvc and Visual Studio Build Tools with Desktop development with C++ and a Windows SDK. Use Developer PowerShell for Visual Studio. The Bash launcher is for Linux only.

```powershell
rustup target add x86_64-pc-windows-msvc
cargo test --manifest-path vendor/librqbit-9.0.1/Cargo.toml --lib
cargo test --locked
if ($LASTEXITCODE -ne 0) { throw "Tests failed" }
cargo build --release --locked
if ($LASTEXITCODE -ne 0) { throw "Build failed" }
.\target\release\torrent-finder.exe
```

build.rs embeds assets/cat-icon.ico in the Windows executable. This guide describes native compilation on Windows; cross-compiling from Arch is a separate workflow and is not validated here. A successful build still needs UI, file-dialog, indexing, download, seeding, move and shutdown tests on Windows.

## Data and behavior

On first use, output defaults to the user's Downloads/Torrents folder; an existing saved output path is retained. Data remains under the legacy torrent-finder application name: SQLite in the OS local application data directory, temporary downloads in its cache directory, editable translations in the user configuration directory. On Linux these default to ~/.local/share/torrent-finder, ~/.cache/torrent-finder and ~/.config/torrent-finder/translations. Windows translations use %APPDATA%\torrent-finder\userdata\translations; the SQLite catalog uses the local application-data directory.

Closing Settings processes pending sources; sources with a stored index are skipped. Magnet resolution has a 120-second timeout. Metadata transfer has no reliable GUI percentage; saving the file index reports progress. Failed-source messages currently last for the application session. The modified engine is documented in [ENGINE_PATCHES](docs/ENGINE_PATCHES.md).

## Validation and limitations

User-verified on Arch: indexing formerly failing large magnets and downloading selected files. Engine buffer tests: seven passed, one Miri test ignored by design; application search test passed. Further protocol changes were tested by the user through real magnet indexing. Windows remains unverified. These observations do not validate every swarm, platform or edge case.

Interrupted-download resume and re-enabling seeding after stopping are not implemented in this version. Seeder/leecher counts are not available separately. Keep the app open to seed. Diagnostic log capture is opt-in. Review what is staged before publishing.

## Sources and licensing

- https://github.com/ikatson/rqbit
- https://github.com/emilk/egui
- https://www.rust-lang.org/tools/install
- https://doc.rust-lang.org/rustc/platform-support/windows-msvc.html

The local engine is Apache-2.0; preserve its LICENSE/NOTICE and identify modifications in docs/ENGINE_PATCHES.md. No license has yet been selected for the original Nananeas application code; this repository setup does not assign one.
