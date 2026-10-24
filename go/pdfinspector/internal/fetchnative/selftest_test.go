package main

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"crypto/sha256"
	"debug/elf"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"runtime"
	"testing"
)

func TestDownloadExtractRoundtrip(t *testing.T) {
	payload := []byte("fake dylib bytes")
	var tarBuf bytes.Buffer
	gz := gzip.NewWriter(&tarBuf)
	tw := tar.NewWriter(gz)
	hdr := &tar.Header{Name: "libpdf_inspector_go.dylib", Mode: 0o755, Size: int64(len(payload))}
	if err := tw.WriteHeader(hdr); err != nil {
		t.Fatal(err)
	}
	if _, err := tw.Write(payload); err != nil {
		t.Fatal(err)
	}
	tw.Close()
	gz.Close()
	archiveBytes := tarBuf.Bytes()
	sum := sha256.Sum256(archiveBytes)
	checksums := fmt.Sprintf("%s  pdf-inspector-go-darwin-arm64.tar.gz\n", hex.EncodeToString(sum[:]))

	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/checksums.txt":
			w.Write([]byte(checksums))
		case "/pdf-inspector-go-darwin-arm64.tar.gz":
			w.Write(archiveBytes)
		default:
			w.WriteHeader(404)
		}
	}))
	defer srv.Close()

	client := srv.Client()
	cs, err := downloadChecksums(client, srv.URL+"/checksums.txt")
	if err != nil {
		t.Fatal(err)
	}
	want := cs["pdf-inspector-go-darwin-arm64.tar.gz"]
	if want == "" {
		t.Fatal("missing checksum entry")
	}

	data, err := download(client, srv.URL+"/pdf-inspector-go-darwin-arm64.tar.gz")
	if err != nil {
		t.Fatal(err)
	}
	if sha256Hex(data) != want {
		t.Fatal("checksum mismatch")
	}

	dir := filepath.Join(t.TempDir(), "release")
	if _, err := extractTarGz(data, dir); err != nil {
		t.Fatal(err)
	}
	out, err := os.ReadFile(filepath.Join(dir, "libpdf_inspector_go.dylib"))
	if err != nil {
		t.Fatal(err)
	}
	if string(out) != string(payload) {
		t.Fatalf("extracted content mismatch: got %q", out)
	}
}

