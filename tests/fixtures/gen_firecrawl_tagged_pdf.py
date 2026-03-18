#!/usr/bin/env python3
"""Generate a tagged PDF with rich structure tree using Firecrawl docs content.

Uses fpdf2 which supports PDF tagged structure trees natively.
The output exercises: H1, H2, H3, P, Code, LI, Caption, TH, TD structure roles.
"""

from contextlib import contextmanager
from fpdf import FPDF
import sys

OUTPUT = sys.argv[1] if len(sys.argv) > 1 else "tests/fixtures/firecrawl_docs_tagged.pdf"


class TaggedPDF(FPDF):
    """FPDF subclass with tagged structure tree support."""

    def marked_content(self, struct_type, **kwargs):
        """Context manager that wraps drawn content in a marked content sequence."""
        @contextmanager
        def _ctx():
            mcid = self.struct_builder.next_mcid_for_page(self.page)
            struct_elem, spid = self.struct_builder.add_marked_content(
                page_number=self.page, struct_type="/" + struct_type, mcid=mcid, **kwargs
            )
            self.pages[self.page].struct_parents = spid
            self._set_min_pdf_version("1.4")
            self._out(f"/{struct_type} <</MCID {mcid}>> BDC")
            yield struct_elem
            self._out("EMC")
        return _ctx()


class DocBuilder:
    def __init__(self, filename):
        self.pdf = TaggedPDF()
        self.pdf.set_auto_page_break(auto=True, margin=20)
        self.pdf.set_title("Firecrawl Documentation - API Reference")
        self.pdf.set_author("Firecrawl")
        self.pdf.add_page()
        self.filename = filename

    def h1(self, text):
        self.pdf.set_font("Helvetica", "B", 26)
        self.pdf.set_text_color(26, 26, 46)
        with self.pdf.marked_content("H1"):
            self.pdf.cell(text=text, new_x="LEFT", new_y="NEXT")
        self.pdf.ln(8)

    def h2(self, text):
        self.pdf.ln(4)
        self.pdf.set_font("Helvetica", "B", 20)
        self.pdf.set_text_color(255, 106, 0)
        with self.pdf.marked_content("H2"):
            self.pdf.cell(text=text, new_x="LEFT", new_y="NEXT")
        self.pdf.ln(6)

    def h3(self, text):
        self.pdf.ln(3)
        self.pdf.set_font("Helvetica", "B", 14)
        self.pdf.set_text_color(26, 26, 46)
        with self.pdf.marked_content("H3"):
            self.pdf.cell(text=text, new_x="LEFT", new_y="NEXT")
        self.pdf.ln(4)

    def p(self, text):
        self.pdf.set_font("Helvetica", "", 10)
        self.pdf.set_text_color(85, 85, 85)
        with self.pdf.marked_content("P"):
            self.pdf.multi_cell(w=0, text=text, new_x="LEFT", new_y="NEXT")
        self.pdf.ln(3)

    def code_block(self, lines):
        """Each line gets its own Code marked-content sequence."""
        self.pdf.set_font("Courier", "", 9)
        self.pdf.set_text_color(26, 26, 46)
        # Light gray background
        x = self.pdf.get_x()
        y = self.pdf.get_y()
        block_h = 5 * len(lines) + 4
        self.pdf.set_fill_color(240, 240, 240)
        self.pdf.rect(x, y, self.pdf.epw, block_h, style="F")
        self.pdf.ln(2)
        for line in lines:
            with self.pdf.marked_content("Code"):
                self.pdf.cell(text="  " + line, new_x="LEFT", new_y="NEXT")
            self.pdf.ln(1)
        self.pdf.ln(3)

    def list_item(self, text, ordered=False, number=None):
        self.pdf.set_font("Helvetica", "", 10)
        self.pdf.set_text_color(85, 85, 85)
        bullet = f"{number}." if ordered else "-"
        with self.pdf.marked_content("LI"):
            self.pdf.cell(w=8, text=bullet)
            self.pdf.multi_cell(w=0, text=text, new_x="LEFT", new_y="NEXT")
        self.pdf.ln(1)

    def caption(self, text):
        self.pdf.set_font("Helvetica", "I", 9)
        self.pdf.set_text_color(120, 120, 120)
        with self.pdf.marked_content("Caption"):
            self.pdf.cell(text=text, new_x="LEFT", new_y="NEXT")
        self.pdf.ln(4)

    def table(self, headers, rows):
        ncols = len(headers)
        col_w = self.pdf.epw / ncols
        # Header
        self.pdf.set_font("Helvetica", "B", 9)
        self.pdf.set_text_color(26, 26, 46)
        self.pdf.set_fill_color(232, 232, 232)
        for h in headers:
            with self.pdf.marked_content("TH"):
                self.pdf.cell(w=col_w, text=h, border=1, fill=True)
        self.pdf.ln()
        # Rows
        self.pdf.set_font("Helvetica", "", 9)
        self.pdf.set_text_color(85, 85, 85)
        for row in rows:
            for cell in row:
                with self.pdf.marked_content("TD"):
                    self.pdf.cell(w=col_w, text=str(cell), border=1)
            self.pdf.ln()
        self.pdf.ln(2)

    def save(self):
        self.pdf.output(self.filename)


