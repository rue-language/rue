#!/usr/bin/env python3
"""Black-box MCP stdio tests against real and controllable Rue producers."""

import json
import os
import pathlib
import select
import stat
import subprocess
import tempfile
import time

VERSION = "2026-07-28"


def request(request_id, method, params=None):
    params = dict(params or {})
    params["_meta"] = {
        "io.modelcontextprotocol/protocolVersion": VERSION,
        "io.modelcontextprotocol/clientCapabilities": {},
    }
    return {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}


def send(process, message):
    process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
    process.stdin.flush()


def receive(process, timeout=10):
    ready, _, _ = select.select([process.stdout], [], [], timeout)
    if not ready:
        raise AssertionError("MCP server did not respond before the timeout")
    line = process.stdout.readline()
    if not line:
        raise AssertionError("MCP server closed stdout before responding")
    return json.loads(line)


def receive_id(process, request_id, timeout=10):
    response = receive(process, timeout)
    if response.get("id") != request_id:
        raise AssertionError("unexpected response while awaiting {!r}: {!r}".format(request_id, response))
    return response


def server(extra_env=None):
    environment = os.environ.copy()
    environment.update(extra_env or {})
    return subprocess.Popen(
        [environment["RUE_MCP_BINARY"]], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, text=True, env=environment,
    )


def close(process):
    process.stdin.close()
    process.wait(timeout=10)
    stderr = process.stderr.read()
    if process.returncode != 0 or stderr:
        raise AssertionError("MCP server failed: status={} stderr={!r}".format(process.returncode, stderr))


def assert_tool_views(response):
    result = response["result"]
    structured = result["structuredContent"]
    assert json.loads(result["content"][0]["text"]) == structured
    return structured


def assert_rpc_error(response, code):
    assert response["error"]["code"] == code, response
    assert "_meta" not in response


def wait_file(path, timeout=5):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            return
        time.sleep(0.01)
    raise AssertionError("producer did not create synchronization file {}".format(path))


def process_is_alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


def wait_dead(pid, timeout=5):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not process_is_alive(pid):
            return
        time.sleep(0.01)
    raise AssertionError("descendant process {} survived cancellation".format(pid))


def wait_absent(path, timeout=5):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not path.exists():
            return
        time.sleep(0.01)
    raise AssertionError("owned temporary path survived cleanup: {}".format(path))


def tool_call(request_id, name, arguments):
    return request(request_id, "tools/call", {"name": name, "arguments": arguments})


