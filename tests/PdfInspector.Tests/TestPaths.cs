namespace PdfInspector.Tests;

/// <summary>Locations of the reference crate's test data, resolved from the repository root.</summary>
internal static class TestPaths
{
    /// <summary>The repository root, found by walking up from the test assembly.</summary>
    public static string RepositoryRoot { get; } = FindRepositoryRoot();

    public static string Fixtures => Path.Combine(RepositoryRoot, "reference", "tests", "fixtures");

    public static string Snapshots => Path.Combine(RepositoryRoot, "reference", "tests", "snapshots");

    /// <summary>The release Rust binaries, when they have been built.</summary>
    public static string ReferenceBin => Path.Combine(RepositoryRoot, "reference", "target", "release");

    private static string FindRepositoryRoot()
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);

        while (directory is not null)
        {
            // The reference crate sits at the repository root alongside the
            // solution, and is what the fixtures resolve against.
            if (Directory.Exists(Path.Combine(directory.FullName, "reference", "tests", "fixtures")))
            {
                return directory.FullName;
            }

            directory = directory.Parent;
        }

        throw new InvalidOperationException("could not locate the repository root from " + AppContext.BaseDirectory);
    }
}
