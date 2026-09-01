#!/usr/bin/env python3
"""Record docs/demo.mp4 by capturing a live `fun` xterm window."""

from __future__ import annotations

import atexit
import ctypes
import fcntl
import json
import os
import pty
import re
import select
import shutil
import signal
import socket
import struct
import subprocess
import sys
import termios
import time
import tty as tty_mod
from ctypes import (
    CFUNCTYPE,
    POINTER,
    Structure,
    byref,
    c_char_p,
    c_int,
    c_long,
    c_uint,
    c_ulong,
    c_void_p,
    cast,
)
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FUN = ROOT / "target" / "debug" / "fun"
WS = Path.home() / "demo"
OUT = ROOT / "docs" / "demo.mp4"
FPS = 30
TITLE = "fun-coding-agent-demo"
MAX_SEC = 150.0
SOCK = Path("/tmp/fun-demo-inject.sock")

P1 = (
    "create a tiny python greet CLI that says hello in Korean to "
    '"수영 서점" (swimming bookstore) as the default name, with argparse --loud '
    "and --count, write tests, run them, and add a Makefile"
)
P2 = "also add a README in Korean"
P3 = "print a usage line too"

COLS, ROWS = 100, 30
XTERM_BORDER = 12
COMPOSER_H = 3
# Status wraps to 3 rows while working (spinner + model/effort).
STATUS_H = 3
# Commit chip stays visible without origin; it fills the composer instead of sending.
ACTION_H = 1
QUEUE_CTRLS = [
    ("send now", 8),
    ("edit", 4),
    ("move up", 7),
    ("move down", 9),
    ("cancel", 6),
]
CTRL_W = {name: w for name, w in QUEUE_CTRLS}

os.environ.setdefault("DISPLAY", ":0.0")
os.environ.setdefault("XAUTHORITY", str(Path.home() / ".Xauthority"))

CHILDREN: list[subprocess.Popen] = []

x11 = ctypes.CDLL("libX11.so.6")
xtst = ctypes.CDLL("libXtst.so.6")
xcomp = ctypes.CDLL("libXcomposite.so.1")
xfixes = ctypes.CDLL("libXfixes.so.3")

ZPixmap = 2
AllPlanes = c_ulong(~0)
CompositeRedirectAutomatic = 0
RevertToParent = 2
CurrentTime = 0


class XImage(Structure):
    _fields_ = [
        ("width", c_int),
        ("height", c_int),
        ("xoffset", c_int),
        ("format", c_int),
        ("data", c_void_p),
        ("byte_order", c_int),
        ("bitmap_unit", c_int),
        ("bitmap_bit_order", c_int),
        ("bitmap_pad", c_int),
        ("depth", c_int),
        ("bytes_per_line", c_int),
        ("bits_per_pixel", c_int),
        ("red_mask", c_ulong),
        ("green_mask", c_ulong),
        ("blue_mask", c_ulong),
        ("obdata", c_void_p),
        ("f", c_void_p * 6),
    ]