def protocol_and_real_producer_tests():
    process = server()
    send(process, request("discover", "server/discover"))
    discover = receive_id(process, "discover")["result"]
    assert discover["supportedVersions"] == [VERSION]
    assert discover["resultType"] == "complete"
    assert discover["ttlMs"] > 0 and discover["cacheScope"] == "public"
    assert discover["_meta"]["io.modelcontextprotocol/serverInfo"]["name"] == "rue-mcp"

    send(process, request("tools", "tools/list"))
    tools = receive_id(process, "tools")["result"]
    assert tools["ttlMs"] > 0 and tools["cacheScope"] == "public"
    compile_entry = next(tool for tool in tools["tools"] if tool["name"] == "compile")
    assert compile_entry["annotations"] == {
        "readOnlyHint": False, "destructiveHint": False,
        "idempotentHint": True, "openWorldHint": False,
    }

    extended = request("extended", "tools/list", {"cursor": "next", "extension": True})
    extended["params"]["_meta"].update({
        "io.modelcontextprotocol/clientInfo": {
            "name": "protocol-test", "version": "1", "extension": True,
            "icons": [{"src": "data:image/png;base64,AA==", "sizes": ["16x16"], "theme": "light", "extension": True}],
        },
        "progressToken": 4.5,
        "extension": True,
    })
    extended["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"] = {
        "roots": {}, "sampling": {"tools": {}}, "custom.example/capability": True,
    }
    send(process, extended)
    assert receive_id(process, "extended")["result"]["tools"]
    send(process, request(1.5, "tools/list"))
    assert receive_id(process, 1.5)["result"]["tools"]

    input_response = request("input-responses", "tools/call", {
        "name": "error-metadata",
        "inputResponses": {
            "roots": {"roots": [{"uri": "file:///workspace", "extension": True}]},
            "elicitation": {"action": "accept", "content": {"answer": "yes", "score": 1.5, "choices": ["a", "b"]}},
            "sampling": {"content": {"type": "text", "text": "ok"}, "model": "m", "role": "assistant"},
            "tool-result": {
                "content": {
                    "type": "tool_result", "toolUseId": "call-1", "content": [
                        {
                            "type": "resource_link", "name": "source", "title": "Source",
                            "uri": "file:///workspace/main.rue", "description": "root module",
                            "mimeType": "text/plain", "size": 12.5,
                            "icons": [{"src": "data:image/png;base64,AA==", "theme": "dark"}],
                            "annotations": {"priority": 0.5}, "_meta": {"example/key": True},
                        },
                        {
                            "type": "resource",
                            "resource": {
                                "uri": "file:///workspace/main.rue", "mimeType": "text/plain",
                                "text": "fn main() {}", "_meta": {"example/key": True},
                            },
                        },
                    ],
                },
                "model": "m", "role": "assistant",
            },
        },
    })
    send(process, input_response)
    assert assert_tool_views(receive_id(process, "input-responses"))["errors"]

    bad = request("bad-version", "tools/list")
    bad["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = "2025-11-25"
    send(process, bad)
    response = receive_id(process, "bad-version")
    assert_rpc_error(response, -32022)
    assert response["error"]["data"] == {"supported": [VERSION], "requested": "2025-11-25"}
    missing = request("missing-version", "tools/list")
    missing["params"]["_meta"].pop("io.modelcontextprotocol/protocolVersion")
    send(process, missing)
    assert_rpc_error(receive_id(process, "missing-version"), -32602)
    wrong = request("wrong-version-type", "tools/list")
    wrong["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = 7
    send(process, wrong)
    assert_rpc_error(receive_id(process, "wrong-version-type"), -32602)

    for malformed in ([], {}, {"jsonrpc": "2.0", "id": 1}):
        send(process, malformed)
        assert_rpc_error(receive(process), -32600)
    process.stdin.write("{not-json}\n")
    process.stdin.flush()
    assert_rpc_error(receive(process), -32700)
    for notification in (
        {"jsonrpc": "2.0", "method": "notifications/unknown", "params": {}},
        {"jsonrpc": "1.0", "method": "notifications/unknown", "params": []},
        {"jsonrpc": "2.0", "method": 7, "params": {}},
    ):
        send(process, notification)
    send(process, request("after-notification", "tools/list"))
    assert receive_id(process, "after-notification")["result"]["tools"]

    invalid_requests = []
    for request_id, field, value in (
        ("cursor-type", "cursor", 3),
        ("request-state-type", "requestState", 3),
        ("input-responses-type", "inputResponses", []),
        ("input-response-value", "inputResponses", {"key": 3}),
        ("input-response-shape", "inputResponses", {"key": {}}),
        ("input-response-nested-shape", "inputResponses", {
            "key": {"content": {"type": "tool_result", "toolUseId": "call-1"}, "model": "m", "role": "assistant"},
        }),
        ("input-response-root-scheme", "inputResponses", {
            "key": {"roots": [{"uri": "https://example.com"}]},
        }),
        ("input-response-root-meta", "inputResponses", {
            "key": {"roots": [{"uri": "file:///workspace", "_meta": 7}]},
        }),
        ("input-response-resource-meta", "inputResponses", {
            "key": {
                "content": {
                    "type": "tool_result", "toolUseId": "call-1", "content": [
                        {"type": "resource", "resource": {"uri": "file:///x", "text": "x", "_meta": 7}},
                    ],
                },
                "model": "m", "role": "assistant",
            },
        }),
        ("input-response-resource-link-icon", "inputResponses", {
            "key": {
                "content": {
                    "type": "tool_result", "toolUseId": "call-1", "content": [
                        {"type": "resource_link", "name": "x", "uri": "file:///x", "icons": [{"src": 7}]},
                    ],
                },
                "model": "m", "role": "assistant",
            },
        }),
    ):
        item = request(request_id, "tools/list" if field == "cursor" else "tools/call")
        item["params"][field] = value
        if item["method"] == "tools/call":
            item["params"]["name"] = "error-metadata"
        invalid_requests.append(item)
    for request_id, meta_key, value in (
        ("client-info-type", "io.modelcontextprotocol/clientInfo", {}),
        ("capability-type", "io.modelcontextprotocol/clientCapabilities", {"sampling": {"tools": True}}),
        ("progress-token-type", "progressToken", True),
        ("client-icon-shape", "io.modelcontextprotocol/clientInfo", {"name": "x", "version": "1", "icons": [{}]}),
    ):
        item = request(request_id, "server/discover")
        item["params"]["_meta"][meta_key] = value
        invalid_requests.append(item)
    for item in invalid_requests:
        send(process, item)
        assert_rpc_error(receive_id(process, item["id"]), -32602)

    invalid_calls = [
        tool_call("unknown-tool", "unknown", {}),
        tool_call("extra", "check", {"root": "main.rue", "extra": True}),
        tool_call("root-type", "check", {"root": 4}),
        tool_call("root-option", "check", {"root": "--help"}),
        tool_call("manifest-option", "check", {"root": "main.rue", "sourceManifest": "--help"}),
        tool_call("output-option", "compile", {"root": "main.rue", "output": "--help"}),
        tool_call("target-option", "check", {"root": "main.rue", "target": "--help"}),
        tool_call("optimization", "check", {"root": "main.rue", "optimization": 4}),
        tool_call("optimization-float", "check", {"root": "main.rue", "optimization": 1.5}),
        tool_call("preview-type", "check", {"root": "main.rue", "preview": "x"}),
        tool_call("preview-duplicate", "check", {"root": "main.rue", "preview": ["x", "x"]}),
        tool_call("preview-option", "check", {"root": "main.rue", "preview": ["--help"]}),
        tool_call("error-pattern", "spec", {"errorCode": "E12"}),
        tool_call("explain-pattern", "explain-error", {"code": "E12"}),
        tool_call("metadata-extra", "error-metadata", {"x": 1}),
    ]
    for item in invalid_calls:
        send(process, item)
        assert_rpc_error(receive_id(process, item["id"]), -32602)

    with tempfile.TemporaryDirectory(prefix="rue-mcp-protocol-") as directory:
        root = pathlib.Path(directory) / "main.rue"
        root.write_text("fn main() -> i32 { missing }\n", encoding="utf-8")
        send(process, tool_call("check", "check", {"root": str(root)}))
        checked = assert_tool_views(receive_id(process, "check"))
        assert checked["success"] is False
        assert checked["diagnosticSchema"] == "docs/process/diagnostics.md"
        assert checked["diagnostics"][0]["code"] == "E0201"
        assert checked["diagnostics"][0]["spans"][0]["file"].endswith("main.rue")

        helper = pathlib.Path(directory) / "helper.rue"
        helper.write_text("pub fn value() -> i32 { 7 }\n", encoding="utf-8")
        root.write_text('const helper = @import("helper.rue");\nfn main() -> i32 { helper.value() }\n', encoding="utf-8")
        manifest = pathlib.Path(directory) / "sources.manifest"
        manifest.write_text("main.rue\n", encoding="utf-8")
        send(process, tool_call("bounded-check", "check", {"root": str(root), "sourceManifest": str(manifest)}))
        bounded = assert_tool_views(receive_id(process, "bounded-check"))
        assert bounded["success"] is False and bounded["diagnostics"]

        root.write_text("fn main() -> i32 { 0 }\n", encoding="utf-8")
        executable = pathlib.Path(directory) / "program"
        send(process, tool_call("compile", "compile", {"root": str(root), "output": str(executable)}))
        compiled = assert_tool_views(receive_id(process, "compile"))
        assert compiled["success"] is True and compiled["diagnostics"] == []
        assert compiled["output"] == str(executable) and executable.is_file()
        assert executable.stat().st_mode & stat.S_IXUSR

        send(process, tool_call("no-clobber", "compile", {"root": str(root), "output": str(executable)}))
        no_clobber = receive_id(process, "no-clobber")["result"]
        assert no_clobber["isError"] is True
        assert "refusing to replace" in no_clobber["structuredContent"]["error"]
        assert not list(pathlib.Path(directory).glob(".rue-mcp-publish-*"))

    send(process, tool_call("spec", "spec", {"errorCode": "E0201"}))
    index = assert_tool_views(receive_id(process, "spec"))
    assert index["schema_version"] == 1
    assert index["errors"][0]["code"] == "E0201"
    assert all(item["error_code"] == "E0201" for item in index["error_spec_relationships"])

    process.stdin.write(" " * (1024 * 1024 + 1) + "\n")
    process.stdin.flush()
    assert_rpc_error(receive(process), -32600)
    send(process, request("after-limit", "tools/list"))
    assert receive_id(process, "after-limit")["result"]["tools"]
    close(process)


def write_owned_producer(path):
    path.write_text(
        """#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys, time
output = pathlib.Path(sys.argv[sys.argv.index('-o') + 1])
output.with_suffix('.pending').write_text('pending', encoding='utf-8')
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])
pathlib.Path(os.environ['MCP_STARTED']).write_text(json.dumps({'producer': os.getpid(), 'descendant': child.pid, 'output': str(output)}), encoding='utf-8')
time.sleep(30)
""",
        encoding="utf-8",
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def cancellation_and_cleanup_tests():
    if os.name != "posix":
        return
    with tempfile.TemporaryDirectory(prefix="rue-mcp-cancel-") as directory:
        directory = pathlib.Path(directory)
        producer = directory / "slow-rue"
        write_owned_producer(producer)

        marker = directory / "limits.json"
        process = server({"RUE_BINARY": str(producer), "MCP_STARTED": str(marker)})
        for number in range(8):
            send(process, tool_call("busy-{}".format(number), "check", {"root": "unused.rue"}))
        send(process, tool_call("too-busy", "check", {"root": "unused.rue"}))
        assert_rpc_error(receive_id(process, "too-busy"), -32000)
        for number in range(8):
            send(process, {"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "busy-{}".format(number)}})
        close(process)

        marker = directory / "immediate.json"
        process = server({"RUE_BINARY": str(producer), "MCP_STARTED": str(marker)})
        send(process, tool_call("cancel-immediate", "check", {"root": "unused.rue"}))
        send(process, tool_call("cancel-immediate", "check", {"root": "unused.rue"}))
        assert_rpc_error(receive_id(process, "cancel-immediate"), -32600)
        send(process, {"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "cancel-immediate"}})
        send(process, request("after-immediate", "tools/list"))
        assert receive_id(process, "after-immediate")["result"]["tools"]
        close(process)

        marker = directory / "cancel.json"
        process = server({"RUE_BINARY": str(producer), "MCP_STARTED": str(marker)})
        send(process, tool_call("cancel-spawned", "check", {"root": "unused.rue"}))
        wait_file(marker)
        state = json.loads(marker.read_text(encoding="utf-8"))
        send(process, {"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": {"bad": True}}})
        send(process, request("after-malformed-cancel", "tools/list"))
        assert receive_id(process, "after-malformed-cancel")["result"]["tools"]
        assert process_is_alive(state["producer"])
        send(process, {"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "cancel-spawned"}})
        send(process, request("after-cancel", "tools/list"))
        assert receive_id(process, "after-cancel")["result"]["tools"]
        wait_dead(state["descendant"])
        wait_absent(pathlib.Path(state["output"]).parent)
        close(process)

        # A producer that exits while its descendant retains inherited output
        # handles must still complete promptly; the owned group is terminated
        # before the leader is reaped and its PID can be reused.
        inherited = directory / "inherited-rue"
        marker = directory / "inherited.json"
        inherited.write_text(
            """#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
output = pathlib.Path(sys.argv[sys.argv.index('-o') + 1])
output.write_bytes(b'executable')
output.chmod(0o700)
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])
pathlib.Path(os.environ['MCP_STARTED']).write_text(json.dumps({'descendant': child.pid}), encoding='utf-8')
""",
            encoding="utf-8",
        )
        inherited.chmod(inherited.stat().st_mode | stat.S_IXUSR)
        process = server({"RUE_BINARY": str(inherited), "MCP_STARTED": str(marker)})
        send(process, tool_call("inherited-pipe", "check", {"root": "unused.rue"}))
        result = assert_tool_views(receive_id(process, "inherited-pipe", timeout=5))
        assert result["success"] is True
        wait_file(marker)
        wait_dead(json.loads(marker.read_text(encoding="utf-8"))["descendant"])
        close(process)

        marker = directory / "eof.json"
        process = server({"RUE_BINARY": str(producer), "MCP_STARTED": str(marker)})
        send(process, tool_call("eof-spawned", "check", {"root": "unused.rue"}))
        wait_file(marker)
        state = json.loads(marker.read_text(encoding="utf-8"))
        started = time.monotonic()
        close(process)
        assert time.monotonic() - started < 5
        wait_dead(state["descendant"])
        wait_absent(pathlib.Path(state["output"]).parent)


def output_limit_test():
    with tempfile.TemporaryDirectory(prefix="rue-mcp-output-limit-") as directory:
        producer = pathlib.Path(directory) / "noisy-rue"
        producer.write_text("#!/usr/bin/env python3\nimport os\nos.write(1, b'x' * (8 * 1024 * 1024 + 1))\n", encoding="utf-8")
        producer.chmod(producer.stat().st_mode | stat.S_IXUSR)
        process = server({"RUE_BINARY": str(producer)})
        send(process, tool_call("noisy", "check", {"root": "unused.rue"}))
        result = receive_id(process, "noisy", timeout=20)["result"]
        assert result["isError"] is True
        assert "stream limit" in result["structuredContent"]["error"]
        close(process)

        empty = pathlib.Path(directory) / "empty-rue"
        empty.write_text("#!/usr/bin/env python3\n", encoding="utf-8")
        empty.chmod(empty.stat().st_mode | stat.S_IXUSR)
        process = server({"RUE_BINARY": str(empty)})
        destination = pathlib.Path(directory) / "must-not-exist"
        send(process, tool_call("missing-artifact", "compile", {"root": "main.rue", "output": str(destination)}))
        result = receive_id(process, "missing-artifact")["result"]
        assert result["isError"] is True
        assert "without creating" in result["structuredContent"]["error"]
        assert not destination.exists()
        close(process)


def main():
    protocol_and_real_producer_tests()
    cancellation_and_cleanup_tests()
    output_limit_test()


if __name__ == "__main__":
    main()
