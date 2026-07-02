use std::future::Future;

fn assert_future<TFuture, TOutput>(_future: TFuture)
where
    TFuture: Future<Output = TOutput>,
{
}

#[test]
fn commands_module_exposes_wardrobe_command_futures() {
    assert_future(
        armoire_lib::commands::wardrobe::wardrobe_create_source_location(String::from(
            "target/armoire-command-module-test",
        )),
    );
    assert_future(armoire_lib::commands::wardrobe::wardrobe_test_database_access(
        String::from("target/armoire-command-module-test"),
    ));
}
