// Command neko-webshell is a raw WebSocket <-> PTY bridge for the client's
// xterm.js terminal tab. Go replacement for the retired python build
// (apps/chromium/webshell.py), protocol-compatible on purpose.
//
// Each WebSocket connection gets its own login shell running as the invoking
// user (supervisord starts this with user=neko), so every room member has a
// private terminal inside the container - `pi` (the AI agent CLI) included.
//
// Protocol (per connection):
//   - server -> client: binary WebSocket message = raw terminal bytes
//   - client -> server: binary WebSocket message = raw terminal bytes
//   - client -> server: text message {"type":"resize","cols":N,"rows":M}
//     resizes the PTY; any other text frame is raw UTF-8 terminal input
//     (xterm.js onData sends text frames - the python build silently
//     dropped those, only binary frames reached the shell)
package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"net/http"
	"os"
	"os/exec"
	"sync"
	"syscall"
	"time"

	"github.com/creack/pty"
	"github.com/gorilla/websocket"
)

const (
	// match the python build's websockets.serve(ping_interval=30)
	pingInterval = 30 * time.Second
	ptyChunkSize = 32 * 1024
)

var upgrader = websocket.Upgrader{
	// the python build accepted any origin
	CheckOrigin: func(r *http.Request) bool { return true },
}

func main() {
	host := flag.String("host", "0.0.0.0", "listen host")
	port := flag.Int("port", 7681, "listen port")
	shell := flag.String("shell", "/bin/bash", "shell to run per connection")
	flag.Parse()

	http.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		serve(w, r, *shell)
	})

	log.Printf("[webshell] listening on %s:%d, shell=%s", *host, *port, *shell)
	log.Fatal(http.ListenAndServe(fmt.Sprintf("%s:%d", *host, *port), nil))
}

func serve(w http.ResponseWriter, r *http.Request, shell string) {
	ws, err := upgrader.Upgrade(w, r, nil)
	if err != nil {
		return
	}

	cmd := exec.Command(shell, "-l")
	cmd.Env = append(os.Environ(),
		"TERM=xterm-256color",
		"COLORTERM=truecolor",
	)
	ptmx, err := pty.StartWithSize(cmd, &pty.Winsize{Cols: 80, Rows: 24})
	if err != nil {
		log.Printf("[webshell] pty start: %v", err)
		ws.Close()
		return
	}

	var once sync.Once
	closeOnce := func() {
		// hang up the shell and reap it, like the python build's SIGHUP
		if cmd.Process != nil {
			_ = cmd.Process.Signal(syscall.SIGHUP)
		}
		_ = ptmx.Close()
		go func() { _ = cmd.Wait() }()
		_ = ws.Close()
	}

	// PTY -> WebSocket: binary frames; the 30s protocol pings go through the
	// same writer so messages never interleave
	out := make(chan []byte, 16)
	go func() {
		buf := make([]byte, ptyChunkSize)
		for {
			n, err := ptmx.Read(buf)
			if n > 0 {
				chunk := make([]byte, n)
				copy(chunk, buf[:n])
				out <- chunk
			}
			if err != nil {
				out <- nil // EOF: child exited (EIO on linux once reaped)
				return
			}
		}
	}()

	go func() {
		ticker := time.NewTicker(pingInterval)
		defer ticker.Stop()
		for {
			select {
			case data := <-out:
				if data == nil {
					once.Do(closeOnce)
					return
				}
				if err := ws.WriteMessage(websocket.BinaryMessage, data); err != nil {
					once.Do(closeOnce)
					return
				}
			case <-ticker.C:
				deadline := time.Now().Add(10 * time.Second)
				if err := ws.WriteControl(websocket.PingMessage, nil, deadline); err != nil {
					once.Do(closeOnce)
					return
				}
			}
		}
	}()

	// WebSocket -> PTY
	for {
		msgType, data, err := ws.ReadMessage()
		if err != nil {
			break
		}
		if msgType == websocket.TextMessage && applyResize(ptmx, data) {
			continue
		}
		if _, err := ptmx.Write(data); err != nil {
			break
		}
	}
	once.Do(closeOnce)
}

// applyResize interprets a text frame as {"type":"resize","cols":N,"rows":M}
// and applies it to the PTY, reporting whether it did. Anything else
// (non-JSON, other types, nonsense sizes) is terminal input.
func applyResize(ptmx *os.File, data []byte) bool {
	var m struct {
		Type string `json:"type"`
		Cols int    `json:"cols"`
		Rows int    `json:"rows"`
	}
	if err := json.Unmarshal(data, &m); err != nil || m.Type != "resize" {
		return false
	}
	if m.Cols <= 0 || m.Rows <= 0 || m.Cols > 0xffff || m.Rows > 0xffff {
		return true // a resize, just a broken one: consumed, not typed
	}
	return pty.Setsize(ptmx, &pty.Winsize{Cols: uint16(m.Cols), Rows: uint16(m.Rows)}) == nil
}
