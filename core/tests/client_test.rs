mod common;

use common::TempDatabase;
use serde_json::json;
use std::io::ErrorKind;
use wardrobe_core::{DriverKind, WardrobeClient};

#[test]
fn client_direct_disk_path_delegates_to_embedded_engine() {
    let database = TempDatabase::new("client_direct_path_embedded");
    let connection = database.path.to_string_lossy().into_owned();
    let client = WardrobeClient::open(&connection).expect("client should open");

    assert_eq!(client.driver_kind(), DriverKind::Embedded);

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
}

#[test]
fn client_network_driver_is_selected_but_waits_for_protocol() {
    let client = WardrobeClient::open("wardrobe://localhost:24842").expect("client should open");

    assert_eq!(client.driver_kind(), DriverKind::Network);
    let error = client
        .find_all("gem")
        .expect_err("network driver should wait for protocol implementation");
    assert_eq!(error.kind(), ErrorKind::Unsupported);
}

#[test]
fn client_unix_socket_driver_is_selected_but_waits_for_protocol() {
    let client =
        WardrobeClient::open("wardrobe://unix/tmp/wardrobe.sock").expect("client should open");

    assert_eq!(client.driver_kind(), DriverKind::UnixSocket);
    let error = client
        .count("gem", None, None)
        .expect_err("socket driver should wait for protocol implementation");
    assert_eq!(error.kind(), ErrorKind::Unsupported);
}
