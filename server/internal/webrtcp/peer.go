package webrtcp

import (
	"context"
	"sync"

	"github.com/m1k1o/neko/server/internal/webrtcp/pb"
	"github.com/m1k1o/neko/server/pkg/types"

	"github.com/pion/webrtc/v4"
)

// Peer implements types.WebRTCPeer backed by a dataplane-side peer
// connection. State that the control plane must answer synchronously
// (paused/video/audio) is mirrored locally; everything else is an RPC.
type Peer struct {
	manager *Manager
	session types.Session

	mu        sync.Mutex
	paused    bool
	video     types.PeerVideo
	audio     types.PeerAudio
	destroyed bool
}

func NewPeer(manager *Manager, session types.Session) *Peer {
	return &Peer{
		manager: manager,
		session: session,
		video: types.PeerVideo{
			Auto: true,
		},
	}
}

func (p *Peer) CreateOffer(iceRestart bool) (*webrtc.SessionDescription, error) {
	res, err := p.manager.client.CreateOffer(context.Background(), &pb.CreateOfferRequest{
		SessionId:  p.session.ID(),
		IceRestart: iceRestart,
	})
	if err != nil {
		return nil, err
	}
	return &webrtc.SessionDescription{
		Type: webrtc.SDPTypeOffer,
		SDP:  res.Sdp,
	}, nil
}

func (p *Peer) CreateAnswer() (*webrtc.SessionDescription, error) {
	res, err := p.manager.client.CreateAnswer(context.Background(), &pb.CreateAnswerRequest{
		SessionId: p.session.ID(),
	})
	if err != nil {
		return nil, err
	}
	return &webrtc.SessionDescription{
		Type: webrtc.SDPTypeAnswer,
		SDP:  res.Sdp,
	}, nil
}

func (p *Peer) SetRemoteDescription(sessionDescription webrtc.SessionDescription) error {
	_, err := p.manager.client.SetRemoteDescription(context.Background(), &pb.SetRemoteDescriptionRequest{
		SessionId: p.session.ID(),
		Type:      sessionDescription.Type.String(),
		Sdp:       sessionDescription.SDP,
	})
	return err
}

func (p *Peer) SetCandidate(candidate webrtc.ICECandidateInit) error {
	req := &pb.AddIceCandidateRequest{
		SessionId: p.session.ID(),
		Candidate: &pb.IceCandidateInit{
			Candidate: candidate.Candidate,
		},
	}
	if candidate.SDPMid != nil {
		req.Candidate.SdpMid = candidate.SDPMid
	}
	if candidate.SDPMLineIndex != nil {
		mline := uint32(*candidate.SDPMLineIndex)
		req.Candidate.SdpMlineIndex = &mline
	}
	if candidate.UsernameFragment != nil {
		req.Candidate.UsernameFragment = candidate.UsernameFragment
	}

	_, err := p.manager.client.AddIceCandidate(context.Background(), req)
	return err
}

func (p *Peer) SetPaused(isPaused bool) error {
	p.mu.Lock()
	p.paused = isPaused
	p.mu.Unlock()

	_, err := p.manager.client.SetPaused(context.Background(), &pb.SetPausedRequest{
		SessionId: p.session.ID(),
		Paused:    isPaused,
	})
	return err
}

func (p *Peer) Paused() bool {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.paused
}

func (p *Peer) SetVideo(request types.PeerVideoRequest) error {
	req := &pb.SetVideoRequest{
		SessionId: p.session.ID(),
	}
	if request.Disabled != nil {
		req.Disabled = request.Disabled
	}
	if request.Auto != nil {
		req.Auto = request.Auto
	}
	if request.Selector != nil {
		req.Selector = &pb.StreamSelector{
			Id:   request.Selector.ID,
			Type: request.Selector.Type.String(),
		}
	}

	// mirror what the embedded implementation would resolve to; the
	// dataplane re-resolves against its own streams
	p.mu.Lock()
	if request.Disabled != nil {
		p.video.Disabled = *request.Disabled
	}
	if request.Auto != nil {
		p.video.Auto = *request.Auto
	}
	if request.Selector != nil {
		p.video.ID = request.Selector.ID
	}
	p.mu.Unlock()

	_, err := p.manager.client.SetVideo(context.Background(), req)
	return err
}

func (p *Peer) Video() types.PeerVideo {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.video
}

func (p *Peer) SetAudio(request types.PeerAudioRequest) error {
	req := &pb.SetAudioRequest{
		SessionId: p.session.ID(),
	}
	if request.Disabled != nil {
		req.Disabled = request.Disabled
	}

	p.mu.Lock()
	if request.Disabled != nil {
		p.audio.Disabled = *request.Disabled
	}
	p.mu.Unlock()

	_, err := p.manager.client.SetAudio(context.Background(), req)
	return err
}

func (p *Peer) Audio() types.PeerAudio {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.audio
}

func (p *Peer) SendCursorPosition(x, y int) error {
	data, err := encodeCursorPosition(x, y)
	if err != nil {
		return err
	}
	p.manager.sendData(p.session.ID(), data)
	return nil
}

func (p *Peer) SendCursorImage(cur *types.CursorImage, img []byte) error {
	data, err := encodeCursorImage(cur, img)
	if err != nil {
		return err
	}
	p.manager.sendData(p.session.ID(), data)
	return nil
}

func (p *Peer) Destroy() {
	p.mu.Lock()
	if p.destroyed {
		p.mu.Unlock()
		return
	}
	p.destroyed = true
	p.mu.Unlock()

	_, err := p.manager.client.DestroyPeer(context.Background(), &pb.DestroyPeerRequest{
		SessionId: p.session.ID(),
	})
	if err != nil {
		p.manager.logger.Err(err).Str("session_id", p.session.ID()).Msg("dataplane destroy peer failed")
	}

	p.manager.removePeer(p.session.ID())
}
