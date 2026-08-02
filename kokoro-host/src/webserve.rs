// Loopback HTTP transport for the browser extension — the ONLY one.
//
//   GET  /status -> {"ok":true,"voice":…,"voices":[…],"sampleRate":24000}
//   POST /synth  {"text":…,"voice":…,"speed":…} -> raw little-endian f32 PCM
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

use crate::native_synth;
// Shared with the pipe rather than reimplemented: both transports answer from the same voice
// list and stamp the same "audio just went out" clock, so a second copy could only drift.
use crate::pipe::{available_voices, Ctx};
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
    body: Vec<u8>,
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

async fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut line = String::new();
    read_line_capped(reader, &mut line).await?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let (mut origin, mut host, mut auth, mut len) = (None, None, None, 0usize);
    for _ in 0..MAX_HEADERS {
        let mut h = String::new();
        read_line_capped(reader, &mut h).await?;
        let h = h.trim_end();
        if h.is_empty() {
            let mut body = vec![0u8; len.min(MAX_TEXT_BYTES as usize)];
            if !body.is_empty() {
                reader.read_exact(&mut body).await.ok()?;
            }
            return Some(Request { method, path, origin, host, auth, body });
        }
        let (name, value) = h.split_once(':')?;
        let value = value.trim().to_string();
        match name.to_ascii_lowercase().as_str() {
            "origin" => origin = Some(value),
            "host" => host = Some(value),
            "authorization" => auth = Some(value),
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

/// Everything a connection needs: the shared synth context and this endpoint's port/token.
#[derive(Clone)]
pub struct WebCtx {
    pub ctx: Ctx,
    pub endpoint: Arc<Endpoint>,
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
    // Bound the whole request head + body in TIME as well as size. Dropping the connection on
    // expiry is the right answer: nothing has authenticated yet, so there is no one to apologize
    // to, and a parked task would otherwise sit on the runtime the pipe server shares.
    let req = match tokio::time::timeout(REQUEST_TIMEOUT, read_request(&mut stream)).await {
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

    match (req.method.as_str(), req.path.split('?').next().unwrap_or("")) {
        ("GET", "/status") => {
            let (voice, _c) = native_synth::read_controls(&web.ctx.app_data);
            let body = serde_json::json!({
                "ok": true,
                "voice": voice,
                "voices": available_voices(&web.ctx.model_base),
                "sampleRate": SAMPLE_RATE,
            })
            .to_string();
            respond(&mut stream, "200 OK", &cors, "application/json", body.as_bytes()).await;
        }

        ("POST", "/synth") => {
            let v: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string();
            // Narrator: the extension's own picker wins; controls.json is the default, so the
            // panel's narrator is what an unset request gets. Deliberately unlike CMD_SYNTH,
            // where the host owns the narrator because it owns Kindle's settings.
            let (default_voice, controls) = native_synth::read_controls(&web.ctx.app_data);
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

            match web.ctx.native.synth(text, speed, voice, controls.engine).await {
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
                    web.ctx.state.stamp_audio(false);
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
