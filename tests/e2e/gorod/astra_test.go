package astra

// Real browser end-to-end tests: drive Astra with go-rod.
//
// Astra must be running and reachable at ASTRA_HTTP (default
// http://127.0.0.1:9222) and the test page must be served at TEST_URL.
//
//	ASTRA_HTTP=http://127.0.0.1:9222 go test -v ./tests/e2e/gorod/...

import (
	"encoding/json"
	"io"
	"net/http"
	"os"
	"strconv"
	"testing"
	"time"

	"github.com/go-rod/rod"
)

func endpoint(t *testing.T) string {
	t.Helper()
	// ASTRA_WS routes the client through a CDP sniffer, which is how the CI job
	// records the trace that shows where a run stalls.
	if ws := os.Getenv("ASTRA_WS"); ws != "" {
		return ws
	}
	base := os.Getenv("ASTRA_HTTP")
	if base == "" {
		base = "http://127.0.0.1:9222"
	}
	resp, err := http.Get(base + "/json/version")
	if err != nil {
		t.Skipf("astra is not running at %s: %v", base, err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)
	var info struct {
		Browser              string `json:"Browser"`
		WebSocketDebuggerURL string `json:"webSocketDebuggerUrl"`
	}
	if err := json.Unmarshal(body, &info); err != nil {
		t.Fatalf("bad /json/version: %v (%s)", err, body)
	}
	if info.WebSocketDebuggerURL == "" {
		t.Fatalf("no webSocketDebuggerUrl in %s", body)
	}
	t.Logf("connected to %s", info.Browser)
	return info.WebSocketDebuggerURL
}

// evalNumber evaluates JS and parses the result as a number.  It goes through
// encoding/json (gson.JSON implements json.Marshaler) so it does not depend on
// which accessors a given rod/gson version happens to expose.
func evalNumber(t *testing.T, page *rod.Page, expr string) float64 {
	t.Helper()
	raw, err := json.Marshal(page.MustEval(expr))
	if err != nil {
		t.Fatalf("cannot marshal the result of %s: %v", expr, err)
	}
	var v float64
	if err := json.Unmarshal(raw, &v); err != nil {
		var s string
		if err2 := json.Unmarshal(raw, &s); err2 == nil {
			if f, err3 := strconv.ParseFloat(s, 64); err3 == nil {
				return f
			}
		}
		t.Fatalf("cannot read a number from %s (%s): %v", expr, raw, err)
	}
	return v
}

func testURL() string {
	if u := os.Getenv("TEST_URL"); u != "" {
		return u
	}
	return "http://127.0.0.1:8123/index.html"
}

func TestAstraGoRod(t *testing.T) {
	ws := endpoint(t)
	browser := rod.New().ControlURL(ws).Timeout(60 * time.Second).MustConnect()
	defer browser.MustClose()

	page := browser.MustPage(testURL()).Timeout(60 * time.Second)
	page.MustWaitLoad()

	t.Run("title", func(t *testing.T) {
		if got := page.MustInfo().Title; got != "Astra test page" {
			t.Fatalf("title = %q", got)
		}
	})

	t.Run("images decoded", func(t *testing.T) {
		val := page.MustEval(`() => Array.from(document.images).map(i => i.naturalWidth)`)
		var widths []int
		if err := json.Unmarshal([]byte(val.String()), &widths); err != nil {
			t.Fatalf("cannot parse widths: %v", err)
		}
		if len(widths) < 3 {
			t.Fatalf("expected 3 images, got %d", len(widths))
		}
		for i, w := range widths {
			if w <= 0 {
				t.Fatalf("image %d has no pixels (naturalWidth=%d)", i, w)
			}
		}
		t.Logf("image widths: %v", widths)
	})

	t.Run("video plays", func(t *testing.T) {
		if os.Getenv("HAS_VIDEO") == "" {
			t.Skip("no video asset")
		}
		page.MustEval(`() => { const v = document.getElementById('vid'); v.muted = true; v.currentTime = 0; return v.play(); }`)
		time.Sleep(2 * time.Second)
		ct := evalNumber(t, page, `() => document.getElementById('vid').currentTime`)
		if ct <= 0.3 {
			t.Fatalf("video did not advance: currentTime=%v", ct)
		}
		vw := evalNumber(t, page, `() => document.getElementById('vid').videoWidth`)
		if vw <= 0 {
			t.Fatal("no video frames decoded")
		}
		t.Logf("video currentTime=%.2f videoWidth=%.0f", ct, vw)
	})

	t.Run("javascript", func(t *testing.T) {
		if got := evalNumber(t, page, `() => 6 * 7`); got != 42 {
			t.Fatalf("6*7 = %v", got)
		}
	})

	t.Run("screenshot", func(t *testing.T) {
		buf := page.MustScreenshot()
		if len(buf) < 5000 {
			t.Fatalf("screenshot too small: %d bytes", len(buf))
		}
		if buf[0] != 0x89 || string(buf[1:4]) != "PNG" {
			t.Fatal("screenshot is not a png")
		}
		if err := os.WriteFile("/tmp/astra-gorod.png", buf, 0o644); err != nil {
			t.Fatal(err)
		}
		t.Logf("screenshot: %d bytes", len(buf))
	})

	t.Run("astra stats", func(t *testing.T) {
		base := os.Getenv("ASTRA_HTTP")
		if base == "" {
			base = "http://127.0.0.1:9222"
		}
		resp, err := http.Get(base + "/stats")
		if err != nil {
			t.Fatalf("GET /stats failed: %v", err)
		}
		defer resp.Body.Close()
		body, _ := io.ReadAll(resp.Body)
		var s struct {
			Requests       int     `json:"requests"`
			BytesOriginal  int64   `json:"bytesOriginal"`
			BytesDelivered int64   `json:"bytesDelivered"`
			SavingPct      float64 `json:"savingPct"`
		}
		if err := json.Unmarshal(body, &s); err != nil {
			t.Fatalf("cannot parse stats: %v (%s)", err, body)
		}
		t.Logf("stats: %s", body)
		if s.Requests == 0 {
			t.Fatal("astra counted no requests")
		}
		if s.BytesDelivered > s.BytesOriginal {
			t.Fatalf("delivered more bytes than original: %d > %d", s.BytesDelivered, s.BytesOriginal)
		}
	})
}
