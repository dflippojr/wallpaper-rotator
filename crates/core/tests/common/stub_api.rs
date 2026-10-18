//! A local stand-in for every photo API the worker talks to, so journeys
//! run without the network. One thread answers on `127.0.0.1:<free port>`
//! with the fixtures in `tests/fixtures/sources/`, a status per route the
//! test can change, and a log of every request.
//!
//! `https://` URLs in the fixtures are served rewritten to point here, with
//! the host kept as the first path segment
//! (`https://images.unsplash.com/photo-1` becomes
//! `http://127.0.0.1:PORT/images.unsplash.com/photo-1`), so recorded real
//! replies can replace the fixtures unchanged.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use image::codecs::jpeg::JpegEncoder;
use image::{Rgb, RgbImage};
use rotator_core::net::Http;

/// The size of images whose URL doesn't ask for one (`w` and `h`).
pub const DEFAULT_IMAGE: (u32, u32) = (400, 300);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Route {
    /// Unsplash `/photos/random`.
    UnsplashRandom,
    /// Unsplash `/topics/{slug}`.
    UnsplashTopic,
    /// Unsplash `/photos/{id}/download`: the download report.
    UnsplashDownload,
    /// Pexels `/v1/search`.
    PexelsSearch,
    /// Pexels `/v1/curated`.
    PexelsCurated,
    /// Wallhaven `/api/v1/search`.
    WallhavenSearch,
    /// Bing `/HPImageArchive.aspx`.
    Bing,
    /// Wikimedia `/feed/v1/wikipedia/en/featured/{y}/{m}/{d}`.
    Wikimedia,
    /// Anything else: an image.
    Image,
}

impl Route {
    fn of(path: &str) -> Route {
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        match parts.as_slice() {
            ["photos", "random"] => Route::UnsplashRandom,
            ["topics", _] => Route::UnsplashTopic,
            ["photos", _, "download"] => Route::UnsplashDownload,
            ["v1", "search"] => Route::PexelsSearch,
            ["v1", "curated"] => Route::PexelsCurated,
            ["api", "v1", "search"] => Route::WallhavenSearch,
            ["HPImageArchive.aspx"] => Route::Bing,
            ["feed", "v1", "wikipedia", "en", "featured", ..] => Route::Wikimedia,
            _ => Route::Image,
        }
    }

    /// The fixture served with a 200.
    fn fixture(self) -> Option<&'static str> {
        Some(match self {
            Route::UnsplashRandom => "unsplash/random.json",
            Route::UnsplashTopic => "unsplash/topic.json",
            Route::UnsplashDownload => "unsplash/download.json",
            Route::PexelsSearch | Route::PexelsCurated => "pexels/search.json",
            Route::WallhavenSearch => "wallhaven/search.json",
            Route::Bing => "daily/bing.json",
            Route::Wikimedia => "daily/wikimedia.json",
            Route::Image => return None,
        })
    }
}

/// One request as the stub saw it.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    /// The real host a rewritten URL pointed at (`images.unsplash.com`),
    /// or `None` for an API base.
    pub host: Option<String>,
    /// The path without the host segment.
    pub path: String,
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub route: Route,
}

impl Request {
    /// A header's value; names match case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// A query parameter's raw value.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v)
    }
}

#[derive(Default)]
struct Shared {
    statuses: HashMap<Route, u16>,
    bodies: HashMap<Route, String>,
    requests: Vec<Request>,
}

