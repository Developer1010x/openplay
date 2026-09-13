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

The desktop entry launches `openplay-sender`. There is no entry for
`openplay-receiver`, which was correct when the receiver could not host a
session and is now simply missing: the receiver advertises itself, listens and
displays video, so it deserves its own launcher.

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

`flatpak/org.openplay.OpenPlay.yml` targets the GNOME 47 runtime with the
`rust-stable` SDK extension and would install both binaries plus the desktop
entry, metainfo and icon.

**It does not build.** The manifest says so itself, at length, in the comment
above its `sources:` list — read that before spending time on it. In short: the
build commands run `cargo --offline`, flatpak-builder gives the build no
network, and nothing in the manifest supplies the crate sources offline, so the
first dependency fetch fails and in a clean sandbox that is every dependency.
Fixing it means either generating `cargo-sources.json` with
`flatpak-cargo-generator.py` from flatpak-builder-tools and regenerating it
whenever `Cargo.lock` changes, or committing a `cargo vendor` tree with the
matching source replacement. Both are decisions rather than edits, which is why
neither has been guessed at.

So this is the command that *would* build it, not a command that works today:

```bash
flatpak-builder --user --install --force-clean build flatpak/org.openplay.OpenPlay.yml
```

It is not published to any remote.

### Known limitations, once it does build

**Wi-Fi Direct will not work under Flatpak.** Nothing grants access to
`fi.w1.wpa_supplicant1` on the system bus. `--system-talk-name=fi.w1.wpa_supplicant1`
would be required, and it is a broad permission worth thinking about before
granting. MICE and AirPlay are unaffected.

**GStreamer plugins come from the runtime.** The manifest builds no GStreamer
modules of its own, so the available elements are whatever
`org.gnome.Platform//47` ships. Hardware encoders needing a plugin outside the
runtime, or driver access beyond `--device=dri`, may not be found — expect the
x264 fallback more often than on a host build.

That caveat bites hardest on WebRTC. `webrtcbin` needs the `nice` plugin from
libnice, which is a separate package from `gst-plugins-bad` on every
distribution and should not be assumed present in the runtime. Absent it,
`webrtcbin` constructs and then refuses every pad request, so casts fail with no
useful error. Check inside the sandbox before concluding the app is broken:

```bash
flatpak run --command=gst-inspect-1.0 org.openplay.OpenPlay nice
```

If that finds nothing, the manifest needs a libnice module built with its
GStreamer plugin enabled. See
[install.md](install.md#the-nice-plugin-specifically).

Screen capture should work: `--socket=wayland` plus the portal access every
sandbox has by default lets `ashpd` reach the XDG Desktop Portal, the same path
used outside the sandbox. Note that the manifest deliberately requests **no**
`--socket=pulseaudio` and no `--talk-name=org.freedesktop.portal.*` — the first
because there is no audio code to use it, the second because those names are
already reachable by default, so the lines granted nothing.

## CI artifacts

The `Build Release` job uploads `openplay-sender` and `openplay-receiver` as a
GitHub Actions artifact named `openplay-binaries`. These are unsigned Linux
build outputs for convenience, not a release channel.

## Not yet packaged

Homebrew, Winget and AUR are listed as areas where help is useful in
[contributing.md](contributing.md). Note that macOS and Windows packaging is
premature while neither platform has a screen capture backend — see
[install.md](install.md).

Whatever the format, a package must pull in the **`nice` GStreamer plugin**
alongside the other plugins, or every OpenPlay cast fails on a machine that
installed only the package. `packaging/build-deb.sh` is the reference for what
that set is: it lists the GStreamer plugin packages and the GUI libraries by
hand, because both are dlopen'd and `dpkg-shlibdeps` sees neither, and its
comments record why each entry is there. Keep any new packaging in step with
that list and with [install.md](install.md#prerequisites).
