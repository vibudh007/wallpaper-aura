# ASUS Wallpaper Aura

Match a four-zone ASUS laptop keyboard to its wallpaper. Wallpaper Aura is a small Rust daemon that responds to wallpaper changes, divides the image into four equal vertical columns, and applies one color per zone through asusd, the daemon shipped with asusctl.

It sleeps between events, pauses automatic color work while the laptop lid is closed, and refreshes the current wallpaper when the lid opens. Zones are processed independently; the keyboard provides its natural optical diffusion.

## Targets

- Linux with systemd/logind, a user session bus, and asusd.
- COSMIC, GNOME, and KDE Plasma. X11 or Wayland does not affect image-based processing.
- ASUS devices exposing static color mode and four basic keyboard zones, `Key1`–`Key4`, through the supported asusd D-Bus interface.
- Local JPEG, PNG, and WebP wallpapers.

The current hardware validation was on a ROG Strix G513QY, with asusctl 6.5.0, running COSMIC on Fedora. Native GNOME and Plasma sessions have not yet been tested. This is an initial release; reports from other supported setups are welcome.

## How it works

1. Discover the active wallpaper through the desktop's settings or state.
2. Watch filesystem and desktop notifications; there is no periodic wallpaper polling.
3. Decode with input/allocation limits, honor EXIF orientation, and sample a fixed 128 × 64 grid.
4. For each column, compare the mean linear-light luminance of the darkest 5% and brightest 5% of samples. For a luminance difference `D`, the highlight weight is `D / (D + 0.10)`.
5. Blend the column's average RGB with its brightest-5% average in linear light. High-contrast regions favor visible highlights; low-contrast regions stay near their average.
6. Increase saturation and apply a fixed contrast curve. Dark zones remain dim; black/near-black output is `(1,1,1)`. There is no per-wallpaper brightness normalization or blending between zones.
7. Send changed colors through asusd on a persistent D-Bus connection.

COSMIC's active per-display state includes folder rotations. GNOME uses GSettings notifications, including light/dark changes. Plasma uses its live D-Bus wallpaper getter, shell notifications, and configuration changes. The default output is COSMIC's internal panel or Plasma screen `0`.

The event queue is bounded, bursts are coalesced, and the current palette is cached using file identity and nanosecond timestamps. State is checked again after decoding to avoid applying stale work. `logind` lid notifications suppress wallpaper processing while closed. Existing lighting is left as-is; opening the lid queues a refresh. Dry-run analysis remains available independently of lid state.

## Requirements

Runtime:

- `asusd` from asusctl, installed and running normally.
- systemd user services and logind with the `LidClosed` property.
- The desktop's existing `gsettings` for GNOME and `gdbus` for Plasma/ASUS notifications, usually supplied by GLib's utilities package. `gdbus` is also used on COSMIC for ASUS brightness notifications.

There is no Python, Pillow, GTK, ImageMagick, or compiler requirement for running the binary. Python and Pillow are optional test dependencies only. Do not run the daemon as root or grant it raw USB access; it uses the normal asusd authorization policy.

## Installation

### Release binary

