using System.Diagnostics.CodeAnalysis;

namespace PdfInspector.Pdf;

/// <summary>Raised for structural failures that leave the document unusable.</summary>
public sealed class PdfParseException(string message) : Exception(message);

/// <summary>Where an indirect object lives: at a byte offset, or inside an object stream.</summary>
internal readonly record struct XrefEntry(long Offset, int StreamObjectNumber, int IndexInStream, bool InObjectStream)
{
    public static XrefEntry AtOffset(long offset) => new(offset, 0, 0, false);

    public static XrefEntry InStream(int streamNumber, int index) => new(0, streamNumber, index, true);
}

/// <summary>
/// A parsed PDF file. Objects are resolved lazily and cached, so opening a
/// document only costs the cross-reference scan.
/// </summary>
public sealed partial class PdfDocument
{
    private readonly byte[] _data;
    private readonly PdfParser _parser;
    private readonly Dictionary<int, XrefEntry> _xref = [];
    private readonly Dictionary<int, PdfObject> _cache = [];
    private readonly Dictionary<int, Dictionary<int, PdfObject>> _objectStreamCache = [];
    private readonly HashSet<int> _loading = [];
    private PdfDecryptor? _decryptor;
    private bool _recoveryScanDone;
    private List<PdfObjectId>? _pageIndex;

    private PdfDocument(byte[] data)
    {
        _data = data;
        _parser = new PdfParser(data) { ResolveLength = id => GetObject(id) };
    }

    /// <summary>The trailer dictionary, merged across every cross-reference section.</summary>
    public PdfDictionary Trailer { get; private set; } = new();

    /// <summary>True when the file declared an <c>/Encrypt</c> dictionary.</summary>
    public bool IsEncrypted { get; private set; }

    /// <summary>PDF version from the file header, e.g. "1.7".</summary>
    public string Version { get; private set; } = "1.4";

    // ── Loading ──────────────────────────────────────────────────────────

    public static PdfDocument Load(byte[] data, string? password = null)
    {
        // Malformed struct element names are repaired before parsing: some
        // generators write a bare name ("/S Code" rather than "/S /Code"), which
        // otherwise makes the whole object unparseable and silently drops it.
        var document = new PdfDocument(Structure.BareStructNames.Fix(data));
        document.Initialise(password);
        return document;
    }

    public static PdfDocument LoadFile(string path, string? password = null) =>
        Load(File.ReadAllBytes(path), password);

    private void Initialise(string? password)
    {
        ReadHeaderVersion();

        try
        {
            ReadXrefChain();
        }
        catch (PdfEncryptedException)
        {
            throw;
        }
        catch (Exception)
        {
            // A broken cross-reference table is recoverable by scanning.
            _xref.Clear();
        }

        if (_xref.Count == 0 || Trailer.Get("Root") is null)
        {
            RunRecoveryScan();
        }

        SetUpDecryption(password);

        if (FindCatalog() is null)
        {
            throw new PdfParseException("PDF has no document catalog");
        }
    }

    private void ReadHeaderVersion()
    {
        var index = PdfParser.IndexOf(_data, "%PDF-"u8, 0);
        if (index < 0 || index > 1024)
        {
            return;
        }

        var end = Math.Min(_data.Length, index + 12);
        var span = _data.AsSpan(index + 5, end - index - 5);
        var length = 0;
        while (length < span.Length && (char.IsAsciiDigit((char)span[length]) || span[length] == (byte)'.'))
        {
            length++;
        }

        if (length > 0)
        {
            Version = System.Text.Encoding.ASCII.GetString(span[..length]);
        }
    }

    private void SetUpDecryption(string? password)
    {
        var encryptRef = Trailer.Get("Encrypt");
        if (encryptRef is null)
        {
            return;
        }

        IsEncrypted = true;

        // The /Encrypt dictionary is itself never encrypted.
        var encrypt = Resolve(encryptRef).AsDictionary()
            ?? throw new PdfEncryptedException("PDF is encrypted but the /Encrypt dictionary is unreadable");

        var fileId = Resolve(Trailer.Get("ID")).AsArray();
        _decryptor = PdfDecryptor.Create(encrypt, fileId, password, o => Resolve(o));

        // Objects cached before the key was derived hold ciphertext.
        _cache.Clear();
        _objectStreamCache.Clear();
    }

    // ── Cross-reference parsing ──────────────────────────────────────────

