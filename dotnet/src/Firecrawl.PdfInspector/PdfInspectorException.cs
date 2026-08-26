using System;

namespace Firecrawl.PdfInspector;

/// <summary>An error returned by the native pdf-inspector library.</summary>
public sealed class PdfInspectorException : Exception
{
    public PdfInspectorException(int nativeStatus, string message)
        : base(message)
    {
        NativeStatus = nativeStatus;
    }

    /// <summary>The native ABI status code.</summary>
    public int NativeStatus { get; }
}
