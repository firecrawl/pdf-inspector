using System.Diagnostics.CodeAnalysis;
using System.Globalization;
using System.Text;

namespace PdfInspector.Pdf;

/// <summary>Identifies an indirect object: its number and generation.</summary>
public readonly record struct PdfObjectId(int Number, ushort Generation)
{
    public override string ToString() => $"{Number} {Generation} R";
}

/// <summary>How a string literal was written in the file. Round-tripped for callers that re-serialise.</summary>
public enum PdfStringFormat
{
    Literal,
    Hexadecimal,
}

/// <summary>
/// A PDF object. The concrete subclasses mirror the eight basic types of the
/// PDF object model plus indirect references, which the parser leaves unresolved
/// until <see cref="PdfDocument.Resolve"/> is called.
/// </summary>
public abstract class PdfObject
{
    public static readonly PdfNull Null = new();

    public virtual string? AsName() => null;

    public virtual PdfArray? AsArray() => null;

    public virtual PdfDictionary? AsDictionary() => null;

    public virtual PdfStream? AsStream() => null;

    public virtual PdfObjectId? AsReference() => null;

    public virtual bool? AsBoolean() => null;

    public virtual long? AsInteger() => null;

    /// <summary>Numeric value of an integer or real object.</summary>
    public virtual double? AsNumber() => null;

    /// <summary>Raw bytes of a string object, before any text-encoding interpretation.</summary>
    public virtual byte[]? AsStringBytes() => null;

    public float? AsFloat() => (float?)AsNumber();

    public bool IsNull => this is PdfNull;
}

public sealed class PdfNull : PdfObject
{
    public override string ToString() => "null";
}

public sealed class PdfBoolean(bool value) : PdfObject
{
    public static readonly PdfBoolean True = new(true);
    public static readonly PdfBoolean False = new(false);

    public bool Value { get; } = value;

    public static PdfBoolean Of(bool value) => value ? True : False;

    public override bool? AsBoolean() => Value;

    public override string ToString() => Value ? "true" : "false";
}

public sealed class PdfInteger(long value) : PdfObject
{
    /// <summary>
    /// Instances for the small values content streams are made of. Font sizes,
    /// glyph displacements, colour components and array indices land here
    /// almost every time, so caching this range removes most integer
    /// allocation from stream decoding.
    /// </summary>
    private const int CacheMin = -1024;
    private const int CacheMax = 4096;

    private static readonly PdfInteger[] Cache = BuildCache();

    private static PdfInteger[] BuildCache()
    {
        var cache = new PdfInteger[CacheMax - CacheMin + 1];
        for (var i = 0; i < cache.Length; i++)
        {
            cache[i] = new PdfInteger(i + CacheMin);
        }

        return cache;
    }

    /// <summary>Returns a cached instance for small values, or a new one.</summary>
    public static PdfInteger Create(long value) =>
        value is >= CacheMin and <= CacheMax
            ? Cache[(int)value - CacheMin]
            : new PdfInteger(value);

    public long Value { get; } = value;

    public override long? AsInteger() => Value;

    public override double? AsNumber() => Value;

    public override string ToString() => Value.ToString(CultureInfo.InvariantCulture);
}

public sealed class PdfReal(double value) : PdfObject
{
    public double Value { get; } = value;

    public override double? AsNumber() => Value;

    public override string ToString() => Value.ToString("R", CultureInfo.InvariantCulture);
}

public sealed class PdfString(byte[] bytes, PdfStringFormat format = PdfStringFormat.Literal) : PdfObject
{
    public byte[] Bytes { get; } = bytes;

    public PdfStringFormat Format { get; } = format;

    public override byte[]? AsStringBytes() => Bytes;

    /// <summary>
    /// Decodes the string as PDF text: UTF-16BE when the UTF-16 byte-order mark is
    /// present, UTF-8 when the UTF-8 mark is, otherwise PDFDocEncoding (approximated
    /// by Latin-1, which agrees for every code point PDFDocEncoding shares with it).
    /// </summary>
    public string AsText()
    {
        var b = Bytes;
        if (b.Length >= 2 && b[0] == 0xFE && b[1] == 0xFF)
        {
            return Encoding.BigEndianUnicode.GetString(b, 2, b.Length - 2);
        }

        if (b.Length >= 3 && b[0] == 0xEF && b[1] == 0xBB && b[2] == 0xBF)
        {
            return Encoding.UTF8.GetString(b, 3, b.Length - 3);
        }

        return PdfEncodings.PdfDocEncodingToString(b);
    }

