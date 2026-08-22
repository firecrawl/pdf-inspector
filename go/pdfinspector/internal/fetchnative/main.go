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
	"debug/elf"
	"debug/macho"
	"encoding/hex"
	"flag"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"strings"
	"time"
)

const repo = "firecrawl/pdf-inspector"

var versionLineRe = regexp.MustCompile(`^\s*version\s*=\s*"([^"]+)"`)

// releaseVersion reads the go/ crate's release tag straight from
// go/Cargo.toml's `version` field, rather than duplicating it as a
// hand-synced literal in this file — the exact drift risk a hand-synced
// copy would have (fetchnative silently keeps downloading a stale release
// after a version bump nobody remembered to mirror here) is why this
// reads the same file publish-go.yml's tag derivation does, instead of
// trusting a second source of truth to stay in sync.
//
// This is a minimal, deliberately narrow TOML read — just the top-level
// `[package]` table's `version` field — rather than a full TOML parser
// dependency, since that's all this ever needs.
func releaseVersion() (string, error) {
	// `go generate` runs this tool with its working directory set to
	// go/pdfinspector (see nativeLibDir's comment), so go/Cargo.toml is one
	// level up.
	data, err := os.ReadFile(filepath.Join("..", "Cargo.toml"))
	if err != nil {
		return "", fmt.Errorf("read go/Cargo.toml: %w", err)
	}
	inPackageTable := false
	for _, line := range strings.Split(string(data), "\n") {
		trimmed := strings.TrimSpace(line)
		if strings.HasPrefix(trimmed, "[") {
			inPackageTable = trimmed == "[package]"
			continue
		}
		if !inPackageTable {
			continue
		}
		if m := versionLineRe.FindStringSubmatch(line); m != nil {
			return m[1], nil
		}
	}
	return "", fmt.Errorf("no version found in [package] of go/Cargo.toml")
}

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

// hostLibraryName returns the native library filename cgo's LDFLAGS
// actually looks for on this host, per go/pdfinspector/pdfinspector.go's
// `-lpdf_inspector_go` directive and the platform's shared-library naming.
func hostLibraryName() string {
	if runtime.GOOS == "darwin" {
		return "libpdf_inspector_go.dylib"
	}
	return "libpdf_inspector_go.so"
}

// alreadyBuilt checks only the current host's expected library file, not
// "either platform's", and that the file's actual architecture matches
// this host's — go/target/release can end up holding a stale library for
// a different OS or CPU architecture (e.g. checked in by mistake, or left
// over from a previous cross-build attempt on a shared cache), and
// treating that as "this host is already set up" would make cgo link
// against a file that fails at load time with an opaque
// incompatible-architecture error, or link successfully into a binary
// that then crashes.
func alreadyBuilt(dir string) bool {
	path := filepath.Join(dir, hostLibraryName())
	if _, err := os.Stat(path); err != nil {
		return false
	}
	ok, err := matchesHostArch(path)
	if err != nil {
		// Can't verify it's the right architecture (unreadable, corrupt,
		// or not actually an ELF/Mach-O file) -- don't trust it either.
		return false
	}
	return ok
}

// matchesHostArch parses the library's own binary header (ELF on Linux,
// Mach-O on macOS) and reports whether its declared CPU architecture
// matches runtime.GOARCH, using only the standard library's debug/elf and
// debug/macho -- no need to shell out to `file`/`lipo`/etc.
func matchesHostArch(path string) (bool, error) {
	if runtime.GOOS == "darwin" {
		f, err := macho.Open(path)
		if err != nil {
			return false, err
		}
		defer f.Close()
		switch runtime.GOARCH {
		case "arm64":
			return f.Cpu == macho.CpuArm64, nil
		case "amd64":
			return f.Cpu == macho.CpuAmd64, nil
		default:
			return false, fmt.Errorf("unrecognized GOARCH %q", runtime.GOARCH)
		}
	}

	f, err := elf.Open(path)
	if err != nil {
		return false, err
	}
	defer f.Close()
	switch runtime.GOARCH {
	case "arm64":
		return f.Machine == elf.EM_AARCH64, nil
	case "amd64":
		return f.Machine == elf.EM_X86_64, nil
	default:
		return false, fmt.Errorf("unrecognized GOARCH %q", runtime.GOARCH)
	}
}

