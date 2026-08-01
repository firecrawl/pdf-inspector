// Temporary probe harness; replaced by the real CLI entry points in a later phase.
using PdfInspector.Pdf;
using PdfInspector.ToUnicode;

var directory = args.Length > 0 ? args[0] : "reference/tests/fixtures";
var files = Directory.GetFiles(directory, "*.pdf").OrderBy(f => f).ToArray();

foreach (var file in files)
{
    var name = Path.GetFileName(file);
    try
    {
        var password = name.Contains("secret123", StringComparison.Ordinal) ? "secret123" : null;
        var document = PdfDocument.Load(File.ReadAllBytes(file), password);

        // Large fixtures would dominate the run; a few pages exercise the same paths.
        var pages = Math.Min(document.PageCount, 5);
        var filter = new HashSet<uint>(Enumerable.Range(1, pages).Select(p => (uint)p));

        var started = System.Diagnostics.Stopwatch.StartNew();
        var cmaps = FontCMaps.FromDocumentPages(document, filter);
        started.Stop();

        var fontCount = 0;
        for (var p = 1; p <= pages; p++)
        {
            if (document.GetPage(p) is { } page)
            {
                fontCount += document.GetPageFonts(page).Count;
            }
        }

        Console.WriteLine(
            $"OK    {name,-48} pages={document.PageCount,-4} fonts={fontCount,-4} " +
            $"cmaps={cmaps.Count,-3} {started.ElapsedMilliseconds}ms");
    }
    catch (Exception e)
    {
        Console.WriteLine($"FAIL  {name,-48} {e.GetType().Name}: {e.Message}");
    }
}
