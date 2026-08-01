// Ported from reference/src/lib.rs
using System.Text;

namespace PdfInspector;

/// <summary>Checks that a byte buffer or file actually looks like a PDF.</summary>
internal static class Validation
{
    /// <summary>
    /// Validates that a buffer starts with the PDF header, scanning the first
    /// kilobyte and tolerating a UTF-8 byte-order mark and leading whitespace.
    /// </summary>
    /// <exception cref="PdfException">When the bytes are not a PDF.</exception>
    public static void ValidatePdfBytes(byte[] buffer)
    {
        if (buffer.Length == 0)
        {
            throw new PdfException(PdfException.FailureKind.NotAPdf, $"Not a PDF: {DetectFileTypeHint(buffer)}");
        }

        var header = buffer.AsSpan(0, Math.Min(buffer.Length, 1024));
        var trimmed = StripBomAndWhitespace(header);

        if (!trimmed.StartsWith("%PDF-"u8))
        {
            throw new PdfException(PdfException.FailureKind.NotAPdf, $"Not a PDF: {DetectFileTypeHint(buffer)}");
        }
    }

    /// <summary>Strips a UTF-8 byte-order mark and leading ASCII whitespace.</summary>
    private static ReadOnlySpan<byte> StripBomAndWhitespace(ReadOnlySpan<byte> bytes)
    {
        if (bytes.Length >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF)
        {
            bytes = bytes[3..];
        }

        var start = 0;
        while (start < bytes.Length && IsAsciiWhitespace(bytes[start]))
        {
            start++;
        }

        return bytes[start..];
    }

    /// <summary>True when the buffer opens with the given magic bytes.</summary>
    private static bool HasPrefix(byte[] bytes, ReadOnlySpan<byte> magic) =>
        bytes.Length >= magic.Length && bytes.AsSpan(0, magic.Length).SequenceEqual(magic);

    /// <summary>True for the bytes ASCII treats as whitespace.</summary>
    private static bool IsAsciiWhitespace(byte b) =>
        b is (byte)' ' or (byte)'\t' or (byte)'\n' or (byte)'\v' or (byte)'\f' or (byte)'\r';

    /// <summary>A case-insensitive prefix test on bytes.</summary>
    private static bool StartsWithCi(ReadOnlySpan<byte> haystack, ReadOnlySpan<byte> needle)
    {
        if (haystack.Length < needle.Length)
        {
            return false;
        }

        for (var i = 0; i < needle.Length; i++)
        {
            if (char.ToLowerInvariant((char)haystack[i]) != char.ToLowerInvariant((char)needle[i]))
            {
                return false;
            }
        }

        return true;
    }

    /// <summary>Guesses what kind of file the bytes actually are, for the error message.</summary>
    private static string DetectFileTypeHint(byte[] bytes)
    {
        if (bytes.Length == 0)
        {
            return "file is empty";
        }

        var trimmed = StripBomAndWhitespace(bytes);

        if (StartsWithCi(trimmed, "<!doctype html"u8)
            || StartsWithCi(trimmed, "<html"u8)
            || StartsWithCi(trimmed, "<head"u8)
            || StartsWithCi(trimmed, "<body"u8))
        {
            return "file appears to be HTML";
        }

        if (trimmed.StartsWith("<?xml"u8)
            || (trimmed.Length > 0 && trimmed[0] == (byte)'<' && !trimmed.StartsWith("<%"u8)))
        {
            return "file appears to be XML";
        }

        if (trimmed.Length > 0 && trimmed[0] is (byte)'{' or (byte)'[')
        {
            return "file appears to be JSON";
        }

        if (HasPrefix(bytes, [0x89, 0x50, 0x4E, 0x47]))
        {
            return "file appears to be a PNG image";
        }

        if (HasPrefix(bytes, [0xFF, 0xD8, 0xFF]))
        {
            return "file appears to be a JPEG image";
        }

        if (HasPrefix(bytes, [0x50, 0x4B, 0x03, 0x04]))
        {
            return "file appears to be a ZIP archive (possibly an Office document)";
        }

        // Mostly printable bytes read as plain text.
        var sample = bytes.AsSpan(0, Math.Min(bytes.Length, 512));
        var printable = 0;
        foreach (var b in sample)
        {
            if (b is >= 0x21 and <= 0x7E || IsAsciiWhitespace(b))
            {
                printable++;
            }
        }

        return printable > sample.Length * 3 / 4 ? "file appears to be plain text" : "file is not a PDF";
    }

    /// <summary>Validates a file on disk, reading only its first kilobyte.</summary>
    /// <exception cref="PdfException">When the file is not a PDF or cannot be read.</exception>
    public static void ValidatePdfFile(string path)
    {
        try
        {
            using var file = File.OpenRead(path);
            var buf = new byte[1024];
            var n = file.Read(buf, 0, buf.Length);
            ValidatePdfBytes(buf[..n]);
        }
        catch (IOException ex)
        {
            throw new PdfException(PdfException.FailureKind.Io, $"IO error: {ex.Message}", ex);
        }
    }
}
