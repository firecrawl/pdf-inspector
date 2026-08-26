using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading;
using System.Threading.Tasks;

namespace Firecrawl.PdfInspector;

/// <summary>Native PDF classification, extraction, and selective OCR.</summary>
public static class PdfInspector
{
    private static readonly JsonSerializerOptions JsonOptions = CreateJsonOptions();
    private static readonly Lazy<bool> AbiValidated = new Lazy<bool>(ValidateAbi);

    public static PdfProcessResult ProcessPdf(byte[] data, ProcessOptions? options = null)
    {
        ValidateData(data);
        return ProcessPdfCore(data, SerializeOptions(options));
    }

    public static PdfProcessResult ProcessPdf(Stream stream, ProcessOptions? options = null) =>
        ProcessPdf(ReadAllBytes(stream), options);

    public static Task<PdfProcessResult> ProcessPdfAsync(
        byte[] data,
        ProcessOptions? options = null,
        CancellationToken cancellationToken = default)
    {
        ValidateData(data);
        var copy = (byte[])data.Clone();
        var optionBytes = SerializeOptions(options);
        return Task.Run(() => ProcessPdfCore(copy, optionBytes), cancellationToken);
    }

    public static PdfProcessResult DetectPdf(byte[] data, ProcessOptions? options = null)
    {
        ValidateData(data);
        return DetectPdfCore(data, SerializeOptions(options));
    }

    public static PdfProcessResult DetectPdf(Stream stream, ProcessOptions? options = null) =>
        DetectPdf(ReadAllBytes(stream), options);

    public static Task<PdfProcessResult> DetectPdfAsync(
        byte[] data,
        ProcessOptions? options = null,
        CancellationToken cancellationToken = default)
    {
        ValidateData(data);
        var copy = (byte[])data.Clone();
        var optionBytes = SerializeOptions(options);
        return Task.Run(() => DetectPdfCore(copy, optionBytes), cancellationToken);
    }

    public static PdfClassification ClassifyPdf(byte[] data)
    {
        ValidateData(data);
        EnsureAbi();
        return ReadJson<PdfClassification>(
            NativeMethods.pdf_inspector_classify_pdf(data, ToUIntPtr(data.Length)));
    }

    public static PdfClassification ClassifyPdf(Stream stream) =>
        ClassifyPdf(ReadAllBytes(stream));

    public static Task<PdfClassification> ClassifyPdfAsync(
        byte[] data,
        CancellationToken cancellationToken = default)
    {
        ValidateData(data);
        var copy = (byte[])data.Clone();
        return Task.Run(() => ClassifyPdf(copy), cancellationToken);
    }

    public static string ExtractText(byte[] data)
    {
        ValidateData(data);
        EnsureAbi();
        return ReadString(NativeMethods.pdf_inspector_extract_text(data, ToUIntPtr(data.Length)));
    }

    public static string ExtractText(Stream stream) => ExtractText(ReadAllBytes(stream));

    public static Task<string> ExtractTextAsync(
        byte[] data,
        CancellationToken cancellationToken = default)
    {
        ValidateData(data);
        var copy = (byte[])data.Clone();
        return Task.Run(() => ExtractText(copy), cancellationToken);
    }

    public static OcrPdfResult ProcessPdfWithOcr(byte[] data, OcrOptions? options = null)
    {
        ValidateData(data);
        return ProcessPdfWithOcrCore(data, SerializeOptions(options));
    }

    public static OcrPdfResult ProcessPdfWithOcr(Stream stream, OcrOptions? options = null) =>
        ProcessPdfWithOcr(ReadAllBytes(stream), options);

    public static Task<OcrPdfResult> ProcessPdfWithOcrAsync(
        byte[] data,
        OcrOptions? options = null,
        CancellationToken cancellationToken = default)
    {
        ValidateData(data);
        var copy = (byte[])data.Clone();
        var optionBytes = SerializeOptions(options);
        return Task.Run(() => ProcessPdfWithOcrCore(copy, optionBytes), cancellationToken);
    }

    public static string Version()
    {
        EnsureAbi();
        return ReadString(NativeMethods.pdf_inspector_version());
    }

