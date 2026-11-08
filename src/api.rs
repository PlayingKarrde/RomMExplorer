//! Minimal blocking client for the RomM REST API.
//!
//! Every model is deserialised leniently: unknown fields are ignored, missing or
//! `null` fields fall back to defaults, and metadata blobs are kept as raw JSON so
//! that differences between RomM versions don't break the whole library load.

use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::time::Duration;

pub const CANCELLED: &str = "Cancelled";

pub const STAGE_QUEUED: u8 = 0;
pub const STAGE_DOWNLOADING: u8 = 1;
pub const STAGE_EXTRACTING: u8 = 2;

/// Treat `null` the same as a missing field.
fn nd<'de, D, T>(d: D) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

fn first_nonempty<'a>(items: &[&'a str]) -> &'a str {
    items.iter().copied().find(|s| !s.trim().is_empty()).unwrap_or("")
}

#[derive(Debug, Clone)]
pub enum Auth {
    None,
    Basic { username: String, password: String },
    Token(String),
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Platform {
    #[serde(default)]
    pub id: i64,
    #[serde(default, deserialize_with = "nd")]
    pub slug: String,
    #[serde(default, deserialize_with = "nd")]
    pub name: String,
    #[serde(default, deserialize_with = "nd")]
    pub custom_name: String,
    #[serde(default, deserialize_with = "nd")]
    pub display_name: String,
    #[serde(default, deserialize_with = "nd")]
    pub rom_count: i64,
}

impl Platform {
    pub fn label(&self) -> &str {
        first_nonempty(&[&self.display_name, &self.custom_name, &self.name, &self.slug])
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Rom {
    #[serde(default)]
    pub id: i64,
    #[serde(default, deserialize_with = "nd")]
    pub name: String,
    #[serde(default, deserialize_with = "nd")]
    pub fs_name: String,
    #[serde(default, deserialize_with = "nd")]
    pub fs_name_no_ext: String,
    #[serde(default, deserialize_with = "nd")]
    pub fs_size_bytes: u64,
    #[serde(default, deserialize_with = "nd")]
    pub platform_id: i64,
    #[serde(default, deserialize_with = "nd")]
    pub platform_slug: String,
    #[serde(default, deserialize_with = "nd")]
    pub platform_fs_slug: String,
    #[serde(default, deserialize_with = "nd")]
    pub platform_display_name: String,
    #[serde(default, deserialize_with = "nd")]
    pub platform_custom_name: String,
    #[serde(default, deserialize_with = "nd")]
    pub platform_name: String,
    #[serde(default, deserialize_with = "nd")]
    pub summary: String,
    #[serde(default, deserialize_with = "nd")]
    pub regions: Vec<String>,
    #[serde(default, deserialize_with = "nd")]
    pub languages: Vec<String>,
    #[serde(default, deserialize_with = "nd")]
    pub revision: String,
    #[serde(default, deserialize_with = "nd")]
    pub path_cover_small: String,
    #[serde(default, deserialize_with = "nd")]
    pub path_cover_large: String,
    #[serde(default, deserialize_with = "nd")]
    pub url_cover: String,
    #[serde(default, deserialize_with = "nd")]
    pub has_multiple_files: bool,
    /// Older RomM versions.
    #[serde(default, deserialize_with = "nd")]
    pub multi: bool,
    #[serde(default)]
    pub metadatum: Value,
    #[serde(default)]
    pub igdb_metadata: Value,
}

fn string_list(v: &Value) -> Vec<String> {
    match v {
        Value::Array(items) => items
            .iter()
            .filter_map(|i| match i {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => o.get("name").and_then(|n| n.as_str()).map(str::to_owned),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

impl Rom {
    pub fn title(&self) -> &str {
        first_nonempty(&[&self.name, &self.fs_name_no_ext, &self.fs_name])
    }

    pub fn is_multi(&self) -> bool {
        self.has_multiple_files || self.multi
    }

    pub fn platform_label(&self) -> &str {
        first_nonempty(&[
            &self.platform_display_name,
            &self.platform_custom_name,
            &self.platform_name,
            &self.platform_slug,
        ])
    }

    /// File/folder name RomM stores this rom under.
    pub fn file_name(&self) -> String {
        if self.fs_name.is_empty() {
            format!("rom-{}", self.id)
        } else {
            self.fs_name.clone()
        }
    }

    fn meta_list(&self, key: &str) -> Vec<String> {
        for m in [&self.metadatum, &self.igdb_metadata] {
            let list = string_list(m.get(key).unwrap_or(&Value::Null));
            if !list.is_empty() {
                return list;
            }
        }
        Vec::new()
    }

    pub fn genres(&self) -> Vec<String> {
        self.meta_list("genres")
    }

    pub fn companies(&self) -> Vec<String> {
        let devs = self.meta_list("developers");
        if !devs.is_empty() {
            return devs;
        }
        self.meta_list("companies")
    }

    /// Release date as unix seconds.
    pub fn release_ts(&self) -> Option<i64> {
        for m in [&self.metadatum, &self.igdb_metadata] {
            let ts = match m.get("first_release_date") {
                Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
                Some(Value::String(s)) => s.parse::<i64>().ok(),
                _ => None,
            };
            if let Some(ts) = ts.filter(|t| *t != 0) {
                // RomM stores milliseconds; older versions used seconds.
                return Some(if ts.abs() > 100_000_000_000 { ts / 1000 } else { ts });
            }
        }
        None
    }

    /// Rating normalised to 0..=100.
    pub fn rating(&self) -> Option<f64> {
        for (m, key) in [
            (&self.metadatum, "average_rating"),
            (&self.igdb_metadata, "total_rating"),
        ] {
            if let Some(r) = m.get(key).and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok())) {
                if r > 0.0 {
                    return Some(if r <= 10.0 { r * 10.0 } else { r });
                }
            }
        }
        None
    }
}

#[derive(Default, Debug)]
pub struct Progress {
    pub downloaded: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
    pub stage: AtomicU8,
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        (self.downloaded.load(Ordering::Relaxed) as f64 / total as f64).clamp(0.0, 1.0) as f32
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

pub fn normalize_base(url: &str) -> String {
    let mut s = url.trim().trim_end_matches('/').to_string();
    if !s.is_empty() && !s.starts_with("http://") && !s.starts_with("https://") {
        s = format!("http://{s}");
    }
    s
}

#[derive(Clone)]
pub struct RommClient {
    http: Client,
    base: String,
    auth: Auth,
}

fn check(resp: Response) -> Result<Response> {
    let status = resp.status();
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        bail!("Authentication failed ({status}). Check your username/password or API token.");
    }
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        let snippet: String = body.chars().take(200).collect();
        bail!("Server returned {status}: {snippet}");
    }
    Ok(resp)
}

impl RommClient {
    pub fn new(server: &str, auth: Auth, accept_invalid_certs: bool) -> Result<Self> {
        let base = normalize_base(server);
        if base.is_empty() {
            bail!("Please enter your RomM server address");
        }
        Url::parse(&base).with_context(|| format!("'{base}' is not a valid URL"))?;
        let http = Client::builder()
            .user_agent(concat!("RomMExplorer/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(180))
            .danger_accept_invalid_certs(accept_invalid_certs)
            .build()?;
        Ok(Self { http, base, auth })
    }

    pub fn host(&self) -> String {
        Url::parse(&self.base)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_else(|| self.base.clone())
    }

    fn authed(&self, rb: RequestBuilder) -> RequestBuilder {
        match &self.auth {
            Auth::Basic { username, password } => rb.basic_auth(username, Some(password)),
            Auth::Token(t) => rb.bearer_auth(t),
            Auth::None => rb,
        }
    }

    fn get_json(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let url = format!("{}/api/{}", self.base, path);
        let resp = self
            .authed(self.http.get(&url).query(query))
            .send()
            .with_context(|| format!("Could not reach {}", self.base))?;
        check(resp)?.json::<Value>().context("The server sent an unexpected response")
    }

    /// Verifies the connection/credentials and returns the user name.
    pub fn current_user(&self) -> Result<String> {
        if matches!(self.auth, Auth::None) {
            self.platforms()?;
            return Ok("guest".into());
        }
        let v = self.get_json("users/me", &[])?;
        Ok(v.get("username").and_then(|u| u.as_str()).unwrap_or("user").to_string())
    }

    pub fn platforms(&self) -> Result<Vec<Platform>> {
        let v = self.get_json("platforms", &[])?;
        let items = match v {
            Value::Array(a) => a,
            Value::Object(mut o) => match o.remove("items") {
                Some(Value::Array(a)) => a,
                _ => bail!("Unexpected platform list format"),
            },
            _ => bail!("Unexpected platform list format"),
        };
        Ok(items.into_iter().filter_map(|x| serde_json::from_value(x).ok()).collect())
    }

    /// Fetches one page of roms. Returns (roms, total, raw item count, paginated?).
    pub fn roms_page(&self, offset: u64, limit: u64) -> Result<(Vec<Rom>, Option<u64>, usize, bool)> {
        let query = [
            ("limit", limit.to_string()),
            ("offset", offset.to_string()),
            ("order_by", "name".to_string()),
            ("order_dir", "asc".to_string()),
            ("with_rom_id_index", "false".to_string()),
            ("with_char_index", "false".to_string()),
            ("with_filter_values", "false".to_string()),
        ];
        let v = self.get_json("roms", &query)?;
        let (items, total, paginated) = match v {
            Value::Array(a) => (a, None, false),
            Value::Object(mut o) => {
                let total = o.get("total").and_then(|t| t.as_u64());
                match o.remove("items") {
                    Some(Value::Array(a)) => (a, total, true),
                    _ => bail!("Unexpected rom list format"),
                }
            }
            _ => bail!("Unexpected rom list format"),
        };
        let raw = items.len();
        let roms = items.into_iter().filter_map(|x| serde_json::from_value::<Rom>(x).ok()).collect();
        Ok((roms, total, raw, paginated))
    }

    pub fn cover_url(&self, rom: &Rom, large: bool) -> Option<String> {
        let p = if large {
            first_nonempty(&[&rom.path_cover_large, &rom.path_cover_small])
        } else {
            first_nonempty(&[&rom.path_cover_small, &rom.path_cover_large])
        };
        if !p.is_empty() {
            return Some(if p.starts_with("http://") || p.starts_with("https://") {
                p.to_string()
            } else if p.starts_with('/') {
                format!("{}{}", self.base, p)
            } else if p.starts_with("assets/") {
                format!("{}/{}", self.base, p)
            } else {
                format!("{}/assets/romm/resources/{}", self.base, p)
            });
        }
        if !rom.url_cover.is_empty() {
            let u = &rom.url_cover;
            return Some(if u.starts_with("//") { format!("https:{u}") } else { u.clone() });
        }
        None
    }

    pub fn rom_web_url(&self, id: i64) -> String {
        format!("{}/rom/{}", self.base, id)
    }

    pub fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let mut rb = self.http.get(url).timeout(Duration::from_secs(30));
        if url.starts_with(&self.base) {
            rb = self.authed(rb);
        }
        let resp = check(rb.send()?)?;
        Ok(resp.bytes()?.to_vec())
    }

    /// Streams the rom content to `dest`. Multi-file roms arrive as a zip.
    pub fn download_rom(&self, rom: &Rom, dest: &Path, progress: &Progress) -> Result<()> {
        let mut url = Url::parse(&format!("{}/api/roms/{}/content", self.base, rom.id))?;
        url.path_segments_mut()
            .map_err(|_| anyhow!("Invalid server URL"))?
            .push(&rom.file_name());

        let resp = self
            .authed(self.http.get(url).timeout(Duration::from_secs(60 * 60 * 24)))
            .send()
            .with_context(|| format!("Could not reach {}", self.base))?;
        let mut resp = check(resp)?;

        let total = resp.content_length().filter(|l| *l > 0).unwrap_or(rom.fs_size_bytes);
        progress.total.store(total, Ordering::Relaxed);
        progress.downloaded.store(0, Ordering::Relaxed);

        let file = File::create(dest).with_context(|| format!("Could not create {}", dest.display()))?;
        let mut out = BufWriter::with_capacity(1 << 20, file);
        let mut buf = vec![0u8; 256 * 1024];
        let result: Result<()> = (|| {
            loop {
                if progress.cancelled() {
                    bail!(CANCELLED);
                }
                let n = resp.read(&mut buf).context("Download interrupted")?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n]).context("Could not write to disk")?;
                progress.downloaded.fetch_add(n as u64, Ordering::Relaxed);
            }
            out.flush()?;
            Ok(())
        })();
        drop(out);
        if result.is_err() {
            let _ = std::fs::remove_file(dest);
        }
        let done = progress.downloaded.load(Ordering::Relaxed);
        if progress.total.load(Ordering::Relaxed) < done {
            progress.total.store(done, Ordering::Relaxed);
        }
        result
    }
}
