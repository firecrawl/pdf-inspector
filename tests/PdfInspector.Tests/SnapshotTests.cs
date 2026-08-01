// Ported from reference/tests/integration_tests.rs
using Xunit;

namespace PdfInspector.Tests;

/// <summary>
/// Snapshot regression tests against the Rust crate's golden markdown. These
/// catch changes that silently alter extraction or markdown output.
/// </summary>
/// <remarks>
/// The snapshots live in <c>reference/tests/snapshots</c> and are the Rust
/// binary's own output, so a mismatch means this port drifted from the
/// behavioural source of truth. One fixture diverges deliberately and has its
/// own test below rather than a byte comparison.
/// </remarks>
public sealed class SnapshotTests
{
    private static void AssertSnapshot(string fixture)
    {
        var fixturePath = Path.Combine(TestPaths.Fixtures, fixture + ".pdf");
        var snapshotPath = Path.Combine(TestPaths.Snapshots, fixture + ".md");

        var actual = (PdfProcessor.ProcessPdf(fixturePath).Markdown ?? string.Empty).TrimEnd();
        var expected = File.ReadAllText(snapshotPath).TrimEnd();

        if (actual == expected)
        {
            return;
        }

        // Report the first few differing lines rather than dumping both files.
        var actualLines = actual.Split('\n');
        var expectedLines = expected.Split('\n');
        var diffs = new List<string>();
        for (var i = 0; i < Math.Max(actualLines.Length, expectedLines.Length); i++)
        {
            var a = i < actualLines.Length ? actualLines[i] : "<missing>";
            var e = i < expectedLines.Length ? expectedLines[i] : "<missing>";
            if (a == e)
            {
                continue;
            }

            diffs.Add($"  line {i + 1}: expected {Truncate(e)}, got {Truncate(a)}");
            if (diffs.Count >= 5)
            {
                diffs.Add("  ... (more diffs truncated)");
                break;
            }
        }

        Assert.Fail($"Snapshot mismatch for {fixture}:\n{string.Join('\n', diffs)}");
    }

    private static string Truncate(string line) =>
        "\"" + (line.Length <= 80 ? line : line[..80]) + "\"";

    [Fact]
    public void NexoPriceEn() => AssertSnapshot("nexo-price-en");

    [Fact]
    public void ThermoFreon12() => AssertSnapshot("thermo-freon12");

    [Fact]
    public void Td9264() => AssertSnapshot("td9264");

    [Fact]
    public void P1244() => AssertSnapshot("p1244-1996");

    /// <summary>
    /// The reference's parser cannot read page 7 of this fixture — lopdf logs
    /// "Object load error at offset 48234" and extracts zero items — so the
    /// pinned snapshot omits that page's content entirely. This port's recovery
    /// scan reads the page and emits its 48 rows, which is strictly more of the
    /// document, not a regression. Everything the reference did manage to read
    /// must still match, so the snapshot has to reappear exactly once the one
    /// recovered block is removed.
    /// </summary>
    [Fact]
    public void The2013App2SnapshotIsTheRecoveredOutputMinusThePageTheReferenceDropped()
    {
        var actual = (PdfProcessor.ProcessPdf(Path.Combine(TestPaths.Fixtures, "2013-app2.pdf")).Markdown
            ?? string.Empty).TrimEnd().Split('\n');
        var expected = File.ReadAllText(Path.Combine(TestPaths.Snapshots, "2013-app2.md")).TrimEnd().Split('\n');

        Assert.True(actual.Length > expected.Length, "the recovered page should add lines");

        // The common prefix and suffix must together cover the whole snapshot,
        // leaving exactly one contiguous run of added lines in between.
        var prefix = 0;
        while (prefix < expected.Length && actual[prefix] == expected[prefix])
        {
            prefix++;
        }

        var suffix = 0;
        while (suffix < expected.Length - prefix && actual[^(suffix + 1)] == expected[^(suffix + 1)])
        {
            suffix++;
        }

        Assert.Equal(expected.Length, prefix + suffix);

        var recovered = actual[prefix..(actual.Length - suffix)];
        Assert.Equal(actual.Length - expected.Length, recovered.Length);
        Assert.Contains(recovered, line => line.Contains("Security Services for Perseverance School", StringComparison.Ordinal));
    }

    [Fact]
    public void RealEstatePricing() => AssertSnapshot("real-estate-pricing");
}
