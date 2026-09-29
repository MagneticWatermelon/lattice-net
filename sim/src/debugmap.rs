//! The M2c debug map: a top-down view of what one client receives.
//!
//! `lattice-server --debug-http 0.0.0.0:8080` serves a page (open it from
//! Windows via the WSL IP) that polls `/frame` five times a second. The page
//! shows every entity, the watched client's tier rings, and what that client
//! got on the captured tick:
//! - near entities: sent as a delta, sent in full, or held back (faded by age);
//! - mid and far entities sent;
//! - far entities skipped for budget.
//!
//! Click an entity to watch its client instead. Std only: one thread, one
//! request at a time; nothing here touches the tick.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::server::DebugFrame;

#[derive(Default)]
struct Shared {
    frame: Option<DebugFrame>,
    watch: Option<u16>,
}

/// Handle to the debug map's HTTP thread.
#[derive(Clone)]
pub struct DebugMap {
    shared: Arc<Mutex<Shared>>,
}

impl DebugMap {
    /// Starts serving on `addr` in a background thread.
    pub fn start(addr: SocketAddr) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        let map = DebugMap { shared: Arc::default() };
        let shared = map.shared.clone();
        std::thread::Builder::new().name("debug-map".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = serve(stream, &shared);
            }
        })?;
        Ok(map)
    }

    /// The entity whose client the page asked to watch, if any.
    pub fn watch(&self) -> Option<u16> {
        self.shared.lock().unwrap().watch
    }

    pub fn publish(&self, frame: DebugFrame) {
        self.shared.lock().unwrap().frame = Some(frame);
    }
}

