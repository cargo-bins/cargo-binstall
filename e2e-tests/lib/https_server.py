"""Serve a directory over HTTPS on localhost, for e2e tests.

Usage: https_server.py DIR CERT KEY PORT_FILE REQUEST_LOG SENT_LOG [ROUTES]

Listens on an OS-chosen port, on both ::1 and 127.0.0.1 where the OS allows
it, so that `localhost` works whichever address it resolves to first.
The port is written to PORT_FILE once the socket is listening, which is done
before anything else, with nothing but `socket` imported: on a CI runner
busy with the other e2e tests, starting Python and importing http.server and
ssl can take seconds. Connections made meanwhile wait in the listen backlog.

Every request is appended to REQUEST_LOG as `METHOD PATH`, followed by
` Range: ...` when it asks for a range.

Once the whole body of a file has been sent for a GET (or the rest of it,
for a range), its path is appended to SENT_LOG, so that a test can tell a
download from a request whose response wasn't read: for a file larger than
the socket buffers hold, that one isn't logged.

ROUTES, if given, is a JSON file mapping a path to a list of responses to
give, in order, to the requests for that path (HEAD and GET alike); once the
list is used up, the path is served from DIR as usual. Each response is one
of:

    {"status": 503, "headers": {"Retry-After": "1"}}
        that status, with those headers and an empty body
    {"redirect": "/other", "status": 302}
        a redirect; status defaults to 302
    {"truncate": 100}
        the file from DIR with its full Content-Length, but only the first
        100 bytes of it before the connection is closed
    {"ignore_range": true}
        the whole file from DIR with a 200, even if a range was asked for
    {"stall": 100}
        the file from DIR with its full Content-Length, but only the first
        100 bytes of it, then nothing more until the client hangs up
    {"stall": null}
        no response at all until the client hangs up

Otherwise a `Range: bytes=N-` request for a file is answered with a 206 and
the rest of the file from byte N, unless an `If-Range` doesn't match the
file's `Last-Modified`.
"""

import os
import socket
import sys


def listen():
    """Listen on an OS-chosen port, dual-stack if possible."""
    try:
        sock = socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
        sock.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 0)
        sock.bind(("::", 0))
    except OSError:
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.bind(("127.0.0.1", 0))
    sock.listen(128)
    return sock


def serve(sock, directory, cert, key, request_log, sent_log, routes_file):
    import functools
    import http.server
    import json
    import ssl
    import threading
    from email.utils import formatdate

    class Handler(http.server.SimpleHTTPRequestHandler):
        def __init__(self, *args, routes, lock, request_log, sent_log, **kwargs):
            self.routes = routes
            self.lock = lock
            self.request_log = request_log
            self.sent_log = sent_log
            super().__init__(*args, **kwargs)

        def log_sent(self):
            with self.lock:
                with open(self.sent_log, "a", newline="\n") as f:
                    f.write(f"{self.path.split('?', 1)[0]}\n")

        def next_action(self):
            path = self.path.split("?", 1)[0]
            with self.lock:
                with open(self.request_log, "a", newline="\n") as f:
                    range_header = self.headers.get("Range")
                    suffix = f" Range: {range_header}" if range_header else ""
                    f.write(f"{self.command} {path}{suffix}\n")
                actions = self.routes.get(path)
                return actions.pop(0) if actions else None

        def copyfile(self, source, outputfile):
            super().copyfile(source, outputfile)
            outputfile.flush()
            self.log_sent()

        def wait_for_hang_up(self):
            """Send nothing (more), until the client closes the connection."""
            try:
                while self.rfile.read(1):
                    pass
            except OSError:
                pass

        def handle_action(self, action):
            if action is None:
                return False

            if "redirect" in action:
                self.send_response(action.get("status", 302))
                self.send_header("Location", action["redirect"])
                self.send_header("Content-Length", "0")
                self.end_headers()
            elif action.get("ignore_range"):
                del self.headers["Range"]
                return False
            elif "stall" in action and action["stall"] is None:
                self.wait_for_hang_up()
                self.close_connection = True
            elif "truncate" in action or "stall" in action:
                path = self.translate_path(self.path)
                with open(path, "rb") as f:
                    data = f.read()
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(data)))
                self.send_header(
                    "Last-Modified", formatdate(os.path.getmtime(path), usegmt=True)
                )
                self.end_headers()
                if self.command != "HEAD":
                    self.wfile.write(data[: action.get("truncate", action.get("stall"))])
                    self.wfile.flush()
                    if "stall" in action:
                        self.wait_for_hang_up()
                self.close_connection = True
            else:
                self.send_response(action["status"])
                for name, value in action.get("headers", {}).items():
                    self.send_header(name, value)
                self.send_header("Content-Length", "0")
                self.end_headers()

            return True

        def serve_range(self):
            """Answer `Range: bytes=N-` with a 206, returning whether it did."""
            range_header = self.headers.get("Range", "")
            if not range_header.startswith("bytes=") or not range_header.endswith("-"):
                return False
            path = self.translate_path(self.path)
            if not os.path.isfile(path):
                return False

            last_modified = formatdate(os.path.getmtime(path), usegmt=True)
            if_range = self.headers.get("If-Range")
            if if_range is not None and if_range != last_modified:
                return False

            with open(path, "rb") as f:
                data = f.read()
            start = int(range_header[len("bytes=") : -1])
            if start >= len(data):
                return False

            self.send_response(206)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Range", f"bytes {start}-{len(data) - 1}/{len(data)}")
            self.send_header("Content-Length", str(len(data) - start))
            self.send_header("Last-Modified", last_modified)
            self.end_headers()
            self.wfile.write(data[start:])
            self.wfile.flush()
            self.log_sent()
            return True

        def do_GET(self):
            if self.handle_action(self.next_action()):
                return
            if not self.serve_range():
                super().do_GET()

        def do_HEAD(self):
            if not self.handle_action(self.next_action()):
                super().do_HEAD()

    routes = {}
    if routes_file is not None:
        with open(routes_file) as f:
            routes = json.load(f)

    handler = functools.partial(
        Handler,
        directory=directory,
        routes=routes,
        lock=threading.Lock(),
        request_log=request_log,
        sent_log=sent_log,
    )

    class Server(http.server.ThreadingHTTPServer):
        address_family = sock.family

    httpd = Server(sock.getsockname()[:2], handler, bind_and_activate=False)
    httpd.socket.close()
    # What HTTPServer.server_bind would have set.
    httpd.server_name = "localhost"
    httpd.server_port = sock.getsockname()[1]

    ctx = ssl.SSLContext(protocol=ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(certfile=cert, keyfile=key)
    httpd.socket = ctx.wrap_socket(sock, server_side=True)

    httpd.serve_forever()


def main():
    directory, cert, key, port_file, request_log, sent_log = sys.argv[1:7]
    routes_file = sys.argv[7] if len(sys.argv) > 7 else None

    sock = listen()
    port = sock.getsockname()[1]
    tmp = port_file + ".tmp"
    with open(tmp, "w") as f:
        f.write(str(port))
    os.replace(tmp, port_file)
    print(f"https_server.py: listening on port {port}", file=sys.stderr, flush=True)

    serve(sock, directory, cert, key, request_log, sent_log, routes_file)


if __name__ == "__main__":
    main()
