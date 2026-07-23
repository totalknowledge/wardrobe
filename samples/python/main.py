import json
import sys
from importlib.machinery import ExtensionFileLoader
from importlib.util import module_from_spec, spec_from_loader
from pathlib import Path


def load_embedded_binding():
    try:
        from wardrobe_embedded import WardrobeEmbedded

        return WardrobeEmbedded
    except ModuleNotFoundError as error:
        if error.name != "wardrobe_embedded":
            raise

    project_root = Path(__file__).resolve().parents[2]
    binding_root = project_root / "bindings/python/wardrobe-embedded"
    package_source = binding_root / "src"
    if sys.platform == "win32":
        library_name = "wardrobe_embedded_native.dll"
    elif sys.platform == "darwin":
        library_name = "libwardrobe_embedded_native.dylib"
    else:
        library_name = "libwardrobe_embedded_native.so"

    native_library = next(
        (
            candidate
            for profile in ("release", "debug")
            if (candidate := binding_root / "target" / profile / library_name).is_file()
        ),
        None,
    )
    if native_library is None:
        raise ModuleNotFoundError(
            "The local wardrobe_embedded native library is not built. Run "
            "'cargo build --release --manifest-path "
            "bindings/python/wardrobe-embedded/Cargo.toml' from the project root."
        )

    sys.path.insert(0, str(package_source))
    loader = ExtensionFileLoader("wardrobe_embedded._native", str(native_library))
    specification = spec_from_loader("wardrobe_embedded._native", loader)
    if specification is None:
        raise ImportError(f"Could not load native binding from {native_library}")
    native_module = module_from_spec(specification)
    sys.modules["wardrobe_embedded._native"] = native_module
    loader.exec_module(native_module)

    from wardrobe_embedded import WardrobeEmbedded

    return WardrobeEmbedded


WardrobeEmbedded = load_embedded_binding()


database_directory = "./wardrobe"
database_name = "publishing-house"
bay_name = "public_py"
publisher_drawer = "publisher"
person_drawer = "person"
book_drawer = "book"


def print_separator(title):
    print("\n==================================================")
    print(f">>> {title}")
    print("==================================================\n")


def first_pointer(result):
    if not isinstance(result, dict):
        raise ValueError("Upsert returned no pointer")
    pointers = result.get("Pointers", result.get("pointers"))
    if not isinstance(pointers, list) or not pointers or not isinstance(pointers[0], str):
        raise ValueError("Upsert returned no pointer")
    return pointers[0]


def records(result):
    if not isinstance(result, dict):
        raise ValueError("Expected record list")
    values = result.get("Records", result.get("records"))
    if not isinstance(values, list):
        raise ValueError("Expected record list")
    return values


def record(result):
    if not isinstance(result, dict):
        raise ValueError("Expected single record")
    if "Record" in result:
        return result["Record"]
    if "record" in result:
        return result["record"]
    raise ValueError("Expected single record")


