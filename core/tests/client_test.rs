mod common;

use common::TempDatabase;
use serde_json::json;
use std::io::ErrorKind;
use std::path::Path;
use wardrobe_core::{DriverKind, WardrobeClient};

#[test]
fn client_direct_disk_path_delegates_to_embedded_engine() {
    let database = TempDatabase::new("client_direct_path_embedded");
    let connection = database.path.to_string_lossy().into_owned();
    let client = WardrobeClient::open(&connection).expect("client should open");

    assert_eq!(client.driver_kind(), DriverKind::Embedded);
    assert!(client.requires_embedded_engine());
    assert!(!client.uses_socket_transport());

    let pointer = client
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_client_fire",
                "element": "Fire"
            }),
        )
        .expect("embedded upsert should delegate to engine");
    assert_eq!(pointer, "@gem:client_fire");

    let records = client.find_all("gem").expect("records should read");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["element"], "Fire");
}

#[test]
fn client_local_uri_delegates_to_embedded_engine() {
    let database = TempDatabase::new("client_local_uri_embedded");
    let connection = format!("wardrobe://local/{}", database.path.display());
    let client = WardrobeClient::open(&connection).expect("client should open");

    assert_eq!(client.driver_kind(), DriverKind::Embedded);
    assert!(client.requires_embedded_engine());
    assert!(!client.uses_socket_transport());
}

#[test]
fn client_file_uri_delegates_to_embedded_engine() {
    let database = TempDatabase::new("client_file_uri_embedded");
    let connection = format!("wardrobe+file://{}", database.path.display());
    let client = WardrobeClient::open(&connection).expect("client should open");

    assert_eq!(client.driver_kind(), DriverKind::Embedded);
    assert!(client.requires_embedded_engine());
    assert!(!client.uses_socket_transport());

    client
        .upsert(
            "gem",
            json!({
                "_id": "client_file_uri_gem",
                "element": "Water"
            }),
        )
        .expect("embedded file-uri upsert should write locally");

    assert!(database.path.join("gem.drw").is_file());
}

#[test]
fn client_network_driver_is_selected_but_waits_for_protocol() {
    let client = WardrobeClient::open("wardrobe://localhost:24842").expect("client should open");

    assert_eq!(client.driver_kind(), DriverKind::Network);
    assert!(!client.requires_embedded_engine());
    assert!(client.uses_socket_transport());
    let error = client
        .find_all("gem")
        .expect_err("network driver should wait for protocol implementation");
    assert_eq!(error.kind(), ErrorKind::Unsupported);
}

#[test]
fn client_network_driver_does_not_initialize_local_storage() {
    let accidental_path = Path::new("localhost:24842");
    assert!(!accidental_path.exists());

    let client = WardrobeClient::open("wardrobe://localhost:24842").expect("client should open");

    assert_eq!(client.driver_kind(), DriverKind::Network);
    assert!(!accidental_path.exists());
}

#[test]
fn client_unix_socket_driver_is_selected_but_waits_for_protocol() {
    let client =
        WardrobeClient::open("wardrobe://unix/tmp/wardrobe.sock").expect("client should open");

    assert_eq!(client.driver_kind(), DriverKind::UnixSocket);
    assert!(!client.requires_embedded_engine());
    assert!(client.uses_socket_transport());
    let error = client
        .count("gem", None, None)
        .expect_err("socket driver should wait for protocol implementation");
    assert_eq!(error.kind(), ErrorKind::Unsupported);
}