Download `wallpaper-aura-0.1.0-x86_64-unknown-linux-gnu.tar.gz` and `SHA256SUMS` from [Releases](https://github.com/vibudh007/wallpaper-aura/releases). The binary is built for x86-64 Linux with glibc 2.39 or newer; it is not a universal Linux binary. Older glibc releases and musl distributions may need a source build.

```sh
sha256sum --check --ignore-missing SHA256SUMS
tar -xzf wallpaper-aura-0.1.0-x86_64-unknown-linux-gnu.tar.gz
cd wallpaper-aura-0.1.0-x86_64-unknown-linux-gnu
```

First inspect the selected palette and hardware capabilities without changing lighting:

```sh
./wallpaper-aura --dry-run --once
./wallpaper-aura --probe
```

Install the binary and user unit:

```sh
install -Dm755 wallpaper-aura "$HOME/.local/bin/wallpaper-aura"
install -Dm644 wallpaper-aura.service "$HOME/.config/systemd/user/wallpaper-aura.service"
systemctl --user daemon-reload
systemctl --user enable --now wallpaper-aura.service
```

Starting the service applies colors. It is tied to `graphical-session.target` and starts on graphical login. The unit's control group manages its notification helpers. Desktop sessions must activate that target and supply their environment to the user manager.

### Build from source

Use Rust 1.93 or newer. Dependencies are pinned in `Cargo.lock`.

```sh
git clone https://github.com/vibudh007/wallpaper-aura.git
cd wallpaper-aura
git checkout v0.1.0
cargo build --release --locked
./target/release/wallpaper-aura --dry-run --once
./target/release/wallpaper-aura --probe
install -Dm755 target/release/wallpaper-aura "$HOME/.local/bin/wallpaper-aura"
install -Dm644 wallpaper-aura.service "$HOME/.config/systemd/user/wallpaper-aura.service"
systemctl --user daemon-reload
systemctl --user enable --now wallpaper-aura.service
```

The release also includes a source archive with vendored dependencies and an offline Cargo configuration.

## Configuration and diagnostics

```sh
wallpaper-aura --help
wallpaper-aura --backend cosmic --output eDP-1 --dry-run --once
wallpaper-aura --backend kde --output 0 --dry-run --once
journalctl --user -u wallpaper-aura.service
```

If desktop auto-detection fails, set a backend explicitly. To customize the service:

```sh
systemctl --user edit wallpaper-aura.service
```

For example:

```ini
[Service]
ExecStart=
ExecStart=%h/.local/bin/wallpaper-aura --backend cosmic --output eDP-1
```

Then run `systemctl --user daemon-reload` and `systemctl --user restart wallpaper-aura.service`.

`--wallpaper /path/to/current-image` watches a fixed image or symlink. A wallpaper setter can atomically retarget that symlink. Alternatively, `--path-file /path/to/pointer` watches a text file containing one image path or local `file:///` URI. Relative image paths in the pointer are resolved against its directory. These hooks are useful when a wallpaper tool exposes better notifications than the desktop itself.

Use `--device /xyz/ljones/aura/...` if more than one compatible Aura device is discovered. `--once` processes one image and exits; without it, the daemon waits for notifications. A successful automatic update prints JSON containing the selected image, colors, elapsed processing time, and number of color writes.

## Uninstallation

```sh
systemctl --user disable --now wallpaper-aura.service
rm -f "$HOME/.config/systemd/user/wallpaper-aura.service"
rm -rf "$HOME/.config/systemd/user/wallpaper-aura.service.d"
rm -f "$HOME/.local/bin/wallpaper-aura"
systemctl --user daemon-reload
systemctl --user reset-failed wallpaper-aura.service 2>/dev/null || true
```

This removes the application, unit, and any overrides you added for it. It leaves asusctl/asusd installed and leaves the last applied lighting configuration in place. Select another effect through asusctl if desired.

## Limitations

- Brightness preservation is best effort. The installed asusd color setter also reapplies its cached keyboard brightness. Wallpaper Aura reads the current nonzero level and synchronizes the daemon to that same level before color updates; it defers while Off. Fn-key and D-Bus brightness notifications wake deferred work. A brightness change occurring between D-Bus calls can still race. A strict color-only guarantee needs an asusd API change.
- Lightbar power zones are not color zones. On the tested G513QY, asusd exposes independent keyboard/lightbar power flags, but only four keyboard color zones and no independent lightbar color zones. The daemon never changes those power flags. Firmware mirroring to an enabled lightbar has not been verified. Other models are not automatically assumed to support independent lightbar sections.
- Plasma's stock slideshow does not signal every transition externally. Detection there is best effort. Plasma wallpaper packages/directories and third-party wallpaper plugins are unsupported; select an image file or use a setter hook.
- GNOME XML slideshows are unsupported. Solid-color/no-image backgrounds leave the current keyboard colors unchanged.
- Lid pausing follows lid state, not display DPMS state. Turning off the internal display while leaving the lid open does not pause the daemon. Physical lid close/reopen testing was pending when this release was prepared; the live notification subscription and state logic were checked.
- Images are analyzed as files, not as screenshots of the cropped/scaled desktop. Display cropping, color profiles, HDR and physical LED calibration are not reproduced. EXIF orientation is honored.
- File, dimension, and decoder limits reject unusually large or malformed inputs. Full-image decoding is still required for some formats; the bounded sample does not make decoding free.
- No universal POSIX/XDG interface defines the current wallpaper. Support is through the named desktop adapters and explicit hooks.

## Efficiency and verification

The daemon uses event notifications, blocking waits, a bounded queue, bounded image inputs, and a single cached palette. There is no periodic wallpaper or lid polling. Some operations still consume CPU and battery when a wallpaper changes, and LED power consumption is independent of the daemon.

Short measurements on the development machine showed roughly 23 ms for a small synthetic replacement and zero idle CPU ticks over two seconds. Real 4K image processing took roughly 0.1–0.3 seconds, depending on the image. The installed service and helpers used around 6 MB after startup, with larger transient allocations while decoding. These are observations, not battery, memory, or latency guarantees.

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

The live lid subscription test is ignored by default because it needs a real logind system bus. Optional `python3 verify_integration.py` needs Pillow and an adjacent built binary; it checks atomic replacement, startup recovery, directory replacement, helper failure, FIFO rejection and resource stability.

See [AUDIT.md](AUDIT.md) for review findings and verification limits. Own code forbids unsafe Rust; dependencies and platform components still contain native code. Known-advisory checks do not prove the absence of vulnerabilities or leaks.

## License

GNU General Public License, version 3 only (`GPL-3.0-only`). See [LICENSE](LICENSE). Dependency code retains its own licenses; release archives include dependency license information and corresponding source.
