using Firecrawl.PdfInspector;

if (args.Length != 1)
{
    throw new ArgumentException("Expected the path to a PDF fixture.");
}

var pdf = File.ReadAllBytes(args[0]);
var result = PdfInspector.ProcessPdf(pdf);
var classification = PdfInspector.ClassifyPdf(pdf);
var text = PdfInspector.ExtractText(pdf);
var ocr = PdfInspector.ProcessPdfWithOcr(pdf, new OcrOptions { Mode = OcrMode.Off });

if (result.PdfType != PdfType.TextBased ||
    classification.PdfType != PdfType.TextBased ||
    string.IsNullOrWhiteSpace(result.Markdown) ||
    string.IsNullOrWhiteSpace(text) ||
    string.IsNullOrWhiteSpace(ocr.Markdown))
{
    throw new InvalidOperationException("The installed NuGet package returned an invalid result.");
}

Console.WriteLine($"Firecrawl.PdfInspector {PdfInspector.Version()} smoke test passed.");
