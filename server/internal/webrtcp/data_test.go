package webrtcp

import (
	"encoding/binary"
	"testing"

	"github.com/m1k1o/neko/server/internal/webrtc/payload"
	"github.com/m1k1o/neko/server/pkg/types"
)

// The binary payloads produced here are consumed by browsers (cursor
// overlay, latency measurement); the format must match the embedded
// implementation in server/internal/webrtc exactly.

func TestEncodeCursorPosition(t *testing.T) {
	data, err := encodeCursorPosition(258, 515)
	if err != nil {
		t.Fatal(err)
	}

	// header: event=OP_CURSOR_POSITION, length=7
	want := []byte{
		payload.OP_CURSOR_POSITION, 0x00, 0x07,
		0x01, 0x02, // x = 258
		0x02, 0x03, // y = 515
	}

	if len(data) != len(want) {
		t.Fatalf("length = %d, want %d", len(data), len(want))
	}
	for i := range want {
		if data[i] != want[i] {
			t.Fatalf("byte %d = %#x, want %#x (full: %v)", i, data[i], want[i], data)
		}
	}
}

func TestEncodeCursorImage(t *testing.T) {
	img := []byte{0xde, 0xad, 0xbe, 0xef}
	cur := &types.CursorImage{
		Width:  640,
		Height: 480,
		Xhot:   2,
		Yhot:   3,
	}

	data, err := encodeCursorImage(cur, img)
	if err != nil {
		t.Fatal(err)
	}

	if data[0] != payload.OP_CURSOR_IMAGE {
		t.Fatalf("event = %#x, want %#x", data[0], payload.OP_CURSOR_IMAGE)
	}

	length := binary.BigEndian.Uint16(data[1:3])
	if int(length) != 11+len(img) {
		t.Fatalf("length = %d, want %d", length, 11+len(img))
	}

	// body: width, height, xhot, yhot then image bytes
	if data[3] != 0x02 || data[4] != 0x80 { // 640
		t.Fatalf("width bytes = %v", data[3:5])
	}
	if data[11] != 0xde || data[14] != 0xef {
		t.Fatalf("image bytes = %v", data[11:])
	}
}

func TestEncodePong(t *testing.T) {
	ping := payload.Ping{ClientTs1: 1, ClientTs2: 2}

	data, err := encodePong(ping)
	if err != nil {
		t.Fatal(err)
	}

	// header: event=OP_PONG, length=19 (total including header);
	// body: ping (8) + server ts (8)
	if len(data) != 19 {
		t.Fatalf("length = %d, want %d", len(data), 19)
	}
	if data[0] != payload.OP_PONG || data[1] != 0x00 || data[2] != 0x13 {
		t.Fatalf("header = %v", data[:3])
	}
	if binary.BigEndian.Uint32(data[3:7]) != 1 || binary.BigEndian.Uint32(data[7:11]) != 2 {
		t.Fatalf("client ts not preserved: %v", data[3:11])
	}
	if binary.BigEndian.Uint64(data[11:19]) == 0 {
		t.Fatal("server timestamp is zero")
	}
}
