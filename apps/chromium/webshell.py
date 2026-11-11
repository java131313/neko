#!/usr/bin/env python3
"""neko webshell: raw WebSocket <-> PTY bridge for the client's xterm.js terminal tab.

Each WebSocket connection gets its own login shell running as the container user
(supervisord starts this with user=neko), so every room member has a private
terminal inside the container - `pi` (the AI agent CLI) included.

Protocol (per connection):
- either direction: binary WebSocket message = raw terminal bytes
- client -> server: JSON text {"type": "resize", "cols": N, "rows": M}
"""
import argparse
import asyncio
import fcntl
import json
import os
import pty
import signal
import struct
import sys
import termios

SHELL = "/bin/bash"


def set_winsize(master: int, cols: int, rows: int) -> None:
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


async def handle(conn, *_args) -> None:
    loop = asyncio.get_running_loop()
    pid, master = pty.fork()
    if pid == 0:
        # child: pty.fork() made the slave our controlling terminal
        env = dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor")
        os.execvpe(SHELL, [SHELL, "-l"], env)
    # parent
    closed = False

    def close_conn() -> None:
        # websockets >= 12 made close() a coroutine; older versions return None
        try:
            result = conn.close()
            if asyncio.iscoroutine(result):
                asyncio.ensure_future(result)
        except Exception:
            pass

    def cleanup() -> None:
        nonlocal closed
        if closed:
            return
        closed = True
        loop.remove_reader(master)
        try:
            os.close(master)
        except OSError:
            pass
        # shell still running (client went away): hang it up and reap it
        try:
            os.kill(pid, signal.SIGHUP)
        except (ProcessLookupError, PermissionError):
            pass
        asyncio.ensure_future(reap(pid))

    async def send(data: bytes) -> None:
        try:
            await conn.send(data)
        except Exception:
            pass

    def pump() -> None:
        try:
            data = os.read(master, 65536)
        except OSError:
            data = b""  # child exited and reaped: EIO on linux
        if data:
            asyncio.ensure_future(send(data))
        else:
            cleanup()
            close_conn()

    loop.add_reader(master, pump)
    try:
        async for msg in conn:
            if closed:
                break
            if isinstance(msg, (bytes, bytearray)):
                try:
                    os.write(master, bytes(msg))
                except OSError:
                    break
            else:
                try:
                    m = json.loads(msg)
                except (ValueError, TypeError):
                    continue
                if m.get("type") == "resize":
                    try:
                        set_winsize(master, int(m["cols"]), int(m["rows"]))
                    except (KeyError, ValueError, OSError):
                        pass
    except Exception:
        pass
    finally:
        cleanup()
        close_conn()


async def reap(pid: int) -> None:
    try:
        os.waitpid(pid, 0)
    except (ChildProcessError, OSError):
        pass


def main() -> None:
    global SHELL
    ap = argparse.ArgumentParser(description="neko webshell: WebSocket <-> PTY bridge")
    ap.add_argument("--host", default="0.0.0.0")
    ap.add_argument("--port", type=int, default=7681)
    ap.add_argument("--shell", default=SHELL)
    args = ap.parse_args()
    SHELL = args.shell

    try:
        import websockets
    except ImportError:
        sys.exit("python3-websockets is required (apt-get install python3-websockets)")

    async def run() -> None:
        async with websockets.serve(handle, args.host, args.port, max_size=None, ping_interval=30):
            print(f"[webshell] listening on {args.host}:{args.port}, shell={SHELL}", flush=True)
            await asyncio.Future()  # serve forever

    asyncio.run(run())


if __name__ == "__main__":
    main()
