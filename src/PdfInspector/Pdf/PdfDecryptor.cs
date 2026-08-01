using System.Security.Cryptography;
using System.Text;

namespace PdfInspector.Pdf;

/// <summary>Raised when a document is encrypted and cannot be decrypted with the supplied password.</summary>
public sealed class PdfEncryptedException(string message) : Exception(message);

internal enum CryptMethod
{
    None,
    Rc4,
    AesV2,
    AesV3,
}

/// <summary>
/// Implements the standard security handler (revisions 2–6), which is what
/// password-protected PDFs in the wild use.
/// </summary>
internal sealed class PdfDecryptor
{
    private static readonly byte[] Padding =
    [
        0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56,
        0xFF, 0xFA, 0x01, 0x08, 0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80,
        0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
    ];

    private byte[] _fileKey = [];
    private CryptMethod _streamMethod = CryptMethod.Rc4;
    private CryptMethod _stringMethod = CryptMethod.Rc4;
    private int _revision;

    /// <summary>True when the handler is a no-op (all crypt filters are Identity).</summary>
    public bool IsIdentity => _streamMethod == CryptMethod.None && _stringMethod == CryptMethod.None;

    /// <summary>
    /// Builds a decryptor from the document's <c>/Encrypt</c> dictionary.
    /// Throws <see cref="PdfEncryptedException"/> when the password is wrong or
    /// the handler is one this implementation does not cover.
    /// </summary>
    public static PdfDecryptor Create(
        PdfDictionary encrypt,
        PdfArray? fileId,
        string? password,
        Func<PdfObject?, PdfObject?> resolve)
    {
        var filter = resolve(encrypt.Get("Filter"))?.AsName();
        if (filter is not null && filter != "Standard")
        {
            throw new PdfEncryptedException($"unsupported security handler: {filter}");
        }

        var decryptor = new PdfDecryptor();
        var v = (int)(resolve(encrypt.Get("V"))?.AsInteger() ?? 0);
        var r = (int)(resolve(encrypt.Get("R"))?.AsInteger() ?? 0);
        decryptor._revision = r;

        var lengthBits = (int)(resolve(encrypt.Get("Length"))?.AsInteger() ?? 40);
        var keyLength = Math.Clamp(lengthBits / 8, 5, 32);

        var ownerBytes = resolve(encrypt.Get("O"))?.AsStringBytes() ?? [];
        var userBytes = resolve(encrypt.Get("U"))?.AsStringBytes() ?? [];
        var permissions = (int)(resolve(encrypt.Get("P"))?.AsInteger() ?? 0);

        var encryptMetadata = resolve(encrypt.Get("EncryptMetadata"))?.AsBoolean() ?? true;

        if (v is 4 or 5)
        {
            var (stream, str, cfLength) = ReadCryptFilters(encrypt, resolve);
            decryptor._streamMethod = stream;
            decryptor._stringMethod = str;
            if (cfLength > 0)
            {
                keyLength = Math.Clamp(cfLength, 5, 32);
            }
        }
        else
        {
            decryptor._streamMethod = CryptMethod.Rc4;
            decryptor._stringMethod = CryptMethod.Rc4;
            if (v <= 1)
            {
                keyLength = 5;
            }
        }

        var passwordBytes = Encoding.UTF8.GetBytes(password ?? string.Empty);

        if (r >= 5)
        {
            decryptor._fileKey = ComputeKeyR5Plus(passwordBytes, ownerBytes, userBytes, encrypt, resolve, r)
                ?? throw new PdfEncryptedException("incorrect password");
            return decryptor;
        }

        if (r is < 2 or > 4)
        {
            throw new PdfEncryptedException($"unsupported standard security handler revision {r}");
        }

        var idBytes = fileId is { Count: > 0 } ? fileId[0].AsStringBytes() ?? [] : [];

        // Try the supplied password as the user password, then as the owner
        // password (which yields the user password when decrypted).
        var key = ComputeKeyLegacy(passwordBytes, ownerBytes, permissions, idBytes, r, keyLength, encryptMetadata);
        if (!VerifyUserPassword(key, userBytes, idBytes, r))
        {
            var recovered = RecoverUserPassword(passwordBytes, ownerBytes, r, keyLength);
            if (recovered is null)
            {
                throw new PdfEncryptedException("incorrect password");
            }

            key = ComputeKeyLegacy(recovered, ownerBytes, permissions, idBytes, r, keyLength, encryptMetadata);
            if (!VerifyUserPassword(key, userBytes, idBytes, r))
            {
                throw new PdfEncryptedException("incorrect password");
            }
        }

        decryptor._fileKey = key;
        return decryptor;
    }

