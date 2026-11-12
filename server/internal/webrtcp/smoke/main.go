// Local smoke test for the control-plane <-> dataplane gRPC contract.
//
// Runs against a locally started neko-dataplane (no Docker, no full neko
// server needed — the server binary requires cgo deps that don't build on
// macOS):
//
//	NEKO_DATAPLANE_LISTEN=/tmp/neko-smoke.sock ./target/debug/neko-dataplane &
//	(cd server && go run ./internal/webrtcp/smoke -addr unix:///tmp/neko-smoke.sock)
//
// It dials exactly the way Manager.Start does (same passthrough target +
// unix dialer) and walks the whole Phase 1 contract surface, asserting the
// exact status codes the dataplane must return.
//
// Lives under server/internal (allowed to import the internal pb package)
// but deliberately imports nothing else from the server module, so it
// builds on machines without the X11/GStreamer cgo toolchain.
package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"strings"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"

	"github.com/m1k1o/neko/server/internal/webrtcp/pb"
)

func main() {
	addr := flag.String("addr", "unix:///tmp/neko-smoke.sock", "dataplane socket (unix:// URL or plain path)")
	timeout := flag.Duration("timeout", 10*time.Second, "per-RPC timeout")
	flag.Parse()

	target := strings.TrimPrefix(*addr, "unix://")

	// grpc-go dials unix sockets natively; the "unix" scheme yields a valid
	// :authority, unlike the passthrough target (which tonic's h2 rejects
	// with PROTOCOL_ERROR)
	conn, err := grpc.NewClient(
		"unix://"+target,
		grpc.WithTransportCredentials(insecure.NewCredentials()),
	)
	if err != nil {
		fmt.Println("FATAL dial:", err)
		os.Exit(1)
	}
	defer conn.Close()

	client := pb.NewDataplaneClient(conn)

	failures := 0
	expect := func(name string, err error, want codes.Code) {
		got := status.Code(err)
		if got == want {
			fmt.Printf("PASS  %-24s code=%s\n", name, got)
		} else {
			fmt.Printf("FAIL  %-24s got=%s want=%s (%v)\n", name, got, want, err)
			failures++
		}
	}
	expectTrue := func(name string, ok bool, detail string) {
		if ok {
			fmt.Printf("PASS  %-24s %s\n", name, detail)
		} else {
			fmt.Printf("FAIL  %-24s %s\n", name, detail)
			failures++
		}
	}

	ctx, cancel := context.WithTimeout(context.Background(), *timeout)
	defer cancel()

	// contract: Configure must be accepted first
	_, err = client.Configure(ctx, &pb.ConfigureRequest{
		Config: &pb.WebRtcConfig{
			IceLite:      true,
			IceTrickle:   true,
			EphemeralMin: 53000,
			EphemeralMax: 53100,
			Nat1To1Ips:   []string{"192.168.0.165"},
			IceServersBackend: []*pb.IceServer{
				{Urls: []string{"stun:stun.l.google.com:19302"}},
			},
		},
	})
	expect("configure", err, codes.OK)

	// contract: media path implemented — a real server-side offer comes back
	createRes, err := client.CreatePeer(ctx, &pb.CreatePeerRequest{SessionId: "smoke-1"})
	expect("create_peer", err, codes.OK)
	if err == nil {
		sdp := createRes.Sdp
		expectTrue("offer_has_video", strings.Contains(sdp, "m=video"), "")
		expectTrue("offer_has_audio", strings.Contains(sdp, "m=audio"), "")
	}

	_, err = client.SetRemoteDescription(ctx, &pb.SetRemoteDescriptionRequest{
		SessionId: "smoke-1", Type: "answer", Sdp: "v=0\r\no=smoke 0 0 IN IP4 127.0.0.1\r\n",
	})
	expectTrue("garbage_sdp_rejected", err != nil, "")

	disabled := false
	_, err = client.SetVideo(ctx, &pb.SetVideoRequest{SessionId: "smoke-1", Disabled: &disabled})
	expect("set_video", err, codes.OK)

	_, err = client.SetAudio(ctx, &pb.SetAudioRequest{SessionId: "smoke-1", Disabled: &disabled})
	expect("set_audio", err, codes.OK)

	_, err = client.SetPaused(ctx, &pb.SetPausedRequest{SessionId: "smoke-1", Paused: true})
	expect("set_paused", err, codes.OK)

	// empty session id = broadcast; channel is not open yet, send is a no-op
	_, err = client.SendData(ctx, &pb.SendDataRequest{SessionId: "", Data: []byte{0x01, 0x00, 0x07, 0x00, 0x01, 0x00, 0x01}})
	expect("send_data_broadcast", err, codes.OK)

	// ICE gathering must produce candidates on the events stream
	eventsCtx, eventsCancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer eventsCancel()
	stream, err := client.Events(eventsCtx, &pb.EventsRequest{})
	if err != nil {
		fmt.Println("FAIL  events_open            ", err)
		failures++
	} else {
		candidates := 0
		for {
			ev, err := stream.Recv()
			if err != nil {
				break
			}
			if _, ok := ev.Kind.(*pb.Event_IceCandidate); ok {
				candidates++
			}
		}
		expectTrue("ice_candidates_flow", candidates > 0, fmt.Sprintf("got %d", candidates))
	}

	// renegotiation offer can be regenerated
	offerRes, err := client.CreateOffer(ctx, &pb.CreateOfferRequest{SessionId: "smoke-1", IceRestart: true})
	expect("create_offer", err, codes.OK)
	if err == nil {
		expectTrue("offer_nonempty", offerRes.Sdp != "", "")
	}

	// answering without a remote offer must fail
	_, err = client.CreateAnswer(ctx, &pb.CreateAnswerRequest{SessionId: "smoke-1"})
	expectTrue("create_answer_rejected", err != nil, "")

	_, err = client.DestroyPeer(ctx, &pb.DestroyPeerRequest{SessionId: "smoke-1"})
	expect("destroy_peer", err, codes.OK)

	// unknown session must be reported, not silently swallowed
	_, err = client.SetPaused(ctx, &pb.SetPausedRequest{SessionId: "smoke-unknown", Paused: true})
	expect("unknown_session", err, codes.NotFound)

	fmt.Println()
	if failures > 0 {
		fmt.Printf("%d check(s) FAILED\n", failures)
		os.Exit(1)
	}
	fmt.Println("all checks passed")
}
