package webrtcp

import (
	"bytes"
	"context"
	"encoding/binary"
	"math"
	"time"

	"github.com/rs/zerolog"

	"github.com/m1k1o/neko/server/internal/webrtc/payload"
	"github.com/m1k1o/neko/server/internal/webrtcp/pb"
	"github.com/m1k1o/neko/server/pkg/types"
	"github.com/m1k1o/neko/server/pkg/types/event"
	"github.com/m1k1o/neko/server/pkg/types/message"

	"github.com/pion/webrtc/v4"
)

//
// outbound data-channel payload encoding (neko binary protocol,
// mirrored from the embedded implementation)
//

func encodeCursorPosition(x, y int) ([]byte, error) {
	header := payload.Header{
		Event:  payload.OP_CURSOR_POSITION,
		Length: 7,
	}

	data := payload.CursorPosition{
		X: uint16(x),
		Y: uint16(y),
	}

	buffer := &bytes.Buffer{}

	if err := binary.Write(buffer, binary.BigEndian, header); err != nil {
		return nil, err
	}

	if err := binary.Write(buffer, binary.BigEndian, data); err != nil {
		return nil, err
	}

	return buffer.Bytes(), nil
}

func encodeCursorImage(cur *types.CursorImage, img []byte) ([]byte, error) {
	header := payload.Header{
		Event:  payload.OP_CURSOR_IMAGE,
		Length: uint16(11 + len(img)),
	}

	data := payload.CursorImage{
		Width:  cur.Width,
		Height: cur.Height,
		Xhot:   cur.Xhot,
		Yhot:   cur.Yhot,
	}

	buffer := &bytes.Buffer{}

	if err := binary.Write(buffer, binary.BigEndian, header); err != nil {
		return nil, err
	}

	if err := binary.Write(buffer, binary.BigEndian, data); err != nil {
		return nil, err
	}

	if err := binary.Write(buffer, binary.BigEndian, img); err != nil {
		return nil, err
	}

	return buffer.Bytes(), nil
}

func encodePong(ping payload.Ping) ([]byte, error) {
	serverTs := uint64(time.Now().UnixMilli())

	header := payload.Header{
		Event:  payload.OP_PONG,
		Length: 19,
	}

	pong := payload.Pong{
		Ping:      ping,
		ServerTs1: uint32(serverTs / math.MaxUint32),
		ServerTs2: uint32(serverTs % math.MaxUint32),
	}

	buffer := &bytes.Buffer{}

	if err := binary.Write(buffer, binary.BigEndian, header); err != nil {
		return nil, err
	}

	if err := binary.Write(buffer, binary.BigEndian, pong); err != nil {
		return nil, err
	}

	return buffer.Bytes(), nil
}

//
// dataplane event stream
//

// eventsLoop consumes the dataplane event stream, reconnecting on errors
// until shutdown.
func (m *Manager) eventsLoop() {
	for {
		if m.isShutdown() {
			return
		}

		err := m.consumeEvents()
		if m.isShutdown() {
			return
		}

		m.logger.Err(err).Msg("dataplane events stream lost, reconnecting")
		time.Sleep(time.Second)
	}
}

func (m *Manager) consumeEvents() error {
	ctx := context.Background()

	stream, err := m.client.Events(ctx, &pb.EventsRequest{})
	if err != nil {
		return err
	}

	m.logger.Info().Msg("dataplane events stream connected")

	for {
		ev, err := stream.Recv()
		if err != nil {
			return err
		}

		m.dispatchEvent(ev)
	}
}

func (m *Manager) dispatchEvent(ev *pb.Event) {
	logger := m.logger

	switch kind := ev.Kind.(type) {
	case *pb.Event_IceCandidate:
		m.handleIceCandidate(kind.IceCandidate, logger)

	case *pb.Event_ConnectionState:
		m.handleConnectionState(kind.ConnectionState, logger)

	case *pb.Event_DataChannel:
		m.handleDataChannelEvent(kind.DataChannel, logger)

	case *pb.Event_DataMessage:
		m.handleDataMessage(kind.DataMessage, logger)
	}
}

