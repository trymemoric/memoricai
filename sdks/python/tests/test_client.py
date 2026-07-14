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
        self.assertEqual(__version__, "0.3.2")

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

    def test_latest_search_and_profile_fields_are_forwarded(self):
        with patch.object(Client, "_request", return_value={}) as request:
            self.client.search_documents(
                "query",
                doc_id="doc/1",
                include_summary=True,
                filters={"key": "topic", "value": "rust"},
            )
            search_body = request.call_args[0][2]
            self.assertEqual(search_body["docId"], "doc/1")
            self.assertTrue(search_body["includeSummary"])
            self.assertIn("filters", search_body)

            self.client.profile("project", filters={"key": "kind", "value": "note"})
            self.assertIn("filters", request.call_args[0][2])

    def test_management_and_connector_routes_encode_path_segments(self):
        with patch.object(Client, "_request", return_value={}) as request:
            self.client.update_container_tag("team/a", name="Team A")
            self.assertEqual(
                request.call_args[0][1], "/v1/container-tags/team%2Fa"
            )

            self.client.connection_resources("conn/a", page=2, per_page=50)
            self.assertEqual(
                request.call_args[0][1], "/v1/connections/conn%2Fa/resources"
            )
            self.assertEqual(
                request.call_args.kwargs["query"], {"page": 2, "perPage": 50}
            )

    def test_router_uses_separate_memory_and_upstream_credentials(self):
        with patch.object(Client, "_request", return_value={}) as request:
            self.client.router_request(
                "https://api.example/v1/chat/completions?api-version=1",
                {"messages": []},
                "upstream-key",
                container_tag="project-a",
            )
        self.assertEqual(
            request.call_args[0][1],
            "/v1/router/https://api.example/v1/chat/completions%3Fapi-version=1",
        )
        headers = request.call_args.kwargs["headers"]
        self.assertEqual(headers["Authorization"], "Bearer upstream-key")
        self.assertEqual(headers["x-memoricai-api-key"], "mc_test")
        self.assertEqual(headers["x-mc-project"], "project-a")

    def test_oauth_token_exchange_uses_form_encoding(self):
        with patch.object(Client, "_request", return_value={}) as request:
            self.client.exchange_oauth_token(
                "authorization_code",
                "client_1",
                code="code with spaces",
                redirect_uri="http://localhost/callback",
            )
        self.assertEqual(request.call_args[0][:2], ("POST", "/api/auth/oauth2/token"))
        self.assertIn(b"grant_type=authorization_code", request.call_args.kwargs["raw_data"])
        self.assertIn(b"code=code+with+spaces", request.call_args.kwargs["raw_data"])
        self.assertEqual(
            request.call_args.kwargs["headers"]["Content-Type"],
            "application/x-www-form-urlencoded",
        )


if __name__ == "__main__":
    unittest.main()
