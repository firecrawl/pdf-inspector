using System;
using System.Runtime.InteropServices;

namespace Firecrawl.PdfInspector;

internal static class NativeMethods
{
    internal const uint ExpectedAbiVersion = 1;
    private const string LibraryName = "pdf_inspector_dotnet";

    [StructLayout(LayoutKind.Sequential)]
    internal struct NativeResult
    {
        internal int Status;
        internal IntPtr Data;
        internal UIntPtr Length;
    }

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern uint pdf_inspector_abi_version();

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern NativeResult pdf_inspector_process_pdf(
        [In] byte[] data,
        UIntPtr length,
        [In] byte[]? options,
        UIntPtr optionsLength);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern NativeResult pdf_inspector_detect_pdf(
        [In] byte[] data,
        UIntPtr length,
        [In] byte[]? options,
        UIntPtr optionsLength);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern NativeResult pdf_inspector_classify_pdf(
        [In] byte[] data,
        UIntPtr length);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern NativeResult pdf_inspector_extract_text(
        [In] byte[] data,
        UIntPtr length);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern NativeResult pdf_inspector_process_pdf_with_ocr(
        [In] byte[] data,
        UIntPtr length,
        [In] byte[]? options,
        UIntPtr optionsLength);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern NativeResult pdf_inspector_version();

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void pdf_inspector_free_result(IntPtr data, UIntPtr length);
}