def main():
    metadata_client = WardrobeEmbedded.open(database_directory)
    metadata_client.create({"Database": {"database_name": database_name}})
    metadata_client.create(
        {
            "Schema": {
                "database_name": database_name,
                "schema_name": bay_name,
            }
        }
    )
    for drawer_name in [publisher_drawer, person_drawer, book_drawer]:
        metadata_client.create(
            {
                "Drawer": {
                    "database_name": database_name,
                    "schema_name": bay_name,
                    "drawer_name": drawer_name,
                }
            }
        )

    wardrobe = WardrobeEmbedded.open(
        f"{database_directory}/{database_name}/{bay_name}"
    )

    print_separator("Phase 1: Metadata & Inventory Discovery")
    databases = metadata_client.status("Databases")
    bays = metadata_client.status(
        {"Schemas": {"database_name": database_name}}
    )
    drawers = metadata_client.status(
        {
            "Drawers": {
                "database_name": database_name,
                "schema_name": bay_name,
            }
        }
    )
    print("System Databases:", [database["name"] for database in databases])
    print(f"Available Bays in '{database_name}':", bays)
    print(f"Drawers in {database_name}/{bay_name}:")
    for drawer in drawers:
        print(f" - Drawer: {drawer['name']} ({drawer['record_count']} records)")

    print_separator("Phase 2: Relational Data Population")
    publisher_pointer = first_pointer(
        wardrobe.upsert(
            {
                "_id": "pub_001",
                "name": "Apex Press",
                "founded_year": 1994,
                "active": True,
            },
            publisher_drawer,
        )
    )
    print(f"Persisted publisher -> {publisher_pointer}")

    author_pointer = first_pointer(
        wardrobe.upsert(
            {
                "_id": "author_001",
                "name": "Elena Vance",
                "role": "author",
                "genres": ["sci-fi", "thriller"],
            },
            person_drawer,
        )
    )
    print(f"Persisted author (in person drawer) -> {author_pointer}")

    editor_pointer = first_pointer(
        wardrobe.upsert(
            {
                "_id": "editor_001",
                "name": "Marcus Sterling",
                "role": "editor",
                "department": "fiction",
            },
            person_drawer,
        )
    )
    print(f"Persisted editor (in person drawer) -> {editor_pointer}")

    book_pointer = first_pointer(
        wardrobe.upsert(
            {
                "_id": "book_001",
                "title": "The Quantum Horizon",
                "publisher_id": publisher_pointer,
                "author_id": author_pointer,
                "editor_id": editor_pointer,
                "page_count": 420,
            },
            book_drawer,
        )
    )
    print(f"Persisted book -> {book_pointer}")

    print_separator("Phase 3: Filter Query Execution")
    matching_personnel = records(
        wardrobe.read(
            [person_drawer, {"role": "author"}],
            {
                "order_by": "name",
                "order_direction": "asc",
                "offset": 0,
                "limit": 10,
            },
        )
    )
    print(f"Found {len(matching_personnel)} matching personnel records:")
    for person in matching_personnel:
        print(f"  - Match Found: {json.dumps(person)}")

    print_separator("Phase 4: Relation Verification")
    verified_book = record(wardrobe.read(book_pointer))
    verified_author = record(wardrobe.read(author_pointer))
    verified_editor = record(wardrobe.read(editor_pointer))
    print(f"Book lookup check: {str(verified_book is not None).lower()}")
    print(f"Author lookup check: {str(verified_author is not None).lower()}")
    print(f"Editor lookup check: {str(verified_editor is not None).lower()}")

    print_separator("Phase 5: Maintenance & Stress Test Cycle")
    person_count = wardrobe.count(person_drawer)
    book_count = wardrobe.count(book_drawer)
    print(f"Maintenance check: {person_count} personnel, {book_count} books active")
    for index in range(5):
        pointer = first_pointer(
            wardrobe.upsert(
                {
                    "_id": f"temp_book_{index}",
                    "title": "Temporary Draft",
                    "page_count": 100,
                },
                book_drawer,
            )
        )
        wardrobe.delete(pointer)
    print("Stress test cycle completed (5 temporary book upserts/deletes).")

    print_separator("Phase 6: Detailed Embedded Inspection")
    for drawer in drawers:
        count = wardrobe.count(drawer["name"])
        print(f"Drawer: {drawer['name']} ({count} records)")

    print_separator("Phase 7: Final State Reconciliation & Integrity")
    remaining_personnel = records(wardrobe.read(person_drawer))
    remaining_books = records(wardrobe.read(book_drawer))
    publisher = record(wardrobe.read(publisher_pointer))
    print(
        f"Total active records: {len(remaining_personnel)} personnel "
        f"(authors/editors), {len(remaining_books)} books"
    )
    if publisher is None:
        print("INTEGRITY NOTE: Publisher record was not found.")
    else:
        print(
            "INTEGRITY SUCCESS: Publisher record persists intact: "
            f"{json.dumps(publisher)}"
        )

    print(
        "\nPublishing domain integration sample completed successfully. "
        "All 7 phases completed."
    )


if __name__ == "__main__":
    main()
