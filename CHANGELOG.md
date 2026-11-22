[English](CHANGELOG.md) | [Русский](CHANGELOG.ru_RU.md)

# Changelog

## 0.2.0-beta — 2026-11-22

Second prerelease: the features of the Windows client that were missing, and fixes.

### Added

- **Kill-switch** (☰ → *Block untunneled traffic*): nftables rules block traffic that bypasses
  a full tunnel, IPv6 included; after a tunnel crash the window offers to reconnect or to
  remove the block. The deb recommends `nftables`.
- **Restore the connection after reboot** (☰): the tunnel active at shutdown is brought up at
  boot by the `awg-gui-boot` systemd service (`awg-gui --boot`); removed with the package.
- Warning before importing configs with `PreUp`/`PostUp`/`PreDown`/`PostDown` commands (they
  run as root); a *Scripts* row in the details, as well as *Table*.
- Import of `.zip` archives and UTF-16 files; **export of all tunnels to a zip**.
- Ctrl+V imports files copied in the file manager (drag and drop does not work on Wayland in
  winit) or opens the paste dialog with the text; Ctrl+O, Ctrl+N, Ctrl+E, Delete, ↑/↓, Enter;
  context menu on the tunnel list.
- Values are checked while typing in the editor (addresses, AllowedIPs, Endpoint, ports, MTU,
  keepalive); Save is disabled while the config is invalid.
- Tray: status and addresses lines, *Import*, *About*; desktop notifications; About dialog
  with the versions of the embedded components.
- **Update check**, off by default (☰ → *Check for updates*, or once from *About*): asks the
  GitHub API for releases daily via `curl`/`wget` and offers the release page; nothing is
  installed automatically. The README now says the program connects only to your VPN servers
  *without your consent*.
- The log keeps dates, survives restarts (`~/.local/state/awg-gui/awg-gui.log`) and can be
  saved to a file.

### Changed

- *Bypass VPN for local networks* and the kill-switch are on by default, also for settings
  saved by 0.1.0-beta (which stored the bypass as off for everyone). Without `nftables` the
  kill-switch is skipped with a warning in the details instead of failing the activation.
- The embedded `awg`, `awg-quick` and `amneziawg-go` take precedence over system ones;
  `build-backend.sh` builds pinned releases (amneziawg-go v3.1.20261116, amneziawg-tools
  v3.1.20260812) instead of `master`.
- The description of *Bypass VPN for local networks* now matches what it does: the attached
  subnet is reached directly anyway, the option covers all other private networks.
- The MTU of a `vpn://` key is applied when its ready config has none.

### Fixed

- A tunnel could be deleted or toggled again while it was activating.
- LAN bypass was silently not applied when the `[Interface]` line had a comment; `[INTERFACE]`
  in capitals was rejected by import.
- A failing statistics request was logged every second, and a lost helper could stop the
  statistics for good.
- `ip rule`s and awg-quick's nft tables were left behind after a failed activation or a crash.
- The autostart entry kept the old path after the binary was moved.
- Links in the window did not open (egui is built without its browser support); they are
  opened with `xdg-open`.

## 0.1.0-beta — 2026-11-22

First prerelease.

### Added

- Graphical AmneziaWG / WireGuard client for Linux in one executable (Rust, egui), after the
  example of amneziawg-windows-client: tunnel list with status, activate and deactivate,
  details of the interface and peers with live statistics (latest handshake, traffic,
  endpoint, AmneziaWG obfuscation parameters).
- Import of `.conf` files (dialog, drag and drop, `--import`), pasted config text and
  AmneziaVPN `vpn://` keys; new empty tunnel with a generated key pair; editor with syntax
  highlighting; export and delete.
- Built-in backend: `amneziawg-go`, `awg` and `awg-quick` are embedded and unpacked to
  `/var/lib/awg-gui/bin` when no system copies exist, so no kernel module or AmneziaWG
  packages are required.
- Privileged helper started through `pkexec` once per session; the package installs a polkit
  policy with a clear message and remembered authorization.
- System tray icon (StatusNotifierItem) with tunnel toggles; closing the window hides the
  program to the tray; autostart at login into the tray; single instance.
- AmneziaWG 1.0 / 1.5 / 2.0 / 3.0 / 3.1 detection; the obfuscation parameters (junk packets,
  padding, header ranges, protocol imitation I1–I5, header protection, timers, random
  trailers, cookies) are shown in a collapsible block, one row each.
- **Bypass VPN for local networks** option: private addresses go directly while the tunnel's
  own subnet and private DNS servers stay in the VPN (`ip rule`s in the runtime config).
- Light, dark and system themes (default: system); the system theme is read from
  xdg-desktop-portal and followed live, since winit reports none on Linux; English and
  Russian interface; operation log.
- Debian package `awg-gui_<version>_amd64.deb` with menu entry, icons and polkit policy;
  release workflow on GitHub Actions.