    private static (CryptMethod Stream, CryptMethod String, int Length) ReadCryptFilters(
        PdfDictionary encrypt,
        Func<PdfObject?, PdfObject?> resolve)
    {
        var cf = resolve(encrypt.Get("CF"))?.AsDictionary();
        var stmF = resolve(encrypt.Get("StmF"))?.AsName() ?? "Identity";
        var strF = resolve(encrypt.Get("StrF"))?.AsName() ?? "Identity";

        var length = 0;

        CryptMethod Lookup(string name)
        {
            if (name == "Identity")
            {
                return CryptMethod.None;
            }

            var entry = resolve(cf?.Get(name))?.AsDictionary();
            if (entry is null)
            {
                return CryptMethod.None;
            }

            var bits = (int)(resolve(entry.Get("Length"))?.AsInteger() ?? 0);
            if (bits > 0)
            {
                // /Length here is in bytes for some writers and bits for others.
                length = bits > 40 ? bits / 8 : bits;
            }

            return resolve(entry.Get("CFM"))?.AsName() switch
            {
                "V2" => CryptMethod.Rc4,
                "AESV2" => CryptMethod.AesV2,
                "AESV3" => CryptMethod.AesV3,
                "None" => CryptMethod.None,
                _ => CryptMethod.None,
            };
        }

        return (Lookup(stmF), Lookup(strF), length);
    }

    // ── Key derivation ───────────────────────────────────────────────────

    private static byte[] ComputeKeyLegacy(
        byte[] password,
        byte[] ownerBytes,
        int permissions,
        byte[] idBytes,
        int revision,
        int keyLength,
        bool encryptMetadata)
    {
        var padded = PadPassword(password);

        var input = new MemoryStream();
        input.Write(padded, 0, padded.Length);
        input.Write(ownerBytes, 0, Math.Min(32, ownerBytes.Length));
        input.Write(
            [(byte)permissions, (byte)(permissions >> 8), (byte)(permissions >> 16), (byte)(permissions >> 24)],
            0,
            4);
        input.Write(idBytes, 0, idBytes.Length);

        if (revision >= 4 && !encryptMetadata)
        {
            input.Write([0xFF, 0xFF, 0xFF, 0xFF], 0, 4);
        }

        var hash = MD5.HashData(input.ToArray());

        if (revision >= 3)
        {
            for (var i = 0; i < 50; i++)
            {
                hash = MD5.HashData(hash.AsSpan(0, keyLength).ToArray());
            }
        }

        return hash.AsSpan(0, revision == 2 ? 5 : keyLength).ToArray();
    }

    private static byte[] PadPassword(byte[] password)
    {
        var padded = new byte[32];
        var take = Math.Min(32, password.Length);
        Array.Copy(password, padded, take);
        Array.Copy(Padding, 0, padded, take, 32 - take);
        return padded;
    }

    private static bool VerifyUserPassword(byte[] key, byte[] userBytes, byte[] idBytes, int revision)
    {
        if (revision == 2)
        {
            var expected = Rc4.Transform(key, Padding);
            return userBytes.Length >= 32 && expected.AsSpan(0, 32).SequenceEqual(userBytes.AsSpan(0, 32));
        }

        var input = new MemoryStream();
        input.Write(Padding, 0, Padding.Length);
        input.Write(idBytes, 0, idBytes.Length);
        var hash = MD5.HashData(input.ToArray());

        var value = Rc4.Transform(key, hash);
        for (var i = 1; i <= 19; i++)
        {
            var derived = new byte[key.Length];
            for (var j = 0; j < key.Length; j++)
            {
                derived[j] = (byte)(key[j] ^ i);
            }

            value = Rc4.Transform(derived, value);
        }

        // Revision 3+ only guarantees the first 16 bytes match.
        return userBytes.Length >= 16 && value.AsSpan(0, 16).SequenceEqual(userBytes.AsSpan(0, 16));
    }

