"""Headless-Chrome smoke test for the web build (Python stdlib only, no Selenium/Node).

Launches Chrome with a DevTools port, opens URL, prints console messages and exceptions for WAIT
seconds, saves a PNG screenshot via CDP `Page.captureScreenshot` and prints the page state
(loading indicator, error box, canvas size, service worker).

    cargo xtask web && cargo xtask serve --port 8765 &
    python xtask/scripts/cdp_shot.py <chrome> <profile-dir> http://127.0.0.1:8765/ shot.png 20         --use-angle=swiftshader --enable-unsafe-swiftshader

Notes: --no-sandbox is always passed (a portable Chrome cannot use its sandbox); keep the profile
directory between runs (a throwaway dir outside the repo); do not use Chrome's own --screenshot flag
(it hangs with a live WebGL/WebGPU canvas). Env: CDP_PORT (default 9347), DRAG=1 to send a mouse drag
and wheel before the screenshot.
"""
import base64, json, os, socket, struct, subprocess, sys, time, urllib.request

PAGE_JS = """(() => { const c = document.getElementById('webcad_canvas'); const e = document.getElementById('error');
  return 'loading=' + !!document.getElementById('loading') + ' | error_hidden=' + (e ? e.hidden : 'n/a') +
    ' | error_text=' + (document.getElementById('error_text') || {}).textContent + ' | canvas ' + (c ? c.width + 'x' + c.height : 'none') +
    ' | gpu=' + !!navigator.gpu + ' | sw=' + !!(navigator.serviceWorker && navigator.serviceWorker.controller) +
    ' | preload=' + [...document.querySelectorAll('link[rel=modulepreload]')].map(l => l.href).join(','); })()"""
chrome, profile, url, out, wait = sys.argv[1:6]
extra = sys.argv[6:]
port = int(os.environ.get("CDP_PORT", "9347"))
p = subprocess.Popen([chrome, "--headless=new", "--no-sandbox", f"--remote-debugging-port={port}",
                      f"--user-data-dir={profile}", "--window-size=1280,800", "--no-first-run",
                      "--no-default-browser-check", "--disable-extensions", *extra, "about:blank"],
                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
try:
    for _ in range(100):
        try:
            tabs = json.load(urllib.request.urlopen(f"http://127.0.0.1:{port}/json/list", timeout=1)); break
        except Exception: time.sleep(0.2)
    ws_url = next(t["webSocketDebuggerUrl"] for t in tabs if t["type"] == "page")
    host_port, path = ws_url[len("ws://"):].split("/", 1)
    s = socket.create_connection(("127.0.0.1", port)); s.settimeout(120)
    key = base64.b64encode(os.urandom(16)).decode()
    s.sendall(f"GET /{path} HTTP/1.1\r\nHost: {host_port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n".encode())
    buf = b""
    while b"\r\n\r\n" not in buf: buf += s.recv(4096)
    buf = buf.split(b"\r\n\r\n", 1)[1]
    def recv_exact(n):
        global buf
        while len(buf) < n:
            c = s.recv(1 << 20)
            if not c: raise EOFError
            buf += c
        r, buf = buf[:n], buf[n:]; return r
    def recv_msg():
        data = b""
        while True:
            b0, b1 = recv_exact(2); n = b1 & 0x7F
            if n == 126: n = struct.unpack(">H", recv_exact(2))[0]
            elif n == 127: n = struct.unpack(">Q", recv_exact(8))[0]
            data += recv_exact(n)
            if b0 & 0x80: return json.loads(data)
    def send(obj):
        d = json.dumps(obj).encode(); m = os.urandom(4)
        hdr = bytes([0x81]) + (bytes([0x80 | len(d)]) if len(d) < 126 else (bytes([0x80 | 126]) + struct.pack(">H", len(d)) if len(d) < 65536 else bytes([0x80 | 127]) + struct.pack(">Q", len(d))))
        s.sendall(hdr + m + bytes(b ^ m[i % 4] for i, b in enumerate(d)))
    msg_id = [0]
    def call(method, **params):
        msg_id[0] += 1; send({"id": msg_id[0], "method": method, "params": params})
        while True:
            r = recv_msg()
            if r.get("id") == msg_id[0]: return r
            log_event(r)
    def log_event(r):
        m = r.get("method")
        if m == "Runtime.consoleAPICalled":
            print("console." + r["params"]["type"] + ":", " ".join(str(a.get("value", a.get("description", ""))) for a in r["params"]["args"])[:400])
        elif m == "Runtime.exceptionThrown":
            print("EXCEPTION:", json.dumps(r["params"]["exceptionDetails"])[:600])
    call("Runtime.enable"); call("Page.enable")
    call("Page.navigate", url=url)
    t_end = time.time() + float(wait)
    s.settimeout(0.5)
    while time.time() < t_end:
        try: log_event(recv_msg())
        except (socket.timeout, TimeoutError): pass
    s.settimeout(120)
    if os.environ.get("DRAG"):  # orbit: left-drag in the viewport, then wheel-zoom in
        call("Input.dispatchMouseEvent", type="mouseMoved", x=500, y=400)
        call("Input.dispatchMouseEvent", type="mousePressed", x=500, y=400, button="left", clickCount=1, buttons=1)
        for i in range(1, 11):
            call("Input.dispatchMouseEvent", type="mouseMoved", x=500 + 12 * i, y=400 + 6 * i, button="left", buttons=1); time.sleep(0.05)
        call("Input.dispatchMouseEvent", type="mouseReleased", x=620, y=460, button="left", clickCount=1)
        for _ in range(3):
            call("Input.dispatchMouseEvent", type="mouseWheel", x=620, y=460, deltaX=0, deltaY=-120); time.sleep(0.1)
        t_end = time.time() + 3; s.settimeout(0.5)
        while time.time() < t_end:
            try: log_event(recv_msg())
            except (socket.timeout, TimeoutError): pass
        s.settimeout(120)
    r = call("Page.captureScreenshot", format="png")
    open(out, "wb").write(base64.b64decode(r["result"]["data"]))
    print("screenshot", out, os.path.getsize(out), "bytes")
    ev = call("Runtime.evaluate", expression=PAGE_JS, returnByValue=True)
    print("page:", ev["result"]["result"].get("value"))
finally:
    p.kill()
