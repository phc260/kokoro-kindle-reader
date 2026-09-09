// Loopback HTTP transport for the browser extension — the ONLY one.
//
//   GET  /status -> {"ok":true,"voice":…,"voices":[…],"sampleRate":24000,"ocr":{…}}
//   POST /synth  {"text":…,"voice":…,"speed":…} -> raw little-endian f32 PCM
//   POST /ocr    image/png bytes -> {"version":1,"lines":[{"words":[…]}],…}
//
// WHY THIS ONE. A native-messaging bridge was prototyped ahead of this and rejected. It has
// the better security story on paper: no listening socket, and the browser itself enforces
// which extension id may launch it. What it
// does not have is portability, because that transport IS per-browser configuration — registry
// keys per browser, two manifest dialects, a gecko id and a hashed Chrome id, a BOM trap, a
// mandatory browser restart, and failure modes that all surface as the same one string. Which
// is precisely how it failed here: unexplained, with provably correct registration. HTTP is
// `fetch`: it works in any browser, needs no registration, and can be curl'd, so a broken setup
// can be bisected without the browser in the way.
//
// Keeping both was tried and dropped. Two transports mean every failure gets diagnosed twice,
// and the half that broke is never the half you are looking at.
//
// It also reaches further into the extension. A content script has `fetch` and (outside a
// Chrome service worker) its own AudioContext, so the HTTP path does not structurally require
// the background-script-plus-offscreen-document apparatus that exists purely to work around
// Chrome service worker limits. That is what would make this the viable route to Firefox --
// though the extension does not currently take it: its Firefox build has no narrator wired to
// this endpoint. Chrome and Edge are what is actually supported.
//
// The cost is a PAIRING step, once per browser: without the browser vouching for the client,
// the client has to present something. That something is the token below.
//
// The honest security delta is narrower than "socket vs no socket" suggests: `PIPE_NAME` is
// already openable by any process running as this user — that is why `bench_busy` and
// `MAX_FRAME_SAMPLES` exist — so the local-process threat was already accepted. What a port
// uniquely adds is reachability from WEB PAGES, which is exactly what checks 2 and 3 below
// address. They are NOT optional:
//
//   1. Bind 127.0.0.1 — never 0.0.0.0. Nothing off-machine can reach it.
//   2. Origin allowlist. A web page cannot forge its own Origin, so a random site fetching
//      this endpoint is rejected. This is what stops the drive-by case.
//   3. Bearer token, compared in constant time. A non-browser process sets whatever headers
//      it likes, so Origin alone does not stop other local software.
//   4. Host header check, or DNS rebinding walks straight past (2).
//
// TLS is deliberately absent: browsers already treat 127.0.0.1 as a trustworthy origin, so
// there is no mixed-content problem, and a self-signed cert would add a trust prompt and an
// expiry while defending against nobody — a local attacker can read the cert too.
//
// OCR ARRIVES HERE AND NOWHERE ELSE. `/ocr` is one more route behind the same four checks —
// not a second listener, not a WebSocket, and not a server-to-browser channel. The engine
// itself is `kokoro-ocr` (PP-OCR: a DBNet detector, then a CTC recognizer), which knows
// nothing about HTTP; this file is the only thing that knows both. Note what did NOT move
// with it: the dark-page check, the gutter split, the missed-gutter retry and the whole
// evidence-based furniture policy are still in the extension, because those rules decide what
// gets narrated and swapping the engine underneath them is already the entire change.
//
// The posted image is the page as the reader RENDERED it — original colour, not flattened and
// not inverted. A detector that has to find four words inside an illustration needs contrast a
// flatten throws away, and the backend owns whatever preprocessing its own models want.
//
// UNPACED. The extension schedules every frame onto its own AudioContext cursor and *depends*
// on synthesis outrunning playback to build a lead that hides the next chunk's synthesis; pacing
// would clamp it to ~1.0x and put mid-page silence back. This does NOT go through the pipe at
// all — it calls `NativeSynth::synth` directly, on the same serialized worker Kindle queues
// behind, with no pacing and no gain (a client running seconds ahead would otherwise freeze a
// stale volume into audio nobody has heard yet).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::ctx::{available_voices, CoreCtx};
use crate::native_synth;
use kokoro_protocol::{MAX_TEXT_BYTES, SAMPLE_RATE};

/// Fixed rather than random so a pairing string stays valid across restarts; the token is what
/// authorizes, not obscurity of the port. Matches `DEFAULT_PORT` in the extension's
/// `src/kokoro-http.ts`, which probes here to tell "not running" from "not paired".
pub const DEFAULT_PORT: u16 = 8787;

/// The packed extension id of `kokoro-browser-extension`, derived from the `key` pinned in its
/// `manifest.chrome.json` so an unpacked load and a packed one get the same id. Overridable via
/// `KOKORO_ALLOWED_ORIGINS` (comma separated) for a dev build with its own id.
const DEFAULT_EXTENSION_ID: &str = "acbnkbiijeckelpogcboafgllhccbngm";

/// Caps on a request's headers, so a peer cannot make the host buffer without bound before the
/// token check has had a chance to run. Enforced by READING at most this much (see
/// `read_line_capped`) — checking the length after the read is not a cap at all, since the
/// allocation has already happened by then.
const MAX_HEADERS: usize = 64;
const MAX_HEADER_BYTES: usize = 8 * 1024;

