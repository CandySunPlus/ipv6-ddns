#!/usr/bin/env python3
"""Throwaway UI preview: python3 prototypes/preview-ui.py [--port 8765]."""
import argparse
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

parser = argparse.ArgumentParser(description="Preview only: no DNS or system-service operations.")
parser.add_argument("--port", type=int, default=8765)
args = parser.parse_args()
directory = Path(__file__).resolve().parent
handler = partial(SimpleHTTPRequestHandler, directory=str(directory))
server = ThreadingHTTPServer(("127.0.0.1", args.port), handler)
print(f"IPv6 DDNS UI PROTOTYPE: http://127.0.0.1:{args.port}/?platform=mac&variant=A", flush=True)
print("Preview only. Do not enter real credentials. Ctrl+C to stop.", flush=True)
try:
    server.serve_forever()
except KeyboardInterrupt:
    pass
finally:
    server.server_close()
