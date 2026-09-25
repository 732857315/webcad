//! Platform layer: key-value storage (settings, autosave), file open/save dialogs, downloads.
//!
//! - Native: storage is one file per key in the user config directory; dialogs use `rfd` on a
//!   helper thread; saving writes the file directly.
//! - Web: storage is `localStorage`; opening uses `rfd` (browser file picker); saving triggers a
//!   Blob download (no overlay prompt).
//!
//! Results of asynchronous operations arrive through an [`Inbox`] the app drains every frame.

use std::sync::{Arc, Mutex};

/// Simple persistent string storage.
pub trait KvStore {
    fn get(&self, key: &str) -> Option<String>;
    /// Store `value`; returns `false` when the platform refused (quota, I/O error).
    fn set(&mut self, key: &str, value: &str) -> bool;
    fn remove(&mut self, key: &str);
}

/// In-memory store (tests, or when no persistent storage is available).
#[derive(Default, Debug)]
pub struct MemStore(pub std::collections::HashMap<String, String>);

impl KvStore for MemStore {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
    fn set(&mut self, key: &str, value: &str) -> bool {
        self.0.insert(key.to_owned(), value.to_owned());
        true
    }
    fn remove(&mut self, key: &str) {
        self.0.remove(key);
    }
}

/// Storage for this platform (falls back to memory when unavailable).
pub fn default_store() -> Box<dyn KvStore> {
    #[cfg(target_arch = "wasm32")]
    {
        match web::LocalStore::new() {
            Some(s) => Box::new(s),
            None => Box::new(MemStore::default()),
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        match native::FileStore::new() {
            Some(s) => Box::new(s),
            None => Box::new(MemStore::default()),
        }
    }
}

/// Maximum autosave payload (compressed document) per platform.
pub const AUTOSAVE_MAX_BYTES: usize = if cfg!(target_arch = "wasm32") {
    3 * 1024 * 1024
} else {
    64 * 1024 * 1024
};

/// Why a file was requested.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenPurpose {
    /// Open as the current document (native or DXF/DWG).
    Open,
    /// Import DXF/DWG.
    Import,
}

/// Results of asynchronous platform operations.
#[derive(Debug)]
pub enum PlatformEvent {
    FileOpened {
        name: String,
        bytes: Vec<u8>,
        purpose: OpenPurpose,
        path: Option<std::path::PathBuf>,
    },
    FileSaved {
        name: String,
        path: Option<std::path::PathBuf>,
    },
    Error(String),
}

/// Thread-safe queue of [`PlatformEvent`]s.
#[derive(Clone, Default)]
pub struct Inbox(Arc<Mutex<Vec<PlatformEvent>>>);

impl Inbox {
    pub fn push(&self, e: PlatformEvent) {
        if let Ok(mut v) = self.0.lock() {
            v.push(e);
        }
    }
    pub fn drain(&self) -> Vec<PlatformEvent> {
        self.0
            .lock()
            .map(|mut v| std::mem::take(&mut *v))
            .unwrap_or_default()
    }
}

/// Run a future to completion in the background.
#[cfg(target_arch = "wasm32")]
pub fn spawn(fut: impl std::future::Future<Output = ()> + 'static) {
    wasm_bindgen_futures::spawn_local(fut);
}

/// Run a future to completion in the background.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn(fut: impl std::future::Future<Output = ()> + Send + 'static) {
    std::thread::spawn(move || pollster::block_on(fut));
}

/// Show the open dialog; the file arrives as [`PlatformEvent::FileOpened`].
pub fn open_file(
    inbox: Inbox,
    ctx: egui::Context,
    purpose: OpenPurpose,
    filters: &[(&str, &[&str])],
) {
    let mut dialog = rfd::AsyncFileDialog::new();
    for (name, exts) in filters {
        dialog = dialog.add_filter(*name, exts);
    }
    spawn(async move {
        if let Some(fh) = dialog.pick_file().await {
            let bytes = fh.read().await;
            let name = fh.file_name();
            #[cfg(not(target_arch = "wasm32"))]
            let path = Some(fh.path().to_path_buf());
            #[cfg(target_arch = "wasm32")]
            let path = None;
            inbox.push(PlatformEvent::FileOpened {
                name,
                bytes,
                purpose,
                path,
            });
            ctx.request_repaint();
        }
    });
}

/// Save `bytes`: native shows a save dialog (unless `path` is given), web downloads the file.
pub fn save_file(
    inbox: Inbox,
    ctx: egui::Context,
    name: String,
    bytes: Vec<u8>,
    path: Option<std::path::PathBuf>,
) {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = path;
        match web::download(&name, &bytes) {
            Ok(()) => inbox.push(PlatformEvent::FileSaved { name, path: None }),
            Err(e) => inbox.push(PlatformEvent::Error(e)),
        }
        ctx.request_repaint();
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(p) = path {
            match std::fs::write(&p, &bytes) {
                Ok(()) => inbox.push(PlatformEvent::FileSaved {
                    name,
                    path: Some(p),
                }),
                Err(e) => inbox.push(PlatformEvent::Error(e.to_string())),
            }
            ctx.request_repaint();
            return;
        }
        let ext = name.rsplit('.').next().unwrap_or("").to_owned();
        let dialog = rfd::AsyncFileDialog::new()
            .set_file_name(&name)
            .add_filter(ext.to_uppercase(), &[ext.as_str()]);
        spawn(async move {
            if let Some(fh) = dialog.save_file().await {
                let p = fh.path().to_path_buf();
                let shown = fh.file_name();
                match std::fs::write(&p, &bytes) {
                    Ok(()) => inbox.push(PlatformEvent::FileSaved {
                        name: shown,
                        path: Some(p),
                    }),
                    Err(e) => inbox.push(PlatformEvent::Error(e.to_string())),
                }
                ctx.request_repaint();
            }
        });
    }
}

