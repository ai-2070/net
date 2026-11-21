package net

// The sensing C-ABI numeric contract (SENSING_OLB_COMPLETION_PLAN B2): the cgo
// preamble in sensing.go mirrors include/net_sensing.h exactly, both ways.
// org-ffi pins the header against the Rust consts, so all three agree.

import (
	"bufio"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
)

func TestSensingPreambleMatchesTheHeader(t *testing.T) {
	f, err := os.Open(filepath.Join(sensingCrateRoot(t), "include", "net_sensing.h"))
	if err != nil {
		t.Fatalf("open header: %v", err)
	}
	defer f.Close()
	header := map[string]int{}
	scanner := bufio.NewScanner(f)
	for scanner.Scan() {
		fields := strings.Fields(scanner.Text())
		if len(fields) == 3 && fields[0] == "#define" && strings.HasPrefix(fields[1], "NET_SENSING_") {
			v, err := strconv.Atoi(fields[2])
			if err != nil {
				t.Fatalf("parse %s: %v", fields[1], err)
			}
			header[fields[1]] = v
		}
	}
	if len(header) == 0 {
		t.Fatal("found no NET_SENSING_* defines: the parse is not reading the header")
	}
	for name, want := range header {
		got, ok := sensingCConstants[name]
		if !ok {
			t.Errorf("the Go preamble does not mirror %s", name)
			continue
		}
		if got != want {
			t.Errorf("%s: Go preamble %d, header %d", name, got, want)
		}
	}
	for name := range sensingCConstants {
		if _, ok := header[name]; !ok {
			t.Errorf("the Go preamble defines %s, which the header does not", name)
		}
	}
}
