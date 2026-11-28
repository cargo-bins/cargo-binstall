"""Writes v6.asc and v6.sig, an RFC 9580 Ed25519 key and its signature of
`message`, and cross-checks every fixture with Sequoia."""

import glob

from pysequoia import Cert, Profile, Sig, SignatureMode, sign, verify

cert = Cert.generate("binstall-test-v6 <v6@example.com>", profile=Profile.RFC9580)
with open("v6.asc", "w") as f:
    f.write(str(cert))

message = open("message", "rb").read()
signature = sign(cert.secrets.signer(), message, mode=SignatureMode.DETACHED, armor=False)
with open("v6.sig", "wb") as f:
    f.write(signature)

for key in ["ed25519", "rsa3072", "nistp256", "nistp384", "nistp521", "subkey", "v6"]:
    certs = [Cert.from_file(f"{key}.asc")]
    for sig in glob.glob(f"{key}.sig") + glob.glob(f"{key}-sha512.sig"):
        verify(bytes=message, store=lambda ids: certs, signature=Sig.from_file(sig))
