from .model import command_result, decode_result, encode_command, normalize_filter, normalize_options
from ._native import WardrobeEmbeddedEngine


class WardrobeEmbedded:
    def __init__(self, engine):
        self._engine = engine

    @classmethod
    def open(cls, target):
        return cls(WardrobeEmbeddedEngine.open(target))

    def execute(self, command):
        return decode_result(self._engine.execute_json(encode_command(command)))

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