pub struct StubApi {
    addr: String,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl StubApi {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the stub API");
        let addr = listener.local_addr().unwrap().to_string();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (shared, stop, addr) = (shared.clone(), stop.clone(), addr.clone());
            thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        // A broken connection only fails the request.
                        let _ = serve(stream, &addr, &shared);
                    }
                }
            })
        };
        Self {
            addr,
            shared,
            stop,
            thread: Some(thread),
        }
    }

    /// `http://127.0.0.1:PORT`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// An `Http` that sends every source here.
    pub fn http(&self) -> Http {
        let bases = ["Unsplash", "Pexels", "Wallhaven", "Bing", "Wikimedia"]
            .into_iter()
            .map(|name| (name, self.url()))
            .collect();
        Http::with_bases(bases)
    }

    /// Answers `route` with `status` (and a small error body) from now on;
    /// 200 restores the fixture.
    pub fn status(&self, route: Route, status: u16) {
        self.shared.lock().unwrap().statuses.insert(route, status);
    }

    /// Answers `route` with `body` instead of its fixture from now on.
    #[allow(dead_code)] // Used by tests/probe.rs, not the worker journeys.
    pub fn reply(&self, route: Route, body: &str) {
        self.shared
            .lock()
            .unwrap()
            .bodies
            .insert(route, body.to_string());
    }

    /// Every request so far, oldest first.
    pub fn requests(&self) -> Vec<Request> {
        self.shared.lock().unwrap().requests.clone()
    }

    /// The requests so far to `route`.
    pub fn to(&self, route: Route) -> Vec<Request> {
        self.requests()
            .into_iter()
            .filter(|r| r.route == route)
            .collect()
    }
}

impl Drop for StubApi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so it sees the flag.
        let _ = TcpStream::connect(&self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Reads one request and answers it, closing the connection after.
fn serve(stream: TcpStream, addr: &str, shared: &Mutex<Shared>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut words = line.split_whitespace();
    let (Some(method), Some(target)) = (words.next(), words.next()) else {
        return Ok(());
    };
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    // A rewritten fixture URL keeps its real host as the first segment.
    let (host, path) = match path.trim_start_matches('/').split_once('/') {
        Some((first, rest)) if first.contains('.') => (Some(first.to_string()), format!("/{rest}")),
        _ => (None, path.to_string()),
    };
    let route = Route::of(&path);
    let request = Request {
        method: method.to_string(),
        host,
        path,
        query: query.to_string(),
        headers,
        route,
    };
    let (status, body) = {
        let mut shared = shared.lock().unwrap();
        shared.requests.push(request.clone());
        (
            shared.statuses.get(&route).copied().unwrap_or(200),
            shared.bodies.get(&route).cloned(),
        )
    };

    let (content_type, body) = if status != 200 {
        (
            "application/json",
            br#"{"errors": ["stub error"]}"#.to_vec(),
        )
    } else if let Some(body) = body {
        ("application/json", body.into_bytes())
    } else if let Some(fixture) = route.fixture() {
        (
            "application/json",
            rewrite(&read_fixture(fixture), addr).into_bytes(),
        )
    } else {
        ("image/jpeg", jpeg(image_size(&request)))
    };
    let mut stream = stream;
    write!(
        stream,
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reason(status),
        body.len()
    )?;
    stream.write_all(&body)?;
    stream.flush()
}

fn read_fixture(name: &str) -> String {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests/fixtures/sources", name]
        .iter()
        .collect();
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Points every `https://` URL in a reply at the stub, plain or with JSON's
/// escaped slashes.
fn rewrite(body: &str, addr: &str) -> String {
    body.replace("https://", &format!("http://{addr}/"))
        .replace(r"https:\/\/", &format!(r"http:\/\/{addr}\/"))
}

fn image_size(request: &Request) -> (u32, u32) {
    let dim = |name| request.param(name).and_then(|v| v.parse::<u32>().ok());
    match (dim("w"), dim("h")) {
        (Some(w), Some(h)) if w > 0 && h > 0 && w <= 8000 && h <= 8000 => (w, h),
        _ => DEFAULT_IMAGE,
    }
}

fn jpeg((width, height): (u32, u32)) -> Vec<u8> {
    let mut out = Vec::new();
    let img = RgbImage::from_pixel(width, height, Rgb([30, 90, 160]));
    JpegEncoder::new_with_quality(&mut out, 80)
        .encode_image(&img)
        .unwrap();
    out
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Status",
    }
}