    /// <summary>Treats the supplied password as the owner password and recovers the user password from <c>/O</c>.</summary>
    private static byte[]? RecoverUserPassword(byte[] password, byte[] ownerBytes, int revision, int keyLength)
    {
        if (ownerBytes.Length < 32)
        {
            return null;
        }

        var padded = PadPassword(password);
        var hash = MD5.HashData(padded);

        if (revision >= 3)
        {
            for (var i = 0; i < 50; i++)
            {
                hash = MD5.HashData(hash);
            }
        }

        var key = hash.AsSpan(0, revision == 2 ? 5 : keyLength).ToArray();
        var value = ownerBytes.AsSpan(0, 32).ToArray();

        if (revision == 2)
        {
            return Rc4.Transform(key, value);
        }

        for (var i = 19; i >= 0; i--)
        {
            var derived = new byte[key.Length];
            for (var j = 0; j < key.Length; j++)
            {
                derived[j] = (byte)(key[j] ^ i);
            }

            value = Rc4.Transform(derived, value);
        }

        return value;
    }

    /// <summary>Revision 5/6 key derivation (AES-256), per ISO 32000-2.</summary>
    private static byte[]? ComputeKeyR5Plus(
        byte[] password,
        byte[] ownerBytes,
        byte[] userBytes,
        PdfDictionary encrypt,
        Func<PdfObject?, PdfObject?> resolve,
        int revision)
    {
        if (userBytes.Length < 48)
        {
            return null;
        }

        // Try the user password first.
        var userValidation = userBytes.AsSpan(32, 8).ToArray();
        var userSalt = userBytes.AsSpan(40, 8).ToArray();

        var hash = Hash2B(password, userValidation, [], revision);
        if (hash.AsSpan(0, 32).SequenceEqual(userBytes.AsSpan(0, 32)))
        {
            var intermediate = Hash2B(password, userSalt, [], revision);
            var ue = resolve(encrypt.Get("UE"))?.AsStringBytes();
            return ue is { Length: >= 32 } ? AesNoPadding(intermediate, ue.AsSpan(0, 32).ToArray()) : null;
        }

        // Then the owner password, which is salted with /U.
        if (ownerBytes.Length < 48)
        {
            return null;
        }

        var ownerValidation = ownerBytes.AsSpan(32, 8).ToArray();
        var ownerSalt = ownerBytes.AsSpan(40, 8).ToArray();
        var u48 = userBytes.AsSpan(0, 48).ToArray();

        hash = Hash2B(password, ownerValidation, u48, revision);
        if (!hash.AsSpan(0, 32).SequenceEqual(ownerBytes.AsSpan(0, 32)))
        {
            return null;
        }

        var ownerIntermediate = Hash2B(password, ownerSalt, u48, revision);
        var oe = resolve(encrypt.Get("OE"))?.AsStringBytes();
        return oe is { Length: >= 32 } ? AesNoPadding(ownerIntermediate, oe.AsSpan(0, 32).ToArray()) : null;
    }

    /// <summary>
    /// The revision-6 hardened hash (algorithm 2.B). Revision 5 (the deprecated
    /// Adobe extension) stops after the initial SHA-256.
    /// </summary>
    private static byte[] Hash2B(byte[] password, byte[] salt, byte[] userData, int revision)
    {
        var input = new MemoryStream();
        input.Write(password, 0, password.Length);
        input.Write(salt, 0, salt.Length);
        input.Write(userData, 0, userData.Length);

        var k = SHA256.HashData(input.ToArray());

        if (revision < 6)
        {
            return k;
        }

        for (var round = 0; ; round++)
        {
            var k1 = new MemoryStream();
            for (var i = 0; i < 64; i++)
            {
                k1.Write(password, 0, password.Length);
                k1.Write(k, 0, k.Length);
                k1.Write(userData, 0, userData.Length);
            }

            var k1Bytes = k1.ToArray();
            var aesKey = k.AsSpan(0, 16).ToArray();
            var iv = k.AsSpan(16, 16).ToArray();
            var e = AesCbcEncryptNoPadding(aesKey, iv, k1Bytes);

            var modulo = 0;
            for (var i = 0; i < 16; i++)
            {
                modulo += e[i];
            }

            k = (modulo % 3) switch
            {
                0 => SHA256.HashData(e),
                1 => SHA384.HashData(e),
                _ => SHA512.HashData(e),
            };

            // At least 64 rounds run; after that the loop ends once the last
            // byte of E drops to or below (completed rounds - 32).
            var completed = round + 1;
            if (completed >= 64 && e[^1] <= completed - 32)
            {
                break;
            }

            // Backstop against a malformed file driving the loop forever.
            if (round > Hash2BRoundLimit)
            {
                break;
            }
        }

        return k.AsSpan(0, 32).ToArray();
    }

    private const int Hash2BRoundLimit = 4096;

    // ── Per-object decryption ────────────────────────────────────────────