/// How long one connection gets to deliver a complete request head + body.
///
/// Everything above is a SIZE bound; this is the time bound, and both are needed. A peer that
/// declares a body and then drips it a byte a minute never trips a size cap, it just parks the
/// task forever — and these tasks share a Tokio runtime with the named-pipe server that feeds
/// Kindle, so parked connections are not a self-contained problem. Generous enough that a real
/// request can never hit it: the client is on loopback.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Cap on a posted page image, before decoding.
///
/// It is the SAME number `kokoro-ocr`'s limits carry (see `ocr_limits`), because a transport that
/// accepts what the engine will refuse is a 413 delivered a megabyte late.
///
/// **Sized for a picture book, not for prose, and 8 MiB was not.** A column of book text is well
/// under 1 MiB, which is what the first number was reasoned from — and it refused a real Cloud
/// Reader page. The pages that blow a byte cap are full-page colour plates, and they blow it for a
/// reason that is structural rather than marginal: the extension captures at the reader's own
/// render resolution (device-pixel-ratio scaled) and re-encodes to PNG, which is **lossless**, so
/// a painterly page that arrived as a few hundred KiB of JPEG leaves the canvas an order of
/// magnitude larger. PNG-only is not the thing to revisit — one decoder is one parser reachable
/// from a network-facing endpoint — so the cap is what has to fit the format.
///
/// Still an order of magnitude below what `max_pixels` (40 Mpx) would admit at PNG's worst
/// realistic density, so this remains the binding, cheap check it was meant to be.
const MAX_OCR_BODY: usize = 32 * 1024 * 1024;

/// How much of an over-cap body is read and discarded so the 413 can be read (`discard_body`).
///
/// Absolute rather than a multiple of the cap: the point is to bound work done for a request
/// already refused, and pinning it to the cap means every future raise silently doubles the
/// reading too. Comfortably above `MAX_OCR_BODY`, so a client that overshoots by a plate's worth
/// still gets a legible refusal instead of a reset; past it, the reset is the honest answer to a
/// client no browser is. Nothing is allocated either way.
const MAX_DISCARD_BYTES: usize = 64 * 1024 * 1024;

/// The only image type `/ocr` accepts.
///
/// One, deliberately. The extension's `convertToBlob` emits PNG and nothing else, and every
/// additional decoder is another parser reachable from a network-facing endpoint by anyone
/// holding the pairing token. The crate is compiled with only this codec, so the allowlist and
/// what can actually be decoded are the same fact stated twice.
const OCR_CONTENT_TYPE: &str = "image/png";

/// How long a whole `/ocr` request gets, queue time included.
///
/// `Limits::deadline` bounds the recognition itself; this bounds the wait. They are different
/// failures — a page with forty lines on it versus a page that is fourth in line behind three
/// others — and only the second one is fixed by asking again later. Generous, because the
/// honest answer to a slow page is the page, not a 504.
const OCR_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Endpoint {
    pub port: u16,
    pub token: String,
    pub allowed_origins: Vec<String>,
}

impl Endpoint {
    /// `kwr_<port>_<token>` — one opaque string is easier to paste correctly than two fields,
    /// and it is the format `parsePairing` in the extension already accepts.
    pub fn pairing_string(&self) -> String {
        format!("kwr_{}_{}", self.port, self.token)
    }

    /// Read the persisted endpoint, creating it on first run. Persisted rather than regenerated
    /// so pairing survives a restart — a token that changed every boot would force the user to
    /// re-pair daily. Lives beside controls.json; on Windows the profile ACL already restricts
    /// that directory to this user.
    pub fn load_or_create(app_data: &Path) -> Result<Endpoint, String> {
        let path = endpoint_path(app_data);
        let allowed_origins = allowed_origins();

        if let Ok(txt) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                if let Some(tok) = v.get("token").and_then(|t| t.as_str()) {
                    // try_from, not `as`: `as u16` turns a hand-edited 65536 into 0, which binds
                    // an ephemeral port while the pairing string still says `kwr_0_...` — the
                    // extension then cannot connect and nothing says why. Out of range falls
                    // back to the default, which at least matches what the extension probes.
                    let port = v
                        .get("port")
                        .and_then(|p| p.as_u64())
                        .and_then(|p| u16::try_from(p).ok())
                        .filter(|p| *p != 0)
                        .unwrap_or(DEFAULT_PORT);
                    return Ok(Endpoint { port, token: tok.to_string(), allowed_origins });
                }
            }
        }

        let ep = Endpoint { port: DEFAULT_PORT, token: random_token()?, allowed_origins };
        std::fs::create_dir_all(app_data).map_err(|e| e.to_string())?;
        // `pairing` is written for the human: the tray's "Web pairing code" item opens this
        // file, and a release host is a windows-subsystem exe with no console to print it to.
        // The daemon itself only ever reads `port` and `token`.
        let body = format!(
            "{{\n  \"port\": {},\n  \"token\": \"{}\",\n  \"pairing\": \"{}\"\n}}\n",
            ep.port,
            ep.token,
            ep.pairing_string()
        );
        std::fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(ep)
    }
}

/// Where the port + token live, beside controls.json.
pub fn endpoint_path(app_data: &Path) -> PathBuf {
    app_data.join("web-endpoint.json")
}

