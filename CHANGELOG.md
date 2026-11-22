[English](CHANGELOG.md) | [Русский](CHANGELOG.ru_RU.md)

# Changelog

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
