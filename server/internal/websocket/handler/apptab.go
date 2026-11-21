package handler

import (
	"github.com/m1k1o/neko/server/pkg/types"
	"github.com/m1k1o/neko/server/pkg/types/event"
	"github.com/m1k1o/neko/server/pkg/types/message"
)

// apptab switcher: raise the named app's window; on success the new state
// goes to everyone (shared desktop — the video all clients watch flips too).
// Unknown apps and windows that are not up yet are ignored, like the
// neko-rs counterpart.
func (h *MessageHandlerCtx) controlApptab(session types.Session, payload *message.ControlApptab) error {
	if !h.desktop.ActivateApptab(payload.App) {
		return nil
	}

	active, apps := h.desktop.Apptabs()
	h.sessions.Broadcast(event.APPTAB, message.Apptab{
		App:  active,
		Apps: apps,
	})

	return nil
}