/// 256 bits from the OS CSPRNG, hex encoded. Never a PRNG seeded from the clock: this value is
/// the only thing standing between another local process and a synthesis oracle.
fn random_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("getrandom failed: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn allowed_origins() -> Vec<String> {
    let mut origins: Vec<String> = std::env::var("KOKORO_ALLOWED_ORIGINS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    origins.push(format!("chrome-extension://{DEFAULT_EXTENSION_ID}"));
    origins
}

struct Request {
    method: String,
    path: String,
    origin: Option<String>,
    host: Option<String>,
    auth: Option<String>,
    content_type: Option<String>,
    /// What the peer says it is about to send. Read from the head and acted on only after the
    /// four checks pass — see `serve_conn`.
    len: usize,
    /// Empty until `read_body` fills it, which happens after authentication and never for an
    /// over-cap request.
    body: Vec<u8>,
    /// The peer declared a `Content-Length` over this route's cap. Carried as a flag rather than
    /// as a short read because truncating a PNG and then decoding it reports "not a PNG", which
    /// is a lie about what went wrong.
    oversized: bool,
}

impl Request {
    /// The media type without its parameters, lowercased. `image/png; charset=binary` is a
    /// thing browsers have been known to send.
    fn media_type(&self) -> String {
        self.content_type
            .as_deref()
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
    }
}

/// The body cap for one route.
///
/// Per route, because the two are different by orders of magnitude and one bound cannot be
/// right for both: `/synth` takes a chunk of text, `/ocr` takes a page image. Picked from the
/// request line, which is parsed before any header, so the cap is known before a single body
/// byte is accepted.
fn body_cap(path: &str) -> usize {
    match path.split('?').next().unwrap_or("") {
        "/ocr" => MAX_OCR_BODY,
        _ => MAX_TEXT_BYTES as usize,
    }
}

/// Constant-time in the token's CONTENTS: the fold always visits every byte, so no early exit
/// leaks where two tokens first differ.
///
/// It is not constant-time in LENGTH — a wrong-length guess returns immediately. That is
/// deliberate and harmless here: the token is always 64 hex characters (`random_token`), so the
/// length carries no secret. Don't restate this as "length-independent"; it isn't.
fn secret_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Read one CRLF-terminated line, reading AT MOST `MAX_HEADER_BYTES`.
///
/// The bound has to be on the read itself. Calling `read_line` and then checking `.len()`
/// afterwards bounds nothing — an unterminated line grows the `String` until the peer stops or
/// the host is out of memory, and the check never runs. `take` makes the reader itself refuse to
/// supply more, so an over-long line comes back truncated (no trailing newline) and is rejected.
///
/// Returns None on EOF or a line that hit the cap.
async fn read_line_capped(reader: &mut BufReader<TcpStream>, out: &mut String) -> Option<()> {
    let n = reader
        .take(MAX_HEADER_BYTES as u64)
        .read_line(out)
        .await
        .ok()?;
    if n == 0 || !out.ends_with('\n') {
        return None;
    }
    Some(())
}

/// Read and throw away an over-cap body, so the 413 can actually be delivered.
///
/// Called ONLY after the token check has passed (`serve_conn`). That ordering is what keeps this
/// from being a gift to an unauthenticated peer: reading 64 MiB on behalf of a request that has
/// already been refused is affordable for a client holding the pairing token — that peer was
/// inside the accepted threat model before any of this existed — and is not something a stranger
/// should be able to ask for.
///
/// Bounded twice over: by `MAX_DISCARD_BYTES` here, and in time by the connection deadline the
/// caller passes. A peer declaring more than the bound gets the connection reset — the honest
/// answer to a client no legitimate browser is, and the only case left where an over-cap POST
/// fails without a status.
///
/// It reads rather than allocating: `take` bounds the reader itself, and `sink` keeps nothing.
async fn discard_body(reader: &mut BufReader<TcpStream>, len: usize) {
    let want = len.min(MAX_DISCARD_BYTES) as u64;
    let _ = tokio::io::copy(&mut (&mut *reader).take(want), &mut tokio::io::sink()).await;
}

/// Answer an over-cap request: drain first, THEN reply.
///
/// One function rather than two statements at the call site, because the order is the whole fix
/// and a 413 written without the drain is invisible to a browser. Nothing else writes the
/// TRANSPORT's 413 — the one for a declared over-cap `Content-Length` — so that ordering cannot be
/// skipped by adding a branch elsewhere, which is as close to a guarantee as this gets: driving
/// `serve_conn` from a test would mean standing up a `WebCtx` — a live `NativeSynth` worker, and
/// an OCR worker beside it.
/// (`ocr_status_line` also answers 413, for a decoded image over `max_pixels`/`max_dimension`.
/// That one needs no drain: its body was read in full before the engine ever saw it.)
///
/// Called only after the token check. See the ordering note in `serve_conn`.
///
/// **The drain gets its own budget, not the connection deadline.** Sharing it re-created the exact
/// bug this exists to fix: an upload still running when the 15 s expired had its drain cancelled
/// and the 413 written underneath it anyway, which is the undeliverable refusal again — reachable
/// by nothing more exotic than a large page on a busy machine. A fresh window is affordable here
/// and nowhere else, because this runs only after the token check: the peer holds the pairing
/// token, and `MAX_DISCARD_BYTES` still bounds the bytes even when the clock does not.
async fn refuse_oversized(
    stream: &mut BufReader<TcpStream>,
    req: &Request,
    cors: &str,
) {
    let _ = tokio::time::timeout(REQUEST_TIMEOUT, discard_body(stream, req.len)).await;
    let body = format!(
        "{{\"ok\":false,\"code\":\"too_large\",\"error\":\"body over the {} byte limit for {}\"}}",
        body_cap(&req.path),
        req.path
    );
    respond(stream, "413 Payload Too Large", cors, "application/json", body.as_bytes()).await;
}

/// Allocate and read exactly the body the head declared.
///
/// Separate from `read_head` because WHEN it runs is a security property, not a detail: this is
/// the only allocation in the request path whose size a peer chooses, and it must not happen for
/// a peer that has not authenticated. See `serve_conn`.
async fn read_body(reader: &mut BufReader<TcpStream>, len: usize) -> Option<Vec<u8>> {
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await.ok()?;
    Some(body)
}

/// Read the request line and headers, and NOTHING of the body.
///
/// Stopping here is the point. The four checks all read from the head, so parsing the head is the
/// least a request can be understood by — and every byte read past it is work done for a peer
/// that has not yet proved it may ask for any.
async fn read_head(reader: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut line = String::new();
    read_line_capped(reader, &mut line).await?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let cap = body_cap(&path);
    let (mut origin, mut host, mut auth, mut ctype, mut len) = (None, None, None, None, 0usize);
    for _ in 0..MAX_HEADERS {
        let mut h = String::new();
        read_line_capped(reader, &mut h).await?;
        let h = h.trim_end();
        if h.is_empty() {
            // The blank line ends the head. The body stays on the socket: what happens to it is
            // `serve_conn`'s to decide, once it knows whether this peer is allowed to ask.
            return Some(Request {
                method,
                path,
                origin,
                host,
                auth,
                content_type: ctype,
                len,
                body: Vec::new(),
                oversized: len > cap,
            });
        }
        let (name, value) = h.split_once(':')?;
        let value = value.trim().to_string();
        match name.to_ascii_lowercase().as_str() {
            "origin" => origin = Some(value),
            "host" => host = Some(value),
            "authorization" => auth = Some(value),
            "content-type" => ctype = Some(value),
            "content-length" => len = value.parse().unwrap_or(0),
            _ => {}
        }
    }
    None // more headers than any real client sends
}

fn cors_headers(origin: Option<&str>, allowed: &[String]) -> String {
    match origin {
        Some(o) if allowed.iter().any(|a| a == o) => format!(
            "Access-Control-Allow-Origin: {o}\r\n\
             Access-Control-Allow-Headers: authorization, content-type\r\n\
             Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
             Access-Control-Allow-Private-Network: true\r\n\
             Access-Control-Max-Age: 600\r\n\
             Vary: Origin\r\n"
        ),
        // Never echo an origin we do not trust, and never use `*`.
        _ => String::new(),
    }
}

/// Write one response; the caller drops the socket. `Connection: close` is explicit so a
/// browser does not hold a keep-alive socket this server will never read from again.
async fn respond(
    stream: &mut BufReader<TcpStream>,
    status: &str,
    extra: &str,
    ctype: &str,
    body: &[u8],
) {
    let head = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {ctype}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         {extra}\r\n",
        body.len()
    );
    let s = stream.get_mut();
    let _ = s.write_all(head.as_bytes()).await;
    let _ = s.write_all(body).await;
    let _ = s.flush().await;
}

