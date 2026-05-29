<p align="center">
  <img src="ui/assets/waywallen-ui.svg" alt="Waywallen" width="128" />
</p>

<h1 align="center">Waywallen</h1>

<p align="center"><strong>CamelliaV fork of the Linux dynamic wallpaper manager</strong></p>

<a href="README.CN.md">中文 README</a> · <a href="https://github.com/waywallen/waywallen">Upstream</a>

---

This fork keeps the upstream Waywallen foundation and carries the changes I use for a quieter desktop workflow on Arch/KDE.

## Fork Changes

- Silent startup is enabled by default: launching Waywallen starts the daemon and tray entry without popping the main window.
- Startup restore prefers the last wallpaper you applied, so a cold start resumes the last `Apply` result.
- Auto-pause can pause wallpapers when another application is playing media through MPRIS.
- Wallpaper Engine libraries are normalized by their real filesystem path, avoiding duplicate entries from symlink-equivalent Steam roots such as `~/.steam/steam` and `~/.local/share/Steam`.
- In-app Material icons load from the system Material Symbols Rounded font when available, avoiding missing icons caused by incomplete Git LFS font assets.
- Wallpaper details include a folder button that opens the wallpaper's containing directory.

## Install

### Replace an AUR `waywallen-bin` Install

If you installed the upstream binary package through `paru`, remove it first so your shell does not keep launching the old build:

```bash
paru -Rns waywallen-bin
```

Then build and install this fork from source:

```bash
git clone https://github.com/CamelliaV/waywallen.git
cd waywallen
cmake --preset clang-release -DCMAKE_INSTALL_PREFIX=/usr/local
cmake --build build/clang-release -j"$(nproc)"
sudo cmake --install build/clang-release
```

Run it with:

```bash
waywallen
```

If your Qt installation does not search `/usr/local/lib/qt6/qml`, launch with:

```bash
export QML_IMPORT_PATH=/usr/local/lib/qt6/qml
export QML2_IMPORT_PATH="$QML_IMPORT_PATH"
waywallen
```

### Local Staged Install

For testing without touching `/usr/local`:

```bash
cmake --preset clang-release -DCMAKE_INSTALL_PREFIX="$PWD/install"
cmake --build build/clang-release -j"$(nproc)"
cmake --install build/clang-release

export LD_LIBRARY_PATH="$PWD/install/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export QML_IMPORT_PATH="$PWD/install/lib/qt6/qml"
export QML2_IMPORT_PATH="$QML_IMPORT_PATH"
"$PWD/install/bin/waywallen"
```

## Desktop Integration

| Desktop | Integration | Mouse input | Auto pause |
|---------|-------------|:-----------:|:----------:|
| **KDE Plasma** | [waywallen-display](https://github.com/waywallen/waywallen-display/) | yes | yes |
| **Niri** | `zwlr_layer_shell_v1` | yes | partial |
| **Sway** | `zwlr_layer_shell_v1` | yes | partial |
| **GNOME** | planned | - | - |

## Compatibility

| Item | Status |
|------|--------|
| Image wallpapers | yes |
| Scene wallpapers | yes, via [open-wallpaper-engine](https://github.com/waywallen/open-wallpaper-engine) |
| Video wallpapers | yes |
| Web wallpapers | yes, via [open-wallpaper-engine](https://github.com/waywallen/open-wallpaper-engine) |

For lower-level build details, see [BUILD.md](BUILD.md).