x11.XOpenDisplay.restype = c_void_p
x11.XOpenDisplay.argtypes = [c_char_p]
x11.XCloseDisplay.argtypes = [c_void_p]
x11.XDefaultRootWindow.restype = c_ulong
x11.XDefaultRootWindow.argtypes = [c_void_p]
x11.XQueryTree.restype = c_int
x11.XQueryTree.argtypes = [
    c_void_p,
    c_ulong,
    POINTER(c_ulong),
    POINTER(c_ulong),
    POINTER(POINTER(c_ulong)),
    POINTER(c_uint),
]
x11.XFetchName.restype = c_int
x11.XFetchName.argtypes = [c_void_p, c_ulong, POINTER(c_char_p)]
x11.XGetGeometry.restype = c_int
x11.XGetGeometry.argtypes = [
    c_void_p,
    c_ulong,
    POINTER(c_ulong),
    POINTER(c_int),
    POINTER(c_int),
    POINTER(c_uint),
    POINTER(c_uint),
    POINTER(c_uint),
    POINTER(c_uint),
]
x11.XGetImage.restype = POINTER(XImage)
x11.XGetImage.argtypes = [
    c_void_p,
    c_ulong,
    c_int,
    c_int,
    c_uint,
    c_uint,
    c_ulong,
    c_int,
]
x11.XDestroyImage.restype = c_int
x11.XDestroyImage.argtypes = [POINTER(XImage)]
x11.XFree.argtypes = [c_void_p]
x11.XFreePixmap.argtypes = [c_void_p, c_ulong]
x11.XWarpPointer.argtypes = [
    c_void_p,
    c_ulong,
    c_ulong,
    c_int,
    c_int,
    c_uint,
    c_uint,
    c_int,
    c_int,
]
x11.XFlush.argtypes = [c_void_p]
x11.XSync.argtypes = [c_void_p, c_int]
x11.XSetErrorHandler.restype = c_void_p
x11.XSetErrorHandler.argtypes = [c_void_p]
x11.XInternAtom.restype = c_ulong
x11.XInternAtom.argtypes = [c_void_p, c_char_p, c_int]
x11.XGetWindowProperty.restype = c_int
x11.XGetWindowProperty.argtypes = [
    c_void_p,
    c_ulong,
    c_ulong,
    c_long,
    c_long,
    c_int,
    c_ulong,
    POINTER(c_ulong),
    POINTER(c_int),
    POINTER(c_ulong),
    POINTER(c_ulong),
    POINTER(c_void_p),
]
x11.XStringToKeysym.restype = c_ulong
x11.XStringToKeysym.argtypes = [c_char_p]
x11.XKeysymToKeycode.restype = ctypes.c_ubyte
x11.XKeysymToKeycode.argtypes = [c_void_p, c_ulong]
x11.XSetInputFocus.argtypes = [c_void_p, c_ulong, c_int, c_ulong]
x11.XRaiseWindow.argtypes = [c_void_p, c_ulong]
x11.XMapRaised.argtypes = [c_void_p, c_ulong]

xtst.XTestFakeKeyEvent.restype = c_int
xtst.XTestFakeKeyEvent.argtypes = [c_void_p, c_uint, c_int, c_ulong]
xtst.XTestFakeButtonEvent.restype = c_int
xtst.XTestFakeButtonEvent.argtypes = [c_void_p, c_uint, c_int, c_ulong]

xcomp.XCompositeQueryExtension.restype = c_int
xcomp.XCompositeQueryExtension.argtypes = [c_void_p, POINTER(c_int), POINTER(c_int)]
xcomp.XCompositeRedirectWindow.argtypes = [c_void_p, c_ulong, c_int]
xcomp.XCompositeNameWindowPixmap.restype = c_ulong
xcomp.XCompositeNameWindowPixmap.argtypes = [c_void_p, c_ulong]
xfixes.XFixesQueryExtension.restype = c_int
xfixes.XFixesQueryExtension.argtypes = [c_void_p, POINTER(c_int), POINTER(c_int)]
xfixes.XFixesHideCursor.argtypes = [c_void_p, c_ulong]
xfixes.XFixesShowCursor.argtypes = [c_void_p, c_ulong]


@CFUNCTYPE(c_int, c_void_p, c_void_p)
def _xerr(_dpy, _ev):
    return 0


_KEEP_HANDLER = _xerr

SHIFT = {
    "!": "1",
    "@": "2",
    "#": "3",
    "$": "4",
    "%": "5",
    "^": "6",
    "&": "7",
    "*": "8",
    "(": "9",
    ")": "0",
    "_": "minus",
    "+": "equal",
    "{": "bracketleft",
    "}": "bracketright",
    "|": "backslash",
    ":": "semicolon",
    '"': "apostrophe",
    "<": "comma",
    ">": "period",
    "?": "slash",
    "~": "grave",
}

NAMED = {
    " ": "space",
    "-": "minus",
    "=": "equal",
    "[": "bracketleft",
    "]": "bracketright",
    "\\": "backslash",
    ";": "semicolon",
    "'": "apostrophe",
    ",": "comma",
    ".": "period",
    "/": "slash",
    "`": "grave",
    "\n": "Return",
}


def die(msg: str, code: int = 1) -> None:
    print(msg, file=sys.stderr)
    sys.exit(code)


def cleanup() -> None:
    for p in reversed(CHILDREN):
        if p.poll() is None:
            p.send_signal(signal.SIGTERM)
    time.sleep(0.2)
    for p in reversed(CHILDREN):
        if p.poll() is None:
            p.kill()
    try:
        SOCK.unlink()
    except OSError:
        pass