/// Everything a connection needs: the shared synth context, this endpoint's port/token, and
/// the OCR worker.
///
/// It holds [`CoreCtx`] directly and nothing Kindle-shaped. That is the point of the split:
/// this used to hold the whole pipe context, so serving a page image over HTTP dragged in
/// `KindleCtl` — a UI Automation thread for a reader the browser client does not use and a
/// non-Windows host will not have. What it shares with the pipe it shares deliberately:
/// both transports answer `/status` from the same voice list and stamp the same "audio just
/// went out" clock, so a second copy could only drift.
///
/// OCR hangs off the WEB context rather than off [`CoreCtx`], because the browser is the only
/// client that has a page image to recognize. The pipe has no OCR command and is not getting
/// one: Kindle for PC narrates from its own text, and a second caller would put the whole
/// picture path in front of a release that is only meant to replace the browser's engine.
#[derive(Clone)]
pub struct WebCtx {
    pub core: CoreCtx,
    pub endpoint: Arc<Endpoint>,
    /// One worker for the process, shared by every connection. Started at construction; no
    /// model is loaded until a page actually arrives.
    pub ocr: Arc<kokoro_ocr::Ocr>,
}

impl WebCtx {
    pub fn new(core: CoreCtx, endpoint: Arc<Endpoint>, app_data: &Path) -> WebCtx {
        let assets = ocr_assets(app_data);
        eprintln!("[host] OCR models = {}", assets.dir.display());
        WebCtx { core, endpoint, ocr: Arc::new(kokoro_ocr::Ocr::new(assets, ocr_limits())) }
    }
}

/// The OCR model directory name, `ocr\` under the app-data dir. Not `models\`:
/// `model_base` already means the Kokoro voice model. The two models and the dictionary are
/// **downloaded at first run** by the panel into `<app_data>/ocr/` (like the voice model, and
/// no longer bundled in the installer), and pinned by digest in `kokoro-ocr`.
const OCR_DIR: &str = "ocr";

/// Where the two models and the dictionary live: `<app_data>/ocr/` — the same location the
/// panel downloads them into (`kokoro-panel::download::ocr_dir`), so the two ends agree. The
/// dev fallback is the provisioned tree, so a `cargo run` works without downloading — and it
/// is `debug_assertions`-gated, because a release build that silently read a developer's
/// `native-deps` would hide exactly the "models weren't downloaded" bug this path exposes.
fn ocr_assets(app_data: &Path) -> kokoro_ocr::Assets {
    let downloaded = app_data.join(OCR_DIR);
    if downloaded.exists() {
        return kokoro_ocr::Assets::new(downloaded);
    }
    #[cfg(debug_assertions)]
    {
        let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("native-deps")
            .join(OCR_DIR);
        if dev.exists() {
            return kokoro_ocr::Assets::new(dev);
        }
    }
    // Nothing found. Returning the expected location rather than erroring is what lets
    // `/status` say `missing` with a path in it, instead of the endpoint refusing to start —
    // and `missing` is the correct state until the panel's first-run download lands.
    kokoro_ocr::Assets::new(downloaded)
}

/// The request bounds. `max_body_bytes` is deliberately the same constant the transport
/// enforces above — one number, checked at both ends of the same hop.
fn ocr_limits() -> kokoro_ocr::Limits {
    kokoro_ocr::Limits { max_body_bytes: MAX_OCR_BODY, ..kokoro_ocr::Limits::default() }
}

/// Serve until a fatal listener error. Returns immediately if the port is already taken (a
/// second host instance, or the sibling `kokoro-web-host`); the caller logs it and carries on,
/// because Kindle does not depend on this transport.
pub async fn serve_loop(web: WebCtx) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", web.endpoint.port)).await?;
    eprintln!(
        "[host] web endpoint on http://127.0.0.1:{} (origins: {})",
        web.endpoint.port,
        web.endpoint.allowed_origins.join(", ")
    );
    eprintln!("[host] pair with: {}", web.endpoint.pairing_string());

    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let web = web.clone();
        tokio::spawn(async move {
            let _ = serve_conn(stream, web).await;
        });
    }
}

