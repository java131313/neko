package desktop

import (
	"os/exec"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/kataras/go-events"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"

	"github.com/m1k1o/neko/server/internal/config"
	"github.com/m1k1o/neko/server/pkg/types"
	"github.com/m1k1o/neko/server/pkg/xevent"
	"github.com/m1k1o/neko/server/pkg/xinput"
	"github.com/m1k1o/neko/server/pkg/xorg"
)

const (
	// debounce frequency for key events
	debounceFrequency = 1 * time.Second

	// debounce duration for key events
	debounceDuration = 10 * time.Second
)

var mu = sync.Mutex{}

// one app of the apptab switcher: raise the window whose WM_CLASS contains
// one of the case-insensitive Matches substrings
type Apptab struct {
	Id      string
	Matches []string
}

// "id:m1[|m2],..." → tabs; empty/invalid entries skipped
func parseApptabs(raw string) []Apptab {
	var out []Apptab
	for _, entry := range strings.Split(raw, ",") {
		id, matches, ok := strings.Cut(entry, ":")
		if !ok {
			continue
		}
		var ms []string
		for _, m := range strings.Split(matches, "|") {
			if m = strings.TrimSpace(m); m != "" {
				ms = append(ms, m)
			}
		}
		id = strings.TrimSpace(id)
		if id == "" || len(ms) == 0 {
			continue
		}
		out = append(out, Apptab{Id: id, Matches: ms})
	}
	return out
}

type DesktopManagerCtx struct {
	logger     zerolog.Logger
	wg         sync.WaitGroup
	shutdown   chan struct{}
	emmiter    events.EventEmmiter
	config     *config.Desktop
	screenSize types.ScreenSize // cached screen size
	input      xinput.Driver

	// Clipboard process holding the most recent clipboard data.
	// It must remain running to allow pasting clipboard data.
	// The last command is kept running until it is replaced or shutdown.
	clipboardCommand atomic.Pointer[exec.Cmd]

	// apptab switcher: the configured apps and the last raised one
	apptabs      []Apptab
	activeApptab string
}

func New(config *config.Desktop) *DesktopManagerCtx {
	var input xinput.Driver
	if config.UseInputDriver {
		input = xinput.NewDriver(config.InputSocket)
	} else {
		input = xinput.NewDummy()
	}

	apptabs := parseApptabs(config.Apps)
	activeApptab := ""
	if len(apptabs) > 0 {
		activeApptab = apptabs[0].Id
	}

	return &DesktopManagerCtx{
		logger:       log.With().Str("module", "desktop").Logger(),
		shutdown:     make(chan struct{}),
		emmiter:      events.New(),
		config:       config,
		screenSize:   config.ScreenSize,
		input:        input,
		apptabs:      parseApptabs(config.Apps),
		activeApptab: activeApptab,
	}
}

func (manager *DesktopManagerCtx) Start() {
	if xorg.DisplayOpen(manager.config.Display) {
		manager.logger.Panic().Str("display", manager.config.Display).Msg("unable to open display")
	}

	// X11 can throw errors below, and the default error handler exits
	xevent.SetupErrorHandler()

	xorg.GetScreenConfigurations()

	screenSize, err := xorg.ChangeScreenSize(manager.config.ScreenSize)
	if err != nil {
		manager.logger.Err(err).
			Str("screen_size", screenSize.String()).
			Msgf("unable to set initial screen size")
	} else {
		// cache screen size
		manager.screenSize = screenSize
		manager.logger.Info().
			Str("screen_size", screenSize.String()).
			Msgf("setting initial screen size")
	}

	err = manager.input.Connect()
	if err != nil {
		// TODO: fail silently to dummy driver?
		manager.logger.Panic().Err(err).Msg("unable to connect to input driver")
	}

	// set up event listeners
	xevent.Unminimize = manager.config.Unminimize
	xevent.FileChooserDialog = manager.config.FileChooserDialog
	go xevent.EventLoop(manager.config.Display)

	// in case it was opened
	if manager.config.FileChooserDialog {
		go manager.CloseFileChooserDialog()
	}

	manager.OnEventError(func(error_code uint8, message string, request_code uint8, minor_code uint8) {
		manager.logger.Warn().
			Uint8("error_code", error_code).
			Str("message", message).
			Uint8("request_code", request_code).
			Uint8("minor_code", minor_code).
			Msg("X event error occured")
	})

	manager.wg.Go(func() {
		ticker := time.NewTicker(debounceFrequency)
		defer ticker.Stop()

		for {
			select {
			case <-manager.shutdown:
				return
			case <-ticker.C:
				xorg.CheckKeys(debounceDuration)
				manager.input.Debounce(debounceDuration)
			}
		}
	})
}

func (manager *DesktopManagerCtx) OnBeforeScreenSizeChange(listener func()) {
	manager.emmiter.On("before_screen_size_change", func(payload ...any) {
		listener()
	})
}

func (manager *DesktopManagerCtx) OnAfterScreenSizeChange(listener func()) {
	manager.emmiter.On("after_screen_size_change", func(payload ...any) {
		listener()
	})
}

func (manager *DesktopManagerCtx) Shutdown() error {
	manager.logger.Info().Msgf("shutdown")

	close(manager.shutdown)

	manager.replaceClipboardCommand(nil)
	manager.wg.Wait()

	xorg.DisplayClose()
	return nil
}

func (manager *DesktopManagerCtx) Apptabs() (string, []string) {
	mu.Lock()
	defer mu.Unlock()

	apps := make([]string, 0, len(manager.apptabs))
	for _, tab := range manager.apptabs {
		apps = append(apps, tab.Id)
	}
	return manager.activeApptab, apps
}

func (manager *DesktopManagerCtx) ActivateApptab(id string) bool {
	mu.Lock()
	var target *Apptab
	for i := range manager.apptabs {
		if manager.apptabs[i].Id == id {
			target = &manager.apptabs[i]
			break
		}
	}
	mu.Unlock()

	if target == nil {
		return false
	}

	// the C side mutates the buffer (strtok_r): join fresh per call
	if !xorg.ApptabActivate(strings.Join(target.Matches, "|")) {
		return false
	}

	mu.Lock()
	manager.activeApptab = id
	mu.Unlock()

	manager.logger.Info().Str("app", id).Msg("apptab raised")
	return true
}
