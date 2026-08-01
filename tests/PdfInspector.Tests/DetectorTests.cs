using System.Diagnostics;
using System.Text.Json;
using PdfInspector.Detector;
using PdfInspector.Pdf;
using Xunit;

namespace PdfInspector.Tests;

/// <summary>
/// Detection tests over the fixtures, including a differential comparison
/// against the Rust reference binary, which is this port's source of truth.
/// </summary>
public sealed class DetectorFixtureTests
{
    /// <summary>The fixture that is password-protected, so both implementations refuse it.</summary>
    private const string EncryptedFixture = "encrypted-secret123.pdf";

    /// <summary>The reference's snake_case name for each classification.</summary>
    private static readonly Dictionary<string, PdfType> ReferenceTypeNames = new(StringComparer.Ordinal)
    {
        ["text_based"] = PdfType.TextBased,
        ["scanned"] = PdfType.Scanned,
        ["image_based"] = PdfType.ImageBased,
        ["mixed"] = PdfType.Mixed,
    };

    /// <summary>The four documented OCR reason codes.</summary>
    private static readonly string[] KnownReasons =
    [
        OcrReason.SuspectedGarbledText, OcrReason.Scanned, OcrReason.NoText, OcrReason.VectorText,
    ];

    /// <summary>Every fixture except the encrypted one, in a stable order.</summary>
    private static List<string> OpenFixtures() =>
    [
        .. Directory.GetFiles(TestPaths.Fixtures, "*.pdf")
            .Where(f => Path.GetFileName(f) != EncryptedFixture)
            .OrderBy(f => f, StringComparer.Ordinal),
    ];

    [Fact]
    public void EncryptedFixtureIsRefusedWithoutAPassword()
    {
        var path = Path.Combine(TestPaths.Fixtures, EncryptedFixture);
        Assert.Throws<PdfEncryptedException>(() => PdfDetector.DetectPdfType(path));
    }

    [Fact]
    public void ClassifiesEveryFixtureConsistently()
    {
        var files = OpenFixtures();
        Assert.NotEmpty(files);

        foreach (var file in files)
        {
            var name = Path.GetFileName(file);
            var result = PdfDetector.DetectPdfType(file);

            Assert.True(result.PageCount > 0, $"{name}: page count was {result.PageCount}");
            Assert.InRange(result.Confidence, 0.0f, 1.0f);

            foreach (var page in result.PagesNeedingOcr)
            {
                Assert.InRange(page, 1u, result.PageCount);
                Assert.True(
                    result.OcrReasonsByPage.ContainsKey(page),
                    $"{name}: page {page} needs OCR but carries no reason");
            }

            foreach (var reasons in result.OcrReasonsByPage.Values)
            {
                Assert.NotEmpty(reasons);
                foreach (var reason in reasons)
                {
                    Assert.Contains(reason, KnownReasons);
                }
            }
        }
    }

    [Fact]
    public void MatchesTheReferenceDetector()
    {
        var referenceBinary = Path.Combine(TestPaths.ReferenceBin, "detect-pdf");
        if (!File.Exists(referenceBinary))
        {
            // The reference binaries are built on demand; without them this
            // comparison has nothing to compare against.
            return;
        }

        var mismatches = new List<string>();

        foreach (var file in OpenFixtures())
        {
            var name = Path.GetFileName(file);
            if (RunReference(referenceBinary, file) is not { } expected)
            {
                continue;
            }

            var actual = PdfDetector.DetectPdfType(file);

            if (actual.PageCount != expected.PageCount)
            {
                mismatches.Add($"{name}: page count {actual.PageCount} != {expected.PageCount}");
            }

            if (actual.PdfType != expected.PdfType)
            {
                mismatches.Add($"{name}: type {actual.PdfType} != {expected.PdfType}");
            }

            if (actual.OcrRecommended != expected.OcrRecommended)
            {
                mismatches.Add($"{name}: ocr_recommended {actual.OcrRecommended} != {expected.OcrRecommended}");
            }

            if (!actual.PagesNeedingOcr.SequenceEqual(expected.PagesNeedingOcr))
            {
                mismatches.Add(
                    $"{name}: pages_needing_ocr [{string.Join(',', actual.PagesNeedingOcr)}] != " +
                    $"[{string.Join(',', expected.PagesNeedingOcr)}]");
            }
        }

        Assert.True(mismatches.Count == 0, string.Join('\n', mismatches));
    }

    /// <summary>The fields of the reference binary's JSON report that this suite compares.</summary>
    private readonly record struct ReferenceResult(
        PdfType PdfType,
        uint PageCount,
        bool OcrRecommended,
        List<uint> PagesNeedingOcr);

    /// <summary>Runs the reference binary over a fixture and parses its JSON report.</summary>
    private static ReferenceResult? RunReference(string binary, string file)
    {
        var psi = new ProcessStartInfo(binary)
        {
            RedirectStandardOutput = true,
            RedirectStandardError = true,
        };
        psi.ArgumentList.Add(file);
        psi.ArgumentList.Add("--json");

        using var process = Process.Start(psi);
        if (process is null)
        {
            return null;
        }

        var stdout = process.StandardOutput.ReadToEnd();
        process.WaitForExit();

        using var json = JsonDocument.Parse(stdout);
        var root = json.RootElement;
        if (root.TryGetProperty("error", out _))
        {
            return null;
        }

        var typeName = root.GetProperty("pdf_type").GetString() ?? string.Empty;
        var pagesNeedingOcr = root.GetProperty("pages_needing_ocr")
            .EnumerateArray()
            .Select(e => e.GetUInt32())
            .ToList();

        return new ReferenceResult(
            ReferenceTypeNames[typeName],
            root.GetProperty("page_count").GetUInt32(),
            root.GetProperty("ocr_recommended").GetBoolean(),
            pagesNeedingOcr);
    }
}