async fn serve_conn(stream: TcpStream, web: WebCtx) -> std::io::Result<()> {
    let mut stream = BufReader::new(stream);
    // ONE deadline for the whole connection, head and body together. Taken once and passed to
    // each read as an absolute instant rather than re-armed per stage: two 15 s timeouts in
    // sequence is a 30 s budget, and the point of the bound is that a peer cannot park a task on
    // the runtime the pipe server — the one feeding Kindle audio — shares. Dropping the
    // connection on expiry is the right answer: nothing has authenticated, so there is no one to
    // apologize to.
    let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
    let mut req = match tokio::time::timeout_at(deadline, read_head(&mut stream)).await {
        Ok(Some(req)) => req,
        _ => return Ok(()),
    };

    let ep = &web.endpoint;
    let cors = cors_headers(req.origin.as_deref(), &ep.allowed_origins);

    // Preflight, before auth: the browser sends no Authorization on OPTIONS.
    if req.method == "OPTIONS" {
        respond(&mut stream, "204 No Content", &cors, "text/plain", b"").await;
        return Ok(());
    }

    // (4) rebinding guard: only names that resolve to this machine by definition.
    let host_ok = req
        .host
        .as_deref()
        .map(|h| {
            let h = h.rsplit_once(':').map(|(a, _)| a).unwrap_or(h);
            h == "127.0.0.1" || h == "localhost" || h == "[::1]"
        })
        .unwrap_or(false);
    if !host_ok {
        respond(&mut stream, "421 Misdirected Request", "", "text/plain", b"bad Host\n").await;
        return Ok(());
    }

    // (2) origin allowlist. `cors` is empty exactly when the origin was not allowed. A request
    // with no Origin at all is a non-browser client; the token still gates it.
    if req.origin.is_some() && cors.is_empty() {
        respond(&mut stream, "403 Forbidden", "", "text/plain", b"origin not allowed\n").await;
        return Ok(());
    }

    // (3) bearer token
    let presented = req.auth.as_deref().and_then(|a| a.strip_prefix("Bearer ")).unwrap_or("");
    if !secret_eq(presented, &ep.token) {
        respond(&mut stream, "401 Unauthorized", &cors, "text/plain", b"bad token\n").await;
        return Ok(());
    }

    // ---- authenticated from here, and ONLY from here is a byte of body read ----
    //
    // The order is the security property. `read_head` stops at the blank line, so an
    // unauthenticated peer cannot make this process allocate the buffer it named in
    // `Content-Length` (up to `MAX_OCR_BODY`, held for the whole deadline while `read_exact`
    // waits for bytes that need never come), and cannot make it copy an over-cap body either.
    // Neither is bounded by anything else: `serve_loop` spawns one task per accepted socket with
    // no cap on how many. Past this line the peer holds the pairing token — which is the point at
    // which it was already inside the accepted threat model, the same as any process that can
    // open `PIPE_NAME`.
    //
    // The cost of putting the checks first: a request with a BAD token and a large body has its
    // 401 written under an upload still in flight, so the write may fail and the reply be lost —
    // the same shape as the 413 bug this ordering's sibling fix cured. Accepted deliberately.
    // `connectKokoro` handshakes on `/status`, which has no body at all, so a stale token is
    // surfaced there long before anything posts an image; and any body inside a socket buffer
    // (every `/synth` request) completes its write and reads the 401 normally.

    // Declared longer than its route allows. Answered here rather than per route: the reason is
    // the same one for all of them and the connection is done.
    if req.oversized {
        refuse_oversized(&mut stream, &req, &cors).await;
        return Ok(());
    }

    if req.len > 0 {
        match tokio::time::timeout_at(deadline, read_body(&mut stream, req.len)).await {
            Ok(Some(body)) => req.body = body,
            _ => return Ok(()),
        }
    }

    match (req.method.as_str(), req.path.split('?').next().unwrap_or("")) {
        ("GET", "/status") => {
            let (voice, _c) = native_synth::read_controls(&web.core.app_data);
            // `probe` reads the file system and hashes what it finds; it does NOT build a
            // session, which is what makes it safe to answer a polled endpoint with.
            let ocr = kokoro_ocr::probe(web.ocr.assets());
            let body = serde_json::json!({
                "ok": true,
                "voice": voice,
                "voices": available_voices(&web.core.model_base),
                "sampleRate": SAMPLE_RATE,
                "ocr": {
                    "state": ocr.state.as_str(),
                    "engine": ocr.engine,
                    // Both halves, because "pp-ocr" alone is two independently pinned models
                    // and a capture has to be traceable to what actually read it.
                    "detector": ocr.detector,
                    "recognizer": ocr.recognizer,
                    "language": ocr.language,
                    "provider": ocr.provider,
                    // Only ever present when the state is not `ready`. The extension shows
                    // it: "OCR unavailable" with no path in it is the message that gets
                    // reported as a bug against the wrong component.
                    "detail": ocr.detail,
                },
            })
            .to_string();
            respond(&mut stream, "200 OK", &cors, "application/json", body.as_bytes()).await;
        }

        ("POST", "/ocr") => {
            if req.media_type() != OCR_CONTENT_TYPE {
                let body = format!(
                    "{{\"ok\":false,\"code\":\"unsupported_media_type\",\
                      \"error\":\"send {OCR_CONTENT_TYPE}, not '{}'\"}}",
                    req.media_type()
                );
                respond(
                    &mut stream,
                    "415 Unsupported Media Type",
                    &cors,
                    "application/json",
                    body.as_bytes(),
                )
                .await;
                return Ok(());
            }

            match run_ocr(&mut stream, &web, req.body).await {
                Ok(Some(body)) => {
                    respond(&mut stream, "200 OK", &cors, "application/json", body.as_bytes()).await
                }
                // The peer went away mid-recognition. There is nobody to answer and the job
                // has been told to stop; anything it still produces is discarded.
                Ok(None) => return Ok(()),
                Err(e) => {
                    let body = serde_json::json!({
                        "ok": false,
                        "code": e.code(),
                        "error": e.to_string(),
                    })
                    .to_string();
                    respond(&mut stream, ocr_status_line(&e), &cors, "application/json", body.as_bytes())
                        .await;
                }
            }
        }

        ("POST", "/synth") => {
            let v: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string();
            // Narrator: the extension's own picker wins; controls.json is the default, so the
            // panel's narrator is what an unset request gets. Deliberately unlike CMD_SYNTH,
            // where the host owns the narrator because it owns Kindle's settings.
            let (default_voice, controls) = native_synth::read_controls(&web.core.app_data);
            let voice = v
                .get("voice")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or(default_voice);
            // Speed is the client's alone, NOT multiplied by controls.json `speed` — the
            // extension has its own control and folding both in would apply it twice. Gain is
            // likewise not baked in: this hands back a whole chunk at once, so a gain frozen
            // into it could not respond to a slider anyway (the pipe re-reads gain per
            // sub-frame precisely because it streams). The execution provider IS read live —
            // that is a machine setting, and the worker rebuilds its session on a mismatch.
            let speed = v.get("speed").and_then(|x| x.as_f64()).unwrap_or(1.0) as f32;

            if text.is_empty() || text.len() > MAX_TEXT_BYTES as usize {
                let body = b"{\"ok\":false,\"error\":\"empty or oversized text\"}";
                respond(&mut stream, "400 Bad Request", &cors, "application/json", body).await;
                return Ok(());
            }

            match web.core.native.synth(text, speed, voice, controls.engine).await {
                // `.pcm` only: the browser highlights on its own estimated boundaries
                // (`word-timing.ts`), and handing it real marks means a response shape that
                // carries both — a change on the extension side too. The marks exist and are
                // the better source; wiring them across is the browser path's own increment.
                Some(out) => {
                    let pcm = out.pcm;
                    // Stamp the shared "audio just went out" clock so the panel's CMD_STATUS
                    // sees browser narration too and will not start a bench underneath it.
                    // Not the *Kindle* clock: the browser is a third source, and conflating
                    // it would have the panel report Kindle as reading a page it isn't on.
                    web.core.state.stamp_audio();
                    let extra = format!(
                        "{cors}X-Sample-Rate: {SAMPLE_RATE}\r\nX-Samples: {}\r\n",
                        pcm.len() / 4
                    );
                    respond(&mut stream, "200 OK", &extra, "application/octet-stream", &pcm).await;
                }
                None => {
                    let body = b"{\"ok\":false,\"error\":\"synthesis failed\"}";
                    respond(&mut stream, "500 Internal Server Error", &cors, "application/json", body)
                        .await;
                }
            }
        }

        _ => respond(&mut stream, "404 Not Found", &cors, "text/plain", b"no such endpoint\n").await,
    }
    Ok(())
}

