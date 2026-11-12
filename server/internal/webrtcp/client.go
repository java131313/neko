package webrtcp

import (
	"context"
	"fmt"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"

	"github.com/m1k1o/neko/server/internal/config"
	"github.com/m1k1o/neko/server/internal/webrtc/cursor"
	"github.com/m1k1o/neko/server/internal/webrtcp/pb"
	"github.com/m1k1o/neko/server/pkg/types"

	"github.com/pion/webrtc/v4"
)

// WebRTC manager implementation that delegates the whole media path to the
// Rust dataplane process (neko-dataplane) over a Unix socket, implementing
// the same types.WebRTCManager interface as the embedded pion-based
// manager. Selected by setting NEKO_WEBRTC_DATAPLANE.
type Manager struct {
	logger  zerolog.Logger
	desktop types.DesktopManager
	capture types.CaptureManager
	config  *config.WebRTC

	addr string // unix socket path (unix:// prefix allowed)

	mu     sync.Mutex
	conn   *grpc.ClientConn
	client pb.DataplaneClient

	shutdown bool

	// session id -> peer created by this manager
	peers map[string]*Peer

	// session ids whose "data" channel is currently open
	dataMu       sync.Mutex
	dataChannels map[string]bool

	curImage    cursor.Image
	curPosition cursor.Position
}

func New(desktop types.DesktopManager, capture types.CaptureManager, config *config.WebRTC, addr string) *Manager {
	logger := log.With().Str("module", "webrtcp").Logger()

	return &Manager{
		logger:       logger,
		desktop:      desktop,
		capture:      capture,
		config:       config,
		addr:         addr,
		peers:        map[string]*Peer{},
		dataChannels: map[string]bool{},
		curImage:     cursor.NewImage(logger, desktop),
		curPosition:  cursor.NewPosition(logger),
	}
}

// Connects to the dataplane socket with retries, then pushes the resolved
// WebRTC configuration and starts the event stream.
func (m *Manager) Start() {
	// grpc-go dials unix sockets natively via the "unix" scheme; a
	// passthrough target with a custom dialer produces an empty :authority,
	// which tonic's h2 rejects with PROTOCOL_ERROR
	dialTarget := "unix://" + strings.TrimPrefix(m.addr, "unix://")

	var conn *grpc.ClientConn
	for {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		c, err := grpc.DialContext(ctx, dialTarget,
			grpc.WithTransportCredentials(insecure.NewCredentials()),
			grpc.WithBlock(),
		)
		cancel()
		if err == nil {
			conn = c
			break
		}
		if m.isShutdown() {
			return
		}
		m.logger.Warn().Err(err).Str("addr", m.addr).Msg("dataplane not available, retrying")
		time.Sleep(time.Second)
	}

	m.mu.Lock()
	m.conn = conn
	m.client = pb.NewDataplaneClient(conn)
	m.mu.Unlock()

	m.logger.Info().Str("addr", m.addr).Msg("connected to dataplane")

	if err := m.configure(); err != nil {
		m.logger.Panic().Err(err).Msg("dataplane configure failed")
	}

	go m.eventsLoop()
}

func (m *Manager) Shutdown() error {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.shutdown = true
	if m.conn != nil {
		return m.conn.Close()
	}
	return nil
}

func (m *Manager) ICEServers() []types.ICEServer {
	return m.config.ICEServersFrontend
}

func (m *Manager) CreatePeer(session types.Session) (*webrtc.SessionDescription, types.WebRTCPeer, error) {
	m.mu.Lock()
	defer m.mu.Unlock()

	_, err := m.client.CreatePeer(context.Background(), &pb.CreatePeerRequest{
		SessionId: session.ID(),
	})
	if err != nil {
		return nil, nil, fmt.Errorf("dataplane create peer: %w", err)
	}

	// TODO(phase 2): the answer carries the generated offer SDP.
	offer := &webrtc.SessionDescription{}

	peer := NewPeer(m, session)
	m.peers[session.ID()] = peer
	session.SetWebRTCPeer(peer)

	return offer, peer, nil
}