def wrap_main() -> None:
    """Sit between xterm and fun so the recorder can inject SGR mouse bytes."""
    sock_path = sys.argv[1]
    cmd = sys.argv[2:]
    try:
        os.unlink(sock_path)
    except OSError:
        pass
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
    srv.bind(sock_path)
    srv.setblocking(False)

    pid, fd = pty.fork()
    if pid == 0:
        os.execvp(cmd[0], cmd)
        os._exit(127)

    def copy_winsize(_signum=None, _frame=None):
        try:
            packed = fcntl.ioctl(0, termios.TIOCGWINSZ, struct.pack("HHHH", 0, 0, 0, 0))
            fcntl.ioctl(fd, termios.TIOCSWINSZ, packed)
        except OSError:
            pass

    copy_winsize()
    signal.signal(signal.SIGWINCH, copy_winsize)
    tty_mod.setraw(0)

    child_dead = False

    def reap(_signum=None, _frame=None):
        nonlocal child_dead
        try:
            os.waitpid(pid, os.WNOHANG)
        except OSError:
            pass
        child_dead = True

    signal.signal(signal.SIGCHLD, reap)
    try:
        while True:
            fds = [0, fd, srv]
            try:
                r, _, _ = select.select(fds, [], [], 0.25)
            except InterruptedError:
                continue
            if 0 in r:
                try:
                    data = os.read(0, 4096)
                except OSError:
                    data = b""
                if not data:
                    break
                os.write(fd, data)
            if fd in r:
                try:
                    data = os.read(fd, 4096)
                except OSError:
                    data = b""
                if not data:
                    break
                os.write(1, data)
            if srv in r:
                try:
                    data, _ = srv.recvfrom(4096)
                except OSError:
                    data = b""
                if data:
                    os.write(fd, data)
            if child_dead:
                break
    finally:
        try:
            os.kill(pid, signal.SIGHUP)
        except OSError:
            pass
        try:
            srv.close()
        except OSError:
            pass
        try:
            os.unlink(sock_path)
        except OSError:
            pass


def children_of(dpy, win: int) -> list[int]:
    root = c_ulong()
    parent = c_ulong()
    kids = POINTER(c_ulong)()
    n = c_uint()
    if not x11.XQueryTree(dpy, win, byref(root), byref(parent), byref(kids), byref(n)):
        return []
    out = [kids[i] for i in range(n.value)]
    if kids:
        x11.XFree(cast(kids, c_void_p))
    return out


def net_wm_name(dpy, win: int) -> str:
    atom = x11.XInternAtom(dpy, b"_NET_WM_NAME", 0)
    utf8 = x11.XInternAtom(dpy, b"UTF8_STRING", 0)
    actual_type = c_ulong()
    actual_fmt = c_int()
    nitems = c_ulong()
    bytes_after = c_ulong()
    prop = c_void_p()
    status = x11.XGetWindowProperty(
        dpy,
        win,
        atom,
        0,
        1024,
        0,
        utf8,
        byref(actual_type),
        byref(actual_fmt),
        byref(nitems),
        byref(bytes_after),
        byref(prop),
    )
    if status != 0 or not prop:
        return ""
    s = ctypes.string_at(prop, nitems.value).decode("utf-8", "replace")
    x11.XFree(prop)
    return s


def window_name(dpy, win: int) -> str:
    n = net_wm_name(dpy, win)
    if n:
        return n
    name = c_char_p()
    if x11.XFetchName(dpy, win, byref(name)) and name.value:
        s = name.value.decode("utf-8", "replace")
        x11.XFree(name)
        return s
    return ""


def find_via_xwininfo(title: str) -> int:
    try:
        out = subprocess.check_output(
            ["xwininfo", "-name", title],
            text=True,
            stderr=subprocess.DEVNULL,
        )
    except subprocess.CalledProcessError:
        return 0
    m = re.search(r"Window id:\s+(0x[0-9a-fA-F]+)", out)
    return int(m.group(1), 16) if m else 0


def find_window(dpy, title: str) -> int:
    wid = find_via_xwininfo(title)
    if wid:
        return wid
    root = x11.XDefaultRootWindow(dpy)
    stack = children_of(dpy, root)
    while stack:
        w = stack.pop()
        name = window_name(dpy, w)
        if name == title or name.startswith(title):
            return w
        stack.extend(children_of(dpy, w))
    return 0


