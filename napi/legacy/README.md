# firecrawl-pdf-inspector

> Deprecated: this package has moved to [`@firecrawl/pdf-inspector`](https://www.npmjs.com/package/@firecrawl/pdf-inspector).
> Please install and import the scoped Firecrawl package instead.

```bash
npm install @firecrawl/pdf-inspector
# or
bun add @firecrawl/pdf-inspector
```

```typescript
import { processPdf, classifyPdf } from '@firecrawl/pdf-inspector'
import { readFileSync } from 'fs'

const pdf = readFileSync('document.pdf')
const result = processPdf(pdf)

console.log(result.pdfType)
console.log(result.markdown)
```

This package remains only as a compatibility wrapper for older installs:

```typescript
import { processPdf } from 'firecrawl-pdf-inspector'
```

New projects should use `@firecrawl/pdf-inspector` directly.

