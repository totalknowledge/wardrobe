use wardrobe_core::Recycler;

#[test]
fn calculate_aligned_size_rounds_up_to_the_next_bracket() {
    let recycler = Recycler::new();

    assert_eq!(recycler.calculate_aligned_size(0), 8);
    assert_eq!(recycler.calculate_aligned_size(1), 8);
    assert_eq!(recycler.calculate_aligned_size(7), 8);
    assert_eq!(recycler.calculate_aligned_size(8), 16);
}

#[test]
fn register_and_pop_free_slots_are_lifo_per_size_class() {
    let mut recycler = Recycler::new();

    recycler.register_free_slot(16, 10);
    recycler.register_free_slot(16, 20);
    recycler.register_free_slot(24, 30);

    assert_eq!(recycler.pop_available_slot(16), Some(20));
    assert_eq!(recycler.pop_available_slot(16), Some(10));
    assert_eq!(recycler.pop_available_slot(16), None);
    assert_eq!(recycler.pop_available_slot(24), Some(30));
    assert_eq!(recycler.pop_available_slot(24), None);
}