    public override string ToString() => AsText();
}

public sealed class PdfName(string value) : PdfObject
{
    public string Value { get; } = value;

    public override string? AsName() => Value;

    public override string ToString() => "/" + Value;
}

public sealed class PdfReference(PdfObjectId id) : PdfObject
{
    public PdfObjectId Id { get; } = id;

    public override PdfObjectId? AsReference() => Id;

    public override string ToString() => Id.ToString();
}

public sealed class PdfArray : PdfObject, IReadOnlyList<PdfObject>
{
    private readonly List<PdfObject> _items;

    public PdfArray() => _items = [];

    public PdfArray(IEnumerable<PdfObject> items) => _items = [.. items];

    public PdfArray(List<PdfObject> items) => _items = items;

    public PdfObject this[int index] => _items[index];

    public int Count => _items.Count;

    public void Add(PdfObject item) => _items.Add(item);

    public override PdfArray? AsArray() => this;

    public IEnumerator<PdfObject> GetEnumerator() => _items.GetEnumerator();

    System.Collections.IEnumerator System.Collections.IEnumerable.GetEnumerator() => GetEnumerator();

    public override string ToString() => "[" + string.Join(" ", _items) + "]";
}

/// <summary>
/// A PDF dictionary. Key order is preserved because a few heuristics in the
/// extractor depend on the order resources were written in.
/// </summary>
public sealed class PdfDictionary : PdfObject, IEnumerable<KeyValuePair<string, PdfObject>>
{
    private readonly Dictionary<string, PdfObject> _map;
    private readonly List<string> _order;

    public PdfDictionary()
    {
        _map = new Dictionary<string, PdfObject>(StringComparer.Ordinal);
        _order = [];
    }

    public int Count => _map.Count;

    public IReadOnlyList<string> Keys => _order;

    public PdfObject? this[string key]
    {
        get => _map.GetValueOrDefault(key);
        set
        {
            if (value is null)
            {
                if (_map.Remove(key))
                {
                    _order.Remove(key);
                }

                return;
            }

            if (!_map.ContainsKey(key))
            {
                _order.Add(key);
            }

            _map[key] = value;
        }
    }

    public bool ContainsKey(string key) => _map.ContainsKey(key);

    public bool TryGetValue(string key, [NotNullWhen(true)] out PdfObject? value) => _map.TryGetValue(key, out value);

    public PdfObject? Get(string key) => _map.GetValueOrDefault(key);

    public void Remove(string key)
    {
        if (_map.Remove(key))
        {
            _order.Remove(key);
        }
    }

    public override PdfDictionary? AsDictionary() => this;

    public IEnumerator<KeyValuePair<string, PdfObject>> GetEnumerator()
    {
        foreach (var key in _order)
        {
            yield return new KeyValuePair<string, PdfObject>(key, _map[key]);
        }
    }

    System.Collections.IEnumerator System.Collections.IEnumerable.GetEnumerator() => GetEnumerator();

    public override string ToString() =>
        "<<" + string.Join(" ", this.Select(kv => "/" + kv.Key + " " + kv.Value)) + ">>";
}

/// <summary>
/// A stream object: a dictionary plus raw (still encoded) bytes. Decoding is
/// deferred and cached, because most streams in a document are never read.
/// </summary>
public sealed class PdfStream(PdfDictionary dictionary, byte[] rawData) : PdfObject
{
    private byte[]? _decoded;
    private bool _decodeFailed;

    public PdfDictionary Dictionary { get; } = dictionary;

    /// <summary>Stream bytes exactly as they appeared in the file (still filtered/encrypted).</summary>
    public byte[] RawData { get; internal set; } = rawData;

    public override PdfStream? AsStream() => this;

    public override PdfDictionary? AsDictionary() => Dictionary;

    /// <summary>
    /// Applies the stream's filter chain. Image-only filters (DCT, JPX, CCITT,
    /// JBIG2) are left encoded, matching the behaviour the extractor relies on:
    /// it only ever inspects such streams' dictionaries.
    /// </summary>
    public byte[]? DecompressedContent()
    {
        if (_decoded is not null)
        {
            return _decoded;
        }

        if (_decodeFailed)
        {
            return null;
        }

        var result = StreamFilters.Decode(this);
        if (result is null)
        {
            _decodeFailed = true;
            return null;
        }

        _decoded = result;
        return result;
    }

    public override string ToString() => $"stream({RawData.Length} bytes) {Dictionary}";
}