// ------------------------------------------------------------------------------------ OCR

/// Recognize one posted column. `Ok(None)` means the peer went away and there is nobody to
/// answer.
///
/// Three things can end this and they are deliberately three separate arms:
///
///   * the worker answers — the ordinary case;
///   * the browser aborts (`AbortController`, a closed tab, a Stop), which shows up as the
///     socket reaching EOF. The job is told to stop, and whatever it produces anyway is
///     dropped. ORT offers no way to abandon a run in progress, so this is a discard contract
///     rather than a stop button — safe here only because the cross-page furniture memory
///     lives in the extension, so a late result has nothing left to poison;
///   * the request deadline, which covers QUEUE TIME as well as recognition. That is the case
///     `Limits::deadline` cannot see: a page fourth in line behind three others is not a page
///     the models are struggling with, and only one of those is worth asking about again
///     later.
async fn run_ocr(
    stream: &mut BufReader<TcpStream>,
    web: &WebCtx,
    body: Vec<u8>,
) -> Result<Option<String>, kokoro_ocr::Error> {
    let handle = web.ocr.submit(body)?;
    let cancel = handle.cancel_handle();

    // `wait` blocks, and recognition is hundreds of milliseconds of CPU. This runtime also
    // carries the named pipe that feeds Kindle's audio, so it does not get to run here.
    let job = tokio::task::spawn_blocking(move || handle.wait());
    tokio::pin!(job);

    let finished = tokio::select! {
        joined = &mut job => Some(joined),
        _ = peer_gone(stream) => None,
        _ = tokio::time::sleep(OCR_REQUEST_TIMEOUT) => Some(Ok(Err(kokoro_ocr::Error::Timeout))),
    };

    let Some(joined) = finished else {
        cancel.cancel();
        return Ok(None);
    };

    match joined {
        Ok(Ok(page)) => Ok(Some(ocr_json(&page))),
        Ok(Err(e)) => {
            // A no-op unless this was the deadline arm, where the job is still running.
            cancel.cancel();
            Err(e)
        }
        // A panic on the blocking pool leaves nothing to report but the fact of it. Same
        // class as a worker that stopped: nothing arriving later can help.
        Err(_) => Err(kokoro_ocr::Error::Unavailable("the OCR task did not finish".into())),
    }
}

/// Resolves when the peer closes the connection (or resets it). Never resolves while the
/// browser is still waiting for its answer.
///
/// The request body has already been consumed, so anything further on this socket is either
/// EOF — which is what an aborted `fetch` looks like — or a pipelined request this server has
/// no intention of serving (`Connection: close` is on every response).
async fn peer_gone(stream: &mut BufReader<TcpStream>) {
    let mut sink = [0u8; 256];
    loop {
        match stream.read(&mut sink).await {
            Ok(0) | Err(_) => return,
            Ok(_) => continue,
        }
    }
}

