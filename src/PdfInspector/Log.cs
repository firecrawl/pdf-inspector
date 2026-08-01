namespace PdfInspector;

/// <summary>
/// Minimal trace logging, standing in for the Rust build's <c>log</c> crate.
/// Enabled per-module through the <c>PDFINSPECTOR_LOG</c> environment variable,
/// which takes a comma-separated list of module names (or <c>all</c>).
/// </summary>
internal static class Log
{
    private static readonly HashSet<string> Enabled = ReadConfiguration();

    private static readonly bool AnyEnabled = Enabled.Count > 0;

    private static HashSet<string> ReadConfiguration()
    {
        var value = Environment.GetEnvironmentVariable("PDFINSPECTOR_LOG");
        if (string.IsNullOrWhiteSpace(value))
        {
            return [];
        }

        return value
            .Split(',', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)
            .Select(s => s.ToLowerInvariant())
            .ToHashSet(StringComparer.Ordinal);
    }

    public static bool IsEnabled(string module) =>
        AnyEnabled && (Enabled.Contains("all") || Enabled.Contains(module));

    public static void Debug(string module, string message) => Write("DEBUG", module, message);

    public static void Trace(string module, string message) => Write("TRACE", module, message);

    public static void Warn(string module, string message) => Write("WARN", module, message);

    /// <summary>
    /// Defers formatting until the module is known to be enabled, so trace calls
    /// in hot loops cost only a set lookup when logging is off.
    /// </summary>
    public static void Debug(string module, Func<string> message)
    {
        if (IsEnabled(module))
        {
            Write("DEBUG", module, message());
        }
    }

    public static void Trace(string module, Func<string> message)
    {
        if (IsEnabled(module))
        {
            Write("TRACE", module, message());
        }
    }

    private static void Write(string level, string module, string message)
    {
        if (!IsEnabled(module))
        {
            return;
        }

        Console.Error.WriteLine($"[{level} {module}] {message}");
    }
}
