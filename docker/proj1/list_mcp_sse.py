import urllib.request
import urllib.parse
import json
import time
import threading

def list_tools():
    url = "http://agy-flash:7080/mcp"
    
    def read_sse():
        req = urllib.request.Request(url)
        req.add_header("Accept", "text/event-stream")
        with urllib.request.urlopen(req) as response:
            post_url = None
            for line in response:
                line = line.decode('utf-8').strip()
                if line.startswith("endpoint:"):
                    post_url = urllib.parse.urljoin(url, line.split(":", 1)[1].strip())
                    print("Found endpoint:", post_url)
                if line.startswith("data:"):
                    data_str = line[5:].strip()
                    if data_str:
                        print("SSE Data:", data_str)
                if post_url:
                    break
            
            if not post_url:
                print("No endpoint found")
                return
            
            time.sleep(0.5)
            # Send initialize
            req2 = urllib.request.Request(post_url, method="POST")
            req2.add_header("Content-Type", "application/json")
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
                urllib.request.urlopen(req2, data=init_data)
            except Exception as e:
                print("Init err:", e)
                
            time.sleep(0.5)
            # Send tools/list
            req3 = urllib.request.Request(post_url, method="POST")
            req3.add_header("Content-Type", "application/json")
            list_data = json.dumps({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list"
            }).encode("utf-8")
            try:
                urllib.request.urlopen(req3, data=list_data)
            except Exception as e:
                print("Tools List err:", e)
            
            # Keep reading a bit more
            for i in range(10):
                line = response.readline().decode('utf-8').strip()
                if line.startswith("data:"):
                    print("SSE Data:", line[5:].strip())

    t = threading.Thread(target=read_sse)
    t.start()
    t.join(timeout=5)

list_tools()