// TestExtractTarGz_FailureLeavesNoPartialFile guards against the bug class
// where a mid-extraction failure leaves a truncated file at destDir, which
// alreadyBuilt (an existence check, not an integrity check) would then
// treat as "already built" forever.
//
// The archive here deliberately contains one fully-valid file entry
// followed by a second entry whose header claims more bytes than are
// actually present — so the failure happens only *after* the first file
// has already been completely staged in tmpDir, which is the exact
// partial-extraction scenario extractTarGz's temp-dir-then-move design
// exists to prevent. A naive corrupt-from-the-first-byte archive (as an
// earlier version of this test used) fails before any file is written at
// all, so it can't tell a correct temp-dir implementation apart from a
// broken one that extracts straight into destDir.
func TestExtractTarGz_FailureLeavesNoPartialFile(t *testing.T) {
	var tarBuf bytes.Buffer
	tw := tar.NewWriter(&tarBuf)

	firstFile := []byte("this file is complete and should never reach destDir")
	if err := tw.WriteHeader(&tar.Header{
		Name: "libpdf_inspector_go.dylib",
		Mode: 0o755,
		Size: int64(len(firstFile)),
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := tw.Write(firstFile); err != nil {
		t.Fatal(err)
	}

	// Second entry: the header promises more bytes than we actually
	// write, and we deliberately never call tw.Close() (which would pad
	// and finalize the entry) — the resulting raw tar bytes end mid-entry,
	// simulating a truncated download.
	truncated := []byte("this entry's body is shorter than its header claims")
	if err := tw.WriteHeader(&tar.Header{
		Name: "truncated.txt",
		Mode: 0o644,
		Size: int64(len(truncated)) + 100,
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := tw.Write(truncated); err != nil {
		t.Fatal(err)
	}

	var gzBuf bytes.Buffer
	gz := gzip.NewWriter(&gzBuf)
	if _, err := gz.Write(tarBuf.Bytes()); err != nil {
		t.Fatal(err)
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}

	dir := filepath.Join(t.TempDir(), "release")
	if _, err := extractTarGz(gzBuf.Bytes(), dir); err == nil {
		t.Fatal("extractTarGz with a truncated second entry: want error, got nil")
	}

	if _, err := os.Stat(dir); !os.IsNotExist(err) {
		t.Errorf("destDir %s should not exist after a failed extraction (the first, fully-valid file must not have leaked through), stat error = %v", dir, err)
	}
	entries, err := os.ReadDir(filepath.Dir(dir))
	if err != nil {
		t.Fatalf("read %s: %v", filepath.Dir(dir), err)
	}
	for _, e := range entries {
		t.Errorf("leftover entry after failed extraction: %s", e.Name())
	}
}

// TestAlreadyBuilt_RejectsWrongArchitecture guards against alreadyBuilt
// trusting a library whose filename and OS match but whose actual CPU
// architecture doesn't -- e.g. an x86_64 .so left over on an arm64 host.
// Uses the real native library this dev/CI checkout already built (skips
// if none is present) and patches a copy's own architecture field to a
// different valid value, rather than hand-rolling a synthetic ELF/Mach-O
// file.
func TestAlreadyBuilt_RejectsWrongArchitecture(t *testing.T) {
	src := filepath.Join("..", "..", "..", "target", "release", hostLibraryName())
	data, err := os.ReadFile(src)
	if err != nil {
		t.Skipf("no built native library at %s (run `cargo build --release` in go/ first): %v", src, err)
	}

	dir := t.TempDir()
	libPath := filepath.Join(dir, hostLibraryName())
	if err := os.WriteFile(libPath, data, 0o755); err != nil {
		t.Fatal(err)
	}
	if !alreadyBuilt(dir) {
		t.Fatal("alreadyBuilt() = false for an unmodified, correct-architecture library")
	}

	patched := append([]byte(nil), data...)
	patchToWrongArch(t, patched)
	if err := os.WriteFile(libPath, patched, 0o755); err != nil {
		t.Fatal(err)
	}
	if alreadyBuilt(dir) {
		t.Error("alreadyBuilt() = true for a library patched to report a different architecture")
	}
}

// patchToWrongArch flips the architecture field embedded in a real
// ELF/Mach-O binary's own header to some other valid architecture, in
// place, so the file otherwise still looks structurally valid to
// debug/elf or debug/macho.
func patchToWrongArch(t *testing.T, data []byte) {
	t.Helper()
	if runtime.GOOS == "darwin" {
		// mach_header/mach_header_64: magic (4 bytes) then a little-endian
		// uint32 cputype at offset 4.
		if len(data) < 8 {
			t.Fatal("library too small to patch")
		}
		const cpuTypeX8664 = 0x01000007
		const cpuTypeArm64 = 0x0100000C
		wrong := uint32(cpuTypeX8664)
		if runtime.GOARCH != "arm64" {
			wrong = cpuTypeArm64
		}
		binary.LittleEndian.PutUint32(data[4:8], wrong)
		return
	}
	// ELF: e_ident is 16 bytes, then a 2-byte e_type, then a little-endian
	// 2-byte e_machine at offset 18.
	if len(data) < 20 {
		t.Fatal("library too small to patch")
	}
	wrong := uint16(elf.EM_X86_64)
	if runtime.GOARCH != "arm64" {
		wrong = uint16(elf.EM_AARCH64)
	}
	binary.LittleEndian.PutUint16(data[18:20], wrong)
}

// TestDownloadWithLimit_RejectsOversizedResponse guards the bound added to
// download(): a response over the limit must error rather than being
// buffered fully into memory, while a response exactly at the limit must
// still succeed (the bound isn't off-by-one in the wrong direction).
func TestDownloadWithLimit_RejectsOversizedResponse(t *testing.T) {
	const limit = 1000

	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		size := limit
		if r.URL.Path == "/over" {
			size = limit + 1
		}
		w.Write(bytes.Repeat([]byte("x"), size))
	}))
	defer srv.Close()

	if _, err := downloadWithLimit(srv.Client(), srv.URL+"/over", limit); err == nil {
		t.Fatal("downloadWithLimit with a response one byte over the limit: want error, got nil")
	}

	data, err := downloadWithLimit(srv.Client(), srv.URL+"/at-limit", limit)
	if err != nil {
		t.Fatalf("downloadWithLimit with a response exactly at the limit: %v", err)
	}
	if len(data) != limit {
		t.Errorf("len(data) = %d, want %d", len(data), limit)
	}
}

// TestExtractThenVerify_CatchesArchiveMissingHostLibrary guards the
// post-extraction check in main(): an archive that extracts perfectly
// cleanly but simply doesn't contain the expected host library at all
// (e.g. a release accidentally published with the wrong asset under a
// given name) has nothing for extractTarGz itself to fail on -- it's
// alreadyBuilt's existence-and-architecture check, called again after
// extraction, that has to catch this.
func TestExtractThenVerify_CatchesArchiveMissingHostLibrary(t *testing.T) {
	var tarBuf bytes.Buffer
	gz := gzip.NewWriter(&tarBuf)
	tw := tar.NewWriter(gz)
	payload := []byte("not the library you're looking for")
	if err := tw.WriteHeader(&tar.Header{Name: "README.txt", Mode: 0o644, Size: int64(len(payload))}); err != nil {
		t.Fatal(err)
	}
	if _, err := tw.Write(payload); err != nil {
		t.Fatal(err)
	}
	if err := tw.Close(); err != nil {
		t.Fatal(err)
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}

	dir := filepath.Join(t.TempDir(), "release")
	extracted, err := extractTarGz(tarBuf.Bytes(), dir)
	if err != nil {
		t.Fatalf("extractTarGz: %v", err)
	}
	for _, name := range extracted {
		if name == hostLibraryName() {
			t.Fatalf("test archive should not contain %s, but extractTarGz reports it did", hostLibraryName())
		}
	}
	if alreadyBuilt(dir) {
		t.Error("alreadyBuilt() = true after extracting an archive containing no host library at all")
	}
}

// TestLibraryWasInstalled_DetectsStaleLibraryNotReplaced reproduces the
// specific scenario alreadyBuilt(dir) alone can't catch: `-force`
// re-extracting into a directory that *already has a valid library in
// it* from a previous run, against a new archive that happens to omit
// the expected file. alreadyBuilt(dir) reports the old file as if it
// were fresh (it has no way to know it wasn't touched by this
// extraction).
//
// This calls libraryWasInstalled directly -- the exact function main()
// uses to decide whether a download succeeded -- rather than re-deriving
// the same two conditions in a parallel assertion here: a test that only
// checks alreadyBuilt(dir) and separately checks the extracted-names list
// would keep passing even if main() were changed to stop calling (or
// stop trusting the result of) libraryWasInstalled, since neither
// assertion actually exercises that function.
func TestLibraryWasInstalled_DetectsStaleLibraryNotReplaced(t *testing.T) {
	stale, err := os.ReadFile(filepath.Join("..", "..", "..", "target", "release", hostLibraryName()))
	if err != nil {
		t.Skipf("no built native library to use as a stale pre-existing file (run `cargo build --release` in go/ first): %v", err)
	}

	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, hostLibraryName()), stale, 0o755); err != nil {
		t.Fatal(err)
	}
	if !alreadyBuilt(dir) {
		t.Fatal("alreadyBuilt() = false for the pre-seeded stale library; test setup is broken")
	}

	// Archive that omits the host library entirely -- simulating a bad
	// release asset -- extracted into the SAME dir that already has the
	// stale library sitting in it.
	var tarBuf bytes.Buffer
	gz := gzip.NewWriter(&tarBuf)
	tw := tar.NewWriter(gz)
	payload := []byte("unrelated file, not the library")
	if err := tw.WriteHeader(&tar.Header{Name: "README.txt", Mode: 0o644, Size: int64(len(payload))}); err != nil {
		t.Fatal(err)
	}
	if _, err := tw.Write(payload); err != nil {
		t.Fatal(err)
	}
	if err := tw.Close(); err != nil {
		t.Fatal(err)
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}

	extracted, err := extractTarGz(tarBuf.Bytes(), dir)
	if err != nil {
		t.Fatalf("extractTarGz: %v", err)
	}

	// Sanity check on the scenario itself: the stale file from before
	// this extraction is still sitting there untouched, so alreadyBuilt
	// alone can't tell this apart from a successful install.
	if !alreadyBuilt(dir) {
		t.Fatal("alreadyBuilt() = false after this extraction, but the stale library should still be sitting there untouched -- test setup assumption is wrong")
	}
	// The actual regression this test guards: libraryWasInstalled must
	// see through that and report false, since hostLibraryName() was
	// never part of *this* extraction.
	if libraryWasInstalled(extracted, dir) {
		t.Error("libraryWasInstalled() = true, but the archive never contained the host library -- only a stale pre-existing one is present")
	}
}

