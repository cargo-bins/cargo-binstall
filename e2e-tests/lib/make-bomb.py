"""Write compressed zero bytes, for unpack-limit.sh.

Usage: make-bomb.py DIR NAME SIZE

Writes DIR/NAME.gz, SIZE zero bytes gzipped, and DIR/NAME.zip, holding a
file `payload` of SIZE zero bytes deflated. Both shrink zeros about a
thousandfold.
"""

import gzip
import os
import sys
import zipfile

CHUNK = b"\0" * (1 << 20)


def write_zeros(f, size):
    while size:
        n = min(size, len(CHUNK))
        f.write(CHUNK[:n])
        size -= n


def main():
    out, name, size = sys.argv[1], sys.argv[2], int(sys.argv[3])

    with gzip.open(os.path.join(out, name + ".gz"), "wb", compresslevel=9) as f:
        write_zeros(f, size)

    with zipfile.ZipFile(os.path.join(out, name + ".zip"), "w") as zf:
        info = zipfile.ZipInfo("payload")
        info.external_attr = 0o100755 << 16
        info.compress_type = zipfile.ZIP_DEFLATED
        with zf.open(info, "w", force_zip64=True) as f:
            write_zeros(f, size)


main()
