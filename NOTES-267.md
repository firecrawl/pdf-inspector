\# Notes: issue #267 — OCR confidence threshold



Investigated `src/detector.rs`. Current routing is driven by:

\- `min\_text\_ops\_per\_page` (default 3)

\- `text\_page\_ratio\_threshold` (default 0.6)



`confidence` itself is not currently compared to any threshold anywhere.



Direction pending confirmation from maintainer:

1\. Expose existing thresholds as tunable config, or

2\. Add new post-classification confidence gate.



Implementation to follow once confirmed.