// TestLibraryWasInstalled_AcceptsFreshExtraction is the positive
// counterpart to TestLibraryWasInstalled_DetectsStaleLibraryNotReplaced:
// when the archive genuinely does contain the host library, and it
// extracts to a valid file, libraryWasInstalled must accept it.
func TestLibraryWasInstalled_AcceptsFreshExtraction(t *testing.T) {
	real, err := os.ReadFile(filepath.Join("..", "..", "..", "target", "release", hostLibraryName()))
	if err != nil {
		t.Skipf("no built native library available (run `cargo build --release` in go/ first): %v", err)
	}

	var tarBuf bytes.Buffer
	gz := gzip.NewWriter(&tarBuf)
	tw := tar.NewWriter(gz)
	if err := tw.WriteHeader(&tar.Header{Name: hostLibraryName(), Mode: 0o755, Size: int64(len(real))}); err != nil {
		t.Fatal(err)
	}
	if _, err := tw.Write(real); err != nil {
		t.Fatal(err)
	}
	if err := tw.Close(); err != nil {
		t.Fatal(err)
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}

	dir := t.TempDir()
	extracted, err := extractTarGz(tarBuf.Bytes(), dir)
	if err != nil {
		t.Fatalf("extractTarGz: %v", err)
	}
	if !libraryWasInstalled(extracted, dir) {
		t.Error("libraryWasInstalled() = false for an archive that genuinely contains a valid host library")
	}
}