/// Standard base64 (for binary autosave data in string storage).
pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for c in bytes.chunks(4) {
        let pad = c.iter().rev().take_while(|b| **b == b'=').count();
        if pad > 2 {
            return None;
        }
        let mut n = 0u32;
        for (i, b) in c.iter().enumerate() {
            let v = if i >= 4 - pad { 0 } else { val(*b)? };
            n = n << 6 | v;
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::path::PathBuf;

    use super::KvStore;

    /// One file per key under the user config directory (`%APPDATA%\webcad`, `~/.config/webcad`,
    /// `~/Library/Application Support/webcad`).
    pub struct FileStore {
        dir: PathBuf,
    }

    impl FileStore {
        pub fn new() -> Option<Self> {
            let base = if cfg!(windows) {
                std::env::var_os("APPDATA").map(PathBuf::from)
            } else if cfg!(target_os = "macos") {
                std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join("Library/Application Support"))
            } else {
                std::env::var_os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            }?;
            let dir = base.join(crate::APP_NAME);
            std::fs::create_dir_all(&dir).ok()?;
            Some(Self { dir })
        }

        fn path(&self, key: &str) -> PathBuf {
            let safe: String = key
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            self.dir.join(format!("{safe}.txt"))
        }
    }

    impl KvStore for FileStore {
        fn get(&self, key: &str) -> Option<String> {
            std::fs::read_to_string(self.path(key)).ok()
        }
        fn set(&mut self, key: &str, value: &str) -> bool {
            let p = self.path(key);
            let tmp = p.with_extension("tmp");
            std::fs::write(&tmp, value).is_ok() && std::fs::rename(&tmp, &p).is_ok()
        }
        fn remove(&mut self, key: &str) {
            let _ = std::fs::remove_file(self.path(key));
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use wasm_bindgen::JsCast as _;

    use super::KvStore;

    /// `window.localStorage` with an app prefix.
    pub struct LocalStore {
        storage: web_sys::Storage,
    }

    impl LocalStore {
        pub fn new() -> Option<Self> {
            let storage = web_sys::window()?.local_storage().ok()??;
            Some(Self { storage })
        }
        fn key(k: &str) -> String {
            format!("{}.{k}", crate::APP_NAME)
        }
    }

    impl KvStore for LocalStore {
        fn get(&self, key: &str) -> Option<String> {
            self.storage.get_item(&Self::key(key)).ok().flatten()
        }
        fn set(&mut self, key: &str, value: &str) -> bool {
            self.storage.set_item(&Self::key(key), value).is_ok()
        }
        fn remove(&mut self, key: &str) {
            let _ = self.storage.remove_item(&Self::key(key));
        }
    }

    /// Download `bytes` as `name` via a Blob URL and a temporary `<a download>`.
    pub fn download(name: &str, bytes: &[u8]) -> Result<(), String> {
        let err = |e: wasm_bindgen::JsValue| format!("{e:?}");
        let window = web_sys::window().ok_or("no window")?;
        let document = window.document().ok_or("no document")?;
        let array = js_sys::Uint8Array::from(bytes);
        let parts = js_sys::Array::new();
        parts.push(&array.buffer());
        let opts = web_sys::BlobPropertyBag::new();
        opts.set_type("application/octet-stream");
        let blob = web_sys::Blob::new_with_buffer_source_sequence_and_options(&parts, &opts)
            .map_err(err)?;
        let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(err)?;
        let a = document
            .create_element("a")
            .map_err(err)?
            .dyn_into::<web_sys::HtmlAnchorElement>()
            .map_err(|_| "anchor".to_owned())?;
        a.set_href(&url);
        a.set_download(name);
        a.set_attribute("style", "display:none").ok();
        if let Some(body) = document.body() {
            body.append_child(&a).ok();
        }
        a.click();
        a.remove();
        // Revoking immediately can cancel the download in some browsers; revoke a bit later.
        let cb = wasm_bindgen::closure::Closure::once_into_js(move || {
            let _ = web_sys::Url::revoke_object_url(&url);
        });
        let _ = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(cb.unchecked_ref(), 10_000);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for data in [
            &b""[..],
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"fooba",
            b"foobar",
            &[0u8, 255, 128, 7, 9],
        ] {
            let e = base64_encode(data);
            assert_eq!(base64_decode(&e).as_deref(), Some(data), "{e}");
        }
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_decode("Zm9=v"), None);
        assert_eq!(base64_decode("@@@@"), None);
    }

    #[test]
    fn mem_store() {
        let mut s = MemStore::default();
        assert!(s.set("a", "1"));
        assert_eq!(s.get("a").as_deref(), Some("1"));
        s.remove("a");
        assert_eq!(s.get("a"), None);
    }
}
