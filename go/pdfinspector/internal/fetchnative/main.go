// Command fetchnative downloads the prebuilt pdf-inspector Rust library for
// the host GOOS/GOARCH from a GitHub Release, so `go generate ./... && go
// build ./...` works without a Rust toolchain on supported platforms.
//
// It is a fallback convenience, not a hard requirement: `cargo build
// --release` in go/ (see go/Makefile's `native` target) always works and
// takes priority if a native library is already present at
// go/target/release — this tool never overwrites an existing build.
//
// Invoked via the //go:generate directive in pdfinspector.go.
package main

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"crypto/sha256"
	"encoding/hex"
	"flag"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"time"
)

// releaseVersion is the go/ crate's release tag (see go/Cargo.toml's
// `version`). Bump both together — there is no automated sync for this
// unpublished, independently-versioned crate (see go/README.md's "Building"
// section for why the go/ binding isn't on the repo's shared version.py
// lockstep: it isn't distributed via any package registry yet).
const releaseVersion = "0.1.0"

const repo = "firecrawl/pdf-inspector"

// targetSuffix mirrors the napi/pyproject build matrices' platform naming
// (see .github/workflows/publish-go.yml), restricted to the platforms that
// workflow actually builds.
func targetSuffix(goos, goarch string) (string, bool) {
	switch {
	case goos == "darwin" && goarch == "arm64":
		return "darwin-arm64", true
	case goos == "darwin" && goarch == "amd64":
		return "darwin-x64", true
	case goos == "linux" && goarch == "amd64":
		return "linux-x64", true
	case goos == "linux" && goarch == "arm64":
		return "linux-arm64", true
	default:
		return "", false
	}
}

func nativeLibDir() (string, error) {
	// `go generate` runs `go run ./internal/fetchnative` with its working
	// directory set to the package containing the //go:generate directive
	// (go/pdfinspector), not this tool's own source directory — so the
	// path back to go/target/release only needs one level up.
	wd, err := os.Getwd()
	if err != nil {
		return "", err
	}
	return filepath.Join(wd, "..", "target", "release"), nil
}

func alreadyBuilt(dir string) bool {
	for _, name := range []string{"libpdf_inspector_go.dylib", "libpdf_inspector_go.so"} {
		if _, err := os.Stat(filepath.Join(dir, name)); err == nil {
			return true
		}
	}
	return false
}

func fail(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "fetchnative: "+format+"\n", args...)
	fmt.Fprintln(os.Stderr, "fetchnative: falling back — run `make native` (or `cargo build --release`) in go/ instead.")
	os.Exit(1)
}

func main() {
	force := flag.Bool("force", false, "re-download even if a native library is already present")
	flag.Parse()

	dir, err := nativeLibDir()
	if err != nil {
		fail("resolve native library directory: %v", err)
	}

	if !*force && alreadyBuilt(dir) {
		fmt.Printf("fetchnative: native library already present at %s, skipping\n", dir)
		return
	}

	suffix, ok := targetSuffix(runtime.GOOS, runtime.GOARCH)
	if !ok {
		fail("no prebuilt library published for %s/%s", runtime.GOOS, runtime.GOARCH)
	}

	client := &http.Client{Timeout: 60 * time.Second}
	tag := "go/v" + releaseVersion
	base := fmt.Sprintf("https://github.com/%s/releases/download/%s", repo, tag)
	archiveName := fmt.Sprintf("pdf-inspector-go-%s.tar.gz", suffix)

	checksums, err := downloadChecksums(client, base+"/checksums.txt")
	if err != nil {
		fail("download checksums for %s: %v", tag, err)
	}
	wantSum, ok := checksums[archiveName]
	if !ok {
		fail("no checksum entry for %s in %s/checksums.txt", archiveName, base)
	}

	data, err := download(client, base+"/"+archiveName)
	if err != nil {
		fail("download %s: %v", archiveName, err)
	}

	if gotSum := sha256Hex(data); gotSum != wantSum {
		fail("checksum mismatch for %s: got %s, want %s", archiveName, gotSum, wantSum)
	}

	if err := extractTarGz(data, dir); err != nil {
		fail("extract %s into %s: %v", archiveName, dir, err)
	}

	fmt.Printf("fetchnative: installed %s %s into %s\n", tag, suffix, dir)
}

func download(client *http.Client, url string) ([]byte, error) {
	resp, err := client.Get(url)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("unexpected status %s", resp.Status)
	}
	return io.ReadAll(resp.Body)
}

func downloadChecksums(client *http.Client, url string) (map[string]string, error) {
	data, err := download(client, url)
	if err != nil {
		return nil, err
	}
	out := make(map[string]string)
	for _, line := range strings.Split(string(data), "\n") {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		fields := strings.Fields(line)
		if len(fields) != 2 {
			continue
		}
		out[fields[1]] = fields[0]
	}
	return out, nil
}

func sha256Hex(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}

func extractTarGz(data []byte, destDir string) error {
	if err := os.MkdirAll(destDir, 0o755); err != nil {
		return err
	}
	gz, err := gzip.NewReader(bytes.NewReader(data))
	if err != nil {
		return err
	}
	defer gz.Close()

	tr := tar.NewReader(gz)
	for {
		hdr, err := tr.Next()
		if err == io.EOF {
			return nil
		}
		if err != nil {
			return err
		}
		if hdr.Typeflag != tar.TypeReg {
			continue
		}
		// Flatten: archives are a flat list of library files, not nested
		// directories, so guard against path traversal by taking only the
		// base name.
		name := filepath.Base(hdr.Name)
		out, err := os.OpenFile(filepath.Join(destDir, name), os.O_CREATE|os.O_TRUNC|os.O_WRONLY, 0o755)
		if err != nil {
			return err
		}
		if _, err := io.Copy(out, tr); err != nil { //nolint:gosec // fixed set of trusted, checksum-verified release assets
			out.Close()
			return err
		}
		if err := out.Close(); err != nil {
			return err
		}
	}
}