// TestIsCurrentFetch guards the version/target staleness check used by
// main()'s skip decision: without it, alreadyBuilt(dir) alone can't
// detect that go/Cargo.toml's version moved on since dir's library was
// fetched, so `go generate` would keep linking a previous release's
// library forever after a version bump.
func TestIsCurrentFetch(t *testing.T) {
	t.Run("no marker at all is trusted (locally built, or pre-marker fetch)", func(t *testing.T) {
		dir := t.TempDir()
		if !isCurrentFetch(dir, "1.2.3", "linux-x64") {
			t.Error("isCurrentFetch() = false with no marker present; a locally-built library must never be second-guessed")
		}
	})

	t.Run("matching marker is current", func(t *testing.T) {
		dir := t.TempDir()
		if err := writeFetchMarker(dir, "1.2.3", "linux-x64"); err != nil {
			t.Fatal(err)
		}
		if !isCurrentFetch(dir, "1.2.3", "linux-x64") {
			t.Error("isCurrentFetch() = false for a marker matching the current version and target")
		}
	})

	t.Run("version mismatch is stale", func(t *testing.T) {
		dir := t.TempDir()
		if err := writeFetchMarker(dir, "1.2.3", "linux-x64"); err != nil {
			t.Fatal(err)
		}
		if isCurrentFetch(dir, "1.3.0", "linux-x64") {
			t.Error("isCurrentFetch() = true after a version bump; a stale download should be re-fetched")
		}
	})

	t.Run("target mismatch is stale", func(t *testing.T) {
		dir := t.TempDir()
		if err := writeFetchMarker(dir, "1.2.3", "linux-x64"); err != nil {
			t.Fatal(err)
		}
		if isCurrentFetch(dir, "1.2.3", "linux-arm64") {
			t.Error("isCurrentFetch() = true for a marker recorded for a different target")
		}
	})

	t.Run("corrupt marker is not trusted", func(t *testing.T) {
		dir := t.TempDir()
		if err := os.WriteFile(fetchMarkerPath(dir), []byte("not json"), 0o644); err != nil {
			t.Fatal(err)
		}
		if isCurrentFetch(dir, "1.2.3", "linux-x64") {
			t.Error("isCurrentFetch() = true for an unparseable marker file")
		}
	})
}

