// SPDX-License-Identifier: GPL-3.0-only
#![forbid(unsafe_code)]
use anyhow::{bail, Context, Result};
use futures_lite::future::block_on;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::{BufRead, BufReader, Read},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    sync::{
        mpsc::{self, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use zbus::{Connection, Proxy};
const SERVICE: &str = "xyz.ljones.Asusd";
const IFACE: &str = "xyz.ljones.Aura";
type Palette = [[u8; 3]; 4];
#[derive(Default)]
struct Opt {
    backend: String,
    output: Option<String>,
    file: Option<PathBuf>,
    pointer: Option<PathBuf>,
    dry: bool,
    once: bool,
    probe: bool,
    device: Option<String>,
}
fn options() -> Result<Opt> {
    let mut o = Opt {
        backend: "auto".into(),
        ..Default::default()
    };
    let mut a = env::args().skip(1);
    while let Some(k) = a.next() {
        match k.as_str() {
            "--backend" => o.backend = a.next().context("missing backend")?,
            "--output" => o.output = Some(a.next().context("missing output")?),
            "--wallpaper" => o.file = Some(a.next().context("missing image")?.into()),
            "--path-file" => o.pointer = Some(a.next().context("missing path file")?.into()),
            "--device" => o.device = Some(a.next().context("missing device")?),
            "--dry-run" => o.dry = true,
            "--once" => o.once = true,
            "--probe" => o.probe = true,
            "--help" => {
                println!("wallpaper-aura [--backend auto|cosmic|gnome|kde] [--output NAME_OR_ID] [--wallpaper PATH | --path-file PATH] [--dry-run] [--once] [--probe] [--device DBUS_PATH]");
                std::process::exit(0)
            }
            _ => bail!("unknown option {k}"),
        }
    }
    if o.file.is_some() && o.pointer.is_some() {
        bail!("choose --wallpaper or --path-file")
    }
    if o.file.is_some() || o.pointer.is_some() {
        o.backend = "file".into()
    }
    if o.probe {
        return Ok(o);
    }
    if o.backend == "auto" {
        let d = env::var("XDG_CURRENT_DESKTOP")
            .or_else(|_| env::var("XDG_SESSION_DESKTOP"))
            .unwrap_or_default()
            .to_lowercase();
        o.backend = if d.contains("cosmic") {
            "cosmic"
        } else if d.contains("kde") || d.contains("plasma") {
            "kde"
        } else if d.contains("gnome") {
            "gnome"
        } else {
            bail!("desktop environment unavailable; specify --backend")
        }
        .into()
    }
    if !["cosmic", "gnome", "kde", "file"].contains(&o.backend.as_str()) {
        bail!("unsupported backend")
    };
    Ok(o)
}
mod sources;
use sources::{absolute, Source};
mod color;
mod lid;
use color::palette;

struct Aura {
    bus: Connection,
    path: String,
    last: Option<Palette>,
}
impl Aura {
    fn new(device: Option<&str>) -> Result<Self> {
        let bus = block_on(Connection::system())?;
        let mut pending = vec!["/".to_owned()];
        let mut found = Vec::new();
        let mut visited = HashSet::new();
        while let Some(p) = pending.pop() {
            if !visited.insert(p.clone()) {
                continue;
            };
            if visited.len() > 128 {
                bail!("ASUS object tree too large")
            };
            let proxy = block_on(Proxy::new(
                &bus,
                SERVICE,
                p.as_str(),
                "org.freedesktop.DBus.Introspectable",
            ))?;
            let xml: String = block_on(proxy.call("Introspect", &()))?;
            let doc = roxmltree::Document::parse_with_options(
                &xml,
                roxmltree::ParsingOptions {
                    allow_dtd: true,
                    ..Default::default()
                },
            )?;
            if doc
                .root_element()
                .children()
                .any(|n| n.tag_name().name() == "interface" && n.attribute("name") == Some(IFACE))
            {
                found.push(p.clone())
            };
            for n in doc
                .root_element()
                .children()
                .filter(|n| n.tag_name().name() == "node")
            {
                if let Some(name) = n.attribute("name") {
                    pending.push(format!("{}/{name}", p.trim_end_matches('/')))
                }
            }
        }
        let path = if let Some(d) = device {
            if !found.iter().any(|p| p == d) {
                bail!("selected Aura device unavailable")
            };
            d.to_owned()
        } else {
            if found.len() != 1 {
                bail!("expected one Aura device; use --device; found {found:?}")
            };
            found.remove(0)
        };
        let a = Self {
            bus,
            path,
            last: None,
        };
        let p = a.proxy()?;
        let modes: Vec<u32> = block_on(p.get_property("SupportedBasicModes"))?;
        if !modes.contains(&0) {
            bail!("ASUS device lacks static mode")
        }
        let zones: Vec<u32> = block_on(p.get_property("SupportedBasicZones"))?;
        if !(1..=4).all(|v| zones.contains(&v)) {
            bail!("device lacks four keyboard zones")
        };
        drop(p);
        Ok(a)
    }
    fn proxy(&self) -> Result<Proxy<'_>> {
        Ok(block_on(Proxy::new(
            &self.bus,
            SERVICE,
            self.path.as_str(),
            IFACE,
        ))?)
    }
    fn apply(&mut self, colors: Palette, closed: &AtomicBool) -> Result<Option<usize>> {
        let mut count = 0;
        let mut synced = None;
        for (i, rgb) in colors.iter().enumerate() {
            if closed.load(Ordering::Acquire) {
                self.last = None;
                return Ok(None);
            }
            if self.last.is_some_and(|p| p[i] == *rgb) {
                continue;
            };
            let p = self.proxy()?;
            let b: u32 = block_on(p.get_property("Brightness"))?;
            if b == 0 {
                drop(p);
                self.last = None;
                return Ok(None);
            };
            if b > 3 {
                bail!("unknown ASUS brightness {b}")
            };
            if synced != Some(b) {
                block_on(p.set_property("Brightness", b))?;
                synced = Some(b)
            };
            let effect = (
                0u32,
                i as u32 + 1,
                (rgb[0], rgb[1], rgb[2]),
                (0u8, 0u8, 0u8),
                "Med",
                "Right",
            );
            if closed.load(Ordering::Acquire) {
                drop(p);
                self.last = None;
                return Ok(None);
            }
            if let Err(e) = block_on(p.set_property("LedModeData", effect)) {
                drop(p);
                self.last = None;
                return Err(e.into());
            };
            count += 1
        }
        self.last = Some(colors);
        Ok(Some(count))
    }
}
struct Children(Vec<Child>);
impl Drop for Children {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
fn monitor(
    children: &mut Children,
    tx: SyncSender<()>,
    program: &str,
    args: &[&str],
    filter: Option<&'static str>,
    reset: Option<Arc<AtomicBool>>,
    failed: Arc<AtomicBool>,
) -> Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let stdout = child.stdout.take().unwrap();
    thread::spawn(move || {
        let mut owner = None;
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            let n = match (&mut reader).take(65537).read_line(&mut line) {
                Ok(n) => n,
                Err(_) => break,
            };
            if n == 0 || n > 65536 {
                break;
            }
            match Ok::<_, std::io::Error>(line) {
                Ok(l) => {
                    let owner_line = l.contains("is owned by") || l.contains("has no owner");
                    if owner_line {
                        if owner.as_ref().is_some_and(|old| old != &l) {
                            if let Some(reset) = &reset {
                                reset.store(true, Ordering::Release);
                            }
                            let _ = tx.try_send(());
                        }
                        owner = Some(l.clone());
                    }
                    if filter.is_none_or(|s| l.contains(s)) {
                        let _ = tx.try_send(());
                    }
                }
                Err(_) => break,
            }
        }
        failed.store(true, Ordering::Release);
        // Wake the main loop on EOF so helper failure cannot go unnoticed.
        let _ = tx.try_send(());
    });
    children.0.push(child);
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct ImageKey {
    path: PathBuf,
    device: u64,
    inode: u64,
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl ImageKey {
    fn new(path: &Path) -> Result<Self> {
        let path = path.canonicalize()?;
        let m = fs::metadata(&path)?;
        if !m.is_file() {
            bail!("wallpaper must be a regular file")
        }
        Ok(Self {
            path,
            device: m.dev(),
            inode: m.ino(),
            len: m.len(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        })
    }
}
fn update_watches(
    watcher: &mut RecommendedWatcher,
    dirs: &mut HashMap<PathBuf, (u64, u64)>,
    tracked: &Arc<Mutex<Vec<PathBuf>>>,
    paths: Vec<PathBuf>,
) -> Result<()> {
    let paths = paths
        .iter()
        .map(|p| absolute(p))
        .collect::<Result<Vec<_>>>()?;
    *tracked
        .lock()
        .map_err(|_| anyhow::anyhow!("watch mutex poisoned"))? = paths.clone();
    let mut wanted = HashMap::new();
    for path in paths {
        let mut parent = path.parent().context("path has no parent")?.to_owned();
        while !parent.is_dir() {
            if !parent.pop() {
                bail!("no watchable ancestor")
            }
        }
        let meta = fs::metadata(&parent)?;
        wanted.insert(parent, (meta.dev(), meta.ino()));
    }
    // A directory may be replaced at the same pathname. Its old inotify
    // watch follows the old inode, so compare directory identities too.
    for d in dirs
        .keys()
        .filter(|d| wanted.get(*d) != dirs.get(*d))
        .cloned()
        .collect::<Vec<_>>()
    {
        let _ = watcher.unwatch(&d);
        dirs.remove(&d);
    }
    for (d, id) in wanted {
        if let std::collections::hash_map::Entry::Vacant(entry) = dirs.entry(d) {
            watcher.watch(entry.key(), RecursiveMode::NonRecursive)?;
            entry.insert(id);
        }
    }
    Ok(())
}
fn run() -> Result<()> {
    let mut o = options()?;
    o.file = o.file.as_deref().map(absolute).transpose()?;
    o.pointer = o.pointer.as_deref().map(absolute).transpose()?;
    if o.probe {
        let a = Aura::new(o.device.as_deref())?;
        let p = a.proxy()?;
        let b: u32 = block_on(p.get_property("Brightness"))?;
        let z: Vec<u32> = block_on(p.get_property("SupportedBasicZones"))?;
        println!(
            "{}",
            serde_json::json!({"device":a.path,"brightness":b,"zones":z,"brightness_policy":"best effort; defer Off, synchronize observed level","independent_lightbar_colors":false})
        );
        return Ok(());
    }
    let source = Source::new(&o)?;
    let mut aura = if o.dry {
        None
    } else {
        Some(Aura::new(o.device.as_deref())?)
    };
    let (tx, rx) = mpsc::sync_channel(1);
    let reset = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    // Dry-run is standalone image analysis and requires no hardware bus.
    let closed = if o.dry {
        Arc::new(AtomicBool::new(false))
    } else {
        lid::watch(tx.clone(), failed.clone())?
    };
    let tracked = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let filter = tracked.clone();
    let events = tx.clone();
    let watcher_failed = failed.clone();
    let lid_state = closed.clone();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(
        move |r: notify::Result<notify::Event>| {
            if r.is_err() {
                watcher_failed.store(true, Ordering::Release);
                let _ = events.try_send(());
                return;
            }
            if lid_state.load(Ordering::Acquire) {
                return;
            }
            if let Ok(e) = r {
                if matches!(e.kind, notify::EventKind::Access(kind) if !matches!(kind, notify::event::AccessKind::Close(notify::event::AccessMode::Write)))
                {
                    return;
                };
                if let Ok(paths) = filter.lock() {
                    if e.paths
                        .iter()
                        .any(|p| paths.iter().any(|t| t == p || t.starts_with(p)))
                    {
                        let _ = events.try_send(());
                    }
                }
            }
        },
    )?;
    if !o.dry {
        // Kernel notification for Fn-key brightness changes: poll(2) blocks,
        // it does not repeatedly read the sysfs file.
        use std::io::{Read, Seek, SeekFrom};
        if let Ok(leds) = fs::read_dir("/sys/class/leds") {
            for led in leds.flatten() {
                if !led.file_name().to_string_lossy().contains("kbd_backlight") {
                    continue;
                }
                if let Ok(mut file) = fs::File::open(led.path().join("brightness_hw_changed")) {
                    let tx = tx.clone();
                    thread::spawn(move || {
                        let mut buf = [0u8; 32];
                        let _ = file.read(&mut buf);
                        loop {
                            {
                                let mut fds = [rustix::event::PollFd::new(
                                    &file,
                                    rustix::event::PollFlags::PRI,
                                )];
                                match rustix::event::poll(&mut fds, None) {
                                    Err(rustix::io::Errno::INTR) => continue,
                                    Err(_) => break,
                                    Ok(_) => {}
                                }
                                if fds[0].revents().intersects(
                                    rustix::event::PollFlags::HUP | rustix::event::PollFlags::NVAL,
                                ) {
                                    break;
                                }
                            }
                            if file.seek(SeekFrom::Start(0)).is_err() {
                                break;
                            }
                            if let Err(e) = file.read(&mut buf) {
                                if e.raw_os_error() != Some(libc::ENODATA) {
                                    break;
                                }
                            }
                            if tx
                                .try_send(())
                                .is_err_and(|e| matches!(e, mpsc::TrySendError::Disconnected(_)))
                            {
                                break;
                            }
                        }
                    });
                }
            }
        }
    }
    let mut dirs = HashMap::new();
    let mut children = Children(Vec::new());
    if o.backend == "gnome" {
        monitor(
            &mut children,
            tx.clone(),
            "gsettings",
            &["monitor", "org.gnome.desktop.background"],
            None,
            None,
            failed.clone(),
        )?;
        monitor(
            &mut children,
            tx.clone(),
            "gsettings",
            &["monitor", "org.gnome.desktop.interface", "color-scheme"],
            None,
            None,
            failed.clone(),
        )?
    }
    if o.backend == "kde" {
        monitor(
            &mut children,
            tx.clone(),
            "gdbus",
            &[
                "monitor",
                "--session",
                "--dest",
                "org.kde.plasmashell",
                "--object-path",
                "/PlasmaShell",
            ],
            None,
            None,
            failed.clone(),
        )?
    }
    if let Some(a) = &aura {
        monitor(
            &mut children,
            tx.clone(),
            "gdbus",
            &[
                "monitor",
                "--system",
                "--dest",
                SERVICE,
                "--object-path",
                &a.path,
            ],
            Some("Brightness"),
            Some(reset.clone()),
            failed.clone(),
        )?
    }
    let initial = source
        .config
        .iter()
        .chain(o.file.iter())
        .cloned()
        .collect::<Vec<_>>();
    update_watches(&mut watcher, &mut dirs, &tracked, initial)?;
    let mut cache: Option<(ImageKey, Palette)> = None;
    loop {
        if failed.load(Ordering::Acquire) {
            bail!("notification source failed; restart required")
        }
        for child in &mut children.0 {
            if child.try_wait()?.is_some() {
                bail!("desktop notification helper exited; restart required")
            }
        }
        let result = (|| -> Result<()> {
            if closed.load(Ordering::Acquire) {
                return Ok(());
            }
            // Recover watches even when a source was deleted or malformed.
            let previous = tracked
                .lock()
                .map_err(|_| anyhow::anyhow!("watch mutex poisoned"))?
                .clone();
            update_watches(&mut watcher, &mut dirs, &tracked, previous)?;
            let path = source.read(&o)?;
            let mut paths = source.config.iter().cloned().collect::<Vec<_>>();
            if let Some(p) = &path {
                paths.push(p.clone());
                if let Ok(p) = p.canonicalize() {
                    paths.push(p)
                }
            }
            update_watches(&mut watcher, &mut dirs, &tracked, paths)?;
            let Some(path) = path else { return Ok(()) };
            let key = ImageKey::new(&path)?;
            let start = Instant::now();
            let cached = cache
                .as_ref()
                .filter(|(old, _)| old == &key)
                .map(|(_, c)| *c);
            let colors = match cached {
                Some(c) => c,
                None => palette(&key.path)?,
            };
            // Decode can overlap another wallpaper change. Do not write stale
            // colors or cache a file modified while decoding.
            if ImageKey::new(&path)? != key || source.read(&o)?.as_ref() != Some(&path) {
                if o.once {
                    bail!("wallpaper changed during decoding; retry once")
                }
                let _ = tx.try_send(());
                return Ok(());
            }
            cache = Some((key.clone(), colors));
            if reset.swap(false, Ordering::AcqRel) && !o.dry {
                aura = Some(Aura::new(o.device.as_deref())?);
            }
            // Lid may have closed while decoding: never send that palette.
            if closed.load(Ordering::Acquire) {
                return Ok(());
            }
            let writes = if let Some(a) = &mut aura {
                match a.apply(colors, &closed) {
                    Ok(Some(n)) => n,
                    Ok(None) => {
                        eprintln!("colors deferred until lid is open and brightness is enabled");
                        return Ok(());
                    }
                    Err(e) => {
                        a.last = None;
                        return Err(e);
                    }
                }
            } else {
                0
            };
            if !o.once && cached.is_some() && writes == 0 {
                return Ok(());
            }
            println!(
                "{}",
                serde_json::json!({"wallpaper":key.path,"colors":colors.map(|c|format!("{:02x}{:02x}{:02x}",c[0],c[1],c[2])),"dry_run":o.dry,"writes":writes,"analysis_ms":start.elapsed().as_secs_f64()*1000.})
            );
            Ok(())
        })();
        if let Err(e) = result {
            if o.once {
                return Err(e);
            }
            eprintln!("wallpaper-aura: {e:#}")
        };
        if o.once {
            break;
        };
        rx.recv()?;
        for child in &mut children.0 {
            if let Some(status) = child.try_wait()? {
                bail!("desktop notification helper exited: {status}")
            }
        }
        let deadline = Instant::now() + Duration::from_millis(100);
        while Instant::now() < deadline && rx.recv_timeout(Duration::from_millis(20)).is_ok() {}
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("wallpaper-aura: {e:#}");
        std::process::exit(1)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{contrast_weight, enhance, sample_palette};
    use crate::sources::{cosmic, local_path};
    #[test]
    fn cosmic_internal() {
        assert_eq!(
            cosmic(r#"[("HDMI-1",Path("/a")),("eDP-1",Path("/b"))]"#, None).unwrap(),
            Some(PathBuf::from("/b"))
        );
    }
    #[test]
    fn uri() {
        assert_eq!(
            local_path("file:///a%20b+c").unwrap(),
            PathBuf::from("/a b+c")
        );
        assert!(local_path("file://server/a").is_err());
    }
    #[test]
    fn dominant_and_dim() {
        let mut img = image::RgbaImage::new(128, 64);
        for (x, y, p) in img.enumerate_pixels_mut() {
            let n = [224, 112, 48, 8][x as usize / 32];
            *p = if y < 48 {
                image::Rgba([n, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 255])
            }
        }
        let colors = sample_palette(&image::DynamicImage::ImageRgba8(img));
        assert!(colors[0][0] > colors[1][0]);
        assert!(colors[1][0] > colors[2][0]);
        // The bright blue minority becomes prominent for the dimmest column.
        assert!(colors[3][2] > 0);
    }
    #[test]
    fn uniform_zones_remain_identical_and_black_is_preserved() {
        for rgb in [[0, 0, 0], [8, 2, 4], [224, 32, 64]] {
            let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                128,
                64,
                image::Rgba([rgb[0], rgb[1], rgb[2], 255]),
            ));
            assert_eq!(sample_palette(&image), [enhance(rgb); 4]);
        }
    }
    #[test]
    fn highlights_survive_black_background() {
        let mut im = image::RgbaImage::from_pixel(128, 64, image::Rgba([1, 0, 0, 255]));
        for y in 0..8 {
            for x in 32..96 {
                im.put_pixel(x, y, image::Rgba([255, 64, 96, 255]));
            }
        }
        let colors = sample_palette(&image::DynamicImage::ImageRgba8(im));
        assert_eq!(colors[0], [1; 3]);
        assert!(colors[1][0] > 100);
        assert_eq!(colors[1], colors[2]);
        assert_eq!(colors[0], colors[3]);
    }
    #[test]
    fn hidden_transparent_rgb_does_not_affect_palette() {
        let im = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            128,
            64,
            image::Rgba([255, 32, 128, 0]),
        ));
        assert_eq!(sample_palette(&im), [[1; 3]; 4]);
    }
    #[test]
    fn contrast_response_is_monotonic_and_uses_absolute_light() {
        assert_eq!(contrast_weight(0.2, 0.2), 0.0);
        assert!((contrast_weight(0.0, 0.1) - 0.5).abs() < 1e-12);
        assert!(contrast_weight(0.0, 0.001) < 0.01);
        assert!(contrast_weight(0.0, 0.8) > 0.88);
    }
    #[test]
    fn high_contrast_favors_highlight_hue() {
        let mut im = image::RgbaImage::from_pixel(128, 64, image::Rgba([16, 0, 8, 255]));
        // Approximately 5% per column is bright cyan.
        for col in 0..4 {
            for index in 0..103 {
                im.put_pixel(
                    col * 32 + index % 32,
                    index / 32,
                    image::Rgba([0, 255, 255, 255]),
                );
            }
        }
        let colors = sample_palette(&image::DynamicImage::ImageRgba8(im));
        assert!(colors[0][1] > 220);
        assert!(colors[0][1] > colors[0][0] * 4);
        assert!(colors.iter().all(|c| *c == colors[0]));
    }
    #[test]
    fn isolated_white_pixel_does_not_dominate_dark_zone() {
        let mut im = image::RgbaImage::from_pixel(128, 64, image::Rgba([0, 0, 0, 255]));
        im.put_pixel(0, 0, image::Rgba([255, 255, 255, 255]));
        let colors = sample_palette(&image::DynamicImage::ImageRgba8(im));
        assert!(colors[0][0] < 10);
        assert_eq!(colors[1], [1; 3]);
    }
    #[test]
    fn vivid_colors_gain_saturation_and_zone_contrast() {
        let before = [199, 106, 93];
        let after = enhance(before);
        let sat = |c: [u8; 3]| {
            let max = *c.iter().max().unwrap() as f64;
            let min = *c.iter().min().unwrap() as f64;
            (max - min) / max
        };
        assert!(sat(after) > sat(before));
        assert!(after[0] > before[0]);
        assert!(enhance([14, 1, 7])[0] < 14);
        assert_eq!(enhance([0; 3]), [1; 3]);
        let neutral = enhance([128; 3]);
        assert_eq!(neutral[0], neutral[1]);
        assert_eq!(neutral[1], neutral[2]);
    }
    #[test]
    fn black_floor_white_endpoint_and_contrast_order() {
        assert_eq!(enhance([0; 3]), [1; 3]);
        assert_eq!(enhance([14, 1, 7]), [1; 3]);
        assert_eq!(enhance([255; 3]), [255; 3]);
        let mut previous = 0;
        for v in 0..=255 {
            let c = enhance([v; 3]);
            assert!(c[0] >= previous);
            previous = c[0];
        }
    }
    #[test]
    fn neighboring_zone_changes_do_not_affect_other_zones() {
        let mut im = image::RgbaImage::from_pixel(128, 64, image::Rgba([72, 24, 48, 255]));
        let before = sample_palette(&image::DynamicImage::ImageRgba8(im.clone()));
        for y in 0..64 {
            for x in 32..64 {
                im.put_pixel(x, y, image::Rgba([0, 255, 64, 255]));
            }
        }
        let after = sample_palette(&image::DynamicImage::ImageRgba8(im));
        assert_ne!(before[1], after[1]);
        for zone in [0, 2, 3] {
            assert_eq!(before[zone], after[zone]);
        }
    }
    #[test]
    fn malformed_uris_fail_without_panics() {
        for value in [
            "file:///a%",
            "file:///a%GG",
            "file:///a%00",
            "file:///a#frag",
            "https://example/a",
        ] {
            assert!(local_path(value).is_err());
        }
    }
    #[test]
    fn ron_unicode_paths_are_supported() {
        assert_eq!(
            cosmic(r#"[("eDP-1", Path("/a\u{1f600}.png"))]"#, None).unwrap(),
            Some(PathBuf::from("/a😀.png"))
        );
    }
}
