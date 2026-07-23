use serde_json::json;
use std::io;
use wardrobe_core::{
    Command, CommandResult, CreateRequest, OperationFilter, OperationOptions, OrderDirection,
    QueryModifiers, ReadResult, StatusRequest, StorageScope, WardrobeClient, WardrobeEngine,
};

const DATABASE_DIRECTORY: &str = "./wardrobe";
const DATABASE_NAME: &str = "publishing-house";
const SCHEMA_NAME: &str = "public";
const PERSON_DRAWER: &str = "person";
const PUBLISHER_DRAWER: &str = "publisher";
const BOOK_DRAWER: &str = "book";

fn print_separator(title: &str) {
    println!("\n==================================================");
    println!(">>> {}", title);
    println!("==================================================\n");
}

fn read_records(
    client: &WardrobeClient,
    filter: OperationFilter,
    options: impl Into<OperationOptions>,
) -> io::Result<Vec<serde_json::Value>> {
    match client.read(filter, options.into())? {
        ReadResult::Records(records) => Ok(records),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Expected record list, got {other:?}"),
        )),
    }
}

fn read_single_record(
    client: &WardrobeClient,
    filter: OperationFilter,
) -> io::Result<Option<serde_json::Value>> {
    match client.read(filter, OperationOptions::default())? {
        ReadResult::Record(record) => Ok(record),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Expected single record, got {other:?}"),
        )),
    }
}

fn extract_pointer(result: CommandResult) -> io::Result<String> {
    match result {
        CommandResult::Upsert(res) => res.into_pointers().into_iter().next().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "upsert returned no pointer")
        }),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Expected upsert result, got {other:?}"),
        )),
    }
}

