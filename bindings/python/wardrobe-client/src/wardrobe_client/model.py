SUPPORTED_OPERATIONS = (
    "read",
    "upsert",
    "delete",
    "inspect",
    "count",
    "clean",
    "create",
    "alter",
    "drop",
    "backup",
    "restore",
    "grant",
    "revoke",
    "status",
)


def normalize_filter(filter_value=None):
    if filter_value is None:
        return "None"

    if isinstance(filter_value, str):
        if filter_value.startswith("@"):
            if ":" in filter_value:
                return {"Pointer": filter_value}
            return {"Drawer": filter_value[1:]}
        return {"Drawer": filter_value}

    if isinstance(filter_value, list):
        return {"Many": [normalize_filter(item) for item in filter_value]}

    if isinstance(filter_value, dict):
        if not filter_value:
            return "None"
        if "drawer" in filter_value:
            return {"Drawer": filter_value["drawer"]}
        if "pointer" in filter_value:
            return {"Pointer": filter_value["pointer"]}
        if "query" in filter_value:
            return {"Query": filter_value["query"]}
        return {"Query": filter_value}

    return {"Query": filter_value}


def normalize_options(options=None):
    if options is None:
        return {}

    return {
        "multi": options.get("multi"),
        "atomic": options.get("atomic"),
        "create_if_missing": options.get("create_if_missing", options.get("createIfMissing")),
        "return_shape": normalize_return_shape(
            options.get("return_shape", options.get("returnShape"))
        ),
        "hydrate": options.get("hydrate"),
        "limit": options.get("limit"),
        "offset": options.get("offset"),
        "order_by": options.get("order_by", options.get("orderBy")),
        "order_direction": normalize_order_direction(
            options.get("order_direction", options.get("orderDirection"))
        ),
        "include_diagnostics": options.get(
            "include_diagnostics", options.get("includeDiagnostics")
        ),
    }


def normalize_return_shape(return_shape):
    if not isinstance(return_shape, str):
        return return_shape
    return {
        "default": "Default",
        "records": "Records",
        "record": "Record",
        "pointers": "Pointers",
        "exists": "Exists",
        "diagnostics": "Diagnostics",
    }.get(return_shape.lower(), return_shape)


def normalize_order_direction(order_direction):
    if not isinstance(order_direction, str):
        return order_direction
    return {
        "asc": "Ascending",
        "ascending": "Ascending",
        "desc": "Descending",
        "descending": "Descending",
    }.get(order_direction.lower(), order_direction)


def command_result(result, key):
    if key not in result:
        raise ValueError(f"Expected Wardrobe command result '{key}', got {result!r}")
    return result[key]