    /// <summary>Derives the per-object key and decrypts a stream's bytes.</summary>
    public byte[] DecryptStream(byte[] data, PdfObjectId id) => Decrypt(data, id, _streamMethod);

    /// <summary>Derives the per-object key and decrypts a string's bytes.</summary>
    public byte[] DecryptString(byte[] data, PdfObjectId id) => Decrypt(data, id, _stringMethod);

    private byte[] Decrypt(byte[] data, PdfObjectId id, CryptMethod method)
    {
        if (method == CryptMethod.None || data.Length == 0)
        {
            return data;
        }

        try
        {
            return method switch
            {
                CryptMethod.Rc4 => Rc4.Transform(ObjectKey(id, aes: false), data),
                CryptMethod.AesV2 => AesCbcDecrypt(ObjectKey(id, aes: true), data),
                CryptMethod.AesV3 => AesCbcDecrypt(_fileKey, data),
                _ => data,
            };
        }
        catch (CryptographicException)
        {
            // A single corrupt object should not fail the whole document.
            return data;
        }
    }

    private byte[] ObjectKey(PdfObjectId id, bool aes)
    {
        if (_revision >= 5)
        {
            return _fileKey;
        }

        var input = new MemoryStream();
        input.Write(_fileKey, 0, _fileKey.Length);
        input.Write([(byte)id.Number, (byte)(id.Number >> 8), (byte)(id.Number >> 16)], 0, 3);
        input.Write([(byte)id.Generation, (byte)(id.Generation >> 8)], 0, 2);

        if (aes)
        {
            input.Write([0x73, 0x41, 0x6C, 0x54], 0, 4); // "sAlT"
        }

        var hash = MD5.HashData(input.ToArray());
        var length = Math.Min(_fileKey.Length + 5, 16);
        return hash.AsSpan(0, length).ToArray();
    }

    private static byte[] AesCbcDecrypt(byte[] key, byte[] data)
    {
        if (data.Length <= 16)
        {
            return [];
        }

        using var aes = Aes.Create();
        aes.Key = key;
        aes.IV = data.AsSpan(0, 16).ToArray();
        aes.Mode = CipherMode.CBC;
        aes.Padding = PaddingMode.None;

        var body = data.AsSpan(16).ToArray();
        // CBC requires whole blocks; truncate any trailing partial block.
        var usable = body.Length - (body.Length % 16);
        if (usable <= 0)
        {
            return [];
        }

        using var transform = aes.CreateDecryptor();
        var plain = transform.TransformFinalBlock(body, 0, usable);

        // Strip PKCS#7 padding when it is well-formed.
        if (plain.Length > 0)
        {
            var pad = plain[^1];
            if (pad is >= 1 and <= 16 && pad <= plain.Length)
            {
                return plain.AsSpan(0, plain.Length - pad).ToArray();
            }
        }

        return plain;
    }

    private static byte[] AesNoPadding(byte[] key, byte[] data)
    {
        using var aes = Aes.Create();
        aes.Key = key;
        aes.IV = new byte[16];
        aes.Mode = CipherMode.CBC;
        aes.Padding = PaddingMode.None;
        using var transform = aes.CreateDecryptor();
        return transform.TransformFinalBlock(data, 0, data.Length);
    }

    private static byte[] AesCbcEncryptNoPadding(byte[] key, byte[] iv, byte[] data)
    {
        using var aes = Aes.Create();
        aes.Key = key;
        aes.IV = iv;
        aes.Mode = CipherMode.CBC;
        aes.Padding = PaddingMode.None;
        using var transform = aes.CreateEncryptor();
        return transform.TransformFinalBlock(data, 0, data.Length);
    }
}

/// <summary>RC4, required by the legacy standard security handler.</summary>
internal static class Rc4
{
    public static byte[] Transform(byte[] key, byte[] data)
    {
        if (key.Length == 0)
        {
            return data;
        }

        var s = new byte[256];
        for (var i = 0; i < 256; i++)
        {
            s[i] = (byte)i;
        }

        var j = 0;
        for (var i = 0; i < 256; i++)
        {
            j = (j + s[i] + key[i % key.Length]) & 0xFF;
            (s[i], s[j]) = (s[j], s[i]);
        }

        var output = new byte[data.Length];
        int x = 0, y = 0;
        for (var i = 0; i < data.Length; i++)
        {
            x = (x + 1) & 0xFF;
            y = (y + s[x]) & 0xFF;
            (s[x], s[y]) = (s[y], s[x]);
            output[i] = (byte)(data[i] ^ s[(s[x] + s[y]) & 0xFF]);
        }

        return output;
    }
}
