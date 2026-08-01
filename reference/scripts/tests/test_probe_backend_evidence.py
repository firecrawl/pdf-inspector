import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from probe_backend_evidence import compare_documents


def local_payload(items):
    return {"items": items}


def item(page, text, x=10, item_type="text"):
    return {"page": page, "text": text, "x": x, "item_type": item_type}


def alternate_payload(pages):
    return {"pages": pages}


def page(lines, *, images=0):
    blocks = [
        {
            "type": "text",
            "lines": [
                {"text": text, "bbox": {"x": x, "y": index * 10, "w": 80, "h": 8}}
                for index, (text, x) in enumerate(lines)
            ],
        }
    ]
    blocks.extend({"type": "image"} for _ in range(images))
    return {"blocks": blocks}


class EvidenceComparisonTests(unittest.TestCase):
    def test_accepts_real_top_level_page_array(self):
        local = local_payload([item(1, "alpha beta")])
        alternate = [page([("alpha beta gamma", 10)])]

        result = compare_documents(local, alternate)["pages"][0]

        self.assertEqual(result["tokens"]["alternate"], 3)
        self.assertEqual(result["tokens"]["net_alternate_gain"], 1)

    def test_flags_material_alternate_text_gain(self):
        local = local_payload([item(1, "alpha beta")])
        alternate = alternate_payload(
            [page([("alpha beta gamma delta epsilon zeta", 10)])]
        )

        report = compare_documents(
            local,
            alternate,
            min_token_gain=3,
            min_alternate_only_ratio=0.2,
        )

        result = report["pages"][0]
        self.assertEqual(result["classification"], "investigate_alternate_evidence")
        self.assertIn("alternate_has_more_text", result["reasons"])
        self.assertEqual(result["tokens"]["net_alternate_gain"], 4)

    def test_repeated_alignment_and_image_evidence_are_reported(self):
        local = local_payload([item(1, "one two", 10)])
        alternate = alternate_payload(
            [
                page(
                    [
                        ("one two", 10),
                        ("row three", 100),
                        ("row four", 100),
                        ("row five", 100),
                    ],
                    images=1,
                )
            ]
        )

        result = compare_documents(
            local,
            alternate,
            min_token_gain=99,
            min_anchor_gain=1,
        )["pages"][0]

        self.assertIn("alternate_has_more_alignment_anchors", result["reasons"])
        self.assertIn("alternate_has_more_image_blocks", result["reasons"])
        self.assertEqual(result["layout"]["alternate_repeated_x_anchors"], 1)

    def test_token_segmentation_difference_does_not_imply_more_evidence(self):
        local = local_payload([item(1, "Revenue 2025")])
        alternate = alternate_payload([page([("Revenue 2024", 10)])])

        result = compare_documents(local, alternate, min_token_gain=2)["pages"][0]

        self.assertEqual(result["classification"], "different_segmentation_or_decoding")
        self.assertEqual(result["reasons"], [])
        self.assertEqual(result["tokens"]["alternate_only_sample"], ["2024"])


if __name__ == "__main__":
    unittest.main()