/// One failure, one status code. The `code` field in the body is what a client branches on;
/// this is what a `curl` shows without reading the body at all.
fn ocr_status_line(e: &kokoro_ocr::Error) -> &'static str {
    match e {
        kokoro_ocr::Error::Decode(_) => "400 Bad Request",
        kokoro_ocr::Error::TooLarge(_) => "413 Payload Too Large",
        kokoro_ocr::Error::Busy => "429 Too Many Requests",
        kokoro_ocr::Error::Unavailable(_) => "503 Service Unavailable",
        kokoro_ocr::Error::Timeout => "504 Gateway Timeout",
        // nginx's convention for "the client asked and then left". Only reachable when
        // something other than the socket cancelled the job, which in practice means it was
        // already cancelled when the worker picked it up.
        kokoro_ocr::Error::Cancelled => "499 Client Closed Request",
        kokoro_ocr::Error::Recognize(_) => "500 Internal Server Error",
    }
}

/// The version-1 response. Frozen before the extension adapter was written — this shape is a
/// contract two codebases share, and `version` is in it so a mismatch is legible rather than
/// arriving as a missing field.
///
/// Rectangles are in the coordinate space of the image that was POSTED, whatever the backend
/// resized to internally — the detector runs on a downscaled page and the recognizer on a
/// 48 px line crop, and neither of those coordinate spaces ever leaves the crate. The
/// extension adds its column x-offset to these exactly once.
fn ocr_json(page: &kokoro_ocr::Page) -> String {
    let lines: Vec<serde_json::Value> = page
        .lines
        .iter()
        .map(|line| {
            let words: Vec<serde_json::Value> = line
                .words
                .iter()
                .map(|w| {
                    serde_json::json!({
                        "text": w.text,
                        "confidence": w.confidence,
                        "bbox": {
                            "x0": w.bbox.x0,
                            "y0": w.bbox.y0,
                            "x1": w.bbox.x1,
                            "y1": w.bbox.y1,
                        },
                    })
                })
                .collect();
            serde_json::json!({ "words": words })
        })
        .collect();

    serde_json::json!({
        "version": kokoro_ocr::RESPONSE_VERSION,
        "engine": kokoro_ocr::ENGINE_NAME,
        "detector": kokoro_ocr::DETECTOR_NAME,
        "recognizer": kokoro_ocr::RECOGNIZER_NAME,
        "width": page.width,
        "height": page.height,
        "lines": lines,
        // Both stages, separately. A page that is slow because forty lines were found is a
        // different fact from one that is slow because the detector is grinding, and the two
        // are indistinguishable in a single total.
        "detectMs": page.detect_ms,
        "recognizeMs": page.recognize_ms,
        "ocrMs": page.ocr_ms,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> kokoro_ocr::Page {
        kokoro_ocr::Page {
            width: 1194,
            height: 1681,
            lines: vec![kokoro_ocr::Line {
                words: vec![kokoro_ocr::Word {
                    text: "Example".into(),
                    confidence: 96.5,
                    bbox: kokoro_ocr::Rect { x0: 80, y0: 120, x1: 176, y1: 148 },
                }],
            }],
            detect_ms: 137.8,
            recognize_ms: 615.6,
            ocr_ms: 740.1,
        }
    }

    #[test]
    fn the_response_shape_is_the_frozen_one() {
        let v: serde_json::Value = serde_json::from_str(&ocr_json(&page())).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["engine"], "pp-ocr");
        // Named separately: two models are pinned, and either can move without the other.
        assert_eq!(v["detector"], "en-PP-OCRv3-det");
        assert_eq!(v["recognizer"], "en-PP-OCRv5-mobile-rec");
        assert_eq!(v["width"], 1194);
        assert_eq!(v["height"], 1681);
        let word = &v["lines"][0]["words"][0];
        assert_eq!(word["text"], "Example");
        assert_eq!(word["bbox"]["x0"], 80);
        assert_eq!(word["bbox"]["y1"], 148);
        assert!(v["detectMs"].as_f64().unwrap() > 0.0);
        assert!(v["recognizeMs"].as_f64().unwrap() > 0.0);
        assert!(v["ocrMs"].as_f64().unwrap() >= v["recognizeMs"].as_f64().unwrap());
    }

    #[test]
    fn a_blank_column_is_an_empty_line_list_not_an_error() {
        let mut p = page();
        p.lines.clear();
        let v: serde_json::Value = serde_json::from_str(&ocr_json(&p)).unwrap();
        assert_eq!(v["lines"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn the_ocr_body_cap_is_the_one_the_engine_enforces() {
        // Two places state this bound; a transport that accepts what the engine refuses is a
        // 413 delivered a megabyte late.
        assert_eq!(body_cap("/ocr"), ocr_limits().max_body_bytes);
        assert_eq!(body_cap("/ocr?x=1"), MAX_OCR_BODY);
    }

    #[test]
    fn synth_keeps_its_own_much_smaller_cap() {
        assert_eq!(body_cap("/synth"), MAX_TEXT_BYTES as usize);
        assert_eq!(body_cap("/status"), MAX_TEXT_BYTES as usize);
    }

    #[test]
    fn media_type_ignores_parameters_and_case() {
        let req = |ct: Option<&str>| Request {
            method: "POST".into(),
            path: "/ocr".into(),
            origin: None,
            host: None,
            auth: None,
            content_type: ct.map(str::to_string),
            len: 0,
            body: Vec::new(),
            oversized: false,
        };
        assert_eq!(req(Some("Image/PNG; charset=binary")).media_type(), OCR_CONTENT_TYPE);
        assert_eq!(req(Some(" image/png ")).media_type(), OCR_CONTENT_TYPE);
        assert_ne!(req(Some("image/jpeg")).media_type(), OCR_CONTENT_TYPE);
        assert_ne!(req(None).media_type(), OCR_CONTENT_TYPE);
    }

    #[test]
    fn every_failure_has_its_own_status() {
        use kokoro_ocr::Error::*;
        let codes = [
            ocr_status_line(&Decode(String::new())),
            ocr_status_line(&TooLarge(String::new())),
            ocr_status_line(&Busy),
            ocr_status_line(&Unavailable(String::new())),
            ocr_status_line(&Timeout),
            ocr_status_line(&Recognize(String::new())),
        ];
        let mut seen = codes.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), codes.len(), "two failures share a status: {codes:?}");
    }

    #[test]
    fn the_token_comparison_visits_every_byte() {
        assert!(secret_eq("abc", "abc"));
        assert!(!secret_eq("abc", "abd"));
        assert!(!secret_eq("abc", "abcd"));
        assert!(!secret_eq("", "a"));
    }

    /// The 413 has to survive the wire, not just be constructed.
    ///
    /// This is the one thing the unit tests above could not see and the thing that was wrong: the
    /// refusal was correct, complete and unreadable, because answering without reading the body
    /// closed the socket under a client still uploading. `Expect: 100-continue` is what hid it —
    /// curl sends it and read its 413 cleanly, a browser `fetch` sends none and reported
    /// `TypeError: Failed to fetch`. So this test speaks the browser's dialect deliberately.
    ///
    /// **What it does NOT cover:** that `serve_conn` calls `refuse_oversized` at all, and that it
    /// does so after the token check. Driving `serve_conn` means building a `WebCtx` — a live
    /// `NativeSynth` worker and an OCR worker — which is not a unit test. (Narrowing `WebCtx` off
    /// the pipe context dropped `KindleCtl` from that list; it did not make it a unit test.) What
    /// stands in for it is that `refuse_oversized` is the only writer of this status, so there is
    /// one door and it drains.
    #[tokio::test]
    async fn an_over_cap_post_is_refused_with_a_status_the_client_can_read() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let req = read_head(&mut stream).await.expect("the head must still parse");
            assert!(req.oversized);
            assert!(req.body.is_empty(), "an over-cap body is never allocated");
            // The REAL refusal, not a re-implementation of it. An earlier version of this test
            // inlined the drain and the reply, which meant deleting the drain from the endpoint
            // left it green — it proved the fixture, not the code.
            refuse_oversized(&mut stream, &req, "").await;
        });

        let mut client = TcpStream::connect(addr).await.unwrap();
        let body = vec![0u8; MAX_OCR_BODY + 1024 * 1024];
        let head = format!(
            "POST /ocr HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: image/png\r\n\
             Content-Length: {}\r\n\r\n",
            body.len()
        );
        client.write_all(head.as_bytes()).await.unwrap();
        // The load-bearing assertion is this one: before the fix the peer had closed by now and
        // the write failed, which is the whole of what the browser could see.
        client.write_all(&body).await.expect("the body must be accepted, not reset");

        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 413"), "{reply}");

        server.await.unwrap();
    }

    /// The head read must leave the body where it is, or the checks cannot come first.
    ///
    /// This is the whole basis of the ordering in `serve_conn`: an unauthenticated peer names a
    /// `Content-Length` and nothing in this process acts on it. If `read_head` consumed so much
    /// as a byte, the allocation and the copy would both be back in front of the token check —
    /// where an attacker chooses the size, `serve_loop` spawns a task per socket with no cap on
    /// how many, and the runtime being filled is the one that feeds Kindle its audio.
    #[tokio::test]
    async fn reading_the_head_consumes_none_of_the_body() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let req = read_head(&mut stream).await.unwrap();
            assert_eq!(req.len, 5);
            assert!(!req.oversized);
            assert!(req.body.is_empty());
            // Everything the peer sent is still there to be read - by a caller that has by now
            // decided it is allowed to.
            let body = read_body(&mut stream, req.len).await.unwrap();
            assert_eq!(body, b"hello");
        });

        let mut client = TcpStream::connect(addr).await.unwrap();
        client
            .write_all(
                b"POST /synth HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 5\r\n\r\nhello",
            )
            .await
            .unwrap();

        server.await.unwrap();
    }

    // Drift guard: the OCR download manifest (ocr-manifest.json, embedded by kokoro-panel to
    // fetch the models at first run) must agree with kokoro-ocr's own pinned filenames and
    // digests, which gate the load and every /status probe. The two are deliberately separate
    // authorities - the panel verifies on download, kokoro-ocr re-verifies on load - so a file
    // the panel would accept but the host would reject is exactly the split this test forbids.
    #[test]
    fn ocr_manifest_matches_kokoro_ocr_pins() {
        let m: serde_json::Value =
            serde_json::from_str(include_str!("../../ocr-manifest.json")).unwrap();
        let files = m["files"].as_array().expect("files array");
        let by_name = |name: &str| {
            files
                .iter()
                .find(|f| f["path"] == name)
                .unwrap_or_else(|| panic!("ocr-manifest.json missing {name}"))
                .clone()
        };
        for (name, sha) in [
            (kokoro_ocr::DET_FILE, kokoro_ocr::DET_SHA256),
            (kokoro_ocr::REC_FILE, kokoro_ocr::REC_SHA256),
            (kokoro_ocr::DICT_FILE, kokoro_ocr::DICT_SHA256),
        ] {
            let f = by_name(name);
            assert_eq!(f["sha256"], sha, "sha256 drift for {name}");
            assert!(f["url"].as_str().is_some_and(|u| !u.is_empty()), "no url for {name}");
            assert!(f["size"].as_u64().is_some_and(|s| s > 0), "no size for {name}");
        }
        assert_eq!(files.len(), 3, "ocr-manifest.json should list exactly the three OCR files");
    }
}