def geometry(dpy, win: int) -> tuple[int, int]:
    root = c_ulong()
    x = c_int()
    y = c_int()
    w = c_uint()
    h = c_uint()
    bw = c_uint()
    depth = c_uint()
    if not x11.XGetGeometry(
        dpy, win, byref(root), byref(x), byref(y), byref(w), byref(h), byref(bw), byref(depth)
    ):
        return 0, 0
    return int(w.value), int(h.value)


def grab_bgr(dpy, win: int, w: int, h: int, redirected: bool) -> bytes | None:
    drawable = win
    pix = 0
    if redirected:
        pix = xcomp.XCompositeNameWindowPixmap(dpy, win)
        if pix:
            drawable = pix
    img_p = x11.XGetImage(dpy, drawable, 0, 0, w, h, AllPlanes, ZPixmap)
    if pix:
        x11.XFreePixmap(dpy, pix)
    if not img_p:
        return None
    img = img_p.contents
    if img.bits_per_pixel != 32 or not img.data:
        x11.XDestroyImage(img_p)
        return None
    raw = ctypes.string_at(img.data, img.bytes_per_line * h)
    stride = img.bytes_per_line
    x11.XDestroyImage(img_p)
    row = w * 4
    if stride == row:
        return raw
    packed = bytearray(h * row)
    for y in range(h):
        packed[y * row : (y + 1) * row] = raw[y * stride : y * stride + row]
    return bytes(packed)


def keycode(dpy, name: str) -> int:
    ks = x11.XStringToKeysym(name.encode())
    if not ks:
        return 0
    return int(x11.XKeysymToKeycode(dpy, ks))


def tap(dpy, kc: int, shift: bool = False, ctrl: bool = False) -> None:
    if not kc:
        return
    shift_kc = keycode(dpy, "Shift_L")
    ctrl_kc = keycode(dpy, "Control_L")
    if ctrl:
        xtst.XTestFakeKeyEvent(dpy, ctrl_kc, 1, 0)
    if shift:
        xtst.XTestFakeKeyEvent(dpy, shift_kc, 1, 0)
    xtst.XTestFakeKeyEvent(dpy, kc, 1, 0)
    xtst.XTestFakeKeyEvent(dpy, kc, 0, 0)
    if shift:
        xtst.XTestFakeKeyEvent(dpy, shift_kc, 0, 0)
    if ctrl:
        xtst.XTestFakeKeyEvent(dpy, ctrl_kc, 0, 0)
    x11.XFlush(dpy)


def ensure_focus(dpy, win: int) -> None:
    x11.XRaiseWindow(dpy, win)
    x11.XSetInputFocus(dpy, win, RevertToParent, CurrentTime)
    x11.XFlush(dpy)


def type_char(dpy, win: int, ch: str) -> None:
    ensure_focus(dpy, win)
    if ch == "\n":
        tap(dpy, keycode(dpy, "Return"))
        return
    if ord(ch) > 127:
        inject(ch.encode("utf-8"))
        return
    if ch.isalpha():
        tap(dpy, keycode(dpy, ch.lower()), shift=ch.isupper())
        return
    if ch in SHIFT:
        tap(dpy, keycode(dpy, SHIFT[ch]), shift=True)
        return
    tap(dpy, keycode(dpy, NAMED.get(ch, ch)))


