"""Serve a directory over HTTPS on localhost, for e2e tests.

Usage: https_server.py DIR CERT KEY PORT_FILE

Listens on an OS-chosen port, on both ::1 and 127.0.0.1 where the OS allows
it, so that `localhost` works whichever address it resolves to first.
The port is written to PORT_FILE once the socket is listening.
"""

import functools
import http.server
import os
import socket
import ssl
import sys


class DualStackServer(http.server.ThreadingHTTPServer):
    address_family = socket.AF_INET6

    def server_bind(self):
        self.socket.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 0)
        super().server_bind()


def make_server(handler):
    try:
        return DualStackServer(("::", 0), handler)
    except OSError:
        return http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)


def main():
    directory, cert, key, port_file = sys.argv[1:]

    handler = functools.partial(
        http.server.SimpleHTTPRequestHandler, directory=directory
    )
    httpd = make_server(handler)

    ctx = ssl.SSLContext(protocol=ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(certfile=cert, keyfile=key)
    httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)

    tmp = port_file + ".tmp"
    with open(tmp, "w") as f:
        f.write(str(httpd.server_address[1]))
    os.replace(tmp, port_file)

    httpd.serve_forever()


if __name__ == "__main__":
    main()
