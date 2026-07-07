import json
import socket
import struct

PROTOCOL_MAGIC = b"WD"
OPCODE_COMMAND = 0x01
OPCODE_RESULT = 0x02
OPCODE_ERROR = 0x03
DEFAULT_NETWORK_PORT = 24842


def open_socket(connection_string, timeout=None):
    if connection_string.startswith("wardrobe+unix://"):
        socket_path = connection_string[len("wardrobe+unix://") :]
        return open_unix_socket(socket_path, timeout)

    if connection_string.startswith("wardrobe://unix/"):
        socket_path = connection_string[len("wardrobe://unix/") :]
        return open_unix_socket(socket_path, timeout)

    if connection_string.startswith("wardrobe://"):
        authority = connection_string[len("wardrobe://") :].rstrip("/")
        if not authority:
            raise ValueError("Network Wardrobe connection URI requires a host")
        if "/" in authority:
            raise ValueError("Network Wardrobe connection URI should not contain a path")
        if ":" in authority:
            host, port = authority.rsplit(":", 1)
            return socket.create_connection((host, int(port)), timeout=timeout)
        return socket.create_connection((authority, DEFAULT_NETWORK_PORT), timeout=timeout)

    raise ValueError(f"Unsupported network connection string: {connection_string}")


def open_unix_socket(socket_path, timeout):
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    if timeout is not None:
        sock.settimeout(timeout)
    sock.connect(socket_path)
    return sock


def write_frame(sock, opcode, payload):
    payload_bytes = payload.encode("utf-8")
    header = PROTOCOL_MAGIC + bytes([opcode]) + struct.pack(">I", len(payload_bytes))
    sock.sendall(header + payload_bytes)


def read_frame(sock):
    header = read_exact(sock, 7)
    if header[:2] != PROTOCOL_MAGIC:
        raise ValueError("Invalid Wardrobe protocol magic bytes")
    opcode = header[2]
    payload_len = struct.unpack(">I", header[3:7])[0]
    payload = read_exact(sock, payload_len).decode("utf-8")
    return opcode, payload


def read_exact(sock, length):
    chunks = bytearray()
    while len(chunks) < length:
        chunk = sock.recv(length - len(chunks))
        if not chunk:
            raise ConnectionError("Wardrobe connection closed while reading protocol frame")
        chunks.extend(chunk)
    return bytes(chunks)


def execute(sock, command):
    write_frame(sock, OPCODE_COMMAND, json.dumps(command, separators=(",", ":")))
    opcode, payload = read_frame(sock)
    if opcode == OPCODE_RESULT:
        return json.loads(payload)
    if opcode == OPCODE_ERROR:
        raise RuntimeError(payload)
    raise ValueError(f"Wardrobe server returned unexpected opcode: {opcode}")