    private void ReadXrefChain()
    {
        var startxref = FindStartXref();
        if (startxref < 0)
        {
            return;
        }

        var visited = new HashSet<long>();
        var queue = new Queue<long>();
        queue.Enqueue(startxref);

        while (queue.Count > 0)
        {
            var offset = queue.Dequeue();
            if (offset < 0 || offset >= _data.Length || !visited.Add(offset))
            {
                continue;
            }

            var trailer = ReadXrefSection((int)offset);
            if (trailer is null)
            {
                continue;
            }

            MergeTrailer(trailer);

            // /XRefStm points at a cross-reference stream shadowing a hybrid table.
            if (trailer.Get("XRefStm")?.AsInteger() is { } hybrid)
            {
                queue.Enqueue(hybrid);
            }

            if (trailer.Get("Prev")?.AsInteger() is { } previous)
            {
                queue.Enqueue(previous);
            }
        }
    }

    private long FindStartXref()
    {
        // The keyword lives in the last kilobyte or so of the file.
        var index = PdfParser.LastIndexOf(_data, "startxref"u8, _data.Length);
        if (index < 0)
        {
            return -1;
        }

        var lexer = new PdfLexer(_data, index + "startxref".Length);
        lexer.SkipWhitespace();
        var token = lexer.ReadToken();
        return PdfLexer.ParseNumber(token)?.AsInteger() ?? -1;
    }

    /// <summary>Reads either a classic <c>xref</c> table or a cross-reference stream. Returns its trailer.</summary>
    private PdfDictionary? ReadXrefSection(int offset)
    {
        var lexer = new PdfLexer(_data, offset);
        lexer.SkipWhitespace();

        if (lexer.TryConsume("xref"))
        {
            return ReadXrefTable(lexer);
        }

        // Otherwise it should be "N G obj" introducing a cross-reference stream.
        var parsed = new PdfParser(_data).ParseIndirectObjectAt(offset);
        if (parsed?.Value is not PdfStream stream)
        {
            return null;
        }

        ReadXrefStream(stream);
        return stream.Dictionary;
    }

    private PdfDictionary? ReadXrefTable(PdfLexer lexer)
    {
        while (true)
        {
            lexer.SkipWhitespace();

            if (lexer.TryConsume("trailer"))
            {
                var parser = new PdfParser(_data) { Position = lexer.Position };
                return parser.ParseObject()?.AsDictionary();
            }

            var startToken = lexer.ReadToken();
            if (startToken.Length == 0 || PdfLexer.ParseNumber(startToken) is not PdfInteger start)
            {
                return null;
            }

            lexer.SkipWhitespace();
            var countToken = lexer.ReadToken();
            if (countToken.Length == 0 || PdfLexer.ParseNumber(countToken) is not PdfInteger count)
            {
                return null;
            }

            for (var i = 0; i < count.Value; i++)
            {
                lexer.SkipWhitespace();
                var offsetToken = lexer.ReadToken();
                lexer.SkipWhitespace();
                var genToken = lexer.ReadToken();
                lexer.SkipWhitespace();
                var kindToken = lexer.ReadToken();

                if (offsetToken.Length == 0 || kindToken.Length == 0)
                {
                    return null;
                }

                if (kindToken != "n")
                {
                    continue;
                }

                var number = (int)(start.Value + i);
                if (PdfLexer.ParseNumber(offsetToken)?.AsInteger() is not { } entryOffset || entryOffset <= 0)
                {
                    continue;
                }

                _ = genToken;

                // Earlier sections in the chain must not override later ones.
                _xref.TryAdd(number, XrefEntry.AtOffset(entryOffset));
            }
        }
    }

