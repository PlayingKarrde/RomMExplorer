//! Background cover-art loader with an on-disk cache.
//!
//! Requests go onto a LIFO stack so whatever the user scrolled to most
//! recently loads first.

use crate::api::RommClient;
use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

/// (rom id, large?)
pub type CoverKey = (i64, bool);

enum Slot {
    Loading,
    Ready(TextureHandle),
    Missing,
}

struct Shared {
    queue: Mutex<Vec<(CoverKey, String)>>,
    cv: Condvar,
    client: Mutex<Option<RommClient>>,
}

pub struct CoverLoader {
    shared: Arc<Shared>,
    slots: HashMap<CoverKey, Slot>,
    rx: Receiver<(CoverKey, Option<ColorImage>)>,
}

impl CoverLoader {
    pub fn new(ctx: &egui::Context, threads: usize, cache_dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&cache_dir);
        let shared = Arc::new(Shared { queue: Mutex::new(Vec::new()), cv: Condvar::new(), client: Mutex::new(None) });
        let (tx, rx) = channel();
        for _ in 0..threads {
            let shared = shared.clone();
            let tx: Sender<_> = tx.clone();
            let ctx = ctx.clone();
            let cache_dir = cache_dir.clone();
            std::thread::spawn(move || loop {
                let (key, url) = {
                    let mut q = shared.queue.lock().unwrap();
                    while q.is_empty() {
                        q = shared.cv.wait(q).unwrap();
                    }
                    q.pop().unwrap()
                };
                let client = shared.client.lock().unwrap().clone();
                let img = load(client.as_ref(), &url, key.1, &cache_dir);
                if tx.send((key, img)).is_err() {
                    return;
                }
                ctx.request_repaint();
            });
        }
        Self { shared, slots: HashMap::new(), rx }
    }

    pub fn set_client(&mut self, client: Option<RommClient>) {
        *self.shared.client.lock().unwrap() = client;
        self.shared.queue.lock().unwrap().clear();
        self.slots.clear();
    }

    /// Returns the texture if ready; otherwise queues it (once).
    pub fn get(&mut self, key: CoverKey, url: Option<String>) -> Option<TextureHandle> {
        match self.slots.get(&key) {
            Some(Slot::Ready(t)) => Some(t.clone()),
            Some(_) => None,
            None => {
                match url {
                    Some(u) => {
                        self.shared.queue.lock().unwrap().push((key, u));
                        self.shared.cv.notify_one();
                        self.slots.insert(key, Slot::Loading);
                    }
                    None => {
                        self.slots.insert(key, Slot::Missing);
                    }
                }
                None
            }
        }
    }

    /// Peek without queueing anything.
    pub fn peek(&self, key: CoverKey) -> Option<TextureHandle> {
        match self.slots.get(&key) {
            Some(Slot::Ready(t)) => Some(t.clone()),
            _ => None,
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok((key, img)) = self.rx.try_recv() {
            let slot = match img {
                Some(img) => Slot::Ready(ctx.load_texture(
                    format!("cover-{}-{}", key.0, key.1),
                    img,
                    TextureOptions::LINEAR,
                )),
                None => Slot::Missing,
            };
            self.slots.insert(key, slot);
        }
    }
}

fn cache_path(dir: &Path, url: &str) -> PathBuf {
    let mut h = DefaultHasher::new();
    url.hash(&mut h);
    dir.join(format!("{:016x}.img", h.finish()))
}

fn load(client: Option<&RommClient>, url: &str, large: bool, cache_dir: &Path) -> Option<ColorImage> {
    let path = cache_path(cache_dir, url);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => {
            let b = match client {
                Some(c) => c.fetch_bytes(url).ok()?,
                None => reqwest::blocking::get(url).ok()?.bytes().ok()?.to_vec(),
            };
            let _ = std::fs::write(&path, &b);
            b
        }
    };
    let img = image::load_from_memory(&bytes).ok()?;
    let max = if large { 900 } else { 420 };
    let img = if img.height() > max || img.width() > max {
        img.resize(max, max, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
}
