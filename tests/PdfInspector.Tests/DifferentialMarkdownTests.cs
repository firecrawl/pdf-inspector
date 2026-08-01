using System.Diagnostics;
using Xunit;

namespace PdfInspector.Tests;

/// <summary>
/// Compares this port's markdown against the Rust reference binary over every
/// open fixture, byte for byte. The reference is the behavioural source of
/// truth, so any new difference is a regression unless it is listed below.
/// </summary>
public sealed class DifferentialMarkdownTests
{
    /// <summary>The fixture that is password-protected, so neither side reads it.</summary>
    private const string EncryptedFixture = "encrypted-secret123.pdf";

    /// <summary>
    /// Fixtures whose output is expected to differ, with the reason.
    /// </summary>
    private static readonly Dictionary<string, string> KnownDivergences = new(StringComparer.Ordinal)
    {
        ["2013-app2.pdf"] =
            "the reference's parser cannot read page 7 (lopdf: \"Object load error at offset 48234\") "
            + "and extracts zero items from it, while this port's recovery scan reads the page and "
            + "emits its rows; SnapshotTests pins the exact shape of that addition",
    };

    private static List<string> OpenFixtures() =>
    [
        .. Directory.GetFiles(TestPaths.Fixtures, "*.pdf")
            .Where(f => Path.GetFileName(f) != EncryptedFixture)
            .OrderBy(f => f, StringComparer.Ordinal),
    ];

    /// <remarks>
    /// This runs both implementations over every fixture, so it takes minutes
    /// rather than seconds. Exclude it with
    /// <c>dotnet test --filter Category!=Differential</c> for a quick loop.
    /// </remarks>
    [Fact]
    [Trait("Category", "Differential")]
    public void MatchesTheReferenceBinaryOnEveryFixture()
    {
        var referenceBinary = Path.Combine(TestPaths.ReferenceBin, "pdf2md");
        if (!File.Exists(referenceBinary))
        {
            // The reference binaries are built on demand; without them this
            // comparison has nothing to compare against.
            return;
        }

        var mismatches = new List<string>();
        var comparedDivergences = new List<string>();

        foreach (var file in OpenFixtures())
        {
            var name = Path.GetFileName(file);
            if (RunReference(referenceBinary, file) is not { } expected)
            {
                mismatches.Add($"{name}: the reference binary failed to run");
                continue;
            }

            // `--raw` is the reference CLI's unpostprocessed markdown, which is
            // what the library returns.
            var actual = PdfProcessor.ProcessPdf(file).Markdown ?? string.Empty;

            if (KnownDivergences.ContainsKey(name))
            {
                comparedDivergences.Add(name);
                Assert.True(
                    actual != expected,
                    $"{name} is listed as a known divergence but now matches the reference — "
                    + "remove it from KnownDivergences");
                continue;
            }

            if (actual == expected)
            {
                continue;
            }

            mismatches.Add($"{name}: {DescribeFirstDifference(expected, actual)}");
        }

        Assert.Equal(KnownDivergences.Count, comparedDivergences.Count);
        Assert.Empty(mismatches);
    }

    /// <summary>The first differing line, with enough context to act on.</summary>
    private static string DescribeFirstDifference(string expected, string actual)
    {
        var expectedLines = expected.Split('\n');
        var actualLines = actual.Split('\n');

        for (var i = 0; i < Math.Max(expectedLines.Length, actualLines.Length); i++)
        {
            var e = i < expectedLines.Length ? expectedLines[i] : "<missing>";
            var a = i < actualLines.Length ? actualLines[i] : "<missing>";
            if (e != a)
            {
                return $"line {i + 1}: reference {Truncate(e)}, port {Truncate(a)}";
            }
        }

        return $"{expected.Length} bytes from the reference, {actual.Length} from the port";
    }

    private static string Truncate(string line) =>
        "\"" + (line.Length <= 100 ? line : line[..100]) + "\"";

    /// <summary>Runs the reference binary's raw markdown mode over one fixture.</summary>
    private static string? RunReference(string binary, string fixturePath)
    {
        var startInfo = new ProcessStartInfo(binary)
        {
            RedirectStandardOutput = true,
            RedirectStandardError = true,
        };
        startInfo.ArgumentList.Add(fixturePath);
        startInfo.ArgumentList.Add("--raw");

        using var process = Process.Start(startInfo);
        if (process is null)
        {
            return null;
        }

        // Both pipes must drain concurrently: the reference logs parse errors to
        // stderr, and a full stderr buffer would block the child while this side
        // is still reading stdout.
        var stdoutTask = process.StandardOutput.ReadToEndAsync();
        var stderrTask = process.StandardError.ReadToEndAsync();
        if (!process.WaitForExit(120_000))
        {
            process.Kill(entireProcessTree: true);
            return null;
        }

        var stdout = stdoutTask.GetAwaiter().GetResult();
        stderrTask.GetAwaiter().GetResult();

        return process.ExitCode == 0 ? stdout : null;
    }
}
