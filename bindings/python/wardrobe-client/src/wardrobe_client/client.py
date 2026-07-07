from .model import command_result, normalize_filter, normalize_options
from .protocol import execute, open_socket


class WardrobeClient:
    def __init__(self, sock):
        self._sock = sock

    @classmethod
    def open(cls, connection_string, timeout=None):
        return cls(open_socket(connection_string, timeout=timeout))

    def close(self):
        if self._sock is not None:
            self._sock.close()
            self._sock = None

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, traceback):
        self.close()
        return False

    def execute(self, command):
        if self._sock is None:
            raise RuntimeError("Wardrobe client is closed")
        return execute(self._sock, command)

    def upsert(self, payload, filter=None, options=None):
        result = self.execute(
            {
                "upsert": {
                    "payload": payload,
                    "filter": normalize_filter(filter),
                    "options": normalize_options(options),
                }
            }
        )
        return command_result(result, "upsert")

    def read(self, filter=None, options=None):
        result = self.execute(
            {"read": {"filter": normalize_filter(filter), "options": normalize_options(options)}}
        )
        return command_result(result, "read")

    def delete(self, filter=None, options=None):
        result = self.execute(
            {
                "delete": {
                    "filter": normalize_filter(filter),
                    "options": normalize_options(options),
                }
            }
        )
        return command_result(result, "delete")

    def inspect(self, filter=None, options=None):
        result = self.execute(
            {
                "inspect": {
                    "filter": normalize_filter(filter),
                    "options": normalize_options(options),
                }
            }
        )
        return command_result(result, "inspect")

    def count(self, filter=None, options=None):
        result = self.execute(
            {"count": {"filter": normalize_filter(filter), "options": normalize_options(options)}}
        )
        return command_result(result, "count")

    def clean(self, request=None):
        return command_result(self.execute({"compact": request}), "compact")

    def create(self, request):
        return command_result(self.execute({"create": request}), "create")

    def alter(self, request):
        return command_result(self.execute({"alter": request}), "alter")

    def drop(self, request):
        return command_result(self.execute({"drop": request}), "drop")

    def backup(self, source_path):
        return command_result(self.execute({"backup": {"source_path": source_path}}), "backup")

    def restore(self, destination_path, archive):
        return command_result(
            self.execute({"restore": {"destination_path": destination_path, "archive": archive}}),
            "restore",
        )

    def grant(self, request):
        return command_result(self.execute({"grant": request}), "grant")

    def revoke(self, request):
        return command_result(self.execute({"revoke": request}), "revoke")

    def status(self, request="Storage"):
        return command_result(self.execute({"status": request}), "status")
