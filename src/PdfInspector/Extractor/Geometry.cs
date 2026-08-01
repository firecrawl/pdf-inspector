// Ported from reference/src/extractor/mod.rs
using PdfInspector.Pdf;

namespace PdfInspector.Extractor;

/// <summary>
/// A 2D affine transform written the way PDF does: <c>[a b c d e f]</c>,
/// standing for the matrix rows (a b 0), (c d 0), (e f 1).
/// </summary>
internal readonly struct Matrix(float a, float b, float c, float d, float e, float f)
{
    public readonly float A = a;
    public readonly float B = b;
    public readonly float C = c;
    public readonly float D = d;
    public readonly float E = e;
    public readonly float F = f;

    public static readonly Matrix Identity = new(1f, 0f, 0f, 1f, 0f, 0f);

    /// <summary>Returns <c>this × other</c>, applying this transform first.</summary>
    public Matrix Multiply(in Matrix other) => new(
        (A * other.A) + (B * other.C),
        (A * other.B) + (B * other.D),
        (C * other.A) + (D * other.C),
        (C * other.B) + (D * other.D),
        (E * other.A) + (F * other.C) + other.E,
        (E * other.B) + (F * other.D) + other.F);

    /// <summary>Maps a point through the transform.</summary>
    public (float X, float Y) Apply(float x, float y) =>
        ((x * A) + (y * C) + E, (x * B) + (y * D) + F);

    /// <summary>Returns a copy translated in the transform's own basis.</summary>
    public Matrix TranslatedBy(float tx, float ty) =>
        new(A, B, C, D, E + (tx * A) + (ty * C), F + (tx * B) + (ty * D));

    /// <summary>The six components, for the code that still reads them positionally.</summary>
    public float this[int index] => index switch
    {
        0 => A,
        1 => B,
        2 => C,
        3 => D,
        4 => E,
        5 => F,
        _ => throw new ArgumentOutOfRangeException(nameof(index)),
    };

    public float[] ToArray() => [A, B, C, D, E, F];
}

/// <summary>Small geometric helpers shared by the extractor.</summary>
internal static class Geometry
{
    /// <summary>Reads a numeric operand, or null when the operand is not a number.</summary>
    public static float? GetNumber(PdfObject obj) => (float?)obj.AsNumber();

    /// <summary>
    /// The device-space bounding box of an image XObject, whose unit square the
    /// current transform maps onto the page.
    /// </summary>
    public static (float X, float Y, float Width, float Height) ImageBoundsFromCtm(in Matrix ctm)
    {
        Span<(float X, float Y)> corners =
        [
            ctm.Apply(0f, 0f),
            ctm.Apply(1f, 0f),
            ctm.Apply(1f, 1f),
            ctm.Apply(0f, 1f),
        ];

        var xMin = corners[0].X;
        var xMax = corners[0].X;
        var yMin = corners[0].Y;
        var yMax = corners[0].Y;

        for (var i = 1; i < corners.Length; i++)
        {
            xMin = MathF.Min(xMin, corners[i].X);
            xMax = MathF.Max(xMax, corners[i].X);
            yMin = MathF.Min(yMin, corners[i].Y);
            yMax = MathF.Max(yMax, corners[i].Y);
        }

        return (xMin, yMin, xMax - xMin, yMax - yMin);
    }

    /// <summary>
    /// Applies text rise, which displaces the glyph origin by (0, rise) in
    /// unscaled text space. In the rendering-matrix definition rise sits left of
    /// the text matrix, so the offset maps through that matrix's y column. Rise
    /// never contributes to the advance, so callers apply it only to the
    /// rendering position and keep advancing the unshifted matrix.
    /// </summary>
    public static Matrix RiseAdjusted(in Matrix tm, float rise) =>
        rise == 0f ? tm : new Matrix(tm.A, tm.B, tm.C, tm.D, tm.E + (rise * tm.C), tm.F + (rise * tm.D));

    /// <summary>
    /// The device-space stroke width of a segment. PDF scales stroke width
    /// perpendicular to the path direction.
    /// </summary>
    public static float TransformedStrokeWidth(
        float lineWidth,
        in Matrix ctm,
        float x1,
        float y1,
        float x2,
        float y2)
    {
        var userWidth = MathF.Abs(lineWidth);
        var dx = x2 - x1;
        var dy = y2 - y1;
        var length = MathF.Sqrt((dx * dx) + (dy * dy));

        if (length <= float.Epsilon)
        {
            return userWidth;
        }

        var nx = -dy / length;
        var ny = dx / length;
        var ndx = (nx * ctm.A) + (ny * ctm.C);
        var ndy = (nx * ctm.B) + (ny * ctm.D);

        return userWidth * MathF.Sqrt((ndx * ndx) + (ndy * ndy));
    }
}
