use std::collections::HashMap;

pub struct Recycler {
    alignment_bracket_size: usize,
    free_slot_registry: HashMap<usize, Vec<u64>>,
}

impl Recycler {
    pub fn new() -> Self {
        Self {
            alignment_bracket_size: 8,
            free_slot_registry: HashMap::new(),
        }
    }

    pub fn calculate_aligned_size(&self, raw_payload_bytes: usize) -> usize {
        let line_overhead_bytes = 1;
        let total_required_bytes = raw_payload_bytes + line_overhead_bytes;

        let remainder = total_required_bytes % self.alignment_bracket_size;
        if remainder == 0 {
            total_required_bytes
        } else {
            total_required_bytes + (self.alignment_bracket_size - remainder)
        }
    }

    pub fn register_free_slot(&mut self, slot_byte_size: usize, byte_offset: u64) {
        let offset_stack = self
            .free_slot_registry
            .entry(slot_byte_size)
            .or_insert_with(Vec::new);

        offset_stack.push(byte_offset);
    }

    pub fn pop_available_slot(&mut self, target_byte_size: usize) -> Option<u64> {
        if let Some(offset_stack) = self.free_slot_registry.get_mut(&target_byte_size) {
            return offset_stack.pop();
        }
        None
    }
}