fn main() -> io::Result<()> {
    let engine = WardrobeEngine::open(DATABASE_DIRECTORY)?;
    let metadata_client = WardrobeClient::open(DATABASE_DIRECTORY)?;
    let scope = StorageScope::schema(DATABASE_NAME, SCHEMA_NAME);

    print_separator("Phase 1: Metadata & Inventory Discovery");

    engine.create(CreateRequest::database(DATABASE_NAME))?;
    engine.create(CreateRequest::schema(DATABASE_NAME, SCHEMA_NAME))?;
    ensure_engine_drawer(&engine, PUBLISHER_DRAWER)?;
    ensure_engine_drawer(&engine, PERSON_DRAWER)?;
    ensure_engine_drawer(&engine, BOOK_DRAWER)?;

    let client = WardrobeClient::open(format!(
        "{DATABASE_DIRECTORY}/{DATABASE_NAME}/{SCHEMA_NAME}"
    ))?;

    let databases = metadata_client.status(StatusRequest::databases())?;
    println!("System Databases: {:?}", databases);

    let schemas = metadata_client.status(StatusRequest::schemas(DATABASE_NAME))?;
    println!("Available Schemas in '{}': {:?}", DATABASE_NAME, schemas);

    let drawers = metadata_client.status(StatusRequest::drawers(DATABASE_NAME, SCHEMA_NAME))?;
    println!("Drawers in publishing-house/public:");
    for drawer in drawers {
        println!(
            " - Drawer: {} ({} records)",
            drawer.name, drawer.record_count
        );
    }

    print_separator("Phase 2: Relational Data Population");
    let publisher_res = engine.execute_in_scope(
        scope.clone(),
        Command::Upsert {
            payload: json!({
                "_id": "pub_001",
                "name": "Apex Press",
                "founded_year": 1994,
                "active": true
            }),
            filter: OperationFilter::drawer(PUBLISHER_DRAWER),
            options: OperationOptions::default(),
        },
    )?;
    let publisher_pointer = extract_pointer(publisher_res)?;
    println!("Persisted publisher -> {}", publisher_pointer);

    let author_res = engine.execute_in_scope(
        scope.clone(),
        Command::Upsert {
            payload: json!({
                "_id": "author_001",
                "name": "Elena Vance",
                "role": "author",
                "genres": ["sci-fi", "thriller"]
            }),
            filter: OperationFilter::drawer(PERSON_DRAWER),
            options: OperationOptions::default(),
        },
    )?;
    let author_pointer = extract_pointer(author_res)?;
    println!("Persisted author (in person drawer) -> {}", author_pointer);

    let editor_res = engine.execute_in_scope(
        scope.clone(),
        Command::Upsert {
            payload: json!({
                "_id": "editor_001",
                "name": "Marcus Sterling",
                "role": "editor",
                "department": "fiction"
            }),
            filter: OperationFilter::drawer(PERSON_DRAWER),
            options: OperationOptions::default(),
        },
    )?;
    let editor_pointer = extract_pointer(editor_res)?;
    println!("Persisted editor (in person drawer) -> {}", editor_pointer);

    let book_res = engine.execute_in_scope(
        scope,
        Command::Upsert {
            payload: json!({
                "_id": "book_001",
                "title": "The Quantum Horizon",
                "publisher_id": publisher_pointer,
                "author_id": author_pointer,
                "editor_id": editor_pointer,
                "page_count": 420
            }),
            filter: OperationFilter::drawer(BOOK_DRAWER),
            options: OperationOptions::default(),
        },
    )?;
    let book_pointer = extract_pointer(book_res)?;
    println!("Persisted book -> {}", book_pointer);

    print_separator("Phase 3: Filter Query Execution");
    let query_modifiers = QueryModifiers {
        order_by: Some("name".to_string()),
        order_direction: Some(OrderDirection::Ascending),
        offset: Some(0),
        limit: Some(10),
    };

    let matching_personnel = read_records(
        &client,
        OperationFilter::query_in(
            PERSON_DRAWER,
            json!({
                "role": "author"
            }),
        ),
        query_modifiers,
    )?;

    println!(
        "Found {} matching personnel records:",
        matching_personnel.len()
    );
    for person in matching_personnel {
        println!("  - Match Found: {}", person);
    }

    print_separator("Phase 4: Relation Verification");
    let verified_book = read_single_record(&client, OperationFilter::pointer(&book_pointer))?;
    let verified_author = read_single_record(&client, OperationFilter::pointer(&author_pointer))?;
    let verified_editor = read_single_record(&client, OperationFilter::pointer(&editor_pointer))?;
    println!("Book lookup check: {:?}", verified_book.is_some());
    println!("Author lookup check: {:?}", verified_author.is_some());
    println!("Editor lookup check: {:?}", verified_editor.is_some());

    print_separator("Phase 5: Maintenance & Stress Test Cycle");
    let person_count = client.count(PERSON_DRAWER, None::<OperationOptions>)?;
    let book_count = client.count(BOOK_DRAWER, None::<OperationOptions>)?;
    println!(
        "Maintenance check: {} personnel, {} books active",
        person_count, book_count
    );

    for i in 0..5 {
        let temp_id = format!("temp_book_{i}");
        let res = client.upsert(
            json!({
                "_id": temp_id,
                "title": "Temporary Draft",
                "page_count": 100
            }),
            OperationFilter::drawer(BOOK_DRAWER),
            None::<OperationOptions>,
        )?;
        let ptr = res.into_pointers().into_iter().next().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "upsert returned no pointer")
        })?;
        client.delete(OperationFilter::pointer(&ptr), None::<OperationOptions>)?;
    }
    println!("Stress test cycle completed (5 temporary book upserts/deletes).");

    print_separator("Phase 6: Detailed Engine Inspection");
    let active_databases = metadata_client.status(StatusRequest::databases())?;
    for db in active_databases {
        println!("Inspecting Database: {}", db.name);
        let active_schemas = metadata_client.status(StatusRequest::schemas(&db.name))?;
        for schema_name in active_schemas {
            println!("  Schema: {}", schema_name);
            let active_drawers =
                metadata_client.status(StatusRequest::drawers(&db.name, &schema_name))?;
            for drawer in active_drawers {
                let count = client.count(&drawer.name, None::<OperationOptions>)?;
                println!("    Drawer: {} ({} records)", drawer.name, count);
            }
        }
    }

    print_separator("Phase 7: Final State Reconciliation & Integrity");
    let remaining_person = read_records(
        &client,
        OperationFilter::drawer(PERSON_DRAWER),
        None::<OperationOptions>,
    )?;
    let remaining_books = read_records(
        &client,
        OperationFilter::drawer(BOOK_DRAWER),
        None::<OperationOptions>,
    )?;
    println!(
        "Total active records: {} personnel (authors/editors), {} books",
        remaining_person.len(),
        remaining_books.len()
    );

    match read_single_record(&client, OperationFilter::pointer(&publisher_pointer))? {
        Some(pub_record) => println!(
            "INTEGRITY SUCCESS: Publisher record persists intact: {}",
            pub_record
        ),
        None => println!("INTEGRITY NOTE: Publisher record was not found."),
    }

    println!(
        "\nPublishing domain integration test suite executed successfully. All 7 phases completed."
    );
    Ok(())
}

fn ensure_engine_drawer(engine: &WardrobeEngine, drawer_name: &str) -> io::Result<()> {
    engine.create(CreateRequest::drawer(
        DATABASE_NAME,
        SCHEMA_NAME,
        drawer_name,
    ))?;
    Ok(())
}