// isMuslLinux reports whether the host's active C library is musl (e.g.
// Alpine) rather than glibc. publish-go.yml only publishes glibc-linked
// builds (its `build` job's Linux runners are Ubuntu-based, glibc either
// way), so a musl host must not be told a prebuilt download succeeded: the
// .so would be present but fail to load at runtime with an opaque
// dynamic-linker error far from this tool.
//
// A musl loader file existing is not sufficient evidence on its own: a
// glibc host with musl cross-compilation tools installed (common in Rust
// cross toolchains, e.g. `apt install musl-tools`) also has one present
// despite actually running glibc. The glibc dynamic linker is only ever
// absent on a genuinely musl-only distro (e.g. Alpine ships no glibc
// loader at all), so treat the host as musl only when a musl loader exists
// AND no glibc loader exists.
func isMuslLinux() bool {
	if runtime.GOOS != "linux" {
		return false
	}
	if globAny(
		"/lib/ld-linux*.so*", "/lib64/ld-linux*.so*",
		"/lib/*-linux-gnu/ld-linux*.so*", "/usr/lib/*-linux-gnu/ld-linux*.so*",
	) {
		return false // a glibc loader is present, so glibc is the active libc
	}
	return globAny("/lib/ld-musl-*.so.1", "/lib64/ld-musl-*.so.1")
}