func (m *Manager) SetCursorPosition(x, y int) {
	m.curPosition.Set(x, y)

	data, err := encodeCursorPosition(x, y)
	if err != nil {
		m.logger.Err(err).Msg("failed to encode cursor position")
		return
	}

	// send to all peers with an open data channel, except the host
	for _, sessionID := range m.openDataChannels() {
		if peer := m.getPeer(sessionID); peer != nil && peer.session.IsHost() {
			continue
		}
		m.sendData(sessionID, data)
	}
}

//
// internals
//

func (m *Manager) configure() error {
	iceServers := make([]*pb.IceServer, 0, len(m.config.ICEServersBackend))
	for _, s := range m.config.ICEServersBackend {
		iceServers = append(iceServers, &pb.IceServer{
			Urls:       s.URLs,
			Username:   s.Username,
			Credential: s.Credential,
		})
	}

	// single video stream for now: "main" if present, else the first id
	dpc := types.DataplaneCapture{Videos: map[string]string{}}
	if m.capture != nil {
		var err error
		dpc, err = m.capture.DataplanePipelines()
		if err != nil {
			return fmt.Errorf("assemble capture pipelines: %w", err)
		}
	}

	videoPipeline := dpc.Videos["main"]
	if videoPipeline == "" {
		ids := make([]string, 0, len(dpc.Videos))
		for id := range dpc.Videos {
			ids = append(ids, id)
		}
		sort.Strings(ids)
		if len(ids) > 0 {
			videoPipeline = dpc.Videos[ids[0]]
		}
	}

	m.logger.Info().
		Str("video_codec", dpc.VideoCodec).
		Strs("video_streams", sortedKeys(dpc.Videos)).
		Str("video_pipeline", videoPipeline).
		Bool("audio", dpc.Audio != "").
		Msg("dataplane capture configuration")

	_, err := m.client.Configure(context.Background(), &pb.ConfigureRequest{
		Config: &pb.WebRtcConfig{
			IceLite:           m.config.ICELite,
			IceTrickle:        m.config.ICETrickle,
			IceServersBackend: iceServers,
			EphemeralMin:      uint32(m.config.EphemeralMin),
			EphemeralMax:      uint32(m.config.EphemeralMax),
			TcpMux:            int32(m.config.TCPMux),
			UdpMux:            int32(m.config.UDPMux),
			Nat1To1Ips:        m.config.NAT1To1IPs,
		},
		Capture: &pb.CaptureConfig{
			Display:       dpc.Display,
			AudioDevice:   dpc.AudioDevice,
			VideoPipeline: videoPipeline,
			AudioPipeline: dpc.Audio,
			VideoCodec:    dpc.VideoCodec,
			AudioCodec:    dpc.AudioCodec,
		},
	})
	return err
}

func sortedKeys(m map[string]string) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}

func (m *Manager) isShutdown() bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.shutdown
}

func (m *Manager) getPeer(sessionID string) *Peer {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.peers[sessionID]
}

func (m *Manager) removePeer(sessionID string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	delete(m.peers, sessionID)

	m.dataMu.Lock()
	delete(m.dataChannels, sessionID)
	m.dataMu.Unlock()
}

func (m *Manager) openDataChannels() []string {
	m.dataMu.Lock()
	defer m.dataMu.Unlock()

	ids := make([]string, 0, len(m.dataChannels))
	for id := range m.dataChannels {
		ids = append(ids, id)
	}
	return ids
}

// sendData writes a data-channel message for one session, or to all peers
// when sessionID is empty.
func (m *Manager) sendData(sessionID string, data []byte) {
	_, err := m.client.SendData(context.Background(), &pb.SendDataRequest{
		SessionId: sessionID,
		Data:      data,
	})
	if err != nil {
		m.logger.Err(err).Str("session_id", sessionID).Msg("dataplane send data failed")
	}
}
