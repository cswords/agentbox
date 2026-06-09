import urllib.request
import json
import time

def list_tools():
    url = "http://agy-flash:7080/mcp"
    req = urllib.request.Request(url, method="POST")
    req.add_header("Content-Type", "application/json")
    req.add_header("Accept", "application/json, text/event-stream")
    
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
    
    try:
        with urllib.request.urlopen(req, data=init_data) as response:
            print("Init Response:", response.read().decode())
    except Exception as e:
        print("Init Error:", e)

    req2 = urllib.request.Request(url, method="POST")
    req2.add_header("Content-Type", "application/json")
    req2.add_header("Accept", "application/json, text/event-stream")
    
    list_data = json.dumps({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    }).encode("utf-8")
    
    try:
        with urllib.request.urlopen(req2, data=list_data) as response:
            print("Tools List Response:", response.read().decode())
    except Exception as e:
        print("Tools List Error:", e)

list_tools()