    private void ReadXrefStream(PdfStream stream)
    {
        var content = stream.DecompressedContent();
        if (content is null)
        {
            return;
        }

        var w = stream.Dictionary.Get("W")?.AsArray();
        if (w is null || w.Count < 3)
        {
            return;
        }

        var widths = new int[w.Count];
        for (var i = 0; i < w.Count; i++)
        {
            widths[i] = (int)(w[i].AsInteger() ?? 0);
        }

        var size = (int)(stream.Dictionary.Get("Size")?.AsInteger() ?? 0);

        var ranges = new List<(int Start, int Count)>();
        if (stream.Dictionary.Get("Index")?.AsArray() is { } index)
        {
            for (var i = 0; i + 1 < index.Count; i += 2)
            {
                ranges.Add(((int)(index[i].AsInteger() ?? 0), (int)(index[i + 1].AsInteger() ?? 0)));
            }
        }

        if (ranges.Count == 0)
        {
            ranges.Add((0, size));
        }

        var rowLength = widths.Sum();
        if (rowLength <= 0)
        {
            return;
        }

        var position = 0;
        foreach (var (start, count) in ranges)
        {
            for (var i = 0; i < count; i++)
            {
                if (position + rowLength > content.Length)
                {
                    return;
                }

                var fields = new long[widths.Length];
                for (var f = 0; f < widths.Length; f++)
                {
                    long value = 0;
                    for (var b = 0; b < widths[f]; b++)
                    {
                        value = (value << 8) | content[position++];
                    }

                    fields[f] = value;
                }

                // A zero-width type field defaults to type 1 (an in-file object).
                var type = widths[0] == 0 ? 1 : fields[0];
                var number = start + i;

                switch (type)
                {
                    case 1 when fields[1] > 0:
                        _xref.TryAdd(number, XrefEntry.AtOffset(fields[1]));
                        break;
                    case 2:
                        _xref.TryAdd(number, XrefEntry.InStream((int)fields[1], (int)fields[2]));
                        break;
                }
            }
        }
    }

    private void MergeTrailer(PdfDictionary trailer)
    {
        foreach (var (key, value) in trailer)
        {
            // The newest section wins; earlier ones only fill gaps.
            if (!Trailer.ContainsKey(key))
            {
                Trailer[key] = value;
            }
        }
    }

    // ── Recovery ─────────────────────────────────────────────────────────

    /// <summary>
    /// Rebuilds the cross-reference map by scanning the file for <c>N G obj</c>
    /// headers. This is what makes documents with broken or absent tables load.
    /// </summary>
    private void RunRecoveryScan()
    {
        if (_recoveryScanDone)
        {
            return;
        }

        _recoveryScanDone = true;

        var found = new Dictionary<int, long>();
        var position = 0;

        while (position < _data.Length)
        {
            var index = PdfParser.IndexOf(_data, "obj"u8, position);
            if (index < 0)
            {
                break;
            }

            position = index + 3;

            // "obj" must be preceded by "<num> <gen> " and followed by a non-regular byte.
            if (index + 3 < _data.Length && PdfLexer.IsRegular(_data[index + 3]))
            {
                continue;
            }

            var cursor = index - 1;
            while (cursor >= 0 && PdfLexer.IsWhitespace(_data[cursor]))
            {
                cursor--;
            }

            var genEnd = cursor + 1;
            while (cursor >= 0 && char.IsAsciiDigit((char)_data[cursor]))
            {
                cursor--;
            }

            var genStart = cursor + 1;
            if (genStart == genEnd)
            {
                continue;
            }

            while (cursor >= 0 && PdfLexer.IsWhitespace(_data[cursor]))
            {
                cursor--;
            }

            var numEnd = cursor + 1;
            while (cursor >= 0 && char.IsAsciiDigit((char)_data[cursor]))
            {
                cursor--;
            }

            var numStart = cursor + 1;
            if (numStart == numEnd || numEnd == genStart)
            {
                continue;
            }

            var numberText = System.Text.Encoding.ASCII.GetString(_data, numStart, numEnd - numStart);
            if (!int.TryParse(numberText, out var number))
            {
                continue;
            }

            // A later definition of the same object supersedes an earlier one.
            found[number] = numStart;
        }

        foreach (var (number, offset) in found)
        {
            _xref[number] = XrefEntry.AtOffset(offset);
        }

        _cache.Clear();

        if (Trailer.Get("Root") is null)
        {
            RecoverTrailer(found);
        }
    }