def build_doc():
    b = DocBuilder(OUTPUT)

    # === Title & Intro ===
    b.h1("Firecrawl API Documentation")
    b.p("Firecrawl transforms websites into clean, LLM-ready data through a single API call. "
        "It handles JavaScript rendering, proxy rotation, anti-bot mechanisms, and rate limiting "
        "so you can focus on building your application. Output formats include Markdown, structured "
        "JSON, screenshots, HTML, and more.")
    b.p("This document covers the complete Firecrawl API surface including the Scrape, Crawl, "
        "and Extract endpoints, along with SDK usage for Python, Node.js, and cURL.")

    # === Quick Start ===
    b.h2("Quick Start")
    b.p("Install the SDK for your preferred language and initialize with your API key:")

    b.h3("Python")
    b.code_block([
        "pip install firecrawl-py",
        "",
        "from firecrawl import Firecrawl",
        "",
        "app = Firecrawl(api_key=\"fc-YOUR-API-KEY\")",
        "result = app.scrape(\"https://example.com\")",
        "print(result.markdown)",
    ])

    b.h3("Node.js")
    b.code_block([
        "npm install @mendable/firecrawl-js",
        "",
        "import Firecrawl from '@mendable/firecrawl-js';",
        "",
        "const app = new Firecrawl({ apiKey: 'fc-YOUR-API-KEY' });",
        "const result = await app.scrape('https://example.com');",
        "console.log(result.markdown);",
    ])

    b.h3("cURL")
    b.code_block([
        "curl -X POST 'https://api.firecrawl.dev/v2/scrape' \\",
        "  -H 'Authorization: Bearer fc-YOUR-API-KEY' \\",
        "  -H 'Content-Type: application/json' \\",
        "  -d '{\"url\": \"https://example.com\"}'",
    ])

    # === Scrape Endpoint ===
    b.h2("Scrape Endpoint")
    b.p("The /v2/scrape endpoint converts a single URL into clean data. It handles JavaScript "
        "rendering, manages proxies and caching, and returns content in your preferred format.")

    b.h3("Request Parameters")
    b.table(
        ["Parameter", "Type", "Default", "Description"],
        [
            ["url", "string", "required", "The URL to scrape"],
            ["formats", "string[]", "[\"markdown\"]", "Output formats"],
            ["onlyMainContent", "boolean", "true", "Primary content only"],
            ["includeTags", "string[]", "[]", "CSS selectors to include"],
            ["excludeTags", "string[]", "[]", "CSS selectors to exclude"],
            ["waitFor", "integer", "0", "Wait ms before capture"],
            ["timeout", "integer", "30000", "Max page load time ms"],
            ["mobile", "boolean", "false", "Use mobile viewport"],
        ]
    )
    b.caption("Table 1: Scrape endpoint request parameters")

    b.h3("Response Format")
    b.code_block([
        "{",
        "  \"success\": true,",
        "  \"data\": {",
        "    \"markdown\": \"# Page Title\\n\\nContent...\",",
        "    \"html\": \"<html>...</html>\",",
        "    \"metadata\": {",
        "      \"title\": \"Example Page\",",
        "      \"description\": \"A sample page\",",
        "      \"language\": \"en\",",
        "      \"sourceURL\": \"https://example.com\",",
        "      \"statusCode\": 200",
        "    }",
        "  }",
        "}",
    ])
    b.caption("Figure 1: Typical scrape response structure")

    b.h3("Scrape with Structured Extraction")
    b.p("Use a JSON schema or Pydantic model to extract structured data from any page. "
        "This is useful for pulling product information, article metadata, or any structured "
        "content from web pages.")

    b.code_block([
        "from pydantic import BaseModel",
        "from firecrawl import Firecrawl",
        "",
        "class Product(BaseModel):",
        "    name: str",
        "    price: float",
        "    currency: str",
        "    in_stock: bool",
        "    description: str",
        "",
        "app = Firecrawl(api_key=\"fc-YOUR-API-KEY\")",
        "result = app.scrape(",
        "    \"https://example.com/product/123\",",
        "    params={",
        "        \"formats\": [\"extract\"],",
        "        \"extract\": {",
        "            \"schema\": Product.model_json_schema()",
        "        }",
        "    }",
        ")",
        "product = Product(**result.extract)",
        "print(f\"{product.name}: ${product.price}\")",
    ])

    # === Crawl Endpoint ===
    b.h2("Crawl Endpoint")
    b.p("The /v2/crawl endpoint recursively discovers and scrapes every reachable subpage from "
        "a starting URL. It automatically follows links, discovers sitemaps, and handles "
        "JavaScript-rendered content across the entire site.")

    b.h3("How Crawling Works")
    b.list_item("Submit a crawl job with a starting URL and configuration options")
    b.list_item("Firecrawl discovers linked pages using sitemaps and page links")
    b.list_item("Each discovered page is scraped with your specified format options")
    b.list_item("Results are returned via polling, WebSocket, or webhook notifications")
    b.list_item("Large result sets are paginated with cursor-based pagination")

    b.h3("Crawl Parameters")
    b.table(
        ["Parameter", "Type", "Default", "Description"],
        [
            ["url", "string", "required", "Starting URL for the crawl"],
            ["maxDepth", "integer", "10", "Max link depth to follow"],
            ["limit", "integer", "10000", "Max number of pages"],
            ["includePaths", "string[]", "[]", "Regex patterns to include"],
            ["excludePaths", "string[]", "[]", "Regex patterns to exclude"],
            ["allowSubdomains", "boolean", "false", "Follow subdomain links"],
            ["ignoreSitemap", "boolean", "false", "Skip sitemap.xml"],
            ["deduplicateSimilar", "boolean", "true", "Skip similar pages"],
        ]
    )
    b.caption("Table 2: Crawl endpoint configuration parameters")

    b.h3("Async Crawl with Polling")
    b.code_block([
        "import time",
        "from firecrawl import Firecrawl",
        "",
        "app = Firecrawl(api_key=\"fc-YOUR-API-KEY\")",
        "",
        "# Start crawl job",
        "job = app.async_crawl(\"https://docs.example.com\")",
        "job_id = job.id",
        "",
        "# Poll for results",
        "while True:",
        "    status = app.check_crawl_status(job_id)",
        "    print(f\"Status: {status.status}, Pages: {len(status.data)}\")",
        "    if status.status == \"completed\":",
        "        break",
        "    time.sleep(5)",
        "",
        "# Process results",
        "for page in status.data:",
        "    print(f\"  {page.metadata.sourceURL}\")",
        "    print(f\"  {len(page.markdown)} chars\")",
    ])

    b.h3("WebSocket Real-time Updates")
    b.code_block([
        "const app = new Firecrawl({ apiKey: 'fc-YOUR-API-KEY' });",
        "",
        "const watcher = await app.crawlUrlAndWatch(",
        "  'https://docs.example.com',",
        "  { limit: 100, maxDepth: 3 }",
        ");",
        "",
        "watcher.on('page', (page) => {",
        "  console.log(`Scraped: ${page.metadata.sourceURL}`);",
        "  console.log(`Content: ${page.markdown.slice(0, 200)}...`);",
        "});",
        "",
        "watcher.on('done', (result) => {",
        "  console.log(`Crawl complete: ${result.data.length} pages`);",
        "});",
        "",
        "watcher.on('error', (err) => {",
        "  console.error('Crawl failed:', err);",
        "});",
    ])

    # === Extract Endpoint ===
    b.h2("Extract Endpoint")
    b.p("The /v2/extract endpoint simplifies collecting structured data from any number of URLs "
        "or entire domains. It handles crawling, parsing, and data collation automatically.")

    b.h3("Extraction Modes")
    b.list_item("Schema-based: Define a JSON schema or Pydantic model for precise output", ordered=True, number=1)
    b.list_item("Prompt-based: Describe what you want in natural language", ordered=True, number=2)
    b.list_item("Hybrid: Combine a schema with a prompt for guided extraction", ordered=True, number=3)

    b.h3("Schema-based Extraction")
    b.code_block([
        "from pydantic import BaseModel",
        "from firecrawl import Firecrawl",
        "",
        "class CompanyInfo(BaseModel):",
        "    name: str",
        "    mission: str",
        "    founded_year: int",
        "    headquarters: str",
        "    employee_count: str",
        "    products: list[str]",
        "",
        "app = Firecrawl(api_key=\"fc-YOUR-API-KEY\")",
        "result = app.extract(",
        "    [\"https://example.com/*\"],",
        "    params={",
        "        \"schema\": CompanyInfo.model_json_schema(),",
        "        \"prompt\": \"Extract company information\"",
        "    }",
        ")",
        "info = CompanyInfo(**result.data)",
        "print(f\"{info.name}, est. {info.founded_year}\")",
        "print(f\"Products: {', '.join(info.products)}\")",
    ])

    # === Pricing ===
    b.h2("Pricing and Credits")
    b.p("Firecrawl uses a credit-based billing model. Each operation consumes credits based on "
        "the complexity of the task.")

    b.table(
        ["Operation", "Credits", "Notes"],
        [
            ["Basic scrape", "1", "Standard markdown scrape"],
            ["JSON extraction", "5", "Structured extraction"],
            ["Enhanced proxy", "4", "Premium proxy per page"],
            ["PDF parsing", "1/page", "Per PDF page"],
            ["Crawl", "1/page", "Per page discovered"],
            ["Extract", "15 tok/credit", "LLM token billing"],
            ["Zero retention", "1/page", "Enterprise compliance"],
        ]
    )
    b.caption("Table 3: Credit usage by operation type")

    # === Advanced Features ===
    b.h2("Advanced Features")

    b.h3("Page Actions")
    b.p("Automate browser interactions before scraping using the actions parameter. "
        "Useful for login flows, cookie consent, or dynamic content loading.")

    b.code_block([
        "result = app.scrape(",
        "    \"https://example.com/dashboard\",",
        "    params={",
        "        \"actions\": [",
        "            {\"type\": \"click\", \"selector\": \"#cookie-accept\"},",
        "            {\"type\": \"wait\", \"milliseconds\": 1000},",
        "            {\"type\": \"write\", \"selector\": \"#search\", \"text\": \"firecrawl\"},",
        "            {\"type\": \"press\", \"key\": \"Enter\"},",
        "            {\"type\": \"wait\", \"milliseconds\": 2000},",
        "            {\"type\": \"screenshot\"}",
        "        ]",
        "    }",
        ")",
    ])

    b.h3("Webhook Notifications")
    b.p("Receive real-time notifications as pages are crawled. Webhooks include HMAC-SHA256 "
        "signatures for verification.")

    b.code_block([
        "import hmac",
        "import hashlib",
        "from flask import Flask, request",
        "",
        "app = Flask(__name__)",
        "WEBHOOK_SECRET = \"your-webhook-secret\"",
        "",
        "@app.route(\"/webhook\", methods=[\"POST\"])",
        "def handle_webhook():",
        "    signature = request.headers.get(\"X-Firecrawl-Signature\")",
        "    payload = request.get_data()",
        "    expected = hmac.new(",
        "        WEBHOOK_SECRET.encode(),",
        "        payload,",
        "        hashlib.sha256",
        "    ).hexdigest()",
        "",
        "    if not hmac.compare_digest(signature, expected):",
        "        return \"Invalid signature\", 401",
        "",
        "    data = request.json",
        "    event = data[\"type\"]",
        "    if event == \"crawl.page\":",
        "        print(f\"Scraped: {data['data']['metadata']['sourceURL']}\")",
        "    elif event == \"crawl.completed\":",
        "        print(f\"Done: {data['data']['total']} pages\")",
        "",
        "    return \"OK\", 200",
    ])

    b.h3("Batch Scraping")
    b.p("Scrape multiple URLs in a single request for improved throughput:")

    b.code_block([
        "urls = [",
        "    \"https://example.com/page-1\",",
        "    \"https://example.com/page-2\",",
        "    \"https://example.com/page-3\",",
        "    \"https://example.com/page-4\",",
        "    \"https://example.com/page-5\",",
        "]",
        "",
        "results = app.batch_scrape(urls, params={",
        "    \"formats\": [\"markdown\", \"links\"],",
        "    \"onlyMainContent\": True,",
        "})",
        "",
        "for result in results.data:",
        "    url = result.metadata.sourceURL",
        "    links = len(result.links)",
        "    chars = len(result.markdown)",
        "    print(f\"{url}: {chars} chars, {links} links\")",
    ])

    # === Error Handling ===
    b.h2("Error Handling")
    b.p("The API returns standard HTTP status codes. All error responses follow a consistent format:")

    b.code_block([
        "{",
        "  \"success\": false,",
        "  \"error\": \"Rate limit exceeded\",",
        "  \"details\": \"You have exceeded the rate limit.\",",
        "  \"code\": \"RATE_LIMIT_EXCEEDED\"",
        "}",
    ])

    b.h3("Common Error Codes")
    b.table(
        ["Code", "HTTP", "Description", "Resolution"],
        [
            ["RATE_LIMIT_EXCEEDED", "429", "Too many requests", "Retry with backoff"],
            ["INVALID_API_KEY", "401", "Invalid key", "Check dashboard"],
            ["INSUFFICIENT_CREDITS", "402", "No credits left", "Purchase credits"],
            ["URL_NOT_ACCESSIBLE", "422", "Cannot reach URL", "Verify URL"],
            ["TIMEOUT", "408", "Page timed out", "Increase timeout"],
            ["INTERNAL_ERROR", "500", "Server error", "Retry after wait"],
        ]
    )
    b.caption("Table 4: API error codes and resolutions")

    b.h3("Retry Logic Example")
    b.code_block([
        "import time",
        "from firecrawl import Firecrawl",
        "from firecrawl.exceptions import RateLimitError, FirecrawlError",
        "",
        "app = Firecrawl(api_key=\"fc-YOUR-API-KEY\")",
        "",
        "def scrape_with_retry(url, max_retries=3):",
        "    for attempt in range(max_retries):",
        "        try:",
        "            return app.scrape(url)",
        "        except RateLimitError:",
        "            wait = 2 ** attempt",
        "            print(f\"Rate limited, waiting {wait}s...\")",
        "            time.sleep(wait)",
        "        except FirecrawlError as e:",
        "            print(f\"Error: {e.code} - {e.message}\")",
        "            if e.code == \"INTERNAL_ERROR\":",
        "                time.sleep(1)",
        "                continue",
        "            raise",
        "    raise Exception(\"Max retries exceeded\")",
    ])

    # === SDK Reference ===
    b.h2("SDK Reference")

    b.h3("Python SDK Methods")
    b.table(
        ["Method", "Description", "Returns"],
        [
            ["scrape(url, params)", "Scrape a single URL", "ScrapeResult"],
            ["crawl(url, params)", "Crawl and wait", "CrawlResult"],
            ["async_crawl(url, params)", "Start async crawl", "CrawlJob"],
            ["check_crawl_status(id)", "Check job status", "CrawlStatus"],
            ["cancel_crawl(id)", "Cancel crawl", "None"],
            ["batch_scrape(urls, params)", "Scrape multiple URLs", "BatchResult"],
            ["extract(urls, params)", "Extract structured data", "ExtractResult"],
            ["map(url, params)", "Discover site URLs", "MapResult"],
        ]
    )
    b.caption("Table 5: Python SDK method reference")

    b.h3("Rate Limits")
    b.list_item("Free tier: 10 requests/minute, 500 credits/month")
    b.list_item("Starter: 50 requests/minute, 3,000 credits/month")
    b.list_item("Standard: 250 requests/minute, 100,000 credits/month")
    b.list_item("Scale: 1,000 requests/minute, unlimited credits")
    b.list_item("Enterprise: Custom rate limits and dedicated infrastructure")

    b.h3("Environment Variables")
    b.code_block([
        "# Required",
        "export FIRECRAWL_API_KEY=\"fc-YOUR-API-KEY\"",
        "",
        "# Optional",
        "export FIRECRAWL_BASE_URL=\"https://api.firecrawl.dev\"",
        "export FIRECRAWL_TIMEOUT=30000",
        "export FIRECRAWL_RETRY_COUNT=3",
    ])

    b.p("For the latest documentation, visit docs.firecrawl.dev. For support, reach out "
        "via the Firecrawl Discord community or email support@firecrawl.dev.")

    b.save()

    # Print stats
    import re
    with open(OUTPUT, "rb") as f:
        data = f.read()
    roles = sorted(set(re.findall(rb"/S\s+/(\w+)", data)))
    print(f"Generated {OUTPUT}")
    print(f"  Size: {len(data)} bytes ({len(data)//1024} KB)")
    print(f"  Pages: {b.pdf.pages_count}")
    print(f"  Roles: {[r.decode() for r in roles]}")


if __name__ == "__main__":
    build_doc()
