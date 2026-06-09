import urllib.request
import json

url = "http://agy-flash:7080/mcp"

# 1. Initialize
req1 = urllib.request.Request(url, method="POST")
req1.add_header("Content-Type", "application/json")
req1.add_header("Accept", "application/json, text/event-stream")
init_data = json.dumps({
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": {"name": "test", "version": "1.0"}
    }
}).encode("utf-8")

session_id = None
with urllib.request.urlopen(req1, data=init_data) as response:
    session_id = response.getheader("mcp-session-id")

if not session_id:
    print("No session ID found")
    exit(1)

# 2. notifications/initialized
req2 = urllib.request.Request(url, method="POST")
req2.add_header("Content-Type", "application/json")
req2.add_header("Accept", "application/json, text/event-stream")
req2.add_header("mcp-session-id", session_id)
notif_data = json.dumps({
    "jsonrpc": "2.0",
    "method": "notifications/initialized"
}).encode("utf-8")

urllib.request.urlopen(req2, data=notif_data)

# 3. tools/list
req3 = urllib.request.Request(url, method="POST")
req3.add_header("Content-Type", "application/json")
req3.add_header("Accept", "application/json, text/event-stream")
req3.add_header("mcp-session-id", session_id)
call_data = json.dumps({
    "jsonrpc": "2.0",
    "id": 2,
    "method": "tools/list",
    "params": {}
}).encode("utf-8")

try:
    with urllib.request.urlopen(req3, data=call_data) as response:
        print("Tools:", response.read().decode())
except urllib.error.HTTPError as e:
    print("Call err:", e, e.read().decode())
except Exception as e:
    print("Call err:", e)
