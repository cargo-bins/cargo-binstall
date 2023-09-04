import http.server
import os
import ssl

def remove_file(path):
    try:
        os.remove(path)
    except:
        pass

remove_file('ca.pem')
remove_file('ca.srl')
remove_file('ca.key')
remove_file('server.csr')
remove_file('server.ext')
remove_file('server.pem')
remove_file('server.key')
os.system('openssl req -newkey rsa:4096 -x509 -sha256 -days 1 -nodes -out ca.pem -keyout ca.key -subj "/C=UT/CN=ca.localhost"')
os.system('openssl req -new -newkey rsa:4096 -sha256 -nodes -out server.csr -keyout server.key -subj "/C=UT/CN=localhost"')
with open('server.ext', 'w') as file:
    file.write('''
authorityKeyIdentifier=keyid,issuer
basicConstraints=CA:FALSE
keyUsage = digitalSignature, nonRepudiation, keyEncipherment, dataEncipherment
subjectAltName = @alt_names
[alt_names]
DNS.1 = localhost
''')
os.system('openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -out server.pem -days 1 -sha256 -extfile server.ext')

server_address = ('', 4443)
httpd = http.server.HTTPServer(server_address, http.server.SimpleHTTPRequestHandler)
ctx = ssl.SSLContext(protocol=ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(certfile="server.pem", keyfile="server.key")
httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)
httpd.serve_forever()