func (m *Manager) handleIceCandidate(e *pb.IceCandidateEvent, logger zerolog.Logger) {
	peer := m.getPeer(e.SessionId)
	if peer == nil {
		logger.Debug().Str("session_id", e.SessionId).Msg("ice candidate for unknown session")
		return
	}

	candidate := webrtc.ICECandidateInit{
		Candidate: e.Candidate.Candidate,
	}
	if e.Candidate.SdpMid != nil {
		candidate.SDPMid = e.Candidate.SdpMid
	}
	if e.Candidate.SdpMlineIndex != nil {
		mline := uint16(*e.Candidate.SdpMlineIndex)
		candidate.SDPMLineIndex = &mline
	}
	if e.Candidate.UsernameFragment != nil {
		candidate.UsernameFragment = e.Candidate.UsernameFragment
	}

	peer.session.Send(event.SIGNAL_CANDIDATE, message.SignalCandidate{
		ICECandidateInit: candidate,
	})
}

func (m *Manager) handleConnectionState(e *pb.ConnectionStateEvent, logger zerolog.Logger) {
	peer := m.getPeer(e.SessionId)
	if peer == nil {
		logger.Debug().Str("session_id", e.SessionId).Str("state", e.State).Msg("connection state for unknown session")
		return
	}

	logger.Info().Str("session_id", e.SessionId).Str("state", e.State).Msg("peer connection state changed")

	switch e.State {
	case "connected":
		peer.session.SetWebRTCConnected(peer, true)
	case "disconnected", "failed":
		peer.Destroy()
	case "closed":
		peer.session.SetWebRTCConnected(peer, false)
	}
}

func (m *Manager) handleDataChannelEvent(e *pb.DataChannelEvent, logger zerolog.Logger) {
	peer := m.getPeer(e.SessionId)
	if peer == nil {
		return
	}

	m.dataMu.Lock()
	m.dataChannels[e.SessionId] = e.Open
	m.dataMu.Unlock()

	if !e.Open {
		return
	}

	logger.Debug().Str("session_id", e.SessionId).Msg("data channel opened, sending initial cursor state")

	// mirror the embedded manager's OnOpen behavior
	cur, img, err := m.curImage.GetCurrent()
	if err == nil {
		if err := peer.SendCursorImage(cur, img); err != nil {
			logger.Err(err).Msg("failed to send initial cursor image")
		}
	}

	x, y := m.desktop.GetCursorPosition()
	if err := peer.SendCursorPosition(x, y); err != nil {
		logger.Err(err).Msg("failed to send initial cursor position")
	}
}