def focus(dpy, win: int, w: int, h: int) -> None:
    x11.XMapRaised(dpy, win)
    x11.XRaiseWindow(dpy, win)
    x11.XSetInputFocus(dpy, win, RevertToParent, CurrentTime)
    x11.XWarpPointer(dpy, 0, win, 0, 0, 0, 0, max(2, w // 2), max(2, h // 2))
    x11.XFlush(dpy)
    xtst.XTestFakeButtonEvent(dpy, 1, 1, 0)
    xtst.XTestFakeButtonEvent(dpy, 1, 0, 0)
    x11.XFlush(dpy)


def cell_size(win_w: int, win_h: int) -> tuple[float, float]:
    return (win_w - 2 * XTERM_BORDER) / COLS, (win_h - 2 * XTERM_BORDER) / ROWS


def cell_xy(cw: float, ch: float, col: float, row: float) -> tuple[int, int]:
    x = int(XTERM_BORDER + (col + 0.5) * cw)
    y = int(XTERM_BORDER + (row + 0.5) * ch)
    return x, y


def put_pixel(frame: bytearray, w: int, h: int, x: int, y: int, bgr: tuple[int, int, int]) -> None:
    if 0 <= x < w and 0 <= y < h:
        i = (y * w + x) * 4
        frame[i] = bgr[0]
        frame[i + 1] = bgr[1]
        frame[i + 2] = bgr[2]


def overlay_pointer(frame: bytes, w: int, h: int, x: int, y: int) -> bytes:
    out = bytearray(frame)
    for dy in range(18):
        span = 1 if dy < 2 else dy // 2 + 1
        for dx in range(-1, span + 1):
            edge = dx < 0 or dx == span or dy in (0, 17)
            color = (255, 255, 255) if edge else (239, 207, 125)
            put_pixel(out, w, h, x + dx, y + dy, color)
            put_pixel(out, w, h, x + dx, y + dy + 1, color)
    return bytes(out)


RGB_ACCENT = (125, 207, 239)
RGB_USER = (247, 168, 120)
RGB_TOOL = (232, 196, 104)
RGB_ERROR = (243, 139, 168)


def rgb_at(frame: bytes, w: int, h: int, x: int, y: int) -> tuple[int, int, int] | None:
    if not (0 <= x < w and 0 <= y < h):
        return None
    i = (y * w + x) * 4
    return frame[i + 2], frame[i + 1], frame[i]


def rgb_close(a: tuple[int, int, int] | None, b: tuple[int, int, int], tol: int = 42) -> bool:
    return bool(a) and all(abs(a[i] - b[i]) <= tol for i in range(3))


def cell_has_color(
    frame: bytes,
    win_w: int,
    win_h: int,
    cw: float,
    ch: float,
    col: int,
    row: int,
    color: tuple[int, int, int],
) -> bool:
    x0 = int(XTERM_BORDER + col * cw)
    y0 = int(XTERM_BORDER + row * ch)
    x1 = max(x0 + 1, int(XTERM_BORDER + (col + 1) * cw))
    y1 = max(y0 + 1, int(XTERM_BORDER + (row + 1) * ch))
    for y in range(y0 + 2, y1 - 1, 3):
        for x in range(x0 + 1, x1 - 1, 2):
            if rgb_close(rgb_at(frame, win_w, win_h, x, y), color):
                return True
    return False


def find_queue_rows(
    frame: bytes, win_w: int, win_h: int, cw: float, ch: float, cols: dict[str, int]
) -> list[int]:
    rows: list[int] = []
    for row in range(8, ROWS - STATUS_H - COMPOSER_H - ACTION_H):
        send = any(
            cell_has_color(frame, win_w, win_h, cw, ch, cols["send now"] + i, row, RGB_ACCENT)
            for i in range(CTRL_W["send now"])
        )
        edit = any(
            cell_has_color(frame, win_w, win_h, cw, ch, cols["edit"] + i, row, RGB_USER)
            for i in range(CTRL_W["edit"])
        )
        cancel = any(
            cell_has_color(frame, win_w, win_h, cw, ch, cols["cancel"] + i, row, RGB_ERROR)
            for i in range(CTRL_W["cancel"])
        )
        if send and edit and cancel:
            rows.append(row)
    return rows



def queue_layout(n_items: int) -> tuple[int, dict[str, int]]:
    queue_h = min(n_items, 4) + 2
    queue_y = ROWS - STATUS_H - COMPOSER_H - ACTION_H - queue_h
    item0 = queue_y + 1
    inner_x = 2
    inner_w = COLS - 4
    ctrl_w = 0
    for i, (_, w) in enumerate(QUEUE_CTRLS):
        ctrl_w += (1 if i == 0 else 2) + w
    origin = inner_x + inner_w - ctrl_w
    cols: dict[str, int] = {}
    cx = origin + 1
    for i, (name, w) in enumerate(QUEUE_CTRLS):
        if i:
            cx += 2
        cols[name] = cx
        cx += w
    return item0, cols


def sgr_click(col: int, row: int) -> bytes:
    c, r = col + 1, row + 1
    return f"\x1b[<0;{c};{r}M\x1b[<0;{c};{r}m".encode()


def inject(data: bytes) -> None:
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
    try:
        sock.sendto(data, str(SOCK))
    finally:
        sock.close()


def sessions_root() -> Path:
    xdg = os.environ.get("XDG_DATA_HOME")
    if xdg:
        return Path(xdg) / "fun" / "sessions"
    return Path.home() / ".local/share/fun/sessions"


def newest_session(since: float) -> Path | None:
    root = sessions_root()
    if not root.exists():
        return None
    files = [p for p in root.rglob("*.jsonl") if p.stat().st_mtime >= since - 2]
    if not files:
        return None
    return max(files, key=lambda p: p.stat().st_mtime)


def parse_session(path: Path) -> list[dict]:
    out = []
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return out
    for line in text.splitlines():
        try:
            out.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return out


def users(entries: list[dict]) -> list[str]:
    return [e.get("text", "") for e in entries if e.get("kind") == "user"]


def turn_done_after(entries: list[dict], user_text: str) -> bool:
    idx = None
    for i, e in enumerate(entries):
        if e.get("kind") == "user" and e.get("text") == user_text:
            idx = i
    if idx is None:
        return False
    rest = entries[idx + 1 :]
    pending: set[str] = set()
    last_had_calls = False
    saw_assistant = False
    for e in rest:
        kind = e.get("kind")
        if kind == "user":
            break
        if kind == "assistant":
            saw_assistant = True
            pending = {c.get("id", "") for c in e.get("calls") or [] if c.get("id")}
            last_had_calls = bool(pending)
        elif kind == "tool":
            pending.discard(e.get("id", ""))
    return saw_assistant and not pending and not last_had_calls


def reset_workspace() -> None:
    WS.mkdir(parents=True, exist_ok=True)
    for p in WS.iterdir():
        if p.is_file():
            p.unlink()
        else:
            shutil.rmtree(p)


def main() -> None:
    atexit.register(cleanup)
    x11.XSetErrorHandler(_KEEP_HANDLER)

    ffmpeg = os.environ.get("FF") or str(Path.home() / ".local/bin/ffmpeg")
    if not os.access(ffmpeg, os.X_OK):
        ffmpeg = shutil.which("ffmpeg") or ""
    if not ffmpeg:
        die("ffmpeg not found")
    if not shutil.which("xterm"):
        die("xterm not found")

    auth = Path.home() / ".local/share/fun/auth.json"
    if not auth.exists():
        die("not logged in — run `fun login` once, then re-record")

    subprocess.check_call(["cargo", "build", "-q", "--manifest-path", str(ROOT / "Cargo.toml")])
    if not FUN.exists():
        die(f"missing {FUN}")

    reset_workspace()
    (ROOT / "docs").mkdir(parents=True, exist_ok=True)
    started = time.time()
    try:
        SOCK.unlink()
    except OSError:
        pass

    env = os.environ.copy()
    env["TERM"] = "xterm-256color"
    env["COLORTERM"] = "truecolor"
    env.setdefault("LANG", "en_US.UTF-8")
    env.setdefault("LC_ALL", "en_US.UTF-8")
    xterm = subprocess.Popen(
        [
            "xterm",
            "-T",
            TITLE,
            "-title",
            TITLE,
            "-fa",
            "DejaVu Sans Mono",
            "-fs",
            "15",
            "-xrm",
            "*faceNameDoublesize: Noto Sans Mono CJK KR",
            "-geometry",
            f"{COLS}x{ROWS}+80+50",
            "-bg",
            "#12141c",
            "-fg",
            "#e2e6f1",
            "-cr",
            "#c4a7f7",
            "+sb",
            "-b",
            str(XTERM_BORDER),
            "-e",
            sys.executable,
            str(Path(__file__).resolve()),
            "--wrap",
            str(SOCK),
            str(FUN),
            "--dir",
            str(WS),
            "--new",
        ],
        env=env,
    )
    CHILDREN.append(xterm)

    for _ in range(50):
        if SOCK.exists():
            break
        time.sleep(0.1)
    else:
        die("pty wrapper socket not ready")

    dpy = x11.XOpenDisplay(None)
    if not dpy:
        die("cannot open X display")

    wid = 0
    for _ in range(50):
        wid = find_window(dpy, TITLE)
        if wid:
            break
        time.sleep(0.1)
    if not wid:
        die("xterm window not found")

    ev = c_int()
    er = c_int()
    redirected = bool(xcomp.XCompositeQueryExtension(dpy, byref(ev), byref(er)))
    if redirected:
        xcomp.XCompositeRedirectWindow(dpy, wid, CompositeRedirectAutomatic)
        x11.XSync(dpy, 0)

    root = x11.XDefaultRootWindow(dpy)
    hidden = False
    evb = c_int()
    erb = c_int()
    if xfixes.XFixesQueryExtension(dpy, byref(evb), byref(erb)):
        xfixes.XFixesHideCursor(dpy, root)
        hidden = True

    w, h = geometry(dpy, wid)
    if w < 2 or h < 2:
        die("bad window size")
    w -= w % 2
    h -= h % 2
    focus(dpy, wid, w, h)
    time.sleep(0.4)

    ff = subprocess.Popen(
        [
            ffmpeg,
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "bgr0",
            "-s",
            f"{w}x{h}",
            "-r",
            str(FPS),
            "-i",
            "-",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-crf",
            "18",
            "-preset",
            "fast",
            "-movflags",
            "+faststart",
            str(OUT),
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    CHILDREN.append(ff)
    assert ff.stdin is not None

    sess: Path | None = None
    step = "wait_ui"
    typed = 0
    mark = time.monotonic()
    last = None
    t0 = time.monotonic()
    nframes = int(MAX_SEC * FPS)
    cw, ch = cell_size(w, h)
    ptr = None
    item0, ctrls = queue_layout(2)
    print(
        f"capturing {w}x{h} window {hex(wid)} cell {cw:.1f}x{ch:.1f} queue row {item0} ctrls {ctrls}",
        file=sys.stderr,
    )

    def hover(name: str, item: int, rows: list[int]) -> tuple[int, int, int, int]:
        col = ctrls[name] + max(CTRL_W[name] // 2, 0)
        row = rows[item] if item < len(rows) else item0 + item
        pix = cell_xy(cw, ch, col, row)
        x11.XWarpPointer(dpy, 0, wid, 0, 0, 0, 0, pix[0], pix[1])
        x11.XFlush(dpy)
        return pix[0], pix[1], col, row

    def click(name: str, item: int, rows: list[int]) -> None:
        px, py, col, row = hover(name, item, rows)
        inject(sgr_click(col, row))
        print(f"inject {name} item {item} @ {col},{row} rows {rows}", file=sys.stderr)

    seen_rows: list[int] = []

    try:
        for i in range(nframes):
            now = time.monotonic()
            frame = grab_bgr(dpy, wid, w, h, redirected)
            if frame is None:
                if last is None:
                    die("window capture failed")
                frame = last
            last = frame
            qrows = find_queue_rows(frame, w, h, cw, ch, ctrls)
            if qrows != seen_rows:
                print(f"queue rows {qrows}", file=sys.stderr)
                seen_rows = qrows

            if xterm.poll() is not None and step not in {"done", "quit"}:
                step = "done"

            if sess is None:
                sess = newest_session(started)
            entries = parse_session(sess) if sess else []

            if step == "wait_ui" and now - mark >= 1.0:
                focus(dpy, wid, w, h)
                step = "type1"
                typed = 0
                mark = now
            elif step == "type1":
                if typed < len(P1):
                    if now - mark >= 1 / 16:
                        type_char(dpy, wid, P1[typed])
                        typed += 1
                        mark = now
                elif now - mark >= 0.35:
                    ensure_focus(dpy, wid)
                    tap(dpy, keycode(dpy, "Return"))
                    step = "wait_work"
                    mark = now
            elif step == "wait_work":
                # Queue during the first model call, before tools drain idle prompts.
                if now - mark >= 0.25:
                    step = "type2"
                    typed = 0
                    mark = now
            elif step == "type2":
                if typed < len(P2):
                    if now - mark >= 1 / 22:
                        type_char(dpy, wid, P2[typed])
                        typed += 1
                        mark = now
                elif now - mark >= 0.18:
                    ensure_focus(dpy, wid)
                    tap(dpy, keycode(dpy, "Return"))
                    step = "type3"
                    typed = 0
                    mark = now
            elif step == "type3":
                if now - mark < 0.18:
                    pass
                elif typed < len(P3):
                    if now - mark >= 1 / 22:
                        type_char(dpy, wid, P3[typed])
                        typed += 1
                        mark = now
                elif now - mark >= 0.18:
                    ensure_focus(dpy, wid)
                    tap(dpy, keycode(dpy, "Return"))
                    step = "show_q"
                    mark = now
            elif step == "show_q":
                if len(qrows) >= 2 and now - mark >= 0.35:
                    print(f"queue clicks rows={qrows} ctrls {ctrls}", file=sys.stderr)
                    step = "hover_down"
                    mark = now
                elif now - mark >= 25:
                    print(f"queue not visible in time rows={qrows}", file=sys.stderr)
                    step = "wait_end"
                    mark = now
            elif step == "hover_down":
                if len(qrows) < 2:
                    if now - mark >= 8:
                        print(f"queue lost before clicks rows={qrows}", file=sys.stderr)
                        ptr = None
                        step = "wait_end"
                        mark = now
                else:
                    px, py, _, _ = hover("move down", 0, qrows)
                    ptr = (px, py)
                    if now - mark >= 0.9:
                        click("move down", 0, qrows)
                        step = "after_down"
                        mark = now
            elif step == "after_down":
                if len(qrows) < 2:
                    if now - mark >= 8:
                        ptr = None
                        step = "wait_end"
                        mark = now
                else:
                    px, py, _, _ = hover("move down", 0, qrows)
                    ptr = (px, py)
                    if now - mark >= 0.9:
                        step = "hover_up"
                        mark = now
            elif step == "hover_up":
                if len(qrows) < 2:
                    if now - mark >= 8:
                        ptr = None
                        step = "wait_end"
                        mark = now
                else:
                    px, py, _, _ = hover("move up", 1, qrows)
                    ptr = (px, py)
                    if now - mark >= 0.9:
                        click("move up", 1, qrows)
                        step = "after_up"
                        mark = now
            elif step == "after_up":
                if len(qrows) < 2:
                    if now - mark >= 8:
                        ptr = None
                        step = "wait_end"
                        mark = now
                else:
                    px, py, _, _ = hover("move up", 1, qrows)
                    ptr = (px, py)
                    if now - mark >= 0.9:
                        step = "hover_steer"
                        mark = now
            elif step == "hover_steer":
                if not qrows:
                    if now - mark >= 8:
                        ptr = None
                        step = "wait_end"
                        mark = now
                else:
                    px, py, _, _ = hover("send now", 0, qrows)
                    ptr = (px, py)
                    if now - mark >= 0.9:
                        click("send now", 0, qrows)
                        step = "wait_end"
                        mark = now
                        ptr = None
            elif step == "wait_end":
                done = (
                    turn_done_after(entries, P1)
                    and turn_done_after(entries, P2)
                    and turn_done_after(entries, P3)
                )
                if (done and now - mark >= 3.5) or now - mark >= 90:
                    step = "quit"
                    mark = now
            elif step == "quit":
                if now - mark >= 0.5:
                    ensure_focus(dpy, wid)
                    tap(dpy, keycode(dpy, "c"), ctrl=True)
                    step = "done"
                    mark = now
            elif step == "done" and now - mark >= 1.0:
                break

            shown = overlay_pointer(frame, w, h, *ptr) if ptr else frame
            ff.stdin.write(shown)
            target = t0 + (i + 1) / FPS
            slack = target - time.monotonic()
            if slack > 0:
                time.sleep(slack)
    finally:
        try:
            ff.stdin.close()
        except OSError:
            pass
        if hidden:
            xfixes.XFixesShowCursor(dpy, root)
            x11.XFlush(dpy)
        x11.XCloseDisplay(dpy)

    rc = ff.wait()
    if rc != 0:
        err = ff.stderr.read().decode("utf-8", "replace") if ff.stderr else ""
        die(f"ffmpeg failed ({rc})\n{err[-2000:]}")
    duration = time.monotonic() - t0
    print(OUT, OUT.stat().st_size)
    print(f"wrote {OUT} ({duration:.1f}s)")


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--wrap":
        sys.argv = [sys.argv[0], *sys.argv[2:]]
        wrap_main()
    else:
        main()