    private void RecoverTrailer(Dictionary<int, long> found)
    {
        // Prefer an explicit trailer dictionary if one survived.
        var position = _data.Length;
        while (true)
        {
            var index = PdfParser.LastIndexOf(_data, "trailer"u8, position);
            if (index < 0)
            {
                break;
            }

            var parser = new PdfParser(_data) { Position = index + "trailer".Length };
            if (parser.ParseObject()?.AsDictionary() is { } dict && dict.Get("Root") is not null)
            {
                MergeTrailer(dict);
                return;
            }

            position = index;
        }

        // Otherwise find the catalog (or a cross-reference stream carrying /Root)
        // among the objects the scan recovered.
        foreach (var number in found.Keys.OrderBy(n => n))
        {
            var candidate = LoadObjectFromFile(number);
            switch (candidate)
            {
                case PdfDictionary dict when dict.Get("Type")?.AsName() == "Catalog":
                    Trailer["Root"] = new PdfReference(new PdfObjectId(number, 0));
                    return;
                case PdfStream stream when stream.Dictionary.Get("Type")?.AsName() == "XRef":
                    if (stream.Dictionary.Get("Root") is { } root)
                    {
                        Trailer["Root"] = root;
                        if (stream.Dictionary.Get("Encrypt") is { } encrypt)
                        {
                            Trailer["Encrypt"] = encrypt;
                        }

                        if (stream.Dictionary.Get("ID") is { } id)
                        {
                            Trailer["ID"] = id;
                        }

                        return;
                    }

                    break;
            }
        }

        // Last resort: a page tree without a catalog still yields pages.
        foreach (var number in found.Keys.OrderBy(n => n))
        {
            if (LoadObjectFromFile(number) is PdfDictionary dict &&
                dict.Get("Type")?.AsName() == "Pages" &&
                dict.Get("Parent") is null)
            {
                var synthetic = new PdfDictionary
                {
                    ["Type"] = new PdfName("Catalog"),
                    ["Pages"] = new PdfReference(new PdfObjectId(number, 0)),
                };
                var syntheticId = new PdfObjectId(NextFreeObjectNumber(), 0);
                _cache[syntheticId.Number] = synthetic;
                Trailer["Root"] = new PdfReference(syntheticId);
                return;
            }
        }
    }

    private int NextFreeObjectNumber()
    {
        var max = 0;
        foreach (var number in _xref.Keys)
        {
            max = Math.Max(max, number);
        }

        foreach (var number in _cache.Keys)
        {
            max = Math.Max(max, number);
        }

        return max + 1;
    }

    // ── Object access ────────────────────────────────────────────────────

    /// <summary>Fetches an indirect object, or null when it is missing or unparseable.</summary>
    public PdfObject? GetObject(PdfObjectId id) => GetObject(id.Number);

    private PdfObject? GetObject(int number)
    {
        if (_cache.TryGetValue(number, out var cached))
        {
            return cached;
        }

        // Guards against an object whose /Length references itself.
        if (!_loading.Add(number))
        {
            return null;
        }

        try
        {
            var value = LoadObject(number);
            if (value is null && !_recoveryScanDone)
            {
                RunRecoveryScan();
                value = LoadObject(number);
            }

            if (value is not null)
            {
                _cache[number] = value;
            }

            return value;
        }
        finally
        {
            _loading.Remove(number);
        }
    }

    private PdfObject? LoadObject(int number)
    {
        if (!_xref.TryGetValue(number, out var entry))
        {
            return null;
        }

        return entry.InObjectStream
            ? LoadFromObjectStream(entry.StreamObjectNumber, entry.IndexInStream, number)
            : LoadObjectFromFile(number);
    }

    private PdfObject? LoadObjectFromFile(int number)
    {
        if (!_xref.TryGetValue(number, out var entry) || entry.InObjectStream)
        {
            return null;
        }

        var parsed = _parser.ParseIndirectObjectAt((int)entry.Offset);
        if (parsed is null)
        {
            return null;
        }

        // A mismatched header means the offset was stale; the recovery scan will fix it.
        if (parsed.Value.Id.Number != number)
        {
            return null;
        }

        var value = parsed.Value.Value;
        return _decryptor is { IsIdentity: false } ? DecryptObject(value, parsed.Value.Id) : value;
    }

    private PdfObject? LoadFromObjectStream(int streamNumber, int index, int wantedNumber)
    {
        if (!_objectStreamCache.TryGetValue(streamNumber, out var entries))
        {
            entries = ParseObjectStream(streamNumber);
            _objectStreamCache[streamNumber] = entries;
        }

        // Prefer the recorded index, but fall back to the object number: some
        // writers emit indices that do not match the stream's own /First table.
        return entries.TryGetValue(wantedNumber, out var value) ? value : entries.GetValueOrDefault(-index - 1);
    }

