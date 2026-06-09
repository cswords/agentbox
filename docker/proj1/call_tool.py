import urllib.request
import json
import time

def call_tool():
    url = "http://agy-flash:7080/mcp"
    req = urllib.request.Request(url, method="POST")
    req.add_header("Content-Type", "application/json")
    req.add_header("Accept", "application/json, text/event-stream")
    
    # We might need to initialize first in the same script, but maybe it's stateless?
    # Let's try just calling tools/call.
    call_data = json.dumps([
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "1.0"}
            }
        },
        {
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        },
        {
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "run_agent",
                "arguments": {
                    "prompt": "what is 6*7? Just the number."
                }
            }
        }
    ]).encode("utf-8")
    
    try:
        with urllib.request.urlopen(req, data=call_data) as response:
            print("Call Response:", response.read().decode())
    except urllib.error.HTTPError as e:
        print("Call Error:", e, e.read().decode())
    except Exception as e:
        print("Error:", e)

call_tool()