// TestExtractTarGzWithLimits_RejectsDecompressionBomb guards the
// per-entry/cumulative extraction caps: a highly compressible tar entry
// (a long run of zero bytes) can look tiny as a compressed download while
// still declaring -- and, once decompressed, actually containing -- far
// more data than any legitimate native library would. Bounding only the
// compressed download size (maxDownloadSize, checked separately in
// download()) does nothing to stop this from exhausting disk during
// extraction.
func TestExtractTarGzWithLimits_RejectsDecompressionBomb(t *testing.T) {
	const bombSize = 10_000_000 // 10 MB of zeros, far over the tiny limit below
	var tarBuf bytes.Buffer
	tw := tar.NewWriter(&tarBuf)
	if err := tw.WriteHeader(&tar.Header{Name: "libpdf_inspector_go.so", Mode: 0o755, Size: bombSize}); err != nil {
		t.Fatal(err)
	}
	if _, err := io.CopyN(tw, zeroReader{}, bombSize); err != nil {
		t.Fatal(err)
	}
	if err := tw.Close(); err != nil {
		t.Fatal(err)
	}

	var gzBuf bytes.Buffer
	gz := gzip.NewWriter(&gzBuf)
	if _, err := gz.Write(tarBuf.Bytes()); err != nil {
		t.Fatal(err)
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}
	t.Logf("compressed archive: %d bytes vs. %d bytes decompressed", gzBuf.Len(), bombSize)

	const tinyLimit = 1024 // 1 KiB -- far below bombSize
	dir := filepath.Join(t.TempDir(), "release")
	if _, err := extractTarGzWithLimits(gzBuf.Bytes(), dir, tinyLimit, tinyLimit); err == nil {
		t.Fatal("extractTarGzWithLimits with an oversized entry: want error, got nil")
	}

	if _, err := os.Stat(dir); !os.IsNotExist(err) {
		t.Errorf("destDir %s should not exist after a rejected extraction, stat error = %v", dir, err)
	}
}

// zeroReader streams an endless run of zero bytes without allocating a
// large buffer up front -- exactly what a decompression bomb's payload
// looks like (maximally compressible), without this test needing to hold
// bombSize bytes in memory itself.
type zeroReader struct{}

func (zeroReader) Read(p []byte) (int, error) {
	for i := range p {
		p[i] = 0
	}
	return len(p), nil
}

func TestReleaseVersion_ReadsFromCargoToml(t *testing.T) {
	// releaseVersion() reads ../Cargo.toml relative to the working
	// directory `go generate` actually uses when it runs this tool
	// (go/pdfinspector — see nativeLibDir's comment), which is two levels
	// up from this test's own package directory
	// (go/pdfinspector/internal/fetchnative).
	wd, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	defer os.Chdir(wd)
	if err := os.Chdir(filepath.Join("..", "..")); err != nil {
		t.Fatal(err)
	}

	version, err := releaseVersion()
	if err != nil {
		t.Fatalf("releaseVersion: %v", err)
	}
	if version == "" {
		t.Error("releaseVersion returned an empty string")
	}
}