    private Dictionary<int, PdfObject> ParseObjectStream(int streamNumber)
    {
        var result = new Dictionary<int, PdfObject>();

        if (GetObject(streamNumber) is not PdfStream stream)
        {
            return result;
        }

        var content = stream.DecompressedContent();
        if (content is null)
        {
            return result;
        }

        var count = (int)(Resolve(stream.Dictionary.Get("N"))?.AsInteger() ?? 0);
        var first = (int)(Resolve(stream.Dictionary.Get("First"))?.AsInteger() ?? 0);
        if (count <= 0 || first < 0 || first > content.Length)
        {
            return result;
        }

        var header = new PdfLexer(content);
        var pairs = new List<(int Number, int Offset)>(count);

        for (var i = 0; i < count; i++)
        {
            header.SkipWhitespace();
            var numberToken = header.ReadToken();
            header.SkipWhitespace();
            var offsetToken = header.ReadToken();

            if (PdfLexer.ParseNumber(numberToken)?.AsInteger() is not { } objectNumber ||
                PdfLexer.ParseNumber(offsetToken)?.AsInteger() is not { } objectOffset)
            {
                break;
            }

            if (header.Position > first)
            {
                break;
            }

            pairs.Add(((int)objectNumber, (int)objectOffset));
        }

        // Objects inside an object stream are never individually encrypted.
        var bodyParser = new PdfParser(content);
        for (var i = 0; i < pairs.Count; i++)
        {
            var (objectNumber, objectOffset) = pairs[i];
            var absolute = first + objectOffset;
            if (absolute < 0 || absolute >= content.Length)
            {
                continue;
            }

            bodyParser.Position = absolute;
            var value = bodyParser.ParseObject();
            if (value is null)
            {
                continue;
            }

            result[objectNumber] = value;
            result[-i - 1] = value;
        }

        return result;
    }

    private PdfObject DecryptObject(PdfObject value, PdfObjectId id)
    {
        switch (value)
        {
            case PdfString str:
                return new PdfString(_decryptor!.DecryptString(str.Bytes, id), str.Format);

            case PdfArray array:
                return new PdfArray(array.Select(item => DecryptObject(item, id)));

            case PdfStream stream:
            {
                var dict = (PdfDictionary)DecryptObject(stream.Dictionary, id);
                // Cross-reference streams and streams with an Identity crypt
                // filter are stored in the clear.
                var type = stream.Dictionary.Get("Type")?.AsName();
                var raw = type == "XRef" ? stream.RawData : _decryptor!.DecryptStream(stream.RawData, id);
                return new PdfStream(dict, raw);
            }

            case PdfDictionary dictionary:
            {
                var copy = new PdfDictionary();
                foreach (var (key, entry) in dictionary)
                {
                    copy[key] = DecryptObject(entry, id);
                }

                return copy;
            }

            default:
                return value;
        }
    }

    /// <summary>Follows an indirect reference to the object it names. Non-references pass through.</summary>
    public PdfObject Resolve(PdfObject? value)
    {
        var guard = 0;
        while (value is PdfReference reference && guard++ < 64)
        {
            value = GetObject(reference.Id);
        }

        return value ?? PdfObject.Null;
    }

    /// <summary>Looks up a dictionary key and resolves the result.</summary>
    public PdfObject? GetDeref(PdfDictionary? dict, string key)
    {
        var value = dict?.Get(key);
        if (value is null)
        {
            return null;
        }

        var resolved = Resolve(value);
        return resolved.IsNull ? null : resolved;
    }

    public PdfDictionary? GetDict(PdfDictionary? dict, string key) => GetDeref(dict, key)?.AsDictionary();

    public PdfArray? GetArray(PdfDictionary? dict, string key) => GetDeref(dict, key)?.AsArray();

    public string? GetName(PdfDictionary? dict, string key) => GetDeref(dict, key)?.AsName();

    public long? GetInteger(PdfDictionary? dict, string key) => GetDeref(dict, key)?.AsInteger();

    public double? GetNumber(PdfDictionary? dict, string key) => GetDeref(dict, key)?.AsNumber();

    public PdfStream? GetStream(PdfDictionary? dict, string key) => GetDeref(dict, key)?.AsStream();

    // ── Page tree ────────────────────────────────────────────────────────

    /// <summary>The document catalog.</summary>
    public PdfDictionary? Catalog => FindCatalog();

    private PdfDictionary? FindCatalog() => Resolve(Trailer.Get("Root")).AsDictionary();

    /// <summary>Object ids of every page, in document order.</summary>
    public IReadOnlyList<PdfObjectId> PageIds => _pageIndex ??= BuildPageIndex();

    public int PageCount => PageIds.Count;