func globAny(patterns ...string) bool {
	for _, pattern := range patterns {
		if matches, _ := filepath.Glob(pattern); len(matches) > 0 {
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
	if isMuslLinux() {
		fail("host appears to be musl-based Linux (e.g. Alpine); only glibc builds are published for linux/%s", runtime.GOARCH)
	}

	version, err := releaseVersion()
	if err != nil {
		fail("determine release version: %v", err)
	}

	client := &http.Client{Timeout: 60 * time.Second}
	// go/pdfinspector/v..., not go/v...: Go's subdirectory-module tagging
	// convention requires the full path from the repo root to go.mod, and
	// go/pdfinspector/go.mod's module path is go/pdfinspector, not go --
	// see the matching comment in publish-go.yml, which cuts this same tag.
	tag := "go/pdfinspector/v" + version
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

	extractedNames, err := extractTarGz(data, dir)
	if err != nil {
		fail("extract %s into %s: %v", archiveName, dir, err)
	}

	// extractTarGz succeeding only means every file *in the archive*
	// extracted cleanly -- it says nothing about whether the archive
	// actually contained the library this host needs at all (e.g. a
	// release published with the wrong asset under this name). Checking
	// only alreadyBuilt(dir) afterwards isn't enough on its own: with
	// -force re-extracting over a directory that already has a valid
	// library in it, a bad archive that omits the expected file would
	// leave that old file untouched, and alreadyBuilt would report it as
	// if this run had just produced it. Confirm the expected file was
	// actually *in this archive* first, then confirm what's on disk
	// afterwards is valid -- both checks, not just the second one.
	wasExtracted := false
	for _, name := range extractedNames {
		if name == hostLibraryName() {
			wasExtracted = true
			break
		}
	}
	if !wasExtracted {
		fail("extracted %s, but it did not contain %s at all -- the release archive may be missing the expected asset", archiveName, hostLibraryName())
	}
	if !alreadyBuilt(dir) {
		fail("extracted %s into %s, but %s is missing or not a valid %s/%s library afterwards -- the release archive may be corrupt or mismatched", archiveName, dir, hostLibraryName(), runtime.GOOS, runtime.GOARCH)
	}

	fmt.Printf("fetchnative: installed %s %s into %s\n", tag, suffix, dir)
}

// maxDownloadSize bounds both the checksums manifest and the native
// library archive itself. Legitimate archives here are a few MB at most
// (a single stripped cdylib); this leaves generous headroom while still
// bounding how much a malformed or hostile response can force into
// memory before the checksum in extractTarGz's caller ever gets checked.
const maxDownloadSize = 200 << 20 // 200 MiB

func download(client *http.Client, url string) ([]byte, error) {
	return downloadWithLimit(client, url, maxDownloadSize)
}

func downloadWithLimit(client *http.Client, url string, limit int64) ([]byte, error) {
	resp, err := client.Get(url)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("unexpected status %s", resp.Status)
	}
	if resp.ContentLength > limit {
		return nil, fmt.Errorf("response too large: %d bytes (limit %d)", resp.ContentLength, limit)
	}
	// Content-Length can be absent/unreliable (chunked encoding, a
	// misbehaving server); read at most limit+1 bytes so a response with
	// no declared length still can't be streamed into memory unbounded --
	// getting back exactly limit+1 bytes means the real response was
	// truncated by us, which is itself the "too large" case, not a
	// legitimately limit-sized file.
	data, err := io.ReadAll(io.LimitReader(resp.Body, limit+1))
	if err != nil {
		return nil, err
	}
	if int64(len(data)) > limit {
		return nil, fmt.Errorf("response exceeded %d byte limit", limit)
	}
	return data, nil
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

// maxExtractedFileSize and maxExtractedTotalSize bound decompressed
// output, independently of maxDownloadSize bounding the compressed
// download: gzip's compression ratio is attacker-controllable, so a small,
// well-under-maxDownloadSize archive can still decompress to something far
// larger ("decompression bomb") -- capping only the compressed transfer
// does nothing to stop that from exhausting disk during extraction.
const (
	maxExtractedFileSize  = 200 << 20 // 200 MiB for any single tar entry
	maxExtractedTotalSize = 200 << 20 // 200 MiB across the whole archive
)

// extractTarGz extracts into a temporary directory first and only moves
// completed files into destDir once every file in the archive has been
// fully written — a mid-extraction failure (disk full, killed process)
// must not leave a partial, truncated library at destDir, because
// alreadyBuilt only checks for the file's *existence*, not its integrity,
// and would then skip re-fetching a broken library forever.
//
// Returns the names actually extracted (relative to destDir) so a caller
// can verify a *specific* expected file was really part of this
// extraction — existence alone isn't enough: with `-force` re-extracting
// over an already-populated destDir, a bad archive that simply omits the
// expected file would leave an old, unrelated copy sitting there
// untouched, and a plain existence check afterwards couldn't tell that
// apart from a fresh, correct one.
func extractTarGz(data []byte, destDir string) ([]string, error) {
	return extractTarGzWithLimits(data, destDir, maxExtractedFileSize, maxExtractedTotalSize)
}

func extractTarGzWithLimits(data []byte, destDir string, maxFileSize, maxTotalSize int64) ([]string, error) {
	parent := filepath.Dir(destDir)
	if err := os.MkdirAll(parent, 0o755); err != nil {
		return nil, err
	}
	tmpDir, err := os.MkdirTemp(parent, ".fetchnative-*")
	if err != nil {
		return nil, err
	}
	defer os.RemoveAll(tmpDir) // no-op once files are moved out below

	gz, err := gzip.NewReader(bytes.NewReader(data))
	if err != nil {
		return nil, err
	}
	defer gz.Close()

	var extracted []string
	var totalExtracted int64
	tr := tar.NewReader(gz)
	for {
		hdr, err := tr.Next()
		if err == io.EOF {
			break
		}
		if err != nil {
			return nil, err
		}
		if hdr.Typeflag != tar.TypeReg {
			continue
		}
		// Flatten: archives are a flat list of library files, not nested
		// directories, so guard against path traversal by taking only the
		// base name.
		name := filepath.Base(hdr.Name)
		out, err := os.OpenFile(filepath.Join(tmpDir, name), os.O_CREATE|os.O_TRUNC|os.O_WRONLY, 0o755)
		if err != nil {
			return nil, err
		}

		// Per-entry AND cumulative-across-the-archive caps: copy at most
		// one byte past whichever limit binds tighter, so a legitimate
		// entry that fits still copies in full (io.CopyN returns io.EOF,
		// not an error, once the source itself runs dry) while one that
		// doesn't fit is caught by n exceeding the limit, without ever
		// having to buffer the oversized tail in memory to detect it.
		limit := maxFileSize
		if remaining := maxTotalSize - totalExtracted; remaining < limit {
			limit = remaining
		}
		n, err := io.CopyN(out, tr, limit+1) //nolint:gosec // bounded by limit+1 above; checksum-verified release assets besides
		if err != nil && err != io.EOF {
			out.Close()
			return nil, err
		}
		if n > limit {
			out.Close()
			return nil, fmt.Errorf("tar entry %q exceeds the %d byte per-entry/%d byte total extraction limit", name, maxFileSize, maxTotalSize)
		}
		totalExtracted += n

		if err := out.Close(); err != nil {
			return nil, err
		}
		extracted = append(extracted, name)
	}

	if err := os.MkdirAll(destDir, 0o755); err != nil {
		return nil, err
	}
	for _, name := range extracted {
		if err := os.Rename(filepath.Join(tmpDir, name), filepath.Join(destDir, name)); err != nil {
			return nil, err
		}
	}
	return extracted, nil
}