// handleDataMessage processes an inbound data-channel message. The neko
// binary payload format and all input-injection semantics stay in the
// control plane; the dataplane forwards opaque bytes.
func (m *Manager) handleDataMessage(msg *pb.DataMessage, logger zerolog.Logger) {
	peer := m.getPeer(msg.SessionId)
	if peer == nil {
		return
	}

	buffer := bytes.NewBuffer(msg.Data)

	header := &payload.Header{}
	if err := binary.Read(buffer, binary.BigEndian, header); err != nil {
		logger.Err(err).Msg("failed to parse data channel message header")
		return
	}

	isHost := peer.session.IsHost()

	switch header.Event {
	case payload.OP_MOVE:
		move := &payload.Move{}
		if err := binary.Read(buffer, binary.BigEndian, move); err != nil {
			logger.Err(err).Msg("failed to parse move payload")
			return
		}

		x, y := int(move.X), int(move.Y)
		if isHost {
			m.desktop.Move(x, y)
			m.curPosition.Set(x, y)
		} else {
			peer.session.SetCursor(types.Cursor{X: x, Y: y})
		}

	case payload.OP_PING:
		ping := &payload.Ping{}
		if err := binary.Read(buffer, binary.BigEndian, ping); err != nil {
			logger.Err(err).Msg("failed to parse ping payload")
			return
		}

		pong, err := encodePong(*ping)
		if err != nil {
			logger.Err(err).Msg("failed to encode pong")
			return
		}
		m.sendData(msg.SessionId, pong)
	}

	// continue only if session is host
	if !isHost {
		return
	}

	switch header.Event {
	case payload.OP_SCROLL:
		// TODO: remove this once the client is fixed
		if header.Length == 4 {
			scroll := &payload.Scroll_Old{}
			if err := binary.Read(buffer, binary.BigEndian, scroll); err != nil {
				logger.Err(err).Msg("failed to parse scroll payload")
				return
			}

			m.desktop.Scroll(int(scroll.X), int(scroll.Y), false)
		} else {
			scroll := &payload.Scroll{}
			if err := binary.Read(buffer, binary.BigEndian, scroll); err != nil {
				logger.Err(err).Msg("failed to parse scroll payload")
				return
			}

			m.desktop.Scroll(int(scroll.DeltaX), int(scroll.DeltaY), scroll.ControlKey)
		}

	case payload.OP_KEY_DOWN:
		key := &payload.Key{}
		if err := binary.Read(buffer, binary.BigEndian, key); err != nil {
			logger.Err(err).Msg("failed to parse key payload")
			return
		}

		if err := m.desktop.KeyDown(key.Key); err != nil {
			logger.Warn().Err(err).Uint32("key", key.Key).Msg("key down failed")
		}

	case payload.OP_KEY_UP:
		key := &payload.Key{}
		if err := binary.Read(buffer, binary.BigEndian, key); err != nil {
			logger.Err(err).Msg("failed to parse key payload")
			return
		}

		if err := m.desktop.KeyUp(key.Key); err != nil {
			logger.Warn().Err(err).Uint32("key", key.Key).Msg("key up failed")
		}

	case payload.OP_BTN_DOWN:
		btn := &payload.Key{}
		if err := binary.Read(buffer, binary.BigEndian, btn); err != nil {
			logger.Err(err).Msg("failed to parse button payload")
			return
		}

		if err := m.desktop.ButtonDown(btn.Key); err != nil {
			logger.Warn().Err(err).Uint32("key", btn.Key).Msg("button down failed")
		}

	case payload.OP_BTN_UP:
		btn := &payload.Key{}
		if err := binary.Read(buffer, binary.BigEndian, btn); err != nil {
			logger.Err(err).Msg("failed to parse button payload")
			return
		}

		if err := m.desktop.ButtonUp(btn.Key); err != nil {
			logger.Warn().Err(err).Uint32("key", btn.Key).Msg("button up failed")
		}

	case payload.OP_TOUCH_BEGIN:
		touch := &payload.Touch{}
		if err := binary.Read(buffer, binary.BigEndian, touch); err != nil {
			logger.Err(err).Msg("failed to parse touch payload")
			return
		}

		if err := m.desktop.TouchBegin(touch.TouchId, int(touch.X), int(touch.Y), touch.Pressure); err != nil {
			logger.Warn().Err(err).Uint32("touch_id", touch.TouchId).Msg("touch begin failed")
		}

	case payload.OP_TOUCH_UPDATE:
		touch := &payload.Touch{}
		if err := binary.Read(buffer, binary.BigEndian, touch); err != nil {
			logger.Err(err).Msg("failed to parse touch payload")
			return
		}

		if err := m.desktop.TouchUpdate(touch.TouchId, int(touch.X), int(touch.Y), touch.Pressure); err != nil {
			logger.Warn().Err(err).Uint32("touch_id", touch.TouchId).Msg("touch update failed")
		}

	case payload.OP_TOUCH_END:
		touch := &payload.Touch{}
		if err := binary.Read(buffer, binary.BigEndian, touch); err != nil {
			logger.Err(err).Msg("failed to parse touch payload")
			return
		}

		if err := m.desktop.TouchEnd(touch.TouchId, int(touch.X), int(touch.Y), touch.Pressure); err != nil {
			logger.Warn().Err(err).Uint32("touch_id", touch.TouchId).Msg("touch end failed")
		}

	default:
		logger.Trace().
			Str("session_id", msg.SessionId).
			Uint8("op", header.Event).
			Msg("unhandled data channel message")
	}
}