    /// <summary>The page dictionary for a 1-indexed page number.</summary>
    public PdfDictionary? GetPage(int pageNumber)
    {
        var ids = PageIds;
        if (pageNumber < 1 || pageNumber > ids.Count)
        {
            return null;
        }

        return GetObject(ids[pageNumber - 1])?.AsDictionary();
    }

    private List<PdfObjectId> BuildPageIndex()
    {
        var pages = new List<PdfObjectId>();
        var root = GetDict(FindCatalog(), "Pages");

        if (root is not null)
        {
            var visited = new HashSet<int>();
            CollectPages(FindCatalog()!.Get("Pages")!, pages, visited, 0);
        }

        if (pages.Count == 0)
        {
            // Some damaged files lose the page tree but keep the page objects.
            RunRecoveryScan();
            foreach (var number in _xref.Keys.OrderBy(n => n))
            {
                if (GetObject(number)?.AsDictionary() is { } dict && dict.Get("Type")?.AsName() == "Page")
                {
                    pages.Add(new PdfObjectId(number, 0));
                }
            }
        }

        return pages;
    }

    private void CollectPages(PdfObject node, List<PdfObjectId> pages, HashSet<int> visited, int depth)
    {
        if (depth > 64 || pages.Count > 100_000)
        {
            return;
        }

        var id = node.AsReference();
        if (id is not null && !visited.Add(id.Value.Number))
        {
            return;
        }

        var dict = Resolve(node).AsDictionary();
        if (dict is null)
        {
            return;
        }

        var type = dict.Get("Type")?.AsName();
        var kids = GetArray(dict, "Kids");

        // /Type is optional in practice; the presence of /Kids decides.
        if (type == "Page" || (kids is null && type != "Pages"))
        {
            if (id is not null)
            {
                pages.Add(id.Value);
            }

            return;
        }

        if (kids is null)
        {
            return;
        }

        foreach (var kid in kids)
        {
            CollectPages(kid, pages, visited, depth + 1);
        }
    }

    /// <summary>
    /// Reads a page attribute, walking up <c>/Parent</c> for the keys the
    /// specification declares inheritable (Resources, MediaBox, CropBox, Rotate).
    /// </summary>
    public PdfObject? GetInheritedPageAttribute(PdfDictionary page, string key)
    {
        var current = page;
        for (var depth = 0; depth < 64 && current is not null; depth++)
        {
            if (GetDeref(current, key) is { } value)
            {
                return value;
            }

            current = GetDict(current, "Parent");
        }

        return null;
    }

    /// <summary>Concatenated, decoded content streams for a 1-indexed page.</summary>
    public byte[] GetPageContent(int pageNumber)
    {
        var page = GetPage(pageNumber);
        return page is null ? [] : GetPageContent(page);
    }

    public byte[] GetPageContent(PdfDictionary page)
    {
        var contents = page.Get("Contents");
        if (contents is null)
        {
            return [];
        }

        var buffer = new MemoryStream();
        AppendContent(contents, buffer, 0);
        return buffer.ToArray();
    }

    private void AppendContent(PdfObject node, MemoryStream buffer, int depth)
    {
        if (depth > 8)
        {
            return;
        }

        var resolved = Resolve(node);
        switch (resolved)
        {
            case PdfStream stream:
                var content = stream.DecompressedContent();
                if (content is { Length: > 0 })
                {
                    buffer.Write(content, 0, content.Length);
                    // Streams are concatenated with a separator so a token cannot
                    // straddle the boundary.
                    buffer.WriteByte((byte)'\n');
                }

                break;

            case PdfArray array:
                foreach (var item in array)
                {
                    AppendContent(item, buffer, depth + 1);
                }

                break;
        }
    }

    /// <summary>Every object number the cross-reference map knows about.</summary>
    public IEnumerable<PdfObjectId> ObjectIds => _xref.Keys.OrderBy(n => n).Select(n => new PdfObjectId(n, 0));

    /// <summary>Tries to read the document's <c>/Info</c> title.</summary>
    public bool TryGetTitle([NotNullWhen(true)] out string? title)
    {
        title = null;
        var info = GetDict(Trailer, "Info");
        if (info is null)
        {
            return false;
        }

        if (GetDeref(info, "Title") is not PdfString str)
        {
            return false;
        }

        var text = str.AsText().Trim();
        if (text.Length == 0)
        {
            return false;
        }

        title = text;
        return true;
    }
}
