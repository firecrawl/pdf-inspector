// Ported from reference/src/bin/pdf2md.rs and reference/src/bin/detect_pdf.rs
using PdfInspector.Cli;

// The Rust build ships two binaries; this one dispatches on a leading verb so a
// single executable covers both.
if (args.Length == 0)
{
    Console.Error.WriteLine("Usage: pdf-inspector <command> <pdf_file> [options]");
    Console.Error.WriteLine();
    Console.Error.WriteLine("Commands:");
    Console.Error.WriteLine("  pdf2md      Convert a PDF to Markdown");
    Console.Error.WriteLine("  detect-pdf  Detect whether a PDF is text-based or scanned");
    return 1;
}

var command = args[0];
var rest = args[1..];

return command switch
{
    "pdf2md" => Pdf2MdCommand.Run(rest),
    "detect-pdf" => DetectPdfCommand.Run(rest),
    _ => UnknownCommand(command),
};

static int UnknownCommand(string command)
{
    Console.Error.WriteLine($"Unknown command: {command}");
    Console.Error.WriteLine("Expected one of: pdf2md, detect-pdf");
    return 1;
}
