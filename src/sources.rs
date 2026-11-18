// SPDX-License-Identifier: GPL-3.0-only
//! GNOME, Plasma and COSMIC wallpaper sources; no hardware writes.
use crate::Opt;
use anyhow::{bail, Context, Result};
use futures_lite::future::block_on;
use regex::Regex;
use std::{
    collections::HashMap,
    env, fs,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use zbus::{Connection, OwnedVariant, Proxy};
fn xdg(key: &str, fallback: &str) -> PathBuf {
    env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(fallback))
}
pub(crate) fn local_path(s: &str) -> Result<PathBuf> {
    if s.starts_with("file:") {
        let s = s
            .strip_prefix("file://localhost/")
            .map(|v| format!("/{v}"))
            .or_else(|| s.strip_prefix("file:///").map(|v| format!("/{v}")))
            .context("only local file:/// URIs supported")?;
        if s.contains(['?', '#']) {
            bail!("file URI query/fragment must be percent encoded")
        }
        let mut bytes = Vec::new();
        let b = s.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' {
                if i + 2 >= b.len() {
                    bail!("invalid URI escape")
                };
                bytes.push(u8::from_str_radix(
                    std::str::from_utf8(&b[i + 1..i + 3])?,
                    16,
                )?);
                i += 3
            } else {
                bytes.push(b[i]);
                i += 1
            }
        }
        let s = String::from_utf8(bytes)?;
        if s.contains('\0') {
            bail!("NUL in path")
        };
        return Ok(s.into());
    }
    if s.contains("://") || s.contains('\0') {
        bail!("unsupported wallpaper path")
    };
    Ok(PathBuf::from(s))
}
fn command(program: &str, args: &[&str]) -> Result<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("launch {program}"))?;
    let mut output = String::new();
    let result = child
        .stdout
        .take()
        .context("helper stdout missing")?
        .take(65537)
        .read_to_string(&mut output);
    if result.is_err() || output.len() > 65536 {
        let _ = child.kill();
        let _ = child.wait();
        result?;
        bail!("{program} output exceeds 64 KiB");
    }
    if !child.wait()?.success() {
        bail!("{program} failed; see helper diagnostic")
    }
    Ok(output.trim().to_owned())
}
fn read_text(path: &Path) -> Result<String> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        bail!("expected regular file: {}", path.display())
    }
    let mut data = String::new();
    file.take(65537).read_to_string(&mut data)?;
    if data.len() > 65536 {
        bail!("configuration exceeds 64 KiB")
    }
    Ok(data)
}
pub(crate) fn absolute(path: &Path) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        env::current_dir()?.join(path)
    })
}
fn setting(schema: &str, key: &str) -> Result<String> {
    let v = command("gsettings", &["get", schema, key])?;
    let quote = v.chars().next().context("empty GSettings result")?;
    if v.len() < 2 || !['\'', '"'].contains(&quote) || !v.ends_with(quote) {
        bail!("expected GSettings string")
    }
    let mut out = String::new();
    let mut chars = v[1..v.len() - 1].chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.push(match chars.next().context("truncated GSettings escape")? {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                c @ ('\\' | '\'' | '"') => c,
                _ => bail!("unsupported GSettings escape"),
            });
        } else {
            out.push(c);
        }
    }
    Ok(out)
}
pub(crate) fn cosmic(text: &str, output: Option<&str>) -> Result<Option<PathBuf>> {
    let re =
        Regex::new(r#"\(\s*("(?:[^"\\]|\\.)*")\s*,\s*(Path|Color)\s*\(\s*("(?:[^"\\]|\\.)*")?"#)?;
    let mut entries = Vec::new();
    for c in re.captures_iter(text) {
        let name: String = ron::from_str(&c[1])?;
        let path = if &c[2] == "Path" {
            Some(local_path(&ron::from_str::<String>(
                c.get(3).context("invalid COSMIC path")?.as_str(),
            )?)?)
        } else {
            None
        };
        entries.push((name, path))
    }
    let selected = if let Some(o) = output {
        entries.iter().find(|(n, _)| n == o)
    } else {
        entries
            .iter()
            .find(|(n, _)| ["eDP", "LVDS", "DSI"].iter().any(|p| n.starts_with(p)))
            .or(entries.first())
    };
    Ok(selected.and_then(|(_, p)| p.clone()))
}
pub(crate) struct Source {
    pub(crate) config: Option<PathBuf>,
    bus: Option<Connection>,
}
impl Source {
    pub(crate) fn new(o: &Opt) -> Result<Self> {
        Ok(Self {
            config: match o.backend.as_str() {
                "cosmic" => Some(
                    xdg("XDG_STATE_HOME", ".local/state")
                        .join("cosmic/com.system76.CosmicBackground/v1/wallpapers"),
                ),
                "kde" => Some(
                    xdg("XDG_CONFIG_HOME", ".config")
                        .join("plasma-org.kde.plasma.desktop-appletsrc"),
                ),
                _ => o.pointer.clone(),
            },
            bus: if o.backend == "kde" {
                Some(block_on(Connection::session())?)
            } else {
                None
            },
        })
    }
    pub(crate) fn read(&self, o: &Opt) -> Result<Option<PathBuf>> {
        match o.backend.as_str() {
            "file" => {
                if let Some(p) = &o.file {
                    return Ok(Some(p.clone()));
                };
                let p = o.pointer.as_ref().context("missing source")?;
                let s = read_text(p)?;
                let p2 = local_path(s.trim_end_matches(['\n', '\r']))?;
                Ok(Some(if p2.is_absolute() {
                    p2
                } else {
                    p.parent().unwrap_or(Path::new(".")).join(p2)
                }))
            }
            "cosmic" => cosmic(
                &read_text(self.config.as_ref().context("missing COSMIC state path")?)?,
                o.output.as_deref(),
            ),
            "gnome" => {
                if setting("org.gnome.desktop.background", "picture-options")? == "none" {
                    return Ok(None);
                };
                let dark = setting("org.gnome.desktop.interface", "color-scheme")
                    .unwrap_or_default()
                    == "prefer-dark";
                let s = setting(
                    "org.gnome.desktop.background",
                    if dark {
                        "picture-uri-dark"
                    } else {
                        "picture-uri"
                    },
                )?;
                if s.is_empty() {
                    return Ok(None);
                };
                let p = local_path(&s)?;
                if p.extension().is_some_and(|v| v == "xml") {
                    bail!("GNOME XML slideshow current frame is unsupported")
                };
                Ok(Some(p))
            }
            "kde" => {
                let proxy = block_on(Proxy::new(
                    self.bus.as_ref().unwrap(),
                    "org.kde.plasmashell",
                    "/PlasmaShell",
                    "org.kde.PlasmaShell",
                ))?;
                let screen: u32 = o.output.as_deref().unwrap_or("0").parse()?;
                let values: HashMap<String, OwnedVariant> =
                    block_on(proxy.call("wallpaper", &zbus::DynamicTuple((screen,))))?;
                let v = values.get("Image").context("Plasma returned no Image")?;
                let s = <&str>::try_from(v)?;
                let p = local_path(s)?;
                if p.is_dir() {
                    bail!("Plasma returned a wallpaper package; select an image file")
                };
                Ok(Some(p))
            }
            _ => unreachable!(),
        }
    }
}
