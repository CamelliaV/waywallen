<p align="center">
  <img src="ui/assets/waywallen-ui.svg" alt="Waywallen" width="128" />
</p>

<h1 align="center">Waywallen</h1>

<p align="center"><strong>CamelliaV fork：Linux 动态壁纸管理器</strong></p>

<a href="README.md">English README</a> · <a href="https://github.com/waywallen/waywallen">上游仓库</a>

---

这个 fork 保留上游 Waywallen 的基础能力，并加入我在 Arch/KDE 桌面上实际使用的一组改动，重点是安静启动、恢复上次应用结果、减少重复壁纸，以及修好应用内图标。

## Fork 改动

- 默认静默启动：启动 Waywallen 时只启动 daemon 和托盘入口，不主动弹出主窗口。
- 启动时优先恢复最后一次 `Apply` 的壁纸，而不是被旧的单显示器记录覆盖。
- 支持在其他应用通过 MPRIS 播放媒体时自动暂停壁纸。
- Wallpaper Engine 库路径会按真实文件系统路径归一化，避免 `~/.steam/steam` 和 `~/.local/share/Steam` 这种等价 Steam 路径导致壁纸精准重复两份。
- 应用内 Material 图标优先加载系统的 Material Symbols Rounded 字体，绕开依赖里 Git LFS 字体资源不完整导致的缺图标问题。
- 壁纸详情页的路径旁增加了文件夹按钮，可以直接打开壁纸所在目录。

## 安装

### 替换 AUR 的 `waywallen-bin`

如果之前是通过 `paru` 安装的上游二进制包，先卸载它，避免命令行继续启动旧版本：

```bash
paru -Rns waywallen-bin
```

然后从这个 fork 源码构建并安装：

```bash
git clone https://github.com/CamelliaV/waywallen.git
cd waywallen
cmake --preset clang-release -DCMAKE_INSTALL_PREFIX=/usr/local
cmake --build build/clang-release -j"$(nproc)"
sudo cmake --install build/clang-release
```

启动：

```bash
waywallen
```

如果你的 Qt 没有自动搜索 `/usr/local/lib/qt6/qml`，用下面的方式启动：

```bash
export QML_IMPORT_PATH=/usr/local/lib/qt6/qml
export QML2_IMPORT_PATH="$QML_IMPORT_PATH"
waywallen
```

### 本地临时安装

如果只是想测试，不想写入 `/usr/local`：

```bash
cmake --preset clang-release -DCMAKE_INSTALL_PREFIX="$PWD/install"
cmake --build build/clang-release -j"$(nproc)"
cmake --install build/clang-release

export LD_LIBRARY_PATH="$PWD/install/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export QML_IMPORT_PATH="$PWD/install/lib/qt6/qml"
export QML2_IMPORT_PATH="$QML_IMPORT_PATH"
"$PWD/install/bin/waywallen"
```

## 桌面集成

| 桌面 | 集成 | 鼠标输入 | 自动暂停 |
|------|------|:--------:|:--------:|
| **KDE Plasma** | [waywallen-display](https://github.com/waywallen/waywallen-display/) | 支持 | 支持 |
| **Niri** | `zwlr_layer_shell_v1` | 支持 | 部分支持 |
| **Sway** | `zwlr_layer_shell_v1` | 支持 | 部分支持 |
| **GNOME** | 规划中 | - | - |

## 兼容性

| 项目 | 现状 |
|------|------|
| 图片壁纸 | 支持 |
| 场景壁纸 | 支持，通过 [open-wallpaper-engine](https://github.com/waywallen/open-wallpaper-engine) |
| 视频壁纸 | 支持 |
| 网页壁纸 | 支持，通过 [open-wallpaper-engine](https://github.com/waywallen/open-wallpaper-engine) |

更底层的构建说明见 [BUILD.md](BUILD.md)。
