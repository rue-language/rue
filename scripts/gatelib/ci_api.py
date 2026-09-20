"""JSON transport and Linear authentication shared by CI issue reporters."""
from __future__ import annotations

import json
import urllib.error
import urllib.request

LINEAR_API_URL = "https://api.linear.app/graphql"


class TransportError(RuntimeError):
    """A request could not be completed. Always surfaced; never swallowed."""


class Transport:
    """Minimal injectable HTTP seam.

    Tests substitute a mock; `--dry-run` substitutes a synthesizer. Keeping the
    seam this thin means the client code under test is the same code CI runs.
    """

    def request(
        self, method: str, url: str, headers: dict[str, str], payload: dict | None
    ) -> dict | list:
        """Send `payload` as JSON and return the decoded response.

        The return type is `dict | list` because GitHub's REST list endpoints
        answer with a bare array; callers that expect an object must say so.
        """
        raise NotImplementedError


class UrllibTransport(Transport):
    """The real transport: stdlib only, so the script has no dependencies."""

    def __init__(self, timeout: float = 30.0) -> None:
        self.timeout = timeout

    def request(
        self, method: str, url: str, headers: dict[str, str], payload: dict | None
    ) -> dict | list:
        data = json.dumps(payload).encode() if payload is not None else None
        request = urllib.request.Request(url, data=data, method=method)
        for key, value in headers.items():
            request.add_header(key, value)
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                body = response.read().decode(errors="replace")
        except urllib.error.HTTPError as error:
            detail = error.read().decode(errors="replace")[:500]
            raise TransportError(f"{method} {url} -> HTTP {error.code}: {detail}")
        except urllib.error.URLError as error:
            raise TransportError(f"{method} {url} -> {error.reason}")
        if not body:
            return {}
        try:
            return json.loads(body)
        except json.JSONDecodeError:
            raise TransportError(f"{method} {url} -> non-JSON response: {body[:200]}")


class LinearClient:
    """Small injectable GraphQL client; callers own issue policy and payloads."""

    def __init__(self, transport: Transport, api_key: str) -> None:
        self.transport = transport
        self.api_key = api_key

    def _headers(self) -> dict[str, str]:
        # Personal API keys are sent raw; OAuth access tokens need `Bearer`.
        # Sending a personal key as `Bearer` is rejected, so this is not a
        # cosmetic distinction.
        authorization = (
            f"Bearer {self.api_key}"
            if self.api_key.startswith("lin_oauth")
            else self.api_key
        )
        return {
            "Authorization": authorization,
            "Content-Type": "application/json",
        }

    def _graphql(self, operation: str, query: str, variables: dict) -> dict:
        response = self.transport.request(
            "POST",
            LINEAR_API_URL,
            self._headers(),
            {"operationName": operation, "query": query, "variables": variables},
        )
        if not isinstance(response, dict):
            raise TransportError(f"Linear {operation} returned a non-object response")
        if response.get("errors"):
            raise TransportError(f"Linear {operation} failed: {response['errors']}")
        data = response.get("data")
        if data is None:
            raise TransportError(f"Linear {operation} returned no data: {response}")
        return data

