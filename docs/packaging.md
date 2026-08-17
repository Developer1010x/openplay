# Packaging and system integration

What lives under `data/` and `flatpak/`, what each file is for, and where to
install it. Nothing here is installed by `cargo build` — these are for
distribution packages and for making Wi-Fi Direct work on a developer machine.

## `data/`

| File | Installs to | Purpose |
|---|---|---|
| `org.openplay.OpenPlay.desktop` | `/usr/share/applications/` | Desktop entry for the sender |
| `org.openplay.OpenPlay.metainfo.xml` | `/usr/share/metainfo/` | AppStream metadata for software centres |
| `icons/hicolor/scalable/apps/org.openplay.OpenPlay.svg` | `/usr/share/icons/hicolor/scalable/apps/` | Application icon |
| `dbus/org.openplay.wpa.conf` | `/etc/dbus-1/system.d/` | D-Bus policy for wpa_supplicant access |
| `polkit/10-openplay-wpa.rules` | `/etc/polkit-1/rules.d/` | polkit rule for wpa_supplicant access |

The desktop entry launches `openplay-sender`. There is deliberately no entry for
`openplay-receiver`, since the receiver does not yet host a session.

Validate the first two after editing:

```bash
desktop-file-validate data/org.openplay.OpenPlay.desktop
appstreamcli validate data/org.openplay.OpenPlay.metainfo.xml
```

## Wi-Fi Direct needs D-Bus permission

This is the part most likely to bite you, and it is a prerequisite rather than a
packaging nicety.

Miracast Wi-Fi Direct talks to **wpa_supplicant over the system D-Bus**, not
through NetworkManager. An unprivileged user cannot do that by default, so
`MiracastSession::start_wifi_direct` fails to start the P2P manager and no peers
are ever found.

Both shipped files grant access to the **`netdev`** group:

```bash
sudo install -Dm644 data/dbus/org.openplay.wpa.conf \
  /etc/dbus-1/system.d/org.openplay.wpa.conf
sudo install -Dm644 data/polkit/10-openplay-wpa.rules \
  /etc/polkit-1/rules.d/10-openplay-wpa.rules

sudo usermod -aG netdev "$USER"      # log out and back in for this to take effect
sudo systemctl reload dbus
```

Confirm it worked:

```bash
groups | grep netdev
busctl --system tree fi.w1.wpa_supplicant1     # should not be empty
```

Note that `org.openplay.wpa.conf` also contains a permissive
`<policy context="default">` fallback that allows any user to talk to
wpa_supplicant. That is convenient for development and is **not** what a
distribution package should ship — packagers should drop the fallback block and
rely on the `netdev` policy alone.

MICE (both devices on the same network) needs none of this. It is only Wi-Fi
Direct that requires D-Bus access.

## Flatpak

`flatpak/org.openplay.OpenPlay.yml` builds against the GNOME 47 runtime using
the `rust-stable` SDK extension, and installs both binaries plus the desktop
entry, metainfo and icon.

```bash
flatpak-builder --user --install --force-clean build flatpak/org.openplay.OpenPlay.yml
flatpak run org.openplay.OpenPlay
```

It is not published to any remote.

### Two known limitations

**Wi-Fi Direct does not work under Flatpak.** The manifest's `finish-args` grant
`org.freedesktop.portal.Desktop` and `org.freedesktop.portal.Background`, but
nothing grants access to `fi.w1.wpa_supplicant1` on the system bus. Adding
`--system-talk-name=fi.w1.wpa_supplicant1` would be required, and it is a broad
permission worth thinking about before granting. MICE and AirPlay are
unaffected.

**GStreamer plugins come from the runtime.** The manifest builds no GStreamer
modules of its own, so the available encoders are whatever
`org.gnome.Platform//47` ships. Hardware encoders that need a plugin outside the
runtime, or driver access beyond `--device=dri`, may not be found — expect the
x264 fallback more often than on a host build.

Screen capture works because `--socket=wayland` plus the portal talk-name let
`ashpd` reach the XDG Desktop Portal, which is the same path used outside the
sandbox.

## CI artifacts

The `Build Release` job uploads `openplay-sender` and `openplay-receiver` as a
GitHub Actions artifact named `openplay-binaries`. These are unsigned Linux
build outputs for convenience, not a release channel.

## Not yet packaged

Homebrew, Winget and AUR are listed as areas where help is useful in
[contributing.md](contributing.md). Note that macOS and Windows packaging is
premature while neither platform has a screen capture backend — see
[install.md](install.md).