fn serve(stream: TcpStream, shared: &Mutex<Shared>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(&stream);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    // Drain the headers; nothing in them matters here.
    let mut line = String::new();
    while reader.read_line(&mut line)? > 2 {
        line.clear();
    }
    let path = request.split_whitespace().nth(1).unwrap_or("/");
    let (status, kind, body) = match path.split_once('?').map_or((path, ""), |(p, q)| (p, q)) {
        ("/", _) => ("200 OK", "text/html; charset=utf-8", PAGE.to_string()),
        ("/frame", _) => {
            let json = shared.lock().unwrap().frame.as_ref().map_or_else(|| "{}".to_string(), frame_json);
            ("200 OK", "application/json", json)
        }
        ("/watch", query) => match query.strip_prefix("entity=").and_then(|v| v.parse().ok()) {
            Some(e) => {
                shared.lock().unwrap().watch = Some(e);
                ("200 OK", "text/plain", "ok".to_string())
            }
            None => ("400 Bad Request", "text/plain", "expected ?entity=N".to_string()),
        },
        _ => ("404 Not Found", "text/plain", "not found".to_string()),
    };
    let mut out = &stream;
    write!(
        out,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// The frame as JSON (hand-written: the sim has no serde).
pub fn frame_json(f: &DebugFrame) -> String {
    let mut s = String::with_capacity(32 + f.entities.len() * 20);
    let _ = write!(
        s,
        r#"{{"tick":{},"level":{},"tick_hz":{},"dilation":{:.2},"pace":{:.3},"clients":{},"entities":["#,
        f.tick, f.level, f.tick_hz, f.dilation, f.pace, f.clients
    );
    for (i, (e, p)) in f.entities.iter().enumerate() {
        let _ = write!(s, "{}[{},{:.1},{:.1}]", if i > 0 { "," } else { "" }, e, p[0], p[1]);
    }
    s.push(']');
    if let Some(w) = &f.watched {
        let list = |v: &[u16]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
        let near: Vec<String> =
            w.near.iter().map(|&(e, age, sent, delta)| format!("[{e},{age},{},{}]", sent as u8, delta as u8)).collect();
        let _ = write!(
            s,
            r#","watched":{{"client":{},"entity":{},"pos":[{:.1},{:.1}],"radii":[{:.0},{:.0},{:.0}],"client_level":{},"near":[{}],"mid":[{}],"far":[{}],"far_skipped":[{}],"far_starved":{},"bytes":{},"near_bytes":{}}}"#,
            w.client,
            w.entity,
            w.pos[0],
            w.pos[1],
            w.radii[0],
            w.radii[1],
            w.radii[2],
            w.client_level,
            near.join(","),
            list(&w.mid),
            list(&w.far),
            list(&w.far_skipped),
            w.far_starved,
            w.bytes,
            w.near_bytes
        );
    }
    s.push('}');
    s
}

const PAGE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>lattice debug map</title>
<style>
  :root {
    --bg: #0f1115; --panel: #171a21cc; --text: #e6e8ee; --muted: #8b90a0; --line: #2a2f3a;
    --near: #3ecf8e; --full: #f2c14e; --mid: #5b9dff; --far: #f08a4b; --skip: #ff5c5c; --other: #4a5061;
  }
  html, body { margin: 0; height: 100%; background: var(--bg); color: var(--text);
    font: 13px/1.45 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; overflow: hidden; }
  canvas { display: block; width: 100vw; height: 100vh; cursor: crosshair; }
  #panel { position: fixed; top: 12px; left: 12px; background: var(--panel); border: 1px solid var(--line);
    border-radius: 8px; padding: 10px 12px; min-width: 250px; backdrop-filter: blur(4px); }
  #panel h1 { font-size: 13px; margin: 0 0 6px; font-weight: 600; }
  #panel .row { display: flex; justify-content: space-between; gap: 16px; }
  #panel .muted { color: var(--muted); }
  .key { display: inline-block; width: 9px; height: 9px; border-radius: 50%; margin-right: 6px; vertical-align: -1px; }
  #legend { margin-top: 8px; border-top: 1px solid var(--line); padding-top: 6px; }
  #hint { position: fixed; bottom: 10px; left: 12px; color: var(--muted); }
</style>
</head>
<body>
<canvas id="map"></canvas>
<div id="panel">
  <h1>lattice debug map</h1>
  <div id="stats" class="muted">waiting for the server…</div>
  <div id="legend">
    <div><span class="key" style="background:var(--near)"></span>near, sent as a delta</div>
    <div><span class="key" style="background:var(--full)"></span>near, sent in full</div>
    <div><span class="key" style="background:var(--near);opacity:.35"></span>near, held back (fades with age)</div>
    <div><span class="key" style="background:var(--mid)"></span>mid, sent this tick</div>
    <div><span class="key" style="background:var(--far)"></span>far, sent this tick</div>
    <div><span class="key" style="border:2px solid var(--skip);width:5px;height:5px"></span>far, skipped for budget</div>
    <div><span class="key" style="background:var(--other)"></span>not sent this tick</div>
  </div>
</div>
<div id="hint">scroll to zoom · click an entity to watch its client</div>
<script>
const cv = document.getElementById('map'), ctx = cv.getContext('2d');
const css = n => getComputedStyle(document.documentElement).getPropertyValue(n).trim();
const C = { bg: css('--bg'), near: css('--near'), full: css('--full'), mid: css('--mid'), far: css('--far'),
            skip: css('--skip'), other: css('--other'), line: css('--line'), text: css('--text') };
let frame = null, scale = null;

function resize() { cv.width = innerWidth * devicePixelRatio; cv.height = innerHeight * devicePixelRatio; draw(); }
addEventListener('resize', resize);

async function poll() {
  try { const r = await fetch('/frame', { cache: 'no-store' }); frame = await r.json(); draw(); } catch (e) {}
  setTimeout(poll, 200);
}

function view() {
  const w = frame.watched, W = cv.width, H = cv.height;
  if (scale === null) scale = Math.min(W, H) / (2.3 * w.radii[2]);
  return { sx: x => W / 2 + (x - w.pos[0]) * scale, sy: y => H / 2 - (y - w.pos[1]) * scale };
}

function draw() {
  const W = cv.width, H = cv.height, dpr = devicePixelRatio;
  ctx.fillStyle = C.bg; ctx.fillRect(0, 0, W, H);
  if (!frame || !frame.watched) return;
  const w = frame.watched, { sx, sy } = view();

  ctx.lineWidth = dpr;
  [[w.radii[2], C.far], [w.radii[1], C.mid], [w.radii[0], C.near]].forEach(([r, c]) => {
    ctx.strokeStyle = c; ctx.globalAlpha = 0.45; ctx.beginPath();
    ctx.arc(sx(w.pos[0]), sy(w.pos[1]), r * scale, 0, 2 * Math.PI); ctx.stroke();
  });
  ctx.globalAlpha = 1;

  const near = new Map(w.near.map(n => [n[0], n])), mid = new Set(w.mid), far = new Set(w.far),
        skip = new Set(w.far_skipped);
  const dot = (x, y, r, color, alpha) => {
    ctx.globalAlpha = alpha; ctx.fillStyle = color; ctx.beginPath(); ctx.arc(x, y, r * dpr, 0, 2 * Math.PI); ctx.fill();
  };
  for (const [id, x, y] of frame.entities) {
    const px = sx(x), py = sy(y);
    if (px < -10 || py < -10 || px > W + 10 || py > H + 10) continue;
    const n = near.get(id);
    if (n) {
      const [, age, sent, delta] = n;
      if (sent) dot(px, py, 3.2, delta ? C.near : C.full, 1);
      else dot(px, py, 3.2, C.near, Math.max(0.15, 0.6 - age * 0.08));
    } else if (mid.has(id)) dot(px, py, 2.6, C.mid, 1);
    else if (far.has(id)) dot(px, py, 2.4, C.far, 1);
    else dot(px, py, 1.4, C.other, 1);
    if (skip.has(id)) {
      ctx.globalAlpha = 1; ctx.strokeStyle = C.skip; ctx.lineWidth = 1.5 * dpr;
      ctx.beginPath(); ctx.arc(px, py, 5 * dpr, 0, 2 * Math.PI); ctx.stroke();
    }
  }
  ctx.globalAlpha = 1; ctx.strokeStyle = C.text; ctx.lineWidth = 1.5 * dpr;
  const [cx, cy] = [sx(w.pos[0]), sy(w.pos[1])], k = 7 * dpr;
  ctx.beginPath(); ctx.moveTo(cx - k, cy); ctx.lineTo(cx + k, cy); ctx.moveTo(cx, cy - k); ctx.lineTo(cx, cy + k); ctx.stroke();

  const sent = w.near.filter(n => n[2]), deltas = sent.filter(n => n[3]).length;
  const held = w.near.length - sent.length, oldest = Math.max(0, ...w.near.map(n => n[1]));
  const row = (a, b) => `<div class="row"><span class="muted">${a}</span><span>${b}</span></div>`;
  document.getElementById('stats').innerHTML =
    row('tick', frame.tick) +
    row('server', `level ${frame.level} · ${frame.tick_hz} Hz · dilation ${frame.dilation} · pace ${frame.pace}`) +
    row('clients', frame.clients) +
    row('watching', `entity ${w.entity} (client ${w.client})`) +
    row('client level', w.client_level) +
    row('near', `${sent.length} sent (${deltas} delta, ${sent.length - deltas} full), ${held} held, oldest ${oldest} ticks`) +
    row('mid / far', `${w.mid.length} / ${w.far.length}`) +
    row('far skipped / starved', `${w.far_skipped.length} / ${w.far_starved}`) +
    row('bytes this tick', `${w.bytes} (near ${w.near_bytes})`);
}

cv.addEventListener('wheel', e => {
  e.preventDefault(); if (scale !== null) { scale *= Math.exp(-e.deltaY * 0.0015); draw(); }
}, { passive: false });

cv.addEventListener('click', e => {
  if (!frame || !frame.watched) return;
  const { sx, sy } = view(), mx = e.clientX * devicePixelRatio, my = e.clientY * devicePixelRatio;
  let best = null, bestD = (12 * devicePixelRatio) ** 2;
  for (const [id, x, y] of frame.entities) {
    const d = (sx(x) - mx) ** 2 + (sy(y) - my) ** 2;
    if (d < bestD) { best = id; bestD = d; }
  }
  if (best !== null) fetch('/watch?entity=' + best);
});

resize(); poll();
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::WatchedClient;

    #[test]
    fn frame_json_has_the_fields_the_page_reads() {
        let f = DebugFrame {
            tick: 42,
            level: 6,
            tick_hz: 20,
            dilation: 1.0,
            pace: 0.8,
            clients: 3,
            entities: vec![(1, [10.0, 20.0]), (2, [30.5, 40.24])],
            watched: Some(WatchedClient {
                client: 7,
                entity: 1,
                pos: [10.0, 20.0],
                radii: [150.0, 350.0, 675.0],
                client_level: 2,
                near: vec![(2, 0, true, true), (3, 4, false, false)],
                mid: vec![5],
                far: vec![],
                far_skipped: vec![9],
                far_starved: 1,
                bytes: 300,
                near_bytes: 40,
            }),
        };
        let j = frame_json(&f);
        for want in [
            r#""tick":42"#,
            r#""entities":[[1,10.0,20.0],[2,30.5,40.2]]"#,
            r#""radii":[150,350,675]"#,
            r#""near":[[2,0,1,1],[3,4,0,0]]"#,
            r#""mid":[5],"far":[],"far_skipped":[9]"#,
            r#""bytes":300,"near_bytes":40}}"#,
        ] {
            assert!(j.contains(want), "missing {want} in {j}");
        }
        assert_eq!(frame_json(&DebugFrame::default()).matches('{').count(), 1, "no watched client: no watched object");
    }

    #[test]
    fn serves_the_page_the_frame_and_watch_requests() {
        use std::io::Read;
        // Same serve loop as `start`, on a port picked by the OS.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let map = DebugMap { shared: Arc::default() };
        let shared = map.shared.clone();
        std::thread::spawn(move || {
            for s in listener.incoming().flatten() {
                let _ = serve(s, &shared);
            }
        });
        let get = |path: &str| {
            let mut s = TcpStream::connect(addr).unwrap();
            write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            out
        };
        assert!(get("/").contains("<canvas"));
        assert!(get("/frame").ends_with("{}"));
        map.publish(DebugFrame { tick: 5, ..Default::default() });
        assert!(get("/frame").contains(r#""tick":5"#));
        assert!(get("/watch?entity=12").contains("200 OK"));
        assert_eq!(map.watch(), Some(12));
        assert!(get("/watch?entity=x").contains("400"));
        assert!(get("/nope").contains("404"));
    }
}