    private static JsonSerializerOptions CreateJsonOptions()
    {
        var options = new JsonSerializerOptions
        {
            PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
            PropertyNameCaseInsensitive = false,
            DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        };
        options.Converters.Add(new JsonStringEnumConverter());
        return options;
    }

    private static void EnsureAbi() => _ = AbiValidated.Value;

    private static PdfProcessResult ProcessPdfCore(byte[] data, byte[]? optionBytes)
    {
        EnsureAbi();
        return ReadJson<PdfProcessResult>(
            NativeMethods.pdf_inspector_process_pdf(
                data,
                ToUIntPtr(data.Length),
                optionBytes,
                ToUIntPtr(optionBytes?.Length ?? 0)));
    }

    private static PdfProcessResult DetectPdfCore(byte[] data, byte[]? optionBytes)
    {
        EnsureAbi();
        return ReadJson<PdfProcessResult>(
            NativeMethods.pdf_inspector_detect_pdf(
                data,
                ToUIntPtr(data.Length),
                optionBytes,
                ToUIntPtr(optionBytes?.Length ?? 0)));
    }

    private static OcrPdfResult ProcessPdfWithOcrCore(byte[] data, byte[]? optionBytes)
    {
        EnsureAbi();
        return ReadJson<OcrPdfResult>(
            NativeMethods.pdf_inspector_process_pdf_with_ocr(
                data,
                ToUIntPtr(data.Length),
                optionBytes,
                ToUIntPtr(optionBytes?.Length ?? 0)));
    }

    private static bool ValidateAbi()
    {
        var actual = NativeMethods.pdf_inspector_abi_version();
        if (actual != NativeMethods.ExpectedAbiVersion)
        {
            throw new PdfInspectorException(
                -1,
                $"Incompatible native pdf-inspector ABI. Expected {NativeMethods.ExpectedAbiVersion}, found {actual}.");
        }
        return true;
    }

    private static byte[]? SerializeOptions<T>(T? options)
        where T : class
    {
        return options is null ? null : JsonSerializer.SerializeToUtf8Bytes(options, JsonOptions);
    }

    private static T ReadJson<T>(NativeMethods.NativeResult result)
    {
        var bytes = TakeResult(result);
        return JsonSerializer.Deserialize<T>(bytes, JsonOptions)
            ?? throw new PdfInspectorException(-1, "The native library returned an empty JSON result.");
    }

    private static string ReadString(NativeMethods.NativeResult result) =>
        Encoding.UTF8.GetString(TakeResult(result));

    private static byte[] TakeResult(NativeMethods.NativeResult result)
    {
        try
        {
            var length = result.Length.ToUInt64();
            if (length > int.MaxValue)
            {
                throw new PdfInspectorException(-1, "The native result exceeds the maximum managed array length.");
            }
            if (length != 0 && result.Data == IntPtr.Zero)
            {
                throw new PdfInspectorException(-1, "The native library returned an invalid result buffer.");
            }

            var bytes = new byte[(int)length];
            if (bytes.Length != 0)
            {
                Marshal.Copy(result.Data, bytes, 0, bytes.Length);
            }
            if (result.Status != 0)
            {
                throw new PdfInspectorException(result.Status, Encoding.UTF8.GetString(bytes));
            }
            return bytes;
        }
        finally
        {
            NativeMethods.pdf_inspector_free_result(result.Data, result.Length);
        }
    }

    private static void ValidateData(byte[] data)
    {
        if (data is null)
        {
            throw new ArgumentNullException(nameof(data));
        }
        if (data.Length == 0)
        {
            throw new ArgumentException("PDF data cannot be empty.", nameof(data));
        }
    }

    private static byte[] ReadAllBytes(Stream stream)
    {
        if (stream is null)
        {
            throw new ArgumentNullException(nameof(stream));
        }
        if (!stream.CanRead)
        {
            throw new ArgumentException("The stream must be readable.", nameof(stream));
        }

        using (var buffer = new MemoryStream())
        {
            stream.CopyTo(buffer);
            return buffer.ToArray();
        }
    }

    private static UIntPtr ToUIntPtr(int length) => new UIntPtr((uint)length);
}
