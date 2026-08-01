using PdfInspector.Markdown;
using Xunit;

namespace PdfInspector.Tests;

public sealed class PostprocessTests
{
    [Fact]
    public void FidelityProfilePreservesDotLeaders()
    {
        const string Input = "Introduction............................1";
        Assert.Equal(Input + "\n", Postprocess.CleanMarkdown(Input, new MarkdownOptions()));
    }

    [Fact]
    public void CompactProfileCollapsesDotLeaders()
    {
        var options = new MarkdownOptions { Profile = MarkdownProfile.Compact };
        Assert.Equal(
            "Introduction ... 1\n",
            Postprocess.CleanMarkdown("Introduction............................1", options));
    }

    [Theory]
    [InlineData("Introduction............................1", "Introduction ... 1")]
    [InlineData("wait...what", "wait...what")]
    [InlineData("Hello World", "Hello World")]
    public void CollapsesRunsOfFourOrMoreDots(string input, string expected) =>
        Assert.Equal(expected, Postprocess.CollapseDotLeaders(input));

    [Fact]
    public void CollapseDotLeadersHandlesMixedLines()
    {
        var result = Postprocess.CollapseDotLeaders("Chapter 1.......10\nSome text... ok\nChapter 2........20");
        Assert.Contains("Chapter 1 ... 10", result, StringComparison.Ordinal);
        Assert.Contains("Some text... ok", result, StringComparison.Ordinal);
        Assert.Contains("Chapter 2 ... 20", result, StringComparison.Ordinal);
    }

    [Fact]
    public void RemovesSpacesBeforeClosingBrackets() =>
        Assert.Equal(
            "Density [kg/m3] and [linked text](https://example.com)",
            Postprocess.RemoveSpacesBeforeClosingBrackets("Density [kg/m3 ] and [linked text ](https://example.com)"));

    [Theory]
    [InlineData("Foreign insurance companies . The provisions", "Foreign insurance companies. The provisions")]
    [InlineData("|Applicability date .|This section|", "|Applicability date.|This section|")]
    [InlineData("Introduction ... 1", "Introduction ... 1")]
    [InlineData("version 3 .14 released", "version 3 .14 released")]
    public void RemovesSpacesBeforeSentencePunctuation(string input, string expected) =>
        Assert.Equal(expected, Postprocess.RemoveSpacesBeforeSentencePunctuation(input));

    [Theory]
    [InlineData("Limoeiro - Norte", "Limoeiro-Norte")]
    [InlineData("- item one\n- item two", "- item one\n- item two")]
    [InlineData("São - Paulo", "São-Paulo")]
    [InlineData("one - two and three - four", "one-two and three-four")]
    public void FixesSpacedHyphens(string input, string expected) =>
        Assert.Equal(expected, Postprocess.FixHyphenation(input));

    [Theory]
    [InlineData("1", true)]
    [InlineData("42", true)]
    [InlineData("123", true)]
    [InlineData("9999", true)]
    [InlineData("12345", false)]
    [InlineData("Page 5", true)]
    [InlineData("page 12", true)]
    [InlineData("Page 3 of 10", true)]
    [InlineData("page 1 of 5", true)]
    [InlineData("3 of 10", true)]
    [InlineData("- 5 -", true)]
    [InlineData("-12-", true)]
    [InlineData("Page of", true)]
    [InlineData("page of 10", true)]
    [InlineData("", false)]
    public void DetectsPageNumberLines(string input, bool expected) =>
        Assert.Equal(expected, Postprocess.IsPageNumberLine(input));

    [Fact]
    public void FormatsBareUrlsAsMarkdownLinks() =>
        Assert.Equal(
            "See [https://example.com/x](https://example.com/x) now\n",
            Postprocess.CleanMarkdown("See https://example.com/x now", new MarkdownOptions()));

    [Fact]
    public void LeavesAlreadyLinkedUrlsAlone() =>
        Assert.Equal(
            "[docs](https://example.com/x)\n",
            Postprocess.CleanMarkdown("[docs](https://example.com/x)", new MarkdownOptions()));
}

public sealed class ClassifyTests
{
    [Theory]
    [InlineData("● Item", "- Item")]
    [InlineData("• Item", "- Item")]
    [InlineData("<u>● Item text</u>", "- <u>Item text</u>")]
    [InlineData("**● Fraud: Willing cooperation;**", "- **Fraud: Willing cooperation;**")]
    [InlineData("**● Label:** rest of line", "- **Label:** rest of line")]
    [InlineData("*● Italic:* rest", "- *Italic:* rest")]
    [InlineData("- existing", "- existing")]
    public void FormatsListItems(string input, string expected) =>
        Assert.Equal(expected, Classify.FormatListItem(input));

    [Theory]
    [InlineData("● Item", true)]
    [InlineData("• Item", true)]
    [InlineData("1. First", true)]
    [InlineData("a) Second", true)]
    [InlineData("Ordinary prose line", false)]
    public void DetectsListItems(string input, bool expected) =>
        Assert.Equal(expected, Classify.IsListItem(input));

    [Theory]
    [InlineData("Figure 3.2 shows", true)]
    [InlineData("Table 1", true)]
    [InlineData("Source: annual report", true)]
    [InlineData("Table of Contents", false)]
    [InlineData("Figure drawing techniques", false)]
    public void DetectsCaptionLines(string input, bool expected) =>
        Assert.Equal(expected, Classify.IsCaptionLine(input));

    [Theory]
    [InlineData("Courier New", true)]
    [InlineData("JetBrainsMono-Regular", true)]
    [InlineData("Helvetica", false)]
    public void DetectsMonospaceFonts(string input, bool expected) =>
        Assert.Equal(expected, Classify.IsMonospaceFont(input));
}

public sealed class AnalysisTests
{
    [Theory]
    [InlineData("Section ..... 42", true)]
    [InlineData("Section ... 42 ... 43", true)]
    [InlineData("no leaders here", false)]
    public void DetectsDotLeaders(string input, bool expected) =>
        Assert.Equal(expected, Analysis.HasDotLeaders(input));

    [Theory]
    [InlineData("Measurement Lab worksheet ... 3", true)]
    [InlineData("Introduction .. 4", false)]
    [InlineData("A heading with no number", false)]
    public void DetectsTocEntryLines(string input, bool expected) =>
        Assert.Equal(expected, Analysis.IsTocEntryLine(input));

    [Theory]
    [InlineData("Contents", true)]
    [InlineData("Table of Contents:", true)]
    [InlineData("Introduction", false)]
    public void DetectsTocMarkerHeadings(string input, bool expected) =>
        Assert.Equal(expected, Analysis.IsTocMarkerHeading(input));

    [Theory]
    // A display equation ending in its number, with math evidence.
    [InlineData("S = kB ln W, (2)", true)]
    // A lead-in ending in a colon that references an equation.
    [InlineData("Rearranging Equation (8) gives:", true)]
    // A running header of the page-of-total form.
    [InlineData("LIVSMEDELSVERKET PM 2 (10)", true)]
    // A lowercase one- or two-word fragment beside display math.
    [InlineData("or inversely", true)]
    // A real heading ending in a parenthesised number carries no math evidence.
    [InlineData("Council of Nicaea (325)", false)]
    // A real heading ending in a colon, with no equation reference.
    [InlineData("Steps for Using the Microscope:", false)]
    public void DetectsHeadingFragments(string input, bool expected) =>
        Assert.Equal(expected, Analysis.IsHeadingFragment(input));
}
