"""Offline unit tests for the memoricai Python SDK (no server required).

Run with: python -m pytest sdks/python/tests  (or python -m unittest discover)
"""

import pathlib
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from memoricai import Client, __version__  # noqa: E402


class ClientTests(unittest.TestCase):
    def setUp(self):
        self.client = Client("http://localhost:7373", "mc_test")

    def test_base_url_trailing_slashes_trimmed(self):
        c = Client("http://localhost:7373///", "mc_test")
        self.assertEqual(c.base_url, "http://localhost:7373")

    def test_get_document_url_encodes_id(self):
        with patch.object(Client, "_request", return_value={}) as m:
            self.client.get_document("a/b?x#y")
        path = m.call_args[0][1]
        self.assertIn("a%2Fb%3Fx%23y", path)
        self.assertNotIn("a/b", path)

    def test_delete_document_url_encodes_id(self):
        with patch.object(Client, "_request", return_value={}) as m:
            self.client.delete_document("proj/doc-1")
        self.assertIn("%2F", m.call_args[0][1])

    def test_version_matches_package(self):
        # Guards against __version__ drifting from the packaged version again.
        self.assertEqual(__version__, "0.3.1")

    def test_build_context_uses_context_endpoint_and_camel_case_body(self):
        with patch.object(Client, "_request", return_value={}) as request:
            self.client.build_context(
                "count visits", budget_tokens=1_000, max_sources=5
            )
        method, path, body = request.call_args[0]
        self.assertEqual((method, path), ("POST", "/v1/context"))
        self.assertEqual(body["q"], "count visits")
        self.assertEqual(body["budgetTokens"], 1_000)
        self.assertEqual(body["maxSources"], 5)


if __name__ == "__main__":
    unittest.main()
