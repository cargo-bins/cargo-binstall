import http.server
import os
import ssl
from pathlib import Path

cert_dir = Path(os.environ["CERT_DIR"])

os.chdir(os.path.dirname(__file__))

server_address = ('', 4443)
httpd = http.server.HTTPServer(server_address, http.server.SimpleHTTPRequestHandler)
ctx = ssl.SSLContext(protocol=ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(certfile=cert_dir / "server.pem", keyfile=cert_dir / "server.key")
httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)
httpd.serve_forever()
